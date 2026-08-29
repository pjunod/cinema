//! Separate-process M1b/M1c/M1d/M3a cluster validation.
//!
//! hiqlite owns process-global listener and shutdown state, so an in-process
//! three-client test cannot prove process loss. The controller starts this
//! executable three more times and drives each embedded client over a tiny
//! stdin/stdout protocol.
//!
//! The harness is a library with a thin binary on top. `cargo run -p
//! plurx-cluster-check -- check` still runs the identical three-voter
//! controller; the split exists so the crate's own tests can drive the same
//! protocol, request handlers, and validators against a one-voter cluster,
//! which needs no quorum and therefore no contended host.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::future::Future;
use std::io::Write as _;
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use hiqlite::macros::params;
use hiqlite::tls::ServerTlsConfig;
use hiqlite::{Client, Node, NodeConfig, Row};
use hiqlite_wal::inspection::{
    inspect_logs_dir, InspectedLogId, MetadataInspection, WalFileInspection,
};
use hmac::{Hmac, Mac};
use plurx_core::cluster::coordination::{Lease, LeaseClaim, StoreCoordinator};
use plurx_core::cluster::membership::{
    join_token_digest, ActivityPeerAuth, ActivitySigningKey, ArtworkPeerAuth, ClusterAvailability,
    ClusterPeer, ClusterProtocolStatus, ClusterRole, FinalizeJoinRequest, InternalPeerAuth,
    IssuedJoinToken, JoinSecrets, MembershipError, MembershipManager, MembershipStatus, NodeRole,
    ProtocolChange, RedeemJoinRequest,
};
use plurx_core::cluster::migration::status::{
    ReplicationHealth, ReplicationMonitor, ReplicationStatus,
};
use plurx_core::cluster::migration::{
    production_hiqlite_defaults_with_read_pool, ActivationMarker,
};
use plurx_core::cluster::ClusterIdentity;
use plurx_core::domain::{
    BookMetadataPatch, BookMetadataSource, ItemKind, ItemSort, LibraryKind, MetadataPatch, NewItem,
    NewLibrary, NewOfflinePackage, OfflineCreateOutcome, OfflineLeaseOutcome, PlaybackEvent,
    PlaybackEventQuery, ProbeResult, TraktAuth,
};
use plurx_core::error::StoreError;
use plurx_core::secrets::CredentialKey;
use plurx_core::store::{
    ApiKeyStore, ArtworkRepairFence, CatalogueReader, ClusterCompatibility, CoordinationStore,
    FencedPublicationStore, HiqliteAuthStore, LibraryStore, MediaStore, OfflinePackageStore,
    PlaybackTelemetryStore, ReconcileOutcome, RootFingerprintStatus, SettingsStore, TraktStore,
    TranscodeCacheStore, UserStore, WatchStore, WatchedOutboxStore, AUTH_PROTOCOL_MAX,
    AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_VERSION, AUTH_SCHEMA_VERSION,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, Notify};
use tokio::time::Instant as TokioInstant;

// Compile the production daemon policy directly. This avoids a test-only
// coalescer and keeps the already-governed plurxd source path as its sole home.
#[allow(dead_code)] // Response fields are consumed by HTTP, not this load driver.
#[path = "../../plurxd/src/progress.rs"]
mod production_progress;
use production_progress::ProgressCoalescer;

// Compile the production daemon lease lifecycle directly. A harness-only
// heartbeat would not prove that a paused provider job self-fences in plurxd.
#[allow(dead_code)] // Fixed production policy is used by plurxd; the harness injects a short one.
#[path = "../../plurxd/src/job_lease.rs"]
mod production_job_lease;
use production_job_lease::ActiveJobLease;

// Compile the production daemon serving fence directly. The partition proof
// below is a separate live process, but its readiness decision must remain the
// exact projection used by plurxd rather than a harness imitation.
#[allow(dead_code)]
#[path = "../../plurxd/src/serving_fence.rs"]
mod production_serving_fence;
use production_serving_fence::ServingFence;

mod failure_drills;
mod named_runner;
#[cfg(test)]
mod storage_evidence;
mod topology;
pub use failure_drills::{
    validate_failure_drill_artifact, ClusterFailureDrillArtifact,
    FAILURE_DRILL_ARTIFACT_SCHEMA_VERSION,
};
pub use named_runner::{
    claim_named_output, validate_named_campaign, validate_named_instrumentation_campaign,
    InstrumentationMetric, InstrumentationNodeAttestation, InstrumentationPairReference,
    InstrumentationTopologyAttestation, NamedInstrumentationArmArtifact,
    NamedInstrumentationCampaign, NamedRunnerConfig, NamedTopologyCampaign, NamedVoter,
    INSTRUMENTATION_CAMPAIGN_SCHEMA_VERSION, NAMED_CAMPAIGN_SCHEMA_VERSION,
};
pub use topology::{
    percentile_type7, run_topology_comparison, validate_topology_artifact, ClusterTopologyArtifact,
    NodeAppliedIndex, NodeCorpusObservation, ResourceSample, TopologyRun, TopologyWorkload,
    TOPOLOGY_ARTIFACT_SCHEMA_VERSION, TOPOLOGY_WRITE_OPERATIONS,
};

const RAFT_SECRET: &str = "plurx-m1b-raft-secret";
const API_SECRET: &str = "plurx-m1b-api-secret";
const OLD_WATERMARK_HANDLER_ENV: &str = "HQLITE_TEST_OLD_DB_QUORUM_WATERMARK_HANDLER";
const P3A_WATERMARK_HANDLER_ENV: &str = "HQLITE_TEST_P3A_DB_QUORUM_WATERMARK_HANDLER";
/// Read by `plurx-core`'s `cluster-validation` build to drop the
/// `learner_protocol_v5` capability write from its heartbeat. The name is
/// duplicated rather than exported because a production `plurx-core` does not
/// compile the constant at all.
const PRE_LEARNER_HEARTBEAT_ENV: &str = "PLURX_VALIDATION_PRE_LEARNER_HEARTBEAT";
const WATERMARK_STREAM_COMPAT_PROBE: &str = "SELECT 1 AS hiqlite_watermark_stream_compat_v1";
pub const INSTANCE_ID: &str = "m1b-cluster-check";
const START_TIMEOUT: Duration = Duration::from_secs(45);
/// A newly admitted learner may need to install the compacted state-machine
/// snapshot before Hiqlite can report the database healthy. Keep this aligned
/// with the learner catch-up proof later in the lifecycle scenario.
const LEARNER_START_TIMEOUT: Duration = Duration::from_secs(120);
/// Let the voter report its own typed startup timeout before the controller
/// gives up on the protocol stream at the same instant.
const START_RESPONSE_GRACE: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
/// The no-quorum write exercises the production exact-state retry envelope:
/// five three-second attempts with four 100 ms gaps. Give the voter enough
/// time to return its own bounded failure instead of severing the harness
/// response stream first; ordinary requests retain [`REQUEST_TIMEOUT`].
const WRITE_WITHOUT_QUORUM_RESPONSE_TIMEOUT: Duration = Duration::from_secs(17);
const COMPACTION_RESPONSE_TIMEOUT: Duration = Duration::from_secs(75);
/// The interface every harness listener binds. hiqlite composes each listener
/// from this and the port half of the node's own `addr_raft`/`addr_api`
/// (`hiqlite-0.14.0/src/start.rs:318`), which is why the two must agree.
const LISTEN_ADDR: &str = "127.0.0.1";
/// The phrase every port-collision verdict in this crate carries, and the one
/// [`is_port_collision`] recognises.
const PORT_COLLISION: &str = "port collision";
/// Exit status a voter uses when a listener lost its port. 98 is `EADDRINUSE`,
/// which is what the failed `bind` itself reported.
pub const BIND_FAILURE_EXIT: i32 = 98;
/// How many times a cluster start reallocates its ports before giving up.
const PORT_RETRY_ATTEMPTS: u32 = 5;
/// How long a voter waits for its own listeners to accept before it reports
/// them as never bound.
const LISTENER_PROOF_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the convergence helpers retry before reporting failure.
pub const CONVERGENCE_TIMEOUT: Duration = Duration::from_secs(45);
const SINGLETON_RESOURCE: &str = "provider:cluster-check-takeover";
const SINGLETON_PROOF_KEY: &str = "cluster-check.singleton-provider";
const SINGLETON_LEASE_TTL: Duration = Duration::from_secs(5);
const SINGLETON_HEARTBEAT: Duration = Duration::from_secs(4);
const SINGLETON_POST_BASELINE_COMMIT_BUDGET: u64 = 8;
const SINGLETON_TRIAL_ATTEMPTS: u32 = 3;
const SINGLETON_TRIAL_TIMEOUT: Duration = Duration::from_secs(120);
const SINGLETON_UNSTABLE_MARKER: &str = "CLUSTER_SINGLETON_UNSTABLE";
/// Incoming active-player heartbeats in the compacted-growth record.
pub const GROWTH_INCOMING_BEATS: u64 = 10_000;
/// Independent user/item streams represented by the growth load.
pub const GROWTH_ACTIVE_STREAMS: u64 = 80;
/// Logical playback cadence represented by each incoming heartbeat.
pub const GROWTH_BEAT_INTERVAL_SECS: u64 = 5;
/// Production progress coalescing window represented by the deterministic clock.
pub const GROWTH_COMMIT_WINDOW_SECS: u64 = 10;
/// Production hiqlite snapshot threshold used by the bounded gate.
pub const GROWTH_COMPACTION_LOGS: u64 = 10_000;
/// Fixed applied-log tail retained after each measured snapshot.
///
/// Snapshot construction is asynchronous. The load that triggers it can be a
/// few entries past the snapshot index by the time the controller observes
/// the new snapshot, which otherwise leaves a runner-speed-dependent SQLite
/// WAL tail in the directory-size comparison. Filling both sides to the same
/// post-snapshot index makes the physical comparison phase-identical without
/// changing the byte budget or excluding a durable file.
const GROWTH_SETTLED_LOG_TAIL: u64 = 512;
/// Maximum net compacted directory growth per incoming heartbeat.
pub const GROWTH_BYTES_PER_BEAT_BUDGET: u64 = 512;
/// Payload retained by each uncoalesced control write.
///
/// The former control repeatedly overwrote 80 progress rows. A compacted
/// database is expected to collapse those versions, so that workload could
/// prove the commit budget but not the independent retained-byte budget. One
/// bounded, unique setting per beat gives the negative control exactly the
/// high-cardinality retained state that the byte gate is meant to reject.
const GROWTH_RAW_VALUE_BYTES: usize = GROWTH_BYTES_PER_BEAT_BUDGET as usize + 128;
/// One extra commit window per stream above the deterministic cadence result.
pub const GROWTH_COMMIT_HEADROOM_PER_STREAM: u64 = 1;
/// Maximum accepted lag or internal-entry drift in the sampled applied index.
pub const GROWTH_APPLIED_INDEX_TOLERANCE: u64 = 2;

/// Result of one post-coalescer compacted-growth load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompactedGrowthReport {
    pub incoming_beats: u64,
    pub active_streams: u64,
    pub logical_span_seconds: u64,
    pub physical_progress_commits: u64,
    pub applied_index_delta: u64,
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub compacted_growth_bytes: u64,
}

/// Apply the checked-in write-amplification and compacted-growth budgets.
pub fn validate_compacted_growth(report: &CompactedGrowthReport) -> Result<()> {
    if report.incoming_beats == 0 || report.active_streams == 0 {
        bail!("compacted-growth load must contain beats and active streams");
    }
    if !report.incoming_beats.is_multiple_of(report.active_streams) {
        bail!("compacted-growth beats must divide evenly across active streams");
    }
    let beats_per_stream = report.incoming_beats / report.active_streams;
    let expected_span_seconds = beats_per_stream
        .saturating_sub(1)
        .saturating_mul(GROWTH_BEAT_INTERVAL_SECS);
    if report.logical_span_seconds != expected_span_seconds {
        bail!(
            "compacted-growth logical span {} did not match {} seconds for the declared load",
            report.logical_span_seconds,
            expected_span_seconds
        );
    }
    let commit_windows = report.logical_span_seconds / GROWTH_COMMIT_WINDOW_SECS + 1;
    let commit_budget = report
        .active_streams
        .saturating_mul(commit_windows.saturating_add(GROWTH_COMMIT_HEADROOM_PER_STREAM));
    let mut violations = Vec::new();
    if report.physical_progress_commits > commit_budget {
        violations.push(format!(
            "physical progress commits {} exceeded {} for {} active streams",
            report.physical_progress_commits, commit_budget, report.active_streams
        ));
    }
    let growth_budget = report
        .incoming_beats
        .saturating_mul(GROWTH_BYTES_PER_BEAT_BUDGET);
    if report.compacted_growth_bytes > growth_budget {
        violations.push(format!(
            "compacted growth {} bytes exceeded {} bytes for {} incoming beats",
            report.compacted_growth_bytes, growth_budget, report.incoming_beats
        ));
    }
    let applied_drift = report
        .applied_index_delta
        .abs_diff(report.physical_progress_commits);
    if applied_drift > GROWTH_APPLIED_INDEX_TOLERANCE {
        violations.push(format!(
            "applied-index delta {} drifted {} entries from {} physical progress commits",
            report.applied_index_delta, applied_drift, report.physical_progress_commits
        ));
    }
    if !violations.is_empty() {
        bail!(violations.join("; "));
    }
    Ok(())
}

/// Dispatch one harness mode from a full `argv`.
///
/// `main` passes `std::env::args()` straight through, so the argument
/// contract — including every rejection — is exercised by the crate's tests.
pub async fn run(args: Vec<String>) -> Result<()> {
    install_crypto_provider();
    match args.get(1).map(String::as_str) {
        None | Some("check") => {
            run_growth_subprocess().await?;
            controller().await
        }
        Some("membership") => run_membership_lifecycle_case().await,
        Some("learner") => run_learner_membership_case().await.map(|_| ()),
        Some("proxy-fixture") => failure_drills::run_proxy_fixture_command().await,
        Some("singleton") => run_singleton_takeover_case().await,
        Some("singleton-attempt") => run_singleton_takeover_attempt().await,
        Some("serving-partition") => run_serving_partition_case().await,
        Some("growth") => compacted_growth_gate(args.get(2).map(PathBuf::from)).await,
        Some("inspect-wal") => run_inspect_wal(&args[2..]),
        Some("topology") => {
            let output = args.get(2).map(PathBuf::from).unwrap_or_else(|| {
                PathBuf::from("target/validation/cluster-topology-semantic.json")
            });
            let order = topology::parse_topology_order(args.get(3).map(String::as_str))?;
            run_topology_comparison(&output, order).await
        }
        Some("build-identity") => {
            if args.get(2).is_some() {
                bail!("build-identity accepts no arguments");
            }
            named_runner::print_embedded_build_identity()
        }
        Some("topology-named") => {
            let config = args
                .get(2)
                .map(PathBuf::from)
                .context("topology-named requires a runner config JSON")?;
            let output = args
                .get(3)
                .map(PathBuf::from)
                .context("topology-named requires an output directory")?;
            let source_root = args
                .get(4)
                .map(PathBuf::from)
                .context("topology-named requires the controller source root")?;
            let owner_nonce = args
                .get(5)
                .context("topology-named requires the output owner nonce")?;
            if args.get(6).is_some() {
                bail!(
                    "topology-named accepts exactly config, output, source-root, and owner arguments"
                );
            }
            named_runner::run_named_campaign(&config, &output, &source_root, owner_nonce).await
        }
        Some("instrumentation-named") => {
            let config = args
                .get(2)
                .map(PathBuf::from)
                .context("instrumentation-named requires a runner config JSON")?;
            let output = args
                .get(3)
                .map(PathBuf::from)
                .context("instrumentation-named requires an output directory")?;
            let source_root = args
                .get(4)
                .map(PathBuf::from)
                .context("instrumentation-named requires the controller source root")?;
            let owner_nonce = args
                .get(5)
                .context("instrumentation-named requires the output owner nonce")?;
            if args.get(6).is_some() {
                bail!(
                    "instrumentation-named accepts exactly config, output, source-root, and owner arguments"
                );
            }
            named_runner::run_named_instrumentation_campaign(
                &config,
                &output,
                &source_root,
                owner_nonce,
            )
            .await
        }
        Some("topology-claim") => {
            let output = args
                .get(2)
                .map(PathBuf::from)
                .context("topology-claim requires an absent output directory")?;
            let owner_nonce = args
                .get(3)
                .context("topology-claim requires an owner nonce")?;
            if args.get(4).is_some() {
                bail!("topology-claim accepts exactly output and owner arguments");
            }
            claim_named_output(&output, owner_nonce)
        }
        Some("topology-campaign-validate") => {
            let path = args
                .get(2)
                .map(PathBuf::from)
                .context("topology-campaign-validate requires campaign.json")?;
            let campaign: NamedTopologyCampaign = serde_json::from_slice(
                &std::fs::read(&path)
                    .with_context(|| format!("read named campaign {}", path.display()))?,
            )?;
            let root = path
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            validate_named_campaign(&campaign, Some(root))
        }
        Some("instrumentation-campaign-validate") => {
            let path = args.get(2).map(PathBuf::from).context(
                "instrumentation-campaign-validate requires instrumentation-campaign.json",
            )?;
            let campaign: NamedInstrumentationCampaign =
                serde_json::from_slice(&std::fs::read(&path).with_context(|| {
                    format!("read named instrumentation campaign {}", path.display())
                })?)?;
            let root = path
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            validate_named_instrumentation_campaign(&campaign, Some(root))
        }
        Some("topology-cleanup") => {
            let path = args
                .get(2)
                .map(PathBuf::from)
                .context("topology-cleanup requires an active cleanup manifest")?;
            named_runner::cleanup_named_manifest(&path).await
        }
        Some("node") => {
            let launch: NodeLaunch =
                serde_json::from_str(args.get(2).context("node mode requires its launch JSON")?)?;
            node(launch).await
        }
        Some("node-hex") => {
            let encoded = args
                .get(2)
                .context("node-hex mode requires hex-encoded launch JSON")?;
            let bytes = hex::decode(encoded).context("decode node-hex launch JSON")?;
            let launch: NodeLaunch =
                serde_json::from_slice(&bytes).context("parse node-hex launch JSON")?;
            node(launch).await
        }
        Some("preflight") => {
            let preflight: Preflight = serde_json::from_str(
                args.get(2)
                    .context("preflight mode requires its launch JSON")?,
            )?;
            preflight_voter(preflight).await
        }
        Some("serving-node") => {
            let launch: ServingLaunch = serde_json::from_str(
                args.get(2)
                    .context("serving-node mode requires its launch JSON")?,
            )?;
            serving_node(launch).await
        }
        Some("media-child") => media_child().await,
        Some(other) => bail!("unknown cluster-check mode {other}"),
    }
}

const WAL_INSPECTION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
struct InspectWalArgs {
    hiqlite_dir: PathBuf,
    output: PathBuf,
}

#[derive(Debug, Serialize)]
struct HashedWalFileInspection {
    #[serde(flatten)]
    wal: WalFileInspection,
    sha256: String,
}

#[derive(Debug, Serialize)]
struct WalInspectionArtifact {
    schema_version: u32,
    metadata: MetadataInspection,
    wal_files: Vec<HashedWalFileInspection>,
    snapshot_current_pointer_present: bool,
    snapshot_database_present: bool,
    snapshot_last_log_id: Option<InspectedLogId>,
    local_state_machine_present: bool,
    local_state_machine_wal_present: bool,
    local_applied_log_id: Option<InspectedLogId>,
    invariant_verdicts: Vec<String>,
    observations: Vec<String>,
}

fn parse_inspect_wal_args(args: &[String]) -> Result<InspectWalArgs> {
    let mut hiqlite_dir = None;
    let mut output = None;
    let mut index = 0;
    while index < args.len() {
        let flag = &args[index];
        let value = args
            .get(index + 1)
            .with_context(|| format!("{flag} requires a path"))?;
        match flag.as_str() {
            "--hiqlite-dir" if hiqlite_dir.is_none() => {
                hiqlite_dir = Some(PathBuf::from(value));
            }
            "--output" if output.is_none() => {
                output = Some(PathBuf::from(value));
            }
            "--hiqlite-dir" | "--output" => bail!("duplicate inspect-wal flag {flag}"),
            _ => bail!("unknown inspect-wal flag {flag}"),
        }
        index += 2;
    }
    Ok(InspectWalArgs {
        hiqlite_dir: hiqlite_dir.context("inspect-wal requires --hiqlite-dir PATH")?,
        output: output.context("inspect-wal requires --output PATH")?,
    })
}

fn run_inspect_wal(args: &[String]) -> Result<()> {
    let args = parse_inspect_wal_args(args)?;
    let logs_dir = args.hiqlite_dir.join("logs");
    let report = inspect_logs_dir(&logs_dir).context("inspect stopped Hiqlite WAL")?;
    let mut verdicts = report
        .invariant_verdicts
        .into_iter()
        .filter(|verdict| verdict != "clean")
        .collect::<Vec<_>>();

    let mut wal_files = Vec::with_capacity(report.wal_files.len());
    for wal in report.wal_files {
        let sha256 = sha256_file(&logs_dir.join(&wal.file_name))?;
        wal_files.push(HashedWalFileInspection { wal, sha256 });
    }

    let local_db = args
        .hiqlite_dir
        .join("state_machine")
        .join("db")
        .join("plurx.db");
    let (local_state_machine_present, local_state_machine_wal_present, local_applied_log_id) =
        read_local_state_machine_boundary(&local_db)?;
    if !local_state_machine_present {
        push_unique(&mut verdicts, "local_state_machine_missing");
    }

    let snapshots_dir = args.hiqlite_dir.join("state_machine").join("snapshots");
    let pointer_path = snapshots_dir.join("current");
    let snapshot_current_pointer_present =
        regular_file_present(&pointer_path, "Hiqlite snapshot current pointer")?;
    let mut snapshot_database_present = false;
    let mut snapshot_last_log_id = None;
    if snapshot_current_pointer_present {
        let snapshot_id = std::fs::read_to_string(&pointer_path)
            .context("read Hiqlite snapshot current pointer")?;
        let snapshot_id = snapshot_id.trim();
        if snapshot_id.is_empty()
            || snapshot_id.contains('/')
            || snapshot_id.contains('\\')
            || snapshot_id == "."
            || snapshot_id == ".."
        {
            bail!("Hiqlite snapshot current pointer is not a safe filename");
        }
        (snapshot_database_present, snapshot_last_log_id) =
            read_immutable_state_machine_boundary(&snapshots_dir.join(snapshot_id))?;
        if !snapshot_database_present {
            push_unique(&mut verdicts, "snapshot_database_missing");
        }
    } else {
        push_unique(&mut verdicts, "snapshot_pointer_missing");
    }

    if let Some(purged) = report.metadata.last_purged_log_id {
        if snapshot_last_log_id.is_some_and(|snapshot| snapshot.index < purged.index) {
            push_unique(&mut verdicts, "snapshot_behind_purge_boundary");
        }
        if local_applied_log_id.is_some_and(|applied| applied.index < purged.index) {
            push_unique(&mut verdicts, "local_state_machine_behind_purge_boundary");
        }
    }
    if let (Some(snapshot), Some(first_wal)) = (
        snapshot_last_log_id,
        wal_files
            .iter()
            .find_map(|file| file.wal.first_decodable_log_id),
    ) {
        if first_wal.index > snapshot.index.saturating_add(1) {
            push_unique(&mut verdicts, "snapshot_wal_gap");
        }
    }
    if verdicts.is_empty() {
        verdicts.push("clean".to_owned());
    }

    let artifact = WalInspectionArtifact {
        schema_version: WAL_INSPECTION_SCHEMA_VERSION,
        metadata: report.metadata,
        wal_files,
        snapshot_current_pointer_present,
        snapshot_database_present,
        snapshot_last_log_id,
        local_state_machine_present,
        local_state_machine_wal_present,
        local_applied_log_id,
        invariant_verdicts: verdicts,
        observations: report.observations,
    };
    let mut json = serde_json::to_vec_pretty(&artifact)?;
    json.push(b'\n');
    if args.output == Path::new("-") {
        std::io::stdout().write_all(&json)?;
    } else {
        if let Some(parent) = args
            .output
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("create WAL inspection output parent {}", parent.display())
            })?;
        }
        std::fs::write(&args.output, json)
            .with_context(|| format!("write WAL inspection artifact {}", args.output.display()))?;
    }
    Ok(())
}

fn read_local_state_machine_boundary(path: &Path) -> Result<(bool, bool, Option<InspectedLogId>)> {
    if !regular_file_present(path, "Hiqlite local state machine")? {
        return Ok((false, false, None));
    }
    let wal_path = sqlite_sidecar_path(path, "-wal");
    let wal_present = regular_file_present(&wal_path, "Hiqlite state-machine WAL")?;
    if !wal_present {
        return Ok((true, false, read_sqlite_state_machine_boundary(path, true)?));
    }
    let scratch = tempfile::tempdir().context("create private state-machine inspection copy")?;
    let copied = scratch.path().join("state-machine.db");
    std::fs::copy(path, &copied).context("copy Hiqlite state machine for read-only inspection")?;
    std::fs::copy(&wal_path, sqlite_sidecar_path(&copied, "-wal"))
        .context("copy Hiqlite state-machine WAL for read-only inspection")?;
    let last_applied = read_sqlite_state_machine_boundary(&copied, false)?;
    Ok((true, wal_present, last_applied))
}

fn read_immutable_state_machine_boundary(path: &Path) -> Result<(bool, Option<InspectedLogId>)> {
    if !regular_file_present(path, "Hiqlite snapshot database")? {
        return Ok((false, None));
    }
    Ok((true, read_sqlite_state_machine_boundary(path, true)?))
}

fn read_sqlite_state_machine_boundary(
    path: &Path,
    immutable: bool,
) -> Result<Option<InspectedLogId>> {
    let (database, flags) = if immutable {
        let path = path
            .to_str()
            .context("Hiqlite state-machine path is not valid UTF-8")?;
        (
            format!("file:{}?immutable=1", sqlite_uri_path(path)),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )
    } else {
        (
            path.to_str()
                .context("Hiqlite state-machine path is not valid UTF-8")?
                .to_owned(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
    };
    let connection = Connection::open_with_flags(database, flags)
        .context("open Hiqlite state machine read-only")?;
    let bytes = connection
        .query_row("SELECT data FROM _metadata WHERE key = 'meta'", [], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .optional()
        .context("read Hiqlite state-machine metadata boundary")?;
    let last_applied = bytes
        .as_deref()
        .map(hiqlite_wal::inspection::decode_state_machine_last_applied)
        .transpose()
        .context("decode Hiqlite state-machine applied boundary")?
        .flatten();
    Ok(last_applied)
}

fn regular_file_present(path: &Path, label: &str) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => bail!("{label} is not a regular file: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {label}: {}", path.display())),
    }
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn sqlite_uri_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("open WAL for hashing {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_owned());
    }
}

async fn run_growth_subprocess() -> Result<()> {
    let root = tempfile::tempdir().context("compacted-growth subprocess data root")?;
    let executable = harness_executable()?;
    with_port_retry(|attempt| {
        let executable = executable.clone();
        let attempt_root = root.path().join(format!("attempt-{attempt}"));
        async move {
            std::fs::create_dir_all(&attempt_root)?;
            let mut command = Command::new(&executable);
            command
                .arg("growth")
                .arg(&attempt_root)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .kill_on_drop(true);
            let mut child = command
                .spawn()
                .context("spawn compacted-growth subprocess")?;
            let status = match tokio::time::timeout(Duration::from_secs(300), child.wait()).await {
                Ok(status) => status.context("wait for compacted-growth subprocess")?,
                Err(_) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    bail!("compacted-growth subprocess timed out after 300 seconds");
                }
            };
            if status.code() == Some(BIND_FAILURE_EXIT) {
                bail!("{PORT_COLLISION}: the compacted-growth voter lost one of its ports");
            }
            if !status.success() {
                bail!("compacted-growth subprocess exited {:?}", status.code());
            }
            Ok(())
        }
    })
    .await
}

async fn controller() -> Result<()> {
    let failure_drills_started_at = topology::unix_ms()?;
    println!("cluster-check: membership lifecycle 1 -> 3 -> 2");
    run_membership_lifecycle_case().await?;
    println!("cluster-check: current-leader self-leave");
    run_leader_self_leave_case().await?;
    println!("cluster-check: four-voter leader self-leave with one survivor down");
    run_degraded_four_voter_leader_self_leave_case().await?;
    println!("cluster-check: rolling quorum-watermark stream compatibility");
    run_quorum_watermark_rolling_compatibility_case().await?;
    println!("cluster-check: P3a three-column watermark compatibility");
    run_p3a_watermark_rolling_compatibility_case().await?;
    println!("cluster-check: bounded catalogue apply-pause and follower partition");
    run_bounded_catalogue_failure_case().await?;
    println!("cluster-check: three voters plus an admitted learner");
    let learner = run_learner_membership_case().await?;
    println!("cluster-check: paused singleton provider takeover");
    run_singleton_takeover_case().await?;
    println!("cluster-check: isolated serving-node readiness and media fence");
    run_serving_partition_case().await?;
    println!("cluster-check: follower loss and incompatible-voter guard");
    let follower_loss = run_failure_case(FailureTarget::Follower).await?;
    println!("cluster-check: leader loss");
    let leader_loss = run_failure_case(FailureTarget::Leader).await?;
    println!("cluster-check: sticky HLS proxy backend loss and unsafe mutation refusal");
    let proxy = failure_drills::run_proxy_fixture().await?;
    failure_drills::write_semantic_artifact(
        Path::new("target/validation/cluster-failure-drills.json"),
        failure_drills_started_at,
        learner,
        follower_loss,
        leader_loss,
        proxy,
    )?;
    println!("cluster-check: all M1b/M1c/M1d/M3/M3d/M4 serving contracts passed");
    Ok(())
}

struct ProviderFixture {
    url: String,
    calls: Arc<std::sync::atomic::AtomicU64>,
    events: mpsc::UnboundedReceiver<u64>,
    release_first: Arc<Notify>,
    release_second: Arc<Notify>,
    shutdown: tokio_util::sync::CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl ProviderFixture {
    async fn start() -> Result<Self> {
        let listener = tokio::net::TcpListener::bind((LISTEN_ADDR, 0))
            .await
            .context("bind singleton provider fixture")?;
        let url = format!("http://{}/provider", listener.local_addr()?);
        let calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (event_tx, events) = mpsc::unbounded_channel();
        let release_first = Arc::new(Notify::new());
        let release_second = Arc::new(Notify::new());
        let shutdown = tokio_util::sync::CancellationToken::new();
        let task_calls = Arc::clone(&calls);
        let task_release_first = Arc::clone(&release_first);
        let task_release_second = Arc::clone(&release_second);
        let task_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = task_shutdown.cancelled() => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((mut stream, _)) = accepted else {
                    break;
                };
                let calls = Arc::clone(&task_calls);
                let events = event_tx.clone();
                let release_first = Arc::clone(&task_release_first);
                let release_second = Arc::clone(&task_release_second);
                tokio::spawn(async move {
                    const MAX_REQUEST_BYTES: usize = 8 * 1024;
                    let mut request = Vec::with_capacity(1024);
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        if request.len() == MAX_REQUEST_BYTES {
                            let _ = stream
                                .write_all(b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                                .await;
                            return;
                        }
                        let mut chunk = [0_u8; 1024];
                        let remaining = MAX_REQUEST_BYTES - request.len();
                        let read_capacity = remaining.min(chunk.len());
                        let read = match tokio::time::timeout(
                            Duration::from_secs(5),
                            stream.read(&mut chunk[..read_capacity]),
                        )
                        .await
                        {
                            Ok(Ok(read)) => read,
                            _ => return,
                        };
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&chunk[..read]);
                    }
                    let valid_request = std::str::from_utf8(&request)
                        .ok()
                        .and_then(|request| request.lines().next())
                        .map(|line| {
                            let mut parts = line.split_whitespace();
                            parts.next() == Some("GET")
                                && parts.next() == Some("/provider")
                                && parts
                                    .next()
                                    .is_some_and(|version| version.starts_with("HTTP/1."))
                        })
                        .unwrap_or(false);
                    if !valid_request {
                        let _ = stream
                            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                        return;
                    }
                    let ordinal = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    let _ = events.send(ordinal);
                    match ordinal {
                        1 => release_first.notified().await,
                        2 => release_second.notified().await,
                        _ => {}
                    }
                    let (status, body) = match ordinal {
                        1 => ("200 OK", "stale-owner".to_owned()),
                        2 => ("200 OK", "successor".to_owned()),
                        other => ("500 Internal Server Error", format!("unexpected-{other}")),
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Ok(Self {
            url,
            calls,
            events,
            release_first,
            release_second,
            shutdown,
            task,
        })
    }

    async fn expect_call(&mut self, expected: u64) -> Result<()> {
        let observed = tokio::time::timeout(Duration::from_secs(15), self.events.recv())
            .await
            .context("singleton provider call timed out")?
            .context("singleton provider fixture stopped")?;
        if observed != expected {
            bail!("expected singleton provider call {expected}, observed {observed}");
        }
        Ok(())
    }

    fn call_count(&self) -> u64 {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn release_old_owner(&self) {
        self.release_first.notify_one();
    }

    fn release_successor(&self) {
        self.release_second.notify_one();
    }

    async fn shutdown(self) {
        self.shutdown.cancel();
        let _ = self.task.await;
    }
}

async fn run_singleton_takeover_case() -> Result<()> {
    let executable = harness_executable()?;
    for attempt in 1..=SINGLETON_TRIAL_ATTEMPTS {
        let output = run_singleton_trial_process(&executable).await?;
        std::io::stdout().write_all(&output.stdout)?;
        std::io::stderr().write_all(&output.stderr)?;
        if output.status.success() {
            return Ok(());
        }
        if !singleton_trial_was_unstable(&output.stdout) {
            bail!("singleton trial process exited with {}", output.status);
        }
        if attempt == SINGLETON_TRIAL_ATTEMPTS {
            bail!("singleton topology changed across all {SINGLETON_TRIAL_ATTEMPTS} fresh trials");
        }
        eprintln!(
            "cluster-check: singleton trial {attempt} had unrelated Raft topology churn; retrying a fresh cluster"
        );
    }
    unreachable!("singleton trial loop returns success or its last error")
}

async fn run_singleton_trial_process(executable: &Path) -> Result<std::process::Output> {
    let mut command = Command::new(executable);
    command
        .arg("singleton-attempt")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = command.spawn().context("spawn isolated singleton trial")?;
    let pid = child.id().context("isolated singleton trial has no pid")?;
    let mut stdout = child
        .stdout
        .take()
        .context("capture singleton trial stdout")?;
    let mut stderr = child
        .stderr
        .take()
        .context("capture singleton trial stderr")?;
    let stdout_reader = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
    });
    let stderr_reader = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
    });
    let status = match tokio::time::timeout(SINGLETON_TRIAL_TIMEOUT, child.wait()).await {
        Ok(status) => status.context("wait for isolated singleton trial")?,
        Err(_) => {
            let kill_result = kill_process_group(pid, "singleton trial timeout");
            let _ = child.wait().await;
            let _ = stdout_reader.await;
            let _ = stderr_reader.await;
            kill_result?;
            bail!(
                "isolated singleton trial exceeded its {:?} hard timeout",
                SINGLETON_TRIAL_TIMEOUT
            );
        }
    };
    let stdout = stdout_reader
        .await
        .context("join singleton stdout reader")??;
    let stderr = stderr_reader
        .await
        .context("join singleton stderr reader")??;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn singleton_trial_was_unstable(stdout: &[u8]) -> bool {
    String::from_utf8_lossy(stdout)
        .lines()
        .any(|line| line.starts_with(SINGLETON_UNSTABLE_MARKER))
}

async fn run_singleton_takeover_attempt() -> Result<()> {
    let executable = harness_executable()?;
    let root = tempfile::tempdir().context("singleton takeover data root")?;
    let mut cluster = with_port_retry(|attempt| {
        let reservation = allocate_nodes(3);
        let attempt_root = root.path().join(format!("attempt-{attempt}"));
        let executable = executable.clone();
        async move { ClusterProcesses::start(&executable, &attempt_root, reservation?).await }
    })
    .await?;
    cluster.request(1, Request::Bootstrap).await?.require_ok()?;
    for node_id in 2..=3 {
        cluster
            .request(node_id, Request::Open)
            .await?
            .require_ok()
            .with_context(|| format!("open initial learner-case voter {node_id}"))?;
    }
    cluster.wait_for_voters(&[1, 2, 3]).await?;

    let leader = cluster.leader().await?;
    let old_owner = (1..=3)
        .find(|node_id| *node_id != leader)
        .context("choose singleton follower owner")?;
    let peers = (1..=3)
        .filter(|node_id| *node_id != old_owner)
        .collect::<Vec<_>>();
    let (stable_leader, stable_term, _) = raft_position(&mut cluster, leader).await?;
    if stable_leader != Some(leader) {
        bail!("singleton proof leader changed before its initial boundary sample");
    }
    let mut provider = ProviderFixture::start().await?;

    let initial = start_singleton_probe(&mut cluster, old_owner, &provider.url).await?;
    if initial.owner_node_id != format!("node-{old_owner}") {
        bail!("singleton owner identity drifted: {initial:?}");
    }
    provider.expect_call(1).await?;
    for peer in &peers {
        match cluster
            .request(
                *peer,
                Request::StartSingletonProbe {
                    provider_url: provider.url.clone(),
                },
            )
            .await?
        {
            Response::SingletonProbeStart {
                acquired: false,
                lease,
            } if lease.owner_node_id == initial.owner_node_id
                && lease.fence == initial.fence
                && lease.revision >= initial.revision => {}
            response => bail!("peer {peer} did not observe the held singleton lease: {response:?}"),
        }
    }
    if provider.call_count() != 1 {
        bail!(
            "initial singleton contention made {} physical provider calls instead of one",
            provider.call_count()
        );
    }

    let paused = cluster.pause(old_owner)?;
    let authoritative_old = read_singleton_lease(&mut cluster, leader).await?;
    if authoritative_old.owner_node_id != initial.owner_node_id
        || authoritative_old.fence != initial.fence
        || authoritative_old.revision < initial.revision
    {
        bail!(
            "authoritative lease after SIGSTOP did not extend the initial owner: initial={initial:?} current={authoritative_old:?}"
        );
    }
    wait_until_lease_expired(&mut cluster, leader, &authoritative_old).await?;
    let (baseline_leader, baseline_term, applied_before) =
        raft_position(&mut cluster, leader).await?;
    if baseline_term != stable_term || baseline_leader != Some(leader) {
        bail!("pausing a follower changed leader/term before singleton takeover");
    }

    let (first_peer, second_peer) = (peers[0], peers[1]);
    let (first_response, second_response) = cluster
        .request_pair_concurrently(
            first_peer,
            Request::StartSingletonProbe {
                provider_url: provider.url.clone(),
            },
            second_peer,
            Request::StartSingletonProbe {
                provider_url: provider.url.clone(),
            },
        )
        .await?;
    let contenders = [(first_peer, first_response), (second_peer, second_response)];
    let winners = contenders
        .iter()
        .filter_map(|(node_id, response)| match response {
            Response::SingletonProbeStart {
                acquired: true,
                lease,
            } => Some((*node_id, lease.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    if winners.len() != 1 {
        bail!(
            "singleton takeover had {} winners: {contenders:?}",
            winners.len()
        );
    }
    let (successor_node, successor) = &winners[0];
    if successor.owner_node_id != format!("node-{successor_node}")
        || successor.fence != authoritative_old.fence + 1
        || successor.revision != authoritative_old.revision + 1
    {
        bail!(
            "singleton takeover did not advance the exact generation: old={authoritative_old:?} successor={successor:?}"
        );
    }
    for (node_id, response) in &contenders {
        if node_id == successor_node {
            continue;
        }
        match response {
            Response::SingletonProbeStart {
                acquired: false,
                lease,
            } if lease.owner_node_id == successor.owner_node_id
                && lease.fence == successor.fence
                && lease.revision >= successor.revision => {}
            response => bail!(
                "losing singleton contender {node_id} did not observe the successor fence: {response:?}"
            ),
        }
    }
    provider.expect_call(2).await?;
    provider.release_successor();
    wait_singleton_outcome(&mut cluster, *successor_node, &["published"]).await?;
    require_singleton_value(&mut cluster, leader, "successor", false).await?;

    // Keep the no-election invariant scoped to the takeover itself. A voter
    // resumed after being stopped longer than an election timeout can
    // legitimately increment the term before it receives a fresh heartbeat.
    // Applied indexes are global log positions, so the post-baseline budget
    // below can still include the resumed stale-token rejection even when that
    // scheduling race elects a new leader.
    let (takeover_leader, takeover_term, _) = raft_position(&mut cluster, leader).await?;
    if takeover_term != stable_term || takeover_leader != Some(leader) {
        bail!("singleton takeover changed leader/term before the paused voter resumed");
    }

    paused.resume()?;
    wait_singleton_outcome(&mut cluster, old_owner, &["lease_lost"]).await?;
    provider.release_old_owner();
    // SIGCONT can precede the resumed voter's remote Hiqlite client becoming
    // usable. Poll the existing read-only readiness query first, then dispatch
    // the stale mutation exactly once: retrying an ambiguous write timeout
    // could leave its detached server task running behind a later rejection.
    cluster.wait_for_ready(old_owner).await?;
    match cluster
        .request(
            old_owner,
            Request::ReplaySingletonLease {
                lease: authoritative_old.clone(),
            },
        )
        .await?
    {
        Response::Flag { value: false } => {}
        response => bail!("authoritative stale singleton rejection not observed: {response:?}"),
    }
    let cleanup_result = async {
        wait_singleton_cleanup(&mut cluster, *successor_node).await?;
        wait_singleton_cleanup(&mut cluster, old_owner).await
    }
    .await;
    if let Err(cleanup_error) = cleanup_result {
        // Cleanup remains strict in a stable term. If unrelated Raft churn
        // made the release outcome ambiguous, classify the whole fresh trial
        // as unstable so the parent can discard it instead of weakening the
        // lease-retirement assertion.
        let observed_leader = cluster.leader().await?;
        let (confirmed_leader, observed_term, _) =
            raft_position(&mut cluster, observed_leader).await?;
        if observed_term != stable_term
            || observed_leader != leader
            || confirmed_leader != Some(observed_leader)
        {
            provider.shutdown().await;
            cluster.shutdown_all().await?;
            println!(
                "{SINGLETON_UNSTABLE_MARKER} stage=cleanup expected_leader={leader} observed_leader={observed_leader} expected_term={stable_term} observed_term={observed_term}"
            );
            bail!("singleton proof changed leader/term during cleanup: {cleanup_error}");
        }
        return Err(cleanup_error);
    }

    for node_id in 1..=3 {
        require_singleton_value(&mut cluster, node_id, "successor", true).await?;
    }
    if provider.call_count() != 2 {
        bail!(
            "singleton pause/takeover made {} physical provider calls instead of two",
            provider.call_count()
        );
    }
    // Take the maximum applied position across all live voters. Sampling a
    // separately discovered leader can undercount just after an election: it
    // may contain the committed stale-token entry without having applied it
    // yet, while the former leader already did. A backward maximum is an
    // invalid proof, not a zero-entry interval.
    let applied_after = max_applied_index(&mut cluster, &[1, 2, 3]).await?;
    let delta = applied_after.checked_sub(applied_before).ok_or_else(|| {
        anyhow::anyhow!(
            "singleton applied index moved backward: before={applied_before} after={applied_after}"
        )
    })?;
    if delta > SINGLETON_POST_BASELINE_COMMIT_BUDGET {
        bail!(
            "singleton takeover consumed {delta} post-baseline Raft entries (budget {}): old={authoritative_old:?} successor={successor:?}",
            SINGLETON_POST_BASELINE_COMMIT_BUDGET
        );
    }
    println!(
        "CLUSTER_SINGLETON provider_calls=2 old_owner={old_owner} successor={successor_node} old_fence={} successor_fence={} applied_index_delta={delta} commit_budget={}",
        authoritative_old.fence,
        successor.fence,
        SINGLETON_POST_BASELINE_COMMIT_BUDGET
    );
    provider.shutdown().await;
    cluster.shutdown_all().await
}

const SERVING_PROOF_KEY: &str = "cluster-check.serving-partition-majority";
const SERVING_PARTITION_MAX_ATTEMPTS: usize = 3;

enum ServingPartitionAttempt {
    Complete,
    UnstableInitialAdmission(String),
}

/// A raw TCP cut-point in front of one voter API. It carries Hiqlite's TLS
/// bytes unchanged, and partitioning cancels every accepted connection as
/// well as refusing new ones. The voter itself remains reachable directly by
/// the controller, which separates a serving-node partition from voter loss.
struct TcpPartitionProxy {
    address: String,
    enabled: Arc<AtomicBool>,
    connections: Arc<std::sync::Mutex<tokio_util::sync::CancellationToken>>,
    shutdown: tokio_util::sync::CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl TcpPartitionProxy {
    async fn start(backend: String) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind((LISTEN_ADDR, 0))
            .await
            .context("bind serving partition proxy")?;
        let address = listener.local_addr()?.to_string();
        let enabled = Arc::new(AtomicBool::new(true));
        let connections = Arc::new(std::sync::Mutex::new(
            tokio_util::sync::CancellationToken::new(),
        ));
        let shutdown = tokio_util::sync::CancellationToken::new();
        let task_enabled = Arc::clone(&enabled);
        let task_connections = Arc::clone(&connections);
        let task_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    () = task_shutdown.cancelled() => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((mut downstream, _)) = accepted else {
                    break;
                };
                if !task_enabled.load(AtomicOrdering::Acquire) {
                    let _ = downstream.shutdown().await;
                    continue;
                }
                let connection_shutdown = task_connections
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                let backend = backend.clone();
                tokio::spawn(async move {
                    let upstream = tokio::select! {
                        () = connection_shutdown.cancelled() => return,
                        upstream = tokio::net::TcpStream::connect(&backend) => upstream,
                    };
                    let Ok(mut upstream) = upstream else {
                        return;
                    };
                    tokio::select! {
                        () = connection_shutdown.cancelled() => {}
                        _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream) => {}
                    }
                    let _ = downstream.shutdown().await;
                    let _ = upstream.shutdown().await;
                });
            }
        });
        Ok(Self {
            address,
            enabled,
            connections,
            shutdown,
            task,
        })
    }

    fn address(&self) -> String {
        self.address.clone()
    }

    fn partition(&self) {
        self.enabled.store(false, AtomicOrdering::Release);
        self.connections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .cancel();
    }

    fn restore(&self) {
        let mut connections = self
            .connections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *connections = tokio_util::sync::CancellationToken::new();
        self.enabled.store(true, AtomicOrdering::Release);
    }

    async fn stop(self) {
        self.enabled.store(false, AtomicOrdering::Release);
        self.connections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .cancel();
        self.shutdown.cancel();
        let _ = self.task.await;
    }
}

struct ServingProcess {
    child: Child,
    input: ChildStdin,
    http_base: String,
}

impl ServingProcess {
    async fn spawn(executable: &Path, proxy_addresses: Vec<String>) -> Result<Self> {
        let launch = ServingLaunch { proxy_addresses };
        let mut command = Command::new(executable);
        command
            .arg("serving-node")
            .arg(serde_json::to_string(&launch)?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command.spawn().context("spawn distinct serving node")?;
        let input = child.stdin.take().context("serving-node stdin")?;
        let mut output = BufReader::new(child.stdout.take().context("serving-node stdout")?);
        let mut line = String::new();
        let bytes = tokio::time::timeout(START_TIMEOUT, output.read_line(&mut line))
            .await
            .context("serving-node startup timed out")??;
        if bytes == 0 {
            bail!(
                "serving-node closed its startup stream: {:?}",
                child.try_wait()?
            );
        }
        let ready: ServingReady =
            serde_json::from_str(line.trim()).context("decode serving-node startup")?;
        Ok(Self {
            child,
            input,
            http_base: format!("http://{}", ready.http_address),
        })
    }

    async fn stop(self) -> Result<()> {
        let Self {
            mut child,
            input,
            http_base: _,
        } = self;
        drop(input);
        let status = tokio::time::timeout(START_TIMEOUT, child.wait())
            .await
            .context("serving-node did not stop after stdin closed")??;
        if !status.success() {
            bail!("serving-node exited with {status}");
        }
        Ok(())
    }
}

struct HttpObservation {
    body: String,
    retry_after: Option<String>,
}

async fn wait_serving_http(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    expected: reqwest::StatusCode,
) -> Result<HttpObservation> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let last = match client.get(format!("{base}{path}")).send().await {
            Ok(response) => {
                let status = response.status();
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let body = response.text().await.unwrap_or_default();
                if status == expected {
                    return Ok(HttpObservation { body, retry_after });
                }
                format!("HTTP {status}: {body}")
            }
            Err(error) => error.to_string(),
        };
        if Instant::now() >= deadline {
            bail!("{path} did not reach HTTP {expected}: {last}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn run_serving_partition_case() -> Result<()> {
    let mut unstable = Vec::new();
    for attempt in 1..=SERVING_PARTITION_MAX_ATTEMPTS {
        match run_serving_partition_attempt().await? {
            ServingPartitionAttempt::Complete => return Ok(()),
            ServingPartitionAttempt::UnstableInitialAdmission(error) => {
                println!(
                    "CLUSTER_SERVING_UNSTABLE stage=initial-admission attempt={attempt} error={error}"
                );
                unstable.push(format!("attempt {attempt}: {error}"));
            }
        }
    }
    bail!(
        "serving partition proof exhausted {SERVING_PARTITION_MAX_ATTEMPTS} fresh attempts after pre-proof admission instability: {}",
        unstable.join(" | ")
    )
}

async fn run_serving_partition_attempt() -> Result<ServingPartitionAttempt> {
    let executable = harness_executable()?;
    let root = tempfile::tempdir().context("serving partition data root")?;
    let (mut cluster, specs) = start_cluster_with_port_retry(&executable, root.path(), 3).await?;
    cluster.request(1, Request::Bootstrap).await?.require_ok()?;
    for node_id in 2..=3 {
        cluster
            .request(node_id, Request::Open)
            .await?
            .require_ok()?;
    }
    cluster.wait_for_voters(&[1, 2, 3]).await?;

    let mut proxies = Vec::with_capacity(specs.len());
    for spec in &specs {
        proxies.push(TcpPartitionProxy::start(spec.api.clone()).await?);
    }
    let serving = ServingProcess::spawn(
        &executable,
        proxies.iter().map(TcpPartitionProxy::address).collect(),
    )
    .await?;
    let http = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(2))
        .build()?;

    wait_serving_http(
        &http,
        &serving.http_base,
        "/readyz",
        reqwest::StatusCode::OK,
    )
    .await?;
    wait_serving_http(
        &http,
        &serving.http_base,
        "/api/v1/hls/probe/status",
        reqwest::StatusCode::OK,
    )
    .await?;
    if let Err(error) = admit_serving_media(&http, &serving.http_base).await {
        let error = format!("{error:#}").replace('\n', " ");
        cleanup_serving_partition(serving, proxies, &mut cluster)
            .await
            .context("clean up unstable initial serving admission")?;
        return Ok(ServingPartitionAttempt::UnstableInitialAdmission(error));
    }

    // Establish a real WebSocket through the first configured proxy, then
    // sever that accepted stream while later endpoints remain healthy. A
    // subsequent watermark must recover through the next per-stream cursor
    // position without a direct-voter path.
    let mut stable_drop_client = None;
    for _ in 0..3 {
        if cluster.leader().await? != 1 {
            cluster
                .request(1, Request::TriggerElection)
                .await?
                .require_ok()?;
            let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
            loop {
                if cluster.leader().await? == 1 {
                    break;
                }
                if Instant::now() >= deadline {
                    bail!("established-drop proof could not place leadership on voter 1");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }

        let before = raft_position(&mut cluster, 1).await?;
        let client = Client::remote(
            proxies.iter().map(TcpPartitionProxy::address).collect(),
            true,
            true,
            API_SECRET.to_owned(),
            true,
            None,
        )
        .await?;
        if tokio::time::timeout(START_TIMEOUT, client.wait_until_healthy_db())
            .await
            .is_err()
        {
            client.shutdown().await?;
            continue;
        }
        let watermark =
            match tokio::time::timeout(START_TIMEOUT, client.db_quorum_watermark()).await {
                Ok(Ok(watermark)) => watermark,
                Ok(Err(_)) | Err(_) => {
                    client.shutdown().await?;
                    continue;
                }
            };
        let after = match raft_position(&mut cluster, 1).await {
            Ok(after) => after,
            Err(error) => {
                client.shutdown().await?;
                return Err(error);
            }
        };
        if before.0 == Some(1)
            && after.0 == Some(1)
            && before.1 == after.1
            && watermark.leader_id == 1
            && watermark.term == before.1
        {
            stable_drop_client = Some(client);
            break;
        }
        client
            .shutdown()
            .await
            .context("close unstable established-drop client streams")?;
    }
    let drop_client = stable_drop_client
        .context("established-drop proof did not retain voter 1 leadership for stream setup")?;
    proxies[0].partition();
    cluster
        .request(2, Request::TriggerElection)
        .await?
        .require_ok()?;
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    loop {
        if cluster.leader().await? != 1 {
            break;
        }
        if Instant::now() >= deadline {
            proxies[0].restore();
            bail!("established-drop proof did not move leadership off voter 1");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let established_drop = tokio::time::timeout(CONVERGENCE_TIMEOUT, async {
        loop {
            if drop_client.db_quorum_watermark().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    proxies[0].restore();
    established_drop.context("configured proxy pool did not recover an established stream drop")?;
    drop_client
        .shutdown()
        .await
        .context("close established-drop remote client streams")?;
    wait_serving_http(
        &http,
        &serving.http_base,
        "/readyz",
        reqwest::StatusCode::OK,
    )
    .await?;
    admit_serving_media(&http, &serving.http_base).await?;

    // The cut-points are an authoritative proxy pool, not advertised voter
    // addresses. Suspend the current leader while every proxy remains
    // enabled, require the surviving majority to elect a successor, then
    // restore the old process. This makes the handoff deterministic while
    // proving recovery through another member of the configured pool.
    let former_leader = cluster.leader().await?;
    let eligible = (1..=3)
        .filter(|node_id| *node_id != former_leader)
        .collect::<Vec<_>>();
    let paused_leader = cluster.pause(former_leader)?;
    let successor = cluster.leader_among(&eligible).await;
    paused_leader
        .resume()
        .context("resume former serving-proof leader after deterministic handoff")?;
    let successor = successor.context("serving proxy pool did not observe majority failover")?;
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    wait_serving_http(
        &http,
        &serving.http_base,
        "/readyz",
        reqwest::StatusCode::OK,
    )
    .await?;
    wait_serving_http(
        &http,
        &serving.http_base,
        "/api/v1/hls/probe/status",
        reqwest::StatusCode::OK,
    )
    .await?;
    admit_serving_media(&http, &serving.http_base).await?;

    for proxy in &proxies {
        proxy.partition();
    }
    let fenced = wait_serving_http(
        &http,
        &serving.http_base,
        "/readyz",
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
    )
    .await?;
    if fenced.body != "quorum unavailable\n" {
        bail!(
            "serving-node readiness returned the wrong fence body: {}",
            fenced.body
        );
    }
    let liveness = wait_serving_http(
        &http,
        &serving.http_base,
        "/healthz",
        reqwest::StatusCode::OK,
    )
    .await?;
    if liveness.body != "ok\n" {
        bail!(
            "isolated serving-node lost process liveness: {}",
            liveness.body
        );
    }
    let capability = wait_serving_http(
        &http,
        &serving.http_base,
        "/api/v1/hls/probe/status",
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
    )
    .await?;
    if capability.retry_after.as_deref() != Some("1")
        || !capability.body.contains(r#""code":"serving_fenced""#)
        || capability.body.contains("127.0.0.1")
    {
        bail!(
            "mutable media fence response was not bounded and topology-free: {}",
            capability.body
        );
    }
    let child = wait_serving_http(
        &http,
        &serving.http_base,
        "/childz",
        reqwest::StatusCode::OK,
    )
    .await?;
    if !child.body.contains(r#""alive":false"#) {
        bail!(
            "serving-node media child survived quorum loss: {}",
            child.body
        );
    }

    let leader = cluster.leader().await?;
    cluster
        .request(
            leader,
            Request::PutSetting {
                key: SERVING_PROOF_KEY.to_owned(),
                value: "majority-committed".to_owned(),
            },
        )
        .await?
        .require_ok()?;
    for node_id in 1..=3 {
        wait_for_local_setting(
            &mut cluster,
            node_id,
            SERVING_PROOF_KEY,
            "majority-committed",
        )
        .await?;
    }

    for proxy in &proxies {
        proxy.restore();
    }
    wait_serving_http(
        &http,
        &serving.http_base,
        "/readyz",
        reqwest::StatusCode::OK,
    )
    .await?;
    wait_serving_http(
        &http,
        &serving.http_base,
        "/api/v1/hls/probe/status",
        reqwest::StatusCode::OK,
    )
    .await?;

    admit_serving_media(&http, &serving.http_base).await?;
    for proxy in &proxies {
        proxy.partition();
    }
    wait_serving_http(
        &http,
        &serving.http_base,
        "/readyz",
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
    )
    .await?;
    wait_serving_http(
        &http,
        &serving.http_base,
        "/api/v1/hls/probe/status",
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
    )
    .await?;
    let child = wait_serving_http(
        &http,
        &serving.http_base,
        "/childz",
        reqwest::StatusCode::OK,
    )
    .await?;
    if !child.body.contains(r#""alive":false"#) {
        bail!(
            "new-generation media child survived the second quorum loss: {}",
            child.body
        );
    }
    for proxy in &proxies {
        proxy.restore();
    }
    wait_serving_http(
        &http,
        &serving.http_base,
        "/readyz",
        reqwest::StatusCode::OK,
    )
    .await?;
    wait_serving_http(
        &http,
        &serving.http_base,
        "/api/v1/hls/probe/status",
        reqwest::StatusCode::OK,
    )
    .await?;

    println!(
        "CLUSTER_SERVING_PARTITION cycles=2 established_drop=recovered leader_failover={former_leader}->{successor} proxy_pool=recovered liveness=ok readiness=fenced capability=fenced child_killed=true majority_write=locally_converged recovery=ready"
    );
    cleanup_serving_partition(serving, proxies, &mut cluster).await?;
    Ok(ServingPartitionAttempt::Complete)
}

async fn cleanup_serving_partition(
    serving: ServingProcess,
    proxies: Vec<TcpPartitionProxy>,
    cluster: &mut ClusterProcesses,
) -> Result<()> {
    let mut errors = Vec::new();
    if let Err(error) = serving.stop().await {
        errors.push(format!("serving process: {error:#}"));
    }
    for proxy in proxies {
        proxy.stop().await;
    }
    if let Err(error) = cluster.shutdown_all().await {
        errors.push(format!("voters: {error:#}"));
        cluster.kill_all().await;
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("serving partition cleanup failed: {}", errors.join(" | "))
    }
}

async fn admit_serving_media(client: &reqwest::Client, base: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        wait_serving_http(client, base, "/readyz", reqwest::StatusCode::OK).await?;
        wait_serving_http(client, base, "/spawn-child", reqwest::StatusCode::OK).await?;
        let child = wait_serving_http(client, base, "/childz", reqwest::StatusCode::OK).await?;
        if child.body.contains(r#""alive":true"#) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "serving-node could not retain a current-generation media child: {}",
                child.body
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_local_setting(
    cluster: &mut ClusterProcesses,
    node_id: u64,
    key: &str,
    expected: &str,
) -> Result<()> {
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    loop {
        let response = cluster
            .request(
                node_id,
                Request::ReadLocalSetting {
                    key: key.to_owned(),
                },
            )
            .await?;
        let last = match response {
            Response::Setting { value } if value.as_deref() == Some(expected) => return Ok(()),
            response => format!("{response:?}"),
        };
        if Instant::now() >= deadline {
            bail!(
                "voter {node_id} did not apply setting {key:?}={expected:?} locally before the convergence deadline; last response: {last}"
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Wait until bootstrap's replicated membership schema is visible on a node.
///
/// Raft membership can converge before the bootstrap DDL has applied locally.
/// Opening the production membership manager in that window turns an ordinary
/// follower catch-up into a misleading `no such table` harness failure.
async fn wait_for_local_membership_schema(
    cluster: &mut ClusterProcesses,
    node_id: u64,
) -> Result<()> {
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    loop {
        let last = match cluster
            .request(node_id, Request::MembershipSchemaReady)
            .await?
        {
            Response::Flag { value: true } => return Ok(()),
            response => format!("{response:?}"),
        };
        if Instant::now() >= deadline {
            bail!(
                "voter {node_id} did not apply the bootstrap membership schema before the \
                 convergence deadline; last response: {last}"
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

struct MediaChild {
    child: Child,
    _input: ChildStdin,
    admitted_generation: u64,
    admission_id: u64,
}

async fn reap_media_child(mut media_child: MediaChild) {
    if media_child.child.try_wait().ok().flatten().is_none() {
        let _ = media_child.child.kill().await;
    }
    let _ = media_child.child.wait().await;
}

async fn kill_media_child(media: &Arc<tokio::sync::Mutex<Option<MediaChild>>>) {
    if let Some(media_child) = media.lock().await.take() {
        reap_media_child(media_child).await;
    }
}

async fn take_media_child_if_id(
    media: &Arc<tokio::sync::Mutex<Option<MediaChild>>>,
    admission_id: u64,
) -> Option<MediaChild> {
    let mut slot = media.lock().await;
    if slot
        .as_ref()
        .is_some_and(|child| child.admission_id == admission_id)
    {
        slot.take()
    } else {
        None
    }
}

async fn kill_media_child_if_id(
    media: &Arc<tokio::sync::Mutex<Option<MediaChild>>>,
    admission_id: u64,
) {
    if let Some(media_child) = take_media_child_if_id(media, admission_id).await {
        reap_media_child(media_child).await;
    }
}

/// The serving parent retains this process's stdin pipe. A normal shutdown or
/// an assertion-driven parent kill closes that pipe, so the stand-in cannot
/// leak beyond the process fixture even when the parent never reaches cleanup.
async fn media_child() -> Result<()> {
    let mut stdin = tokio::io::stdin();
    let mut buffer = [0_u8; 128];
    loop {
        match stdin.read(&mut buffer).await {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn spawn_media_child_process(admitted_generation: u64, admission_id: u64) -> Result<MediaChild> {
    let executable = harness_executable()?;
    let mut command = Command::new(executable);
    command
        .arg("media-child")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().context("spawn serving media child")?;
    let input = child
        .stdin
        .take()
        .context("retain serving media child stdin")?;
    Ok(MediaChild {
        child,
        _input: input,
        admitted_generation,
        admission_id,
    })
}

async fn serving_node(launch: ServingLaunch) -> Result<()> {
    install_crypto_provider();
    let client = Client::remote(
        launch.proxy_addresses,
        true,
        true,
        API_SECRET.to_owned(),
        true,
        None,
    )
    .await?;
    tokio::time::timeout(START_TIMEOUT, client.wait_until_healthy_db())
        .await
        .context("serving-node remote client health timed out")?;
    let replication = ReplicationMonitor::replicated_remote(client);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let passive = tokio::spawn(
        replication
            .clone()
            .passive_metrics_loop(shutdown.clone().cancelled_owned()),
    );
    let serving = ServingFence::new(replication.metrics_handle());
    let monitor = tokio::spawn(serving.clone().monitor_loop(shutdown.clone()));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !serving.is_ready() {
        if Instant::now() >= deadline {
            bail!("serving-node never acquired its initial quorum watermark");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let media: Arc<tokio::sync::Mutex<Option<MediaChild>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let next_media_id = Arc::new(AtomicU64::new(1));
    let mut serving_state = serving.subscribe();
    let fenced_media = Arc::clone(&media);
    let media_shutdown = shutdown.clone();
    let media_fence = tokio::spawn(async move {
        loop {
            let state = *serving_state.borrow_and_update();
            let media_child = {
                let mut slot = fenced_media.lock().await;
                if slot
                    .as_ref()
                    .is_some_and(|child| state.authority_lost_since(child.admitted_generation))
                {
                    slot.take()
                } else {
                    None
                }
            };
            if let Some(media_child) = media_child {
                reap_media_child(media_child).await;
            }
            tokio::select! {
                () = media_shutdown.cancelled() => break,
                changed = serving_state.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let listener = tokio::net::TcpListener::bind((LISTEN_ADDR, 0))
        .await
        .context("bind serving-node HTTP")?;
    let ready = ServingReady {
        http_address: listener.local_addr()?.to_string(),
    };
    let mut output = tokio::io::stdout();
    let mut bytes = serde_json::to_vec(&ready)?;
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    output.flush().await?;

    let stdin_shutdown = shutdown.clone();
    let stdin_task = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buffer = [0_u8; 128];
        loop {
            match stdin.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        stdin_shutdown.cancel();
    });
    loop {
        let accepted = tokio::select! {
            () = shutdown.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, _) = accepted.context("accept serving-node HTTP")?;
        let connection_serving = serving.clone();
        let connection_media = Arc::clone(&media);
        let connection_next_media_id = Arc::clone(&next_media_id);
        tokio::spawn(async move {
            let _ = serve_serving_http(
                stream,
                connection_serving,
                connection_media,
                connection_next_media_id,
            )
            .await;
        });
    }

    kill_media_child(&media).await;
    let _ = stdin_task.await;
    let _ = passive.await;
    let _ = monitor.await;
    let _ = media_fence.await;
    Ok(())
}

async fn serve_serving_http(
    mut stream: tokio::net::TcpStream,
    serving: ServingFence,
    media: Arc<tokio::sync::Mutex<Option<MediaChild>>>,
    next_media_id: Arc<AtomicU64>,
) -> Result<()> {
    const MAX_REQUEST_BYTES: usize = 8 * 1024;
    let mut request = Vec::with_capacity(1024);
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        if request.len() >= MAX_REQUEST_BYTES {
            return Ok(());
        }
        let mut chunk = [0_u8; 1024];
        let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut chunk)).await??;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(&chunk[..read]);
    }
    let path = std::str::from_utf8(&request)
        .ok()
        .and_then(|request| request.lines().next())
        .and_then(|line| {
            let mut parts = line.split_whitespace();
            (parts.next() == Some("GET"))
                .then(|| parts.next())
                .flatten()
        })
        .unwrap_or("/");
    let (status, reason, content_type, body, retry_after) =
        if let Some(policy) = serving.http_policy(path) {
            (
                policy.status,
                if policy.status == 200 {
                    "OK"
                } else {
                    "Service Unavailable"
                },
                policy.content_type,
                policy.body.to_owned(),
                policy.retry_after,
            )
        } else {
            match path {
                "/api/v1/hls/probe/status" => (
                    200,
                    "OK",
                    "application/json",
                    r#"{"code":"serving"}"#.to_owned(),
                    false,
                ),
                "/childz" => {
                    let mut media_child = media.lock().await;
                    if let Some(child) = media_child.as_mut() {
                        if !matches!(child.child.try_wait(), Ok(None)) {
                            *media_child = None;
                        }
                    }
                    let alive = media_child.is_some();
                    (
                        200,
                        "OK",
                        "application/json",
                        format!(r#"{{"alive":{alive}}}"#),
                        false,
                    )
                }
                "/spawn-child" => {
                    let admission = *serving.subscribe().borrow();
                    if !admission.ready {
                        return write_serving_http_response(
                            &mut stream,
                            503,
                            "Service Unavailable",
                            "application/json",
                            production_serving_fence::SERVING_FENCED_JSON,
                            true,
                        )
                        .await;
                    }
                    let mut media_child = media.lock().await;
                    if media_child.is_none() {
                        let admission_id = next_media_id.fetch_add(1, AtomicOrdering::Relaxed);
                        *media_child = Some(spawn_media_child_process(
                            admission.loss_generation,
                            admission_id,
                        )?);
                    }
                    let (admitted_generation, admission_id) = media_child
                        .as_ref()
                        .map(|child| (child.admitted_generation, child.admission_id))
                        .context("spawned media child disappeared")?;
                    drop(media_child);
                    let current = *serving.subscribe().borrow();
                    if current.authority_lost_since(admitted_generation) {
                        kill_media_child_if_id(&media, admission_id).await;
                        return write_serving_http_response(
                            &mut stream,
                            503,
                            "Service Unavailable",
                            "application/json",
                            production_serving_fence::SERVING_FENCED_JSON,
                            true,
                        )
                        .await;
                    }
                    (
                        200,
                        "OK",
                        "application/json",
                        r#"{"alive":true}"#.to_owned(),
                        false,
                    )
                }
                _ => (
                    404,
                    "Not Found",
                    "text/plain",
                    "not found\n".to_owned(),
                    false,
                ),
            }
        };
    write_serving_http_response(
        &mut stream,
        status,
        reason,
        content_type,
        &body,
        retry_after,
    )
    .await
}

async fn write_serving_http_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &str,
    retry_after: bool,
) -> Result<()> {
    let retry = if retry_after {
        "Retry-After: 1\r\n"
    } else {
        ""
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{retry}Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

async fn start_singleton_probe(
    cluster: &mut ClusterProcesses,
    node_id: u64,
    provider_url: &str,
) -> Result<Lease> {
    match cluster
        .request(
            node_id,
            Request::StartSingletonProbe {
                provider_url: provider_url.to_owned(),
            },
        )
        .await?
    {
        Response::SingletonProbeStart {
            acquired: true,
            lease,
        } => Ok(lease),
        response => bail!("node {node_id} did not acquire singleton probe: {response:?}"),
    }
}

async fn read_singleton_lease(cluster: &mut ClusterProcesses, node_id: u64) -> Result<Lease> {
    match cluster
        .request(node_id, Request::ReadSingletonLease)
        .await?
    {
        Response::SingletonLease { lease: Some(lease) } => Ok(lease),
        response => bail!("singleton lease row is absent: {response:?}"),
    }
}

async fn wait_until_lease_expired(
    cluster: &mut ClusterProcesses,
    node_id: u64,
    expected: &Lease,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let current = read_singleton_lease(cluster, node_id).await?;
        if &current != expected {
            bail!("paused owner changed its authoritative lease: expected={expected:?} current={current:?}");
        }
        let now_ms = unix_time_ms()?;
        if now_ms >= current.expires_at_unix_ms {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("paused singleton lease did not reach its authoritative expiry");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_singleton_outcome(
    cluster: &mut ClusterProcesses,
    node_id: u64,
    accepted: &[&str],
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match cluster
            .request(node_id, Request::SingletonProbeStatus)
            .await?
        {
            Response::SingletonProbeStatus { outcome, .. }
                if accepted.contains(&outcome.as_str()) =>
            {
                return Ok(outcome);
            }
            Response::SingletonProbeStatus { outcome, .. }
                if outcome == "provider_pending" || outcome == "publishing" =>
            {
                if Instant::now() >= deadline {
                    bail!(
                        "singleton probe on voter {node_id} timed out in outcome {outcome}; expected one of {accepted:?}"
                    );
                }
            }
            response => bail!("singleton probe reached an unexpected outcome: {response:?}"),
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_singleton_cleanup(cluster: &mut ClusterProcesses, node_id: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match cluster
            .request(node_id, Request::SingletonProbeStatus)
            .await?
        {
            Response::SingletonProbeStatus {
                cleanup_error: Some(error),
                ..
            } => {
                bail!("singleton probe cleanup on voter {node_id} was ambiguous: {error}");
            }
            Response::SingletonProbeStatus {
                cleanup_done: true, ..
            } => return Ok(()),
            Response::SingletonProbeStatus {
                outcome,
                cleanup_done: false,
                cleanup_error: None,
            } => {
                if Instant::now() >= deadline {
                    bail!(
                        "singleton probe cleanup on voter {node_id} timed out after outcome {outcome}"
                    );
                }
            }
            response => bail!("unexpected singleton cleanup response: {response:?}"),
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn require_singleton_value(
    cluster: &mut ClusterProcesses,
    node_id: u64,
    expected: &str,
    local: bool,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match cluster
            .request(node_id, Request::ReadSingletonValue { local })
            .await?
        {
            Response::SingletonValue { value } if value.as_deref() == Some(expected) => {
                return Ok(());
            }
            Response::SingletonValue { .. } => {}
            response => bail!("unexpected singleton setting response: {response:?}"),
        }
        if Instant::now() >= deadline {
            bail!("voter {node_id} did not converge on singleton value {expected}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn raft_position(
    cluster: &mut ClusterProcesses,
    node_id: u64,
) -> Result<(Option<u64>, u64, u64)> {
    match cluster.request(node_id, Request::Metrics).await? {
        Response::Metrics {
            leader,
            current_term,
            applied_index: Some(applied_index),
            ..
        } => Ok((leader, current_term, applied_index)),
        response => bail!("singleton proof could not sample Raft position: {response:?}"),
    }
}

async fn max_applied_index(cluster: &mut ClusterProcesses, nodes: &[u64]) -> Result<u64> {
    let mut maximum = None;
    for node_id in nodes {
        let (_, _, applied) = raft_position(cluster, *node_id).await?;
        maximum = Some(maximum.map_or(applied, |current: u64| current.max(applied)));
    }
    maximum.context("singleton proof had no live voter applied-index samples")
}

fn unix_time_ms() -> Result<i64> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("cluster-check clock precedes unix epoch")?
            .as_millis(),
    )
    .context("cluster-check unix millisecond clock overflowed")
}

/// Prove a new follower can talk to a leader that predates the watermark
/// marker. The old handler receives the real serialized consistent-query
/// request, returns SQLite's harmless syntax error, and then serves an
/// ordinary consistent query on that exact WebSocket connection.
async fn run_quorum_watermark_rolling_compatibility_case() -> Result<()> {
    let executable = harness_executable()?;
    let root = tempfile::tempdir().context("watermark rolling-compatibility data root")?;
    let mut cluster = with_port_retry(|attempt| {
        let reservation = allocate_nodes(3);
        let attempt_root = root.path().join(format!("attempt-{attempt}"));
        let executable = executable.clone();
        async move {
            ClusterProcesses::start_with_old_watermark_handler(
                &executable,
                &attempt_root,
                reservation?,
                1,
            )
            .await
        }
    })
    .await?;

    cluster.request(1, Request::Bootstrap).await?.require_ok()?;
    for node_id in 2..=3 {
        wait_for_local_membership_schema(&mut cluster, node_id).await?;
        cluster
            .request(node_id, Request::Open)
            .await?
            .require_ok()?;
    }
    cluster.wait_for_voters(&[1, 2, 3]).await?;
    if cluster.leader().await? != 1 {
        cluster
            .request(1, Request::TriggerElection)
            .await?
            .require_ok()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if cluster.leader().await? == 1 {
                break;
            }
            if Instant::now() >= deadline {
                bail!("old-handler voter 1 did not become compatibility leader");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    cluster
        .request(2, Request::ProveOldWatermarkStreamCompatibility)
        .await?
        .require_ok()?;
    cluster.shutdown_all().await
}

/// Prove a P3b follower preserves P3a's three-column quorum proof for serving
/// readiness while keeping bounded local reads disabled at protocol version 0.
async fn run_p3a_watermark_rolling_compatibility_case() -> Result<()> {
    let executable = harness_executable()?;
    let root = tempfile::tempdir().context("P3a watermark compatibility data root")?;
    let mut cluster = with_port_retry(|attempt| {
        let reservation = allocate_nodes(3);
        let attempt_root = root.path().join(format!("attempt-{attempt}"));
        let executable = executable.clone();
        async move {
            ClusterProcesses::start_with_p3a_watermark_handler(
                &executable,
                &attempt_root,
                reservation?,
                1,
            )
            .await
        }
    })
    .await?;

    cluster.request(1, Request::Bootstrap).await?.require_ok()?;
    for node_id in 2..=3 {
        cluster
            .request(node_id, Request::Open)
            .await?
            .require_ok()?;
    }
    cluster.wait_for_voters(&[1, 2, 3]).await?;
    if cluster.leader().await? != 1 {
        cluster
            .request(1, Request::TriggerElection)
            .await?
            .require_ok()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while cluster.leader().await? != 1 {
            if Instant::now() >= deadline {
                bail!("P3a-handler voter 1 did not become compatibility leader");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    cluster
        .request(2, Request::ProveP3aWatermarkCompatibility)
        .await?
        .require_ok()?;
    cluster.shutdown_all().await
}

#[derive(Debug)]
struct BoundedReadObservation {
    title: Option<String>,
    error: Option<String>,
    consistent_query_calls: u64,
    non_consistent_query_calls: u64,
    watermark_valid: bool,
    watermark_age_millis: Option<u64>,
    local_reads_supported: bool,
    apply_lag_entries: Option<u64>,
    serving_ready: bool,
}

async fn bounded_read(
    cluster: &mut ClusterProcesses,
    node_id: u64,
    item_id: i64,
) -> Result<BoundedReadObservation> {
    match cluster
        .request(node_id, Request::BoundedCatalogueGetItem { item_id })
        .await?
    {
        Response::BoundedCatalogueRead {
            title,
            error,
            consistent_query_calls,
            non_consistent_query_calls,
            watermark_valid,
            watermark_age_millis,
            local_reads_supported,
            apply_lag_entries,
            serving_ready,
        } => Ok(BoundedReadObservation {
            title,
            error,
            consistent_query_calls,
            non_consistent_query_calls,
            watermark_valid,
            watermark_age_millis,
            local_reads_supported,
            apply_lag_entries,
            serving_ready,
        }),
        response => bail!("bounded catalogue request returned {response:?}"),
    }
}

async fn wait_for_local_catalogue_read(
    cluster: &mut ClusterProcesses,
    node_id: u64,
    item_id: i64,
    expected_title: &str,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let observation = bounded_read(cluster, node_id, item_id).await?;
        if observation.error.is_none()
            && observation.title.as_deref() == Some(expected_title)
            && observation.consistent_query_calls == 0
            && observation.non_consistent_query_calls == 1
            && observation.watermark_valid
            && observation
                .watermark_age_millis
                .is_some_and(|age| age <= 250)
            && observation.local_reads_supported
            && observation.apply_lag_entries == Some(0)
            && observation.serving_ready
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("bounded catalogue local read did not recover: {observation:?}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Drive the P3 acceptance against three real voter processes. Apply pause is
/// below Raft's log and transport paths; the partition cuts this follower's
/// Raft and cluster-query transports while leaving the harness control pipe.
async fn run_bounded_catalogue_failure_case() -> Result<()> {
    let executable = harness_executable()?;
    let root = tempfile::tempdir().context("bounded catalogue failure data root")?;
    let mut cluster = with_port_retry(|attempt| {
        let reservation = allocate_nodes(3);
        let attempt_root = root.path().join(format!("attempt-{attempt}"));
        let executable = executable.clone();
        async move { ClusterProcesses::start(&executable, &attempt_root, reservation?).await }
    })
    .await?;

    cluster.request(1, Request::Bootstrap).await?.require_ok()?;
    for node_id in 2..=3 {
        cluster
            .request(node_id, Request::Open)
            .await?
            .require_ok()?;
    }
    cluster.wait_for_voters(&[1, 2, 3]).await?;
    let leader = cluster.leader().await?;
    let follower = [1_u64, 2, 3]
        .into_iter()
        .find(|node_id| *node_id != leader)
        .context("three-voter cluster had no follower")?;
    let item_id = match cluster
        .request(
            leader,
            Request::SeedArtworkRepairFenceItem { ordinal: 7_003 },
        )
        .await?
    {
        Response::ItemId { item_id } => item_id,
        response => bail!("bounded catalogue seed returned {response:?}"),
    };
    const EXPECTED_TITLE: &str = "Original artwork fence title";
    wait_for_local_catalogue_read(&mut cluster, follower, item_id, EXPECTED_TITLE).await?;

    cluster
        .request(follower, Request::PauseApply)
        .await?
        .require_ok()?;
    cluster
        .request(
            leader,
            Request::PutSetting {
                key: "bounded.apply-pause".to_owned(),
                value: "committed".to_owned(),
            },
        )
        .await?
        .require_ok()?;
    let pause_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match cluster
            .request(follower, Request::ApplyPauseObserved)
            .await?
        {
            Response::ApplyPauseObserved { observed: true } => break,
            Response::ApplyPauseObserved { observed: false } => {}
            response => bail!("apply-pause observation returned {response:?}"),
        }
        if Instant::now() >= pause_deadline {
            bail!("follower did not block inside its SQLite apply path");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let fallback_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let observation = bounded_read(&mut cluster, follower, item_id).await?;
        if observation.title.as_deref() == Some(EXPECTED_TITLE)
            && observation.error.is_none()
            && observation.non_consistent_query_calls == 0
            && observation.consistent_query_calls == 1
            && !observation.serving_ready
            && observation.apply_lag_entries.is_some_and(|lag| lag > 0)
        {
            break;
        }
        if Instant::now() >= fallback_deadline {
            bail!("apply-paused follower did not fall back and self-fence: {observation:?}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cluster
        .request(follower, Request::ResumeApply)
        .await?
        .require_ok()?;
    wait_for_local_catalogue_read(&mut cluster, follower, item_id, EXPECTED_TITLE).await?;

    cluster
        .request(follower, Request::SetRaftPartitioned { partitioned: true })
        .await?
        .require_ok()?;
    let partition_started = Instant::now();
    cluster
        .request(
            leader,
            Request::PutSetting {
                key: "bounded.partition-majority".to_owned(),
                value: "committed".to_owned(),
            },
        )
        .await?
        .require_ok()?;
    let before_expiry = bounded_read(&mut cluster, follower, item_id).await?;
    if before_expiry.error.is_some()
        || before_expiry.title.as_deref() != Some(EXPECTED_TITLE)
        || before_expiry.consistent_query_calls != 0
        || before_expiry.non_consistent_query_calls != 1
        || !before_expiry.watermark_valid
        || before_expiry.apply_lag_entries != Some(0)
    {
        bail!("partition precondition did not execute one zero-gap local read: {before_expiry:?}");
    }

    let isolated = loop {
        let observation = bounded_read(&mut cluster, follower, item_id).await?;
        // Proof expiry and the production serving-fence poll are deliberately
        // independent. The bounded reader can reject local SQL a few
        // milliseconds before the common fence publishes not-ready; observe
        // both states within the same 1.2 s contract instead of sampling the
        // transient gap as a failure.
        if observation.non_consistent_query_calls == 0 && !observation.serving_ready {
            break observation;
        }
        if partition_started.elapsed() >= Duration::from_millis(1_200) {
            bail!(
                "partitioned follower did not fully fail closed after the watermark lease: {observation:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    if partition_started.elapsed() >= Duration::from_millis(1_200)
        || isolated.consistent_query_calls != 1
        || isolated.error.is_none()
        || isolated.watermark_valid
        || isolated.serving_ready
    {
        bail!(
            "partitioned follower did not fail closed by the one-second lease: elapsed={:?}, observation={isolated:?}",
            partition_started.elapsed()
        );
    }

    cluster
        .request(follower, Request::SetRaftPartitioned { partitioned: false })
        .await?
        .require_ok()?;
    wait_for_local_catalogue_read(&mut cluster, follower, item_id, EXPECTED_TITLE).await?;
    cluster.shutdown_all().await
}

async fn run_membership_lifecycle_case() -> Result<()> {
    let executable = harness_executable()?;
    let root = tempfile::tempdir().context("membership lifecycle data root")?;
    let (mut cluster, specs, cluster_root) = with_port_retry(|attempt| {
        let executable = executable.clone();
        let attempt_root = root.path().join(format!("attempt-{attempt}"));
        async move {
            let (listeners, all_specs) = allocate_nodes(3)?.into_inner();
            let cluster = ClusterProcesses::start(
                &executable,
                &attempt_root,
                PortReservation {
                    listeners,
                    specs: vec![all_specs[0].clone()],
                },
            )
            .await?;
            Ok((cluster, all_specs, attempt_root))
        }
    })
    .await?;
    cluster
        .request(1, Request::SeedLegacyArtworkUrls)
        .await?
        .require_ok()?;
    cluster.request(1, Request::Bootstrap).await?.require_ok()?;

    let initial_dump = match cluster.request(1, Request::Dump).await? {
        Response::Dump { dump, .. } => dump,
        response => bail!("unexpected initial membership dump: {response:?}"),
    };
    require_dump_setting(&initial_dump, "instance.id", INSTANCE_ID)?;

    // A joiner must claim its HTTP origin before any staged membership row is
    // visible or voter promotion can start. Reuse the same token after the
    // refusal to prove the failed claim reserved neither the credential nor
    // an unusable second voter.
    let first_issued = match cluster
        .request(1, Request::IssueJoinToken { ttl_ms: 120_000 })
        .await?
    {
        Response::IssuedJoinToken { token } => token,
        response => bail!("unexpected duplicate-origin join token response: {response:?}"),
    };
    let first_spec = specs[1].clone();
    require_membership_error(
        cluster
            .request(
                1,
                Request::RedeemJoin {
                    request: RedeemJoinRequest {
                        token_digest: join_token_digest(&first_issued.token),
                        raft_id: first_issued.raft_id,
                        node_id: "node-1".to_owned(),
                        hostname: "identity-collision".to_owned(),
                        raft_address: first_spec.raft.clone(),
                        api_address: first_spec.api.clone(),
                        http_base: String::new(),
                        schema_version: AUTH_SCHEMA_VERSION,
                        protocol_version: AUTH_PROTOCOL_VERSION,
                        protocol_min: AUTH_PROTOCOL_MIN,
                        protocol_max: AUTH_PROTOCOL_MAX,
                    },
                },
            )
            .await?,
        "cluster_node_identity_in_use",
    )?;
    require_membership_error(
        cluster
            .request(
                1,
                Request::RedeemJoin {
                    request: RedeemJoinRequest {
                        token_digest: join_token_digest(&first_issued.token),
                        raft_id: first_issued.raft_id,
                        node_id: "node-2".to_owned(),
                        hostname: "cluster-node-2".to_owned(),
                        raft_address: first_spec.raft,
                        api_address: first_spec.api,
                        http_base: "http://127.0.0.1:33001".to_owned(),
                        schema_version: AUTH_SCHEMA_VERSION,
                        protocol_version: AUTH_PROTOCOL_VERSION,
                        protocol_min: AUTH_PROTOCOL_MIN,
                        protocol_max: AUTH_PROTOCOL_MAX,
                    },
                },
            )
            .await?,
        "cluster_http_endpoint_in_use",
    )?;
    let after_duplicate = match cluster.request(1, Request::MembershipStatus).await? {
        Response::MembershipStatus { status } => status,
        response => bail!("unexpected post-duplicate membership status: {response:?}"),
    };
    if after_duplicate.availability != ClusterAvailability::SingleNode
        || after_duplicate.nodes.len() != 1
    {
        bail!("duplicate origin changed singleton membership: {after_duplicate:?}");
    }
    cluster
        .request(
            1,
            Request::SeedLegacyPartialRedemption {
                token_digest: join_token_digest(&first_issued.token),
                node_id: "node-2".to_owned(),
            },
        )
        .await?
        .require_ok()?;

    let mut first_issued = Some(first_issued);
    let mut first_redeemed = None;
    for node_id in 2..=3 {
        let issued = match first_issued.take().filter(|_| node_id == 2) {
            Some(issued) => issued,
            None => match cluster
                .request(1, Request::IssueJoinToken { ttl_ms: 120_000 })
                .await?
            {
                Response::IssuedJoinToken { token } => token,
                response => bail!("unexpected join-token response: {response:?}"),
            },
        };
        if issued.raft_id != node_id {
            bail!(
                "join token assigned raft id {}, expected {node_id}",
                issued.raft_id
            );
        }
        let spec = specs[(node_id - 1) as usize].clone();
        let token_digest = join_token_digest(&issued.token);
        if node_id == 3 {
            cluster
                .request(
                    1,
                    Request::SeedLegacyPartialRedemption {
                        token_digest: token_digest.clone(),
                        node_id: "node-3".to_owned(),
                    },
                )
                .await?
                .require_ok()?;
        }
        let request = RedeemJoinRequest {
            token_digest: token_digest.clone(),
            raft_id: issued.raft_id,
            node_id: format!("node-{node_id}"),
            hostname: format!("cluster-node-{node_id}"),
            raft_address: spec.raft,
            api_address: spec.api,
            http_base: format!("http://127.0.0.1:{}", 33_000 + node_id),
            schema_version: AUTH_SCHEMA_VERSION,
            protocol_version: AUTH_PROTOCOL_VERSION,
            protocol_min: AUTH_PROTOCOL_MIN,
            protocol_max: AUTH_PROTOCOL_MAX,
        };
        if node_id == 2 {
            let mut no_http_resume = request.clone();
            no_http_resume.http_base.clear();
            cluster
                .request(
                    1,
                    Request::RedeemJoin {
                        request: no_http_resume,
                    },
                )
                .await?
                .require_ok()?;
        }
        cluster
            .request(
                1,
                Request::RedeemJoin {
                    request: request.clone(),
                },
            )
            .await?
            .require_ok()?;
        if node_id == 2 {
            cluster
                .request(
                    1,
                    Request::SeedHistoricalHttpDuplicate {
                        node_id: "historical-node".to_owned(),
                        public_http_url: request.http_base.clone(),
                    },
                )
                .await?
                .require_ok()?;
        }
        let mut changed_origin = request.clone();
        changed_origin.http_base = format!("http://127.0.0.1:{}", 34_000 + node_id);
        require_membership_error(
            cluster
                .request(
                    1,
                    Request::RedeemJoin {
                        request: changed_origin,
                    },
                )
                .await?,
            "cluster_http_endpoint_in_use",
        )?;
        cluster
            .spawn_node(
                &executable,
                NodeLaunch::voter(
                    node_id,
                    cluster_root.clone(),
                    specs[..node_id as usize].to_vec(),
                ),
            )
            .await?;
        cluster
            .request(node_id, Request::Open)
            .await?
            .require_ok()?;
        cluster
            .wait_for_voters(&(1..=node_id).collect::<Vec<_>>())
            .await?;
        cluster
            .request(
                1,
                Request::FinalizeJoin {
                    request: FinalizeJoinRequest {
                        token_digest,
                        raft_id: issued.raft_id,
                        node_id: request.node_id.clone(),
                    },
                },
            )
            .await?
            .require_ok()?;
        if node_id == 2 {
            first_redeemed = Some((request, issued.token));
        }
    }

    let (request, redeemed_token) = first_redeemed.context("missing first redeemed token")?;
    require_membership_error(
        cluster.request(1, Request::RedeemJoin { request }).await?,
        "join_token_reused",
    )?;
    let expired = match cluster
        .request(1, Request::IssueJoinToken { ttl_ms: 1 })
        .await?
    {
        Response::IssuedJoinToken { token } => token,
        response => bail!("unexpected expired-token issue response: {response:?}"),
    };
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    require_membership_error(
        cluster
            .request(
                1,
                Request::RedeemJoin {
                    request: RedeemJoinRequest {
                        token_digest: join_token_digest(&expired.token),
                        raft_id: expired.raft_id,
                        node_id: "expired-candidate".to_owned(),
                        hostname: "expired-host".to_owned(),
                        raft_address: "127.0.0.1:1".to_owned(),
                        api_address: "127.0.0.1:2".to_owned(),
                        http_base: "http://127.0.0.1:3".to_owned(),
                        schema_version: AUTH_SCHEMA_VERSION,
                        protocol_version: AUTH_PROTOCOL_VERSION,
                        protocol_min: AUTH_PROTOCOL_MIN,
                        protocol_max: AUTH_PROTOCOL_MAX,
                    },
                },
            )
            .await?,
        "join_token_expired",
    )?;
    let leader = cluster.leader().await?;
    let status = match cluster.request(1, Request::MembershipStatus).await? {
        Response::MembershipStatus { status } => status,
        response => bail!("unexpected three-voter membership status: {response:?}"),
    };
    if status.availability != ClusterAvailability::HighAvailability
        || status.nodes.len() != 3
        || status.nodes.iter().any(|node| !node.reachable)
        || status.nodes.iter().any(|node| node.hostname.is_empty())
        || status.nodes.iter().any(|node| node.hostname.contains('.'))
        || status
            .nodes
            .iter()
            .any(|node| node.advertised_host != "localhost")
        || status.nodes.iter().filter(|node| node.is_leader).count() != 1
        || !status
            .nodes
            .iter()
            .any(|node| node.raft_id == leader && node.is_leader)
        || status.replication.health != ReplicationHealth::InSync
    {
        bail!("three-voter membership status was not healthy: {status:?}");
    }
    prove_cluster_discovery(&mut cluster).await?;
    let activity_peers = match cluster.request(1, Request::ActivityPeers).await? {
        Response::ActivityPeers { peers } => peers,
        response => bail!("unexpected activity-peer response: {response:?}"),
    };
    if activity_peers.len() != 2
        || activity_peers
            .iter()
            .any(|(_, endpoint, reachable)| endpoint.is_none() || !reachable)
    {
        bail!("cluster-internal activity endpoints were incomplete: {activity_peers:?}");
    }
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let auth = match cluster
        .request(
            1,
            Request::SignActivityRequest {
                target_node_id: "node-2".to_owned(),
                timestamp_ms: now,
            },
        )
        .await?
    {
        Response::ActivityAuth { auth } => auth,
        response => bail!("unexpected activity signature response: {response:?}"),
    };
    match cluster
        .request(2, Request::AuthorizeActivityRequest { auth: auth.clone() })
        .await?
    {
        Response::Flag { value: true } => {}
        response => bail!("peer rejected shared cluster activity authority: {response:?}"),
    }
    match cluster
        .request(3, Request::AuthorizeActivityRequest { auth: auth.clone() })
        .await?
    {
        Response::Flag { value: false } => {}
        response => bail!("cross-target activity proof was accepted: {response:?}"),
    }
    let stale_time = now - 30_001;
    let stale_auth = match cluster
        .request(
            1,
            Request::SignActivityRequest {
                target_node_id: "node-2".to_owned(),
                timestamp_ms: stale_time,
            },
        )
        .await?
    {
        Response::ActivityAuth { auth } => auth,
        response => bail!("unexpected stale activity signature response: {response:?}"),
    };
    let mut forged_auth = auth;
    forged_auth.signature = "00".repeat(32);
    for request in [
        Request::AuthorizeActivityRequest { auth: forged_auth },
        Request::AuthorizeActivityRequest { auth: stale_auth },
    ] {
        match cluster.request(2, request).await? {
            Response::Flag { value: false } => {}
            response => bail!("invalid activity authority was accepted: {response:?}"),
        }
    }
    let offer_body = br#"{"protocol_version":1,"request_fingerprint":"fixture"}"#.to_vec();
    let exact_auth = match cluster
        .request(
            1,
            Request::SignInternalPeerRequest {
                target_node_id: "node-2".to_owned(),
                timestamp_ms: now,
                nonce: "123e4567-e89b-42d3-a456-426614174000".to_owned(),
                method: "POST".to_owned(),
                path: "/internal/v1/media/offers".to_owned(),
                body: offer_body.clone(),
            },
        )
        .await?
    {
        Response::InternalPeerAuth { auth } => auth,
        response => bail!("unexpected exact peer signature response: {response:?}"),
    };
    match cluster
        .request(
            2,
            Request::AuthorizeInternalPeerRequest {
                auth: exact_auth.clone(),
                method: "POST".to_owned(),
                path: "/internal/v1/media/offers".to_owned(),
                body: offer_body.clone(),
            },
        )
        .await?
    {
        Response::Flag { value: true } => {}
        response => bail!("peer rejected exact internal authority: {response:?}"),
    }
    let stale_exact = match cluster
        .request(
            1,
            Request::SignInternalPeerRequest {
                target_node_id: "node-2".to_owned(),
                timestamp_ms: stale_time,
                nonce: "123e4567-e89b-42d3-a456-426614174001".to_owned(),
                method: "POST".to_owned(),
                path: "/internal/v1/media/offers".to_owned(),
                body: offer_body.clone(),
            },
        )
        .await?
    {
        Response::InternalPeerAuth { auth } => auth,
        response => bail!("unexpected stale exact signature response: {response:?}"),
    };
    let mut forged_exact = exact_auth.clone();
    forged_exact.signature = "00".repeat(64);
    for (target, auth, method, path, body) in [
        (
            2,
            forged_exact,
            "POST",
            "/internal/v1/media/offers",
            offer_body.clone(),
        ),
        (
            2,
            stale_exact,
            "POST",
            "/internal/v1/media/offers",
            offer_body.clone(),
        ),
        (
            2,
            exact_auth.clone(),
            "GET",
            "/internal/v1/media/offers",
            offer_body.clone(),
        ),
        (
            2,
            exact_auth.clone(),
            "POST",
            "/internal/v1/media/snapshot",
            offer_body.clone(),
        ),
        (
            2,
            exact_auth.clone(),
            "POST",
            "/internal/v1/media/offers",
            b"mutated".to_vec(),
        ),
        (
            3,
            exact_auth,
            "POST",
            "/internal/v1/media/offers",
            offer_body,
        ),
    ] {
        match cluster
            .request(
                target,
                Request::AuthorizeInternalPeerRequest {
                    auth,
                    method: method.to_owned(),
                    path: path.to_owned(),
                    body,
                },
            )
            .await?
        {
            Response::Flag { value: false } => {}
            response => bail!("invalid exact internal authority was accepted: {response:?}"),
        }
    }
    let public_status = serde_json::to_string(&status)?;
    if public_status.contains(&redeemed_token)
        || public_status.contains("api_address")
        || public_status.contains("raft_address")
        || public_status.contains(":3240")
    {
        bail!("public node records exposed token or listener-port material");
    }
    require_membership_error_message(
        cluster
            .request(
                2,
                Request::RejectDuplicateArtworkUrl {
                    public_http_url: "http://127.0.0.1:33001".to_owned(),
                },
            )
            .await?,
        "membership_internal",
        "conflicts with an existing node claim",
    )?;
    for node_id in 1..=3 {
        let urls = match cluster.request(node_id, Request::ArtworkPeerUrls).await? {
            Response::ArtworkPeerUrls { urls } => urls,
            response => bail!("unexpected artwork peer roster: {response:?}"),
        };
        let expected = (1..=3)
            .filter(|peer| *peer != node_id)
            .map(|peer| format!("http://127.0.0.1:{}", 33_000 + peer))
            .collect::<BTreeSet<_>>();
        if urls.into_iter().collect::<BTreeSet<_>>() != expected {
            bail!("node {node_id} did not learn every peer's public artwork URL");
        }
    }

    let before_heartbeats = quorum_watermark_observation(&mut cluster, leader).await?;
    if before_heartbeats.leader_id != leader {
        bail!("heartbeat budget began on a non-leader watermark: {before_heartbeats:?}");
    }
    cluster
        .request(leader, Request::Heartbeat)
        .await?
        .require_ok()?;
    cluster
        .request(leader, Request::Heartbeat)
        .await?
        .require_ok()?;
    let after_heartbeats = quorum_watermark_observation(&mut cluster, leader).await?;
    if after_heartbeats.leader_id != leader || after_heartbeats.term != before_heartbeats.term {
        bail!(
            "heartbeat budget crossed a leader term: before={before_heartbeats:?} after={after_heartbeats:?}"
        );
    }
    let heartbeat_entries = after_heartbeats
        .committed_index
        .saturating_sub(before_heartbeats.committed_index);
    if heartbeat_entries != 1 {
        bail!(
            "two duplicate liveness heartbeats consumed {heartbeat_entries} Raft entries instead of one"
        );
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    cluster
        .request(leader, Request::Heartbeat)
        .await?
        .require_ok()?;
    let after_heartbeat_window = quorum_watermark_observation(&mut cluster, leader).await?;
    if after_heartbeat_window.leader_id != leader
        || after_heartbeat_window.term != after_heartbeats.term
    {
        bail!(
            "heartbeat window control crossed a leader term: before={after_heartbeats:?} \
             after={after_heartbeat_window:?}"
        );
    }
    let post_window_entries = after_heartbeat_window
        .committed_index
        .saturating_sub(after_heartbeats.committed_index);
    if post_window_entries != 1 {
        bail!(
            "a heartbeat after the duplicate window consumed {post_window_entries} Raft entries instead of one"
        );
    }

    // Four reconciliation loops must not turn one persistent miss into one
    // Raft write per node. Only the current leader submits the claim; after a
    // live election, its successor waits a receiver-local monotonic lease and
    // then fences the old term without consulting either host's wall clock.
    // Process scheduling plus consistent reads can exceed hundreds of
    // milliseconds on a loaded CI host. Keep the active-lease proof far from
    // its deadline; the explicit sleeps below are the only expiry trigger.
    let repair_lease_ms = 5_000_u64;
    let repair_item = match cluster
        .request(leader, Request::SeedArtworkRepairFenceItem { ordinal: 1 })
        .await?
    {
        Response::ItemId { item_id } => item_id,
        response => bail!("unexpected artwork fence fixture response: {response:?}"),
    };
    let paused_claim_item = match cluster
        .request(leader, Request::SeedArtworkRepairFenceItem { ordinal: 2 })
        .await?
    {
        Response::ItemId { item_id } => item_id,
        response => bail!("unexpected paused-claim fixture response: {response:?}"),
    };
    let wrong_target_item = match cluster
        .request(leader, Request::SeedArtworkRepairFenceItem { ordinal: 3 })
        .await?
    {
        Response::ItemId { item_id } => item_id,
        response => bail!("unexpected cross-item fence fixture response: {response:?}"),
    };
    let before_repair = match cluster.request(leader, Request::Metrics).await? {
        Response::Metrics { applied_index, .. } => {
            applied_index.context("repair budget missing initial applied index")?
        }
        response => bail!("unexpected pre-repair metrics: {response:?}"),
    };
    let mut first_winners = Vec::new();
    let mut initial_fence = None;
    for node_id in 1..=3 {
        if node_id == leader {
            match cluster
                .request(
                    node_id,
                    Request::ClaimArtworkRepairFence {
                        item_id: repair_item,
                        lease_ms: repair_lease_ms,
                        inject_leader_change: false,
                    },
                )
                .await?
            {
                Response::ArtworkRepairFence { fence: Some(fence) } => {
                    first_winners.push(node_id);
                    initial_fence = Some(fence);
                }
                Response::ArtworkRepairFence { fence: None } => {}
                response => bail!("unexpected initial artwork fence response: {response:?}"),
            }
        } else {
            match cluster
                .request(
                    node_id,
                    Request::ClaimArtworkRepair {
                        item_id: repair_item,
                        lease_ms: repair_lease_ms,
                    },
                )
                .await?
            {
                Response::Flag { value: true } => first_winners.push(node_id),
                Response::Flag { value: false } => {}
                response => bail!("unexpected artwork repair claim response: {response:?}"),
            }
        }
    }
    if first_winners != [leader] {
        bail!("artwork repair authority was not exactly leader {leader}: {first_winners:?}");
    }
    let after_repair = match cluster.request(leader, Request::Metrics).await? {
        Response::Metrics { applied_index, .. } => {
            applied_index.context("repair budget missing final applied index")?
        }
        response => bail!("unexpected post-repair metrics: {response:?}"),
    };
    if after_repair.saturating_sub(before_repair) != 1 {
        bail!(
            "one artwork repair contention round consumed {} Raft entries instead of one",
            after_repair.saturating_sub(before_repair)
        );
    }
    let held_index = after_repair;
    for node_id in 1..=3 {
        match cluster
            .request(
                node_id,
                Request::ClaimArtworkRepair {
                    item_id: repair_item,
                    lease_ms: repair_lease_ms,
                },
            )
            .await?
        {
            Response::Flag { value: false } => {}
            response => bail!("an active repair lease was reclaimed: {response:?}"),
        }
    }
    let held_after = match cluster.request(leader, Request::Metrics).await? {
        Response::Metrics { applied_index, .. } => {
            applied_index.context("held repair lease missing applied index")?
        }
        response => bail!("unexpected held-lease metrics: {response:?}"),
    };
    if held_after != held_index {
        bail!("failed repair retries wrote inside the active lease");
    }
    tokio::time::sleep(Duration::from_millis(repair_lease_ms + 30)).await;
    let initial_term = u64::try_from(
        initial_fence
            .as_ref()
            .context("initial leader did not return its repair fence")?
            .leader_term,
    )
    .context("initial artwork repair term was negative")?;
    let stable_claim_deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    let old_fence = loop {
        match cluster.request(leader, Request::Metrics).await? {
            Response::Metrics {
                leader: Some(current_leader),
                current_term,
                quorum_acknowledged: true,
                ..
            } if current_leader == leader && current_term == initial_term => {}
            Response::Metrics {
                leader: Some(current_leader),
                current_term,
                ..
            } if current_leader == leader && current_term == initial_term => {
                // The lease has expired, but loaded runners can briefly miss
                // the production one-second quorum-freshness window. Wait for
                // a current acknowledgement before attempting the CAS.
                if Instant::now() >= stable_claim_deadline {
                    bail!("stable-term artwork re-repair never regained a fresh quorum lease");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            Response::Metrics { .. } => {
                bail!("artwork repair topology changed during the stable-term lease proof")
            }
            response => bail!("unexpected stable-term repair metrics response: {response:?}"),
        }
        if let Some(fence) = claim_artwork_repair_fence_once(
            &mut cluster,
            leader,
            repair_item,
            repair_lease_ms,
            false,
        )
        .await?
        {
            if u64::try_from(fence.leader_term).ok() != Some(initial_term) {
                bail!("artwork repair term changed during the stable-term generation CAS");
            }
            break fence;
        }
        // `None` explicitly proves the server reached no generation CAS. A
        // quorum acknowledgement can age between the preflight and the claim,
        // so only this confirmed no-op is safe to retry.
        if Instant::now() >= stable_claim_deadline {
            bail!("stable-term artwork re-repair stayed fenced after the bounded retry window");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let same_term_after = match cluster.request(leader, Request::Metrics).await? {
        Response::Metrics {
            applied_index,
            leader: Some(current_leader),
            current_term,
            ..
        } if current_leader == leader && current_term == initial_term => {
            applied_index.context("stable-term repair reuse missing applied index")?
        }
        Response::Metrics { .. } => {
            bail!("artwork repair topology changed during the stable-term generation CAS")
        }
        response => bail!("unexpected stable-term repair metrics: {response:?}"),
    };
    if same_term_after.saturating_sub(held_index) != 1 {
        bail!(
            "stable-term artwork re-repair consumed {} Raft entries instead of one fenced generation",
            same_term_after.saturating_sub(held_index)
        );
    }
    match cluster
        .request(
            leader,
            Request::ApplyArtworkRepairFence {
                target_item_id: repair_item,
                fence: initial_fence.context("initial leader did not return its repair fence")?,
                title: "stale same-term write".to_owned(),
                stale_job_lease: false,
                stale_book_snapshot: false,
            },
        )
        .await?
    {
        Response::ArtworkFenceApply {
            setting: false,
            metadata: false,
            book: false,
        } => {}
        response => bail!("an old same-term artwork generation remained valid: {response:?}"),
    }
    match cluster
        .request(
            leader,
            Request::ClaimArtworkRepairAfterPause {
                item_id: paused_claim_item,
                lease_ms: 100,
                pause_ms: 150,
            },
        )
        .await?
    {
        Response::Flag { value: false } => {}
        response => bail!("expired post-CAS artwork claim could begin work: {response:?}"),
    }
    let (paused_fence, _) =
        read_artwork_repair_observation(&mut cluster, leader, paused_claim_item).await?;
    let paused_fence =
        paused_fence.context("expired post-CAS artwork claim never committed its fence")?;
    if paused_fence.generation != 1 || paused_fence.owner_node_id != format!("node-{leader}") {
        bail!("expired post-CAS artwork claim committed the wrong fence: {paused_fence:?}");
    }
    let election_target = (1..=3)
        .find(|node_id| *node_id != leader)
        .context("choose artwork leadership successor")?;
    cluster
        .request(election_target, Request::TriggerElection)
        .await?
        .require_ok()?;
    let handoff_deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    let repair_lease = Duration::from_millis(repair_lease_ms);
    let mut force_term_change_during_wait = true;
    let (handoff, handoff_term) = loop {
        let observed =
            observe_artwork_repair_successor(&mut cluster, leader, repair_item, handoff_deadline)
                .await?;
        let observation_confirmed_at = Instant::now();
        if observation_confirmed_at + repair_lease + Duration::from_millis(30) >= handoff_deadline {
            bail!("artwork repair successor did not retain one stable lease window");
        }
        let forced_this_window = force_term_change_during_wait;
        if forced_this_window {
            tokio::time::sleep(repair_lease / 2).await;
            let target = (1..=3)
                .find(|node_id| *node_id != observed.0 && *node_id != leader)
                .context("artwork repair proof has no third election target")?;
            cluster
                .request(target, Request::TriggerElection)
                .await?
                .require_ok()?;
            force_term_change_during_wait = false;
            tokio::time::sleep(repair_lease / 2 + Duration::from_millis(30)).await;
        } else {
            tokio::time::sleep(repair_lease + Duration::from_millis(30)).await;
        }
        match cluster.request(observed.0, Request::Metrics).await? {
            Response::Metrics {
                leader: Some(current_leader),
                current_term,
                quorum_acknowledged: true,
                ..
            } if current_leader == observed.0 && current_term == observed.1 => {
                if forced_this_window {
                    bail!("forced artwork repair election did not change the observed term");
                }
                if observation_confirmed_at.elapsed() < repair_lease {
                    bail!("artwork repair successor did not wait one complete monotonic lease");
                }
            }
            Response::Metrics { .. } => {
                // A new term invalidates the receiver-local observation. The
                // newly confirmed successor must observe the durable fence and
                // wait its own complete monotonic lease before any CAS.
            }
            response => bail!("unexpected post-wait successor metrics response: {response:?}"),
        }

        let Some((before_repeat_fence, before_repeat_index)) =
            try_read_artwork_repair_observation(&mut cluster, observed.0, repair_item).await?
        else {
            continue;
        };
        let repeat = cluster
            .request(
                observed.0,
                Request::ObserveArtworkRepair {
                    item_id: repair_item,
                    inject_leader_change: false,
                },
            )
            .await?;
        if !repeatable_artwork_observation_succeeded(repeat)? {
            // A loaded runner can spend the one-second quorum-freshness
            // window in the read-only evidence requests above. Re-enter the
            // bounded successor proof and require another complete local
            // lease; never advance to the generation CAS on this stale view.
            continue;
        }
        let Some((after_repeat_fence, after_repeat_index)) =
            try_read_artwork_repair_observation(&mut cluster, observed.0, repair_item).await?
        else {
            continue;
        };
        if after_repeat_fence != before_repeat_fence || after_repeat_index != before_repeat_index {
            bail!(
                "read-only repair observation changed durable state: fence {before_repeat_fence:?} -> \
                 {after_repeat_fence:?}, applied index {before_repeat_index:?} -> \
                 {after_repeat_index:?}"
            );
        }
        match cluster.request(observed.0, Request::Metrics).await? {
            Response::Metrics {
                leader: Some(current_leader),
                current_term,
                quorum_acknowledged: true,
                ..
            } if current_leader == observed.0 && current_term == observed.1 => break observed,
            Response::Metrics { .. } => {
                // The repeat was read-only, so retrying the stable-term proof
                // cannot duplicate a mutation or weaken the lease boundary.
            }
            response => bail!("unexpected repeat-observation metrics response: {response:?}"),
        }
    };
    let mut handoff_winners = Vec::new();
    let mut new_fence = None;
    for node_id in 1..=3 {
        if node_id == handoff {
            if let Some(fence) = claim_artwork_repair_fence_once(
                &mut cluster,
                node_id,
                repair_item,
                repair_lease_ms,
                false,
            )
            .await?
            {
                handoff_winners.push(node_id);
                new_fence = Some(fence);
            }
        } else {
            match cluster
                .request(
                    node_id,
                    Request::ClaimArtworkRepair {
                        item_id: repair_item,
                        lease_ms: repair_lease_ms,
                    },
                )
                .await?
            {
                Response::Flag { value: true } => handoff_winners.push(node_id),
                Response::Flag { value: false } => {}
                response => bail!("unexpected handoff repair claim response: {response:?}"),
            }
        }
    }
    require_artwork_repair_cas_topology(
        cluster.request(handoff, Request::Metrics).await?,
        handoff,
        handoff_term,
    )?;
    if handoff_winners != [handoff] {
        bail!("new leader did not exclusively fence artwork repair: {handoff_winners:?}");
    }
    let (before_ambiguous_fence, before_ambiguous_index) =
        read_artwork_repair_observation(&mut cluster, handoff, wrong_target_item).await?;
    if before_ambiguous_fence.is_some() {
        bail!("ambiguous repair fixture already had a durable fence");
    }
    let ambiguous_claim = claim_artwork_repair_fence_once(
        &mut cluster,
        handoff,
        wrong_target_item,
        repair_lease_ms,
        true,
    )
    .await
    .expect_err("a typed leader change from a mutating repair claim must be fatal");
    if !ambiguous_claim
        .to_string()
        .contains("mutating artwork repair claim was ambiguous and was not retried")
    {
        bail!("mutating repair ambiguity had the wrong verdict: {ambiguous_claim:#}");
    }
    let (after_ambiguous_fence, after_ambiguous_index) =
        read_artwork_repair_observation(&mut cluster, handoff, wrong_target_item).await?;
    let after_ambiguous_fence =
        after_ambiguous_fence.context("ambiguous repair claim did not apply its one attempt")?;
    if after_ambiguous_fence.generation != 1
        || after_ambiguous_fence.owner_node_id != format!("node-{handoff}")
        || after_ambiguous_fence.leader_term != i64::try_from(handoff_term)?
        || after_ambiguous_index
            .zip(before_ambiguous_index)
            .is_none_or(|(after, before)| after.saturating_sub(before) != 1)
    {
        bail!(
            "ambiguous repair claim was not exactly one applied attempt: \
             fence={after_ambiguous_fence:?}, applied={before_ambiguous_index:?}->\
             {after_ambiguous_index:?}"
        );
    }
    let new_fence = new_fence.context("new leader did not return its artwork repair fence")?;
    match cluster
        .request(
            handoff,
            Request::ApplyArtworkRepairFence {
                target_item_id: wrong_target_item,
                fence: new_fence.clone(),
                title: "wrong-item write".to_owned(),
                stale_job_lease: false,
                stale_book_snapshot: false,
            },
        )
        .await?
    {
        Response::ArtworkFenceApply {
            setting: false,
            metadata: false,
            book: false,
        } => {}
        response => bail!("an artwork fence authorized a different item: {response:?}"),
    }
    match cluster
        .request(
            handoff,
            Request::ApplyArtworkRepairFence {
                target_item_id: repair_item,
                fence: old_fence,
                title: "stale old-term write".to_owned(),
                stale_job_lease: false,
                stale_book_snapshot: false,
            },
        )
        .await?
    {
        Response::ArtworkFenceApply {
            setting: false,
            metadata: false,
            book: false,
        } => {}
        response => bail!("an old-term artwork mutation was not fenced: {response:?}"),
    }
    match cluster
        .request(
            handoff,
            Request::ApplyArtworkRepairFence {
                target_item_id: repair_item,
                fence: new_fence.clone(),
                title: "stale-job-lease write".to_owned(),
                stale_job_lease: true,
                stale_book_snapshot: false,
            },
        )
        .await?
    {
        Response::ArtworkFenceApply {
            setting: false,
            metadata: false,
            book: false,
        } => {}
        response => bail!("a stale singleton-job lease authorized artwork writes: {response:?}"),
    }
    match cluster
        .request(
            handoff,
            Request::ApplyArtworkRepairFence {
                target_item_id: repair_item,
                fence: new_fence.clone(),
                title: "stale-book-snapshot write".to_owned(),
                stale_job_lease: false,
                stale_book_snapshot: true,
            },
        )
        .await?
    {
        Response::ArtworkFenceApply {
            setting: true,
            metadata: true,
            book: false,
        } => {}
        response => bail!("a stale book snapshot overwrote newer fields: {response:?}"),
    }
    match cluster
        .request(
            handoff,
            Request::ApplyArtworkRepairFence {
                target_item_id: repair_item,
                fence: new_fence.clone(),
                title: "current-term write".to_owned(),
                stale_job_lease: false,
                stale_book_snapshot: false,
            },
        )
        .await?
    {
        Response::ArtworkFenceApply {
            setting: true,
            metadata: true,
            book: true,
        } => {}
        response => bail!("the current artwork repair fence was refused: {response:?}"),
    }
    cluster
        .request(
            handoff,
            Request::ReleaseArtworkRepair {
                fence: new_fence.clone(),
            },
        )
        .await?
        .require_ok()?;
    match cluster
        .request(
            handoff,
            Request::ApplyArtworkRepairFence {
                target_item_id: repair_item,
                fence: new_fence,
                title: "retired-generation write".to_owned(),
                stale_job_lease: false,
                stale_book_snapshot: false,
            },
        )
        .await?
    {
        Response::ArtworkFenceApply {
            setting: false,
            metadata: false,
            book: false,
        } => {}
        response => bail!("a retired artwork generation remained valid: {response:?}"),
    }

    let post_handoff_leader = cluster.leader().await?;
    let target = (2..=3)
        .find(|node_id| *node_id != post_handoff_leader)
        .context("choose a removable follower")?;
    let observer = (1..=3)
        .find(|node_id| *node_id != target)
        .context("choose a surviving membership observer")?;
    let observer_node_id = format!("node-{observer}");
    let activity_now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let departing_activity_proof = match cluster
        .request(
            target,
            Request::SignActivityRequest {
                target_node_id: observer_node_id,
                timestamp_ms: activity_now,
            },
        )
        .await?
    {
        Response::ActivityAuth { auth } => auth,
        response => bail!("unexpected departing activity proof response: {response:?}"),
    };
    match cluster
        .request(
            observer,
            Request::AuthorizeActivityRequest {
                auth: departing_activity_proof.clone(),
            },
        )
        .await?
    {
        Response::Flag { value: true } => {}
        response => bail!("a live voter's activity proof was refused: {response:?}"),
    }
    let departing_artwork_proof = match cluster
        .request(
            target,
            Request::ArtworkPeerAuth {
                filename: "poster.jpg".to_owned(),
            },
        )
        .await?
    {
        Response::ArtworkPeerAuth { auth } => auth,
        response => bail!("unexpected artwork proof response: {response:?}"),
    };
    if !legacy_artwork_proof_is_valid("poster.jpg", &departing_artwork_proof)? {
        bail!("the production artwork signer changed the established v4 HMAC wire");
    }
    match cluster
        .request(
            observer,
            Request::VerifyArtworkPeer {
                filename: "poster.jpg".to_owned(),
                auth: departing_artwork_proof.clone(),
            },
        )
        .await?
    {
        Response::Flag { value: true } => {}
        response => bail!("a live voter's artwork proof was refused: {response:?}"),
    }
    let legacy_artwork_proof =
        shared_secret_artwork_proof(&format!("node-{target}"), "poster.jpg", activity_now)?;
    match cluster
        .request(
            observer,
            Request::VerifyArtworkPeer {
                filename: "poster.jpg".to_owned(),
                auth: legacy_artwork_proof,
            },
        )
        .await?
    {
        Response::Flag { value: true } => {}
        response => bail!("the established v4 artwork HMAC wire was refused: {response:?}"),
    }
    match cluster
        .request(
            observer,
            Request::VerifyArtworkPeer {
                filename: "different.jpg".to_owned(),
                auth: departing_artwork_proof.clone(),
            },
        )
        .await?
    {
        Response::Flag { value: false } => {}
        response => bail!("an artwork proof was not bound to its filename: {response:?}"),
    }
    // M3c (`CLUSTERING-PLAN.md` §6.7): removal resolves the offline work the
    // departing node owns instead of refusing forever. Seeded on the target's
    // own process so the fixture's source files exist where its packages say
    // they do.
    let target_node = format!("node-{target}");
    let departing_job_resource = format!("cluster-check:departing-job:{target_node}");
    let departing_job_lease = match cluster
        .request(
            observer,
            Request::AcquireJobLease {
                resource: departing_job_resource.clone(),
                owner_node_id: target_node.clone(),
            },
        )
        .await?
    {
        Response::JobLease { lease: Some(lease) } => lease,
        response => bail!("could not seed the departing node's job lease: {response:?}"),
    };
    let exhausted_job_lease = match cluster
        .request(
            observer,
            Request::SeedExhaustedJobLease {
                resource: format!("{departing_job_resource}:exhausted"),
                owner_node_id: target_node.clone(),
            },
        )
        .await?
    {
        Response::JobLease { lease: Some(lease) } => lease,
        response => bail!("could not seed the exhausted departing-node lease: {response:?}"),
    };
    let media_dir = root.path().join(format!("media-{target_node}"));
    let offline_user = match cluster
        .request(
            target,
            Request::SeedOfflineRemovalWork {
                node_id: target_node.clone(),
                media_dir: media_dir.to_string_lossy().to_string(),
            },
        )
        .await?
    {
        Response::SeededOfflineRemovalWork { user_id } => user_id,
        response => bail!("unexpected offline removal seed response: {response:?}"),
    };

    // The refusal survives, narrowed to what the policy genuinely cannot
    // resolve: a download that is in flight right now. Lifting the blanket
    // refusal must not turn removal into "always succeeds".
    let transfer_package = format!("{TRANSFER_PACKAGE}-{target_node}");
    match cluster
        .request(
            observer,
            Request::RemoveVoter {
                node_id: target_node.clone(),
            },
        )
        .await?
    {
        Response::MembershipError { code, message } if code == "node_owns_offline_work" => {
            if !message.contains("transferring") {
                bail!("in-flight transfer refusal gave the operator no reason: {message}");
            }
        }
        response => bail!("removal did not refuse an in-flight transfer: {response:?}"),
    }
    // The operator takes the action the refusal named.
    match cluster
        .request(
            observer,
            Request::DeleteOfflinePackage {
                package_id: transfer_package,
                user_id: offline_user,
            },
        )
        .await?
    {
        Response::Flag { value: true } => {}
        response => bail!("could not clear the in-flight transfer fixture: {response:?}"),
    }

    // One more download is requested on the departing node while the removal
    // is resolving. The node serves its API until the membership change
    // commits, so a package created after the plan was drawn is real; a
    // removal that resolved only its opening snapshot would commit and leave
    // this one owned by a node that no longer exists.
    let late_package = format!("{LATE_PACKAGE}-{target_node}");
    cluster
        .request(
            target,
            Request::SeedOfflineWorkDuringRemoval {
                node_id: target_node.clone(),
                media_dir: media_dir.to_string_lossy().to_string(),
                user_id: offline_user,
                delay_ms: LATE_PACKAGE_DELAY_MS,
            },
        )
        .await?
        .require_ok()?;

    cluster
        .request(
            observer,
            Request::RemoveVoter {
                node_id: target_node.clone(),
            },
        )
        .await?
        .require_ok()?;
    let removed_activity_now =
        i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let removed_activity_proof = match cluster
        .request(
            target,
            Request::SignActivityRequest {
                target_node_id: format!("node-{observer}"),
                timestamp_ms: removed_activity_now,
            },
        )
        .await?
    {
        Response::ActivityAuth { auth } => auth,
        response => bail!("removed voter could not mint its retained-key proof: {response:?}"),
    };
    let impersonated_node = (1..=3)
        .find(|node_id| *node_id != target && *node_id != observer)
        .context("choose the other surviving voter")?;
    let impersonated_activity_proof = shared_secret_activity_proof(
        &format!("node-{impersonated_node}"),
        &format!("node-{observer}"),
        removed_activity_now,
    )?;
    match cluster
        .request(
            observer,
            Request::AuthorizeActivityRequest {
                auth: impersonated_activity_proof,
            },
        )
        .await?
    {
        Response::Flag { value: false } => {}
        response => bail!("a removed voter impersonated a surviving activity peer: {response:?}"),
    }
    match cluster
        .request(
            observer,
            Request::AuthorizeActivityRequest {
                auth: removed_activity_proof,
            },
        )
        .await?
    {
        Response::Flag { value: false } => {}
        response => bail!("a removed voter retained activity access: {response:?}"),
    }
    match cluster
        .request(
            observer,
            Request::VerifyArtworkPeer {
                filename: "poster.jpg".to_owned(),
                auth: departing_artwork_proof,
            },
        )
        .await?
    {
        Response::Flag { value: false } => {}
        response => bail!("a removed voter retained artwork access: {response:?}"),
    }
    match cluster
        .request(
            observer,
            Request::ApplyJobLease {
                key: format!("cluster-check.removed-job.{target_node}"),
                lease: departing_job_lease,
                observed_at_ms: None,
            },
        )
        .await?
    {
        Response::Flag { value: false } => {}
        response => bail!("a removed voter's existing job lease still published: {response:?}"),
    }
    match cluster
        .request(
            observer,
            Request::ApplyJobLease {
                key: format!("cluster-check.removed-exhausted-job.{target_node}"),
                observed_at_ms: Some(exhausted_job_lease.expires_at_unix_ms - 1),
                lease: exhausted_job_lease,
            },
        )
        .await?
    {
        Response::Flag { value: false } => {}
        response => bail!(
            "a removed voter's exhausted, already-expired token published from a delayed command: {response:?}"
        ),
    }
    match cluster
        .request(
            observer,
            Request::AcquireJobLease {
                resource: departing_job_resource,
                owner_node_id: target_node.clone(),
            },
        )
        .await?
    {
        Response::JobLease { lease: None } => {}
        response => bail!("a removed voter reacquired singleton work: {response:?}"),
    }
    match cluster
        .request(
            observer,
            Request::AcquireJobLeaseLegacy {
                resource: format!("cluster-check:legacy-reacquire:{target_node}"),
                owner_node_id: target_node.clone(),
            },
        )
        .await?
    {
        Response::Flag { value: false } => {}
        response => bail!("legacy lease SQL bypassed a removed-owner fence: {response:?}"),
    }
    cluster
        .request(
            observer,
            Request::ClearJobOwnerFence {
                owner_node_id: target_node.clone(),
            },
        )
        .await?
        .require_ok()?;
    cluster
        .request(
            observer,
            Request::RemoveVoter {
                node_id: target_node.clone(),
            },
        )
        .await?
        .require_ok()?;

    // A removed identity remains permanently reserved. The same serialized
    // first-redemption predicate covers active, pending-removal, and
    // tombstoned rows, so an otherwise valid token cannot reanimate it.
    let tombstone_collision = match cluster
        .request(observer, Request::IssueJoinToken { ttl_ms: 120_000 })
        .await?
    {
        Response::IssuedJoinToken { token } => token,
        response => bail!("unexpected tombstone-collision token response: {response:?}"),
    };
    let removed_spec = specs[(target - 1) as usize].clone();
    require_membership_error(
        cluster
            .request(
                observer,
                Request::RedeemJoin {
                    request: RedeemJoinRequest {
                        token_digest: join_token_digest(&tombstone_collision.token),
                        raft_id: tombstone_collision.raft_id,
                        node_id: target_node.clone(),
                        hostname: "tombstone-collision".to_owned(),
                        raft_address: removed_spec.raft,
                        api_address: removed_spec.api,
                        http_base: "http://127.0.0.1:33999".to_owned(),
                        schema_version: AUTH_SCHEMA_VERSION,
                        protocol_version: AUTH_PROTOCOL_VERSION,
                        protocol_min: AUTH_PROTOCOL_MIN,
                        protocol_max: AUTH_PROTOCOL_MAX,
                    },
                },
            )
            .await?,
        "cluster_node_identity_in_use",
    )?;
    match cluster
        .request(
            observer,
            Request::AcquireJobLeaseLegacy {
                resource: format!("cluster-check:legacy-backfill:{target_node}"),
                owner_node_id: target_node.clone(),
            },
        )
        .await?
    {
        Response::Flag { value: false } => {}
        response => bail!("idempotent removal did not backfill its job-owner fence: {response:?}"),
    }
    cluster
        .request(
            observer,
            Request::ReuseRemovedArtworkUrl {
                removed_node_id: target_node.clone(),
                public_http_url: format!("http://127.0.0.1:{}", 33_000 + target),
            },
        )
        .await?
        .require_ok()?;
    require_membership_error(
        cluster
            .request(
                target,
                Request::ClaimArtworkRepair {
                    item_id: 9_003,
                    lease_ms: 100,
                },
            )
            .await?,
        "local_node_not_active",
    )?;

    // Either §6.7 outcome closes it — moved to a survivor, or failed with its
    // reservation released. The outcome this exists to catch is the third one:
    // still queued on a node that is now a tombstone, holding the traveller's
    // byte budget until the seven-day expiry.
    match offline_summary(&mut cluster, observer, &late_package, offline_user).await? {
        Response::OfflinePackageSummary {
            state,
            node_id,
            error_code,
            ..
        } if state == "queued" && node_id != target_node && error_code.is_none() => {}
        Response::OfflinePackageSummary {
            state,
            error_code,
            reserved_bytes,
            ..
        } if state == "failed"
            && error_code.as_deref() == Some("node_removed")
            && reserved_bytes == 0 => {}
        response => bail!(
            "a download requested while the removal was resolving was left owned by the removed \
             node: {response:?}"
        ),
    }

    // A survivor proved it reads the source, so the work moved rather than
    // dying with the node.
    let movable_package = format!("{MOVABLE_PACKAGE}-{target_node}");
    let movable = offline_summary(&mut cluster, observer, &movable_package, offline_user).await?;
    match &movable {
        Response::OfflinePackageSummary {
            state,
            node_id,
            error_code,
            ..
        } if state == "queued" && *node_id != target_node && error_code.is_none() => {}
        response => bail!("a verified package was not requeued on a survivor: {response:?}"),
    }
    let Response::OfflinePackageSummary {
        node_id: new_owner, ..
    } = movable
    else {
        unreachable!("matched above")
    };

    // No survivor could read this one, so it failed loudly with the stable
    // code and gave its reservation back instead of holding the traveller's
    // byte budget until the seven-day expiry.
    match offline_summary(
        &mut cluster,
        observer,
        &format!("{STRANDED_PACKAGE}-{target_node}"),
        offline_user,
    )
    .await?
    {
        Response::OfflinePackageSummary {
            state,
            error_code,
            reserved_bytes,
            ..
        } if state == "failed"
            && error_code.as_deref() == Some("node_removed")
            && reserved_bytes == 0 => {}
        response => bail!("an unverifiable package was not failed as node_removed: {response:?}"),
    }

    // A ready package whose bytes lived only on the removed node stops
    // advertising itself as ready, even though its source is perfectly
    // readable elsewhere. Requeueing it would risk handing a resuming
    // downloader a different encoder's generation behind the same lease URL.
    match offline_summary(
        &mut cluster,
        observer,
        &format!("{READY_PACKAGE}-{target_node}"),
        offline_user,
    )
    .await?
    {
        Response::OfflinePackageSummary {
            state,
            error_code,
            reserved_bytes,
            actual_bytes,
            ..
        } if state == "failed"
            && error_code.as_deref() == Some("node_removed")
            && reserved_bytes == 0
            && actual_bytes.is_none() => {}
        response => bail!("a ready package survived its node's removal: {response:?}"),
    }

    // The re-homed package is ordinary claimable work on its new owner, and
    // the node that just left can no longer publish it.
    let new_owner_voter = (1..=3)
        .find(|voter| format!("node-{voter}") == new_owner)
        .context("requeued package landed on a node outside the cluster")?;
    // The package requested mid-removal can share this queue, so claim until
    // the one under test comes out instead of assuming it is first.
    let mut claimed_movable = false;
    for _ in 0..2 {
        match cluster
            .request(
                new_owner_voter,
                Request::ClaimNextOfflinePackage {
                    node_id: new_owner.clone(),
                },
            )
            .await?
        {
            Response::ClaimedOfflinePackage {
                package_id: Some(package_id),
            } if package_id == movable_package => {
                claimed_movable = true;
                break;
            }
            Response::ClaimedOfflinePackage {
                package_id: Some(package_id),
            } if package_id == late_package => {}
            response => {
                bail!("the requeued package was not claimable on its new owner: {response:?}")
            }
        }
    }
    if !claimed_movable {
        bail!("the requeued package never became claimable on its new owner");
    }
    match cluster
        .request(
            observer,
            Request::PublishOfflinePackage {
                package_id: movable_package.clone(),
                node_id: target_node.clone(),
            },
        )
        .await?
    {
        Response::Flag { value: false } => {}
        response => bail!("the removed node still published re-homed work: {response:?}"),
    }
    match cluster
        .request(
            new_owner_voter,
            Request::PublishOfflinePackage {
                package_id: movable_package,
                node_id: new_owner,
            },
        )
        .await?
    {
        Response::Flag { value: true } => {}
        response => bail!("the new owner could not complete the requeued package: {response:?}"),
    }

    cluster
        .request(observer, Request::ResetContractState)
        .await?
        .require_ok()?;
    cluster
        .request(
            target,
            Request::HeartbeatPreservesTombstone {
                node_id: format!("node-{target}"),
            },
        )
        .await?
        .require_ok()?;
    cluster
        .request(
            target,
            Request::TombstoneOfflineFence {
                node_id: format!("node-{target}"),
            },
        )
        .await?
        .require_ok()?;
    let remaining = (1..=3)
        .filter(|node_id| *node_id != target)
        .collect::<Vec<_>>();
    cluster.wait_for_voters(&remaining).await?;
    cluster.kill(target).await?;

    let status = match cluster.request(observer, Request::MembershipStatus).await? {
        Response::MembershipStatus { status } => status,
        response => bail!("unexpected two-voter membership status: {response:?}"),
    };
    if status.availability != ClusterAvailability::DegradedReconfiguration
        || status.nodes.len() != 2
        || status.replication.health != ReplicationHealth::Degraded
    {
        bail!("two-voter membership status did not advertise degradation: {status:?}");
    }

    let leader = cluster.leader().await?;
    let quorum_target = remaining
        .iter()
        .copied()
        .find(|node_id| *node_id != leader)
        .context("choose non-leader for quorum-loss refusal")?;
    require_membership_error(
        cluster
            .request(
                leader,
                Request::RemoveVoter {
                    node_id: format!("node-{quorum_target}"),
                },
            )
            .await?,
        "removal_would_lose_quorum",
    )?;
    let final_dump = match cluster.request(observer, Request::Dump).await? {
        Response::Dump { dump, .. } => dump,
        response => bail!("unexpected final membership dump: {response:?}"),
    };
    require_dump_setting(&final_dump, "instance.id", INSTANCE_ID)?;
    cluster.kill(quorum_target).await?;
    let no_quorum_status = match cluster.request(leader, Request::MembershipStatus).await? {
        Response::MembershipStatus { status } => status,
        response => bail!("roster was unavailable during quorum loss: {response:?}"),
    };
    if no_quorum_status.availability != ClusterAvailability::DegradedReconfiguration
        || no_quorum_status.nodes.len() != 2
        || no_quorum_status.replication.health != ReplicationHealth::Degraded
    {
        bail!("quorum-loss roster did not explain the outage: {no_quorum_status:?}");
    }
    cluster.shutdown_all().await?;
    Ok(())
}

/// Observe an old repair generation through one stable successor term before
/// any call that can advance its durable fence. A Hiqlite leader-change error
/// is retryable only here: `claim_artwork_source_repair` cannot reach its CAS
/// until after this receiver-local observation has succeeded once.
async fn observe_artwork_repair_successor(
    cluster: &mut ClusterProcesses,
    former_leader: u64,
    item_id: i64,
    deadline: Instant,
) -> Result<(u64, u64)> {
    let mut inject_leader_change = true;
    loop {
        if Instant::now() >= deadline {
            bail!("artwork repair leadership did not stabilize away from voter {former_leader}");
        }
        let successor = cluster.leader().await?;
        if successor == former_leader {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }
        let term = match cluster.request(successor, Request::Metrics).await? {
            Response::Metrics {
                leader: Some(current_leader),
                current_term,
                quorum_acknowledged: true,
                ..
            } if current_leader == successor => current_term,
            Response::Metrics { .. } => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            response => bail!("unexpected successor metrics response: {response:?}"),
        };
        let observed = match cluster
            .request(
                successor,
                Request::ObserveArtworkRepair {
                    item_id,
                    inject_leader_change,
                },
            )
            .await?
        {
            Response::Flag { value: true } => true,
            Response::Flag { value: false } => false,
            Response::MembershipLeaderChange { .. } => {
                inject_leader_change = false;
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            response => bail!("successor could not observe the durable repair fence: {response:?}"),
        };
        match cluster.request(successor, Request::Metrics).await? {
            Response::Metrics {
                leader: Some(current_leader),
                current_term,
                quorum_acknowledged: true,
                ..
            } if observed && current_leader == successor && current_term == term => {
                return Ok((successor, term));
            }
            Response::Metrics {
                leader: Some(current_leader),
                current_term,
                quorum_acknowledged: true,
                ..
            } if current_leader == successor && current_term == term => {
                bail!("successor could not observe the durable repair fence")
            }
            Response::Metrics { .. } => {}
            response => bail!("unexpected successor post-observation metrics: {response:?}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn read_artwork_repair_observation(
    cluster: &mut ClusterProcesses,
    node_id: u64,
    item_id: i64,
) -> Result<(Option<ArtworkRepairFence>, Option<u64>)> {
    try_read_artwork_repair_observation(cluster, node_id, item_id)
        .await?
        .context("repair observation changed leader during a required evidence read")
}

/// Read both pieces of durable repeat evidence, preserving a typed routing
/// transition so the caller may retry only this non-mutating operation.
async fn try_read_artwork_repair_observation(
    cluster: &mut ClusterProcesses,
    node_id: u64,
    item_id: i64,
) -> Result<Option<(Option<ArtworkRepairFence>, Option<u64>)>> {
    let fence = match cluster
        .request(node_id, Request::ReadArtworkRepairFence { item_id })
        .await?
    {
        Response::ArtworkRepairFence { fence } => fence,
        Response::MembershipLeaderChange { .. } => return Ok(None),
        response => bail!("unexpected repair fence read response: {response:?}"),
    };
    let applied_index = match cluster.request(node_id, Request::Metrics).await? {
        Response::Metrics { applied_index, .. } => applied_index,
        response => bail!("unexpected repair observation metrics: {response:?}"),
    };
    Ok(Some((fence, applied_index)))
}

/// Classify the read-only repeat probe. A stale quorum view or typed routing
/// transition is safe to retry because this request cannot reach the repair
/// generation CAS; every other response is a semantic harness failure.
fn repeatable_artwork_observation_succeeded(response: Response) -> Result<bool> {
    match response {
        Response::Flag { value: true } => Ok(true),
        Response::Flag { value: false } | Response::MembershipLeaderChange { .. } => Ok(false),
        response => bail!("mature repair observation was not repeatable: {response:?}"),
    }
}

/// Verify the topology identity that fenced the mutating repair claim. Quorum
/// freshness is deliberately not part of this post-CAS proof: the claim
/// itself refuses a stale quorum before submitting its Raft write, while the
/// following loser probes can legitimately consume the one-second freshness
/// window on a loaded runner. A changed leader or term still invalidates the
/// generation proof.
fn require_artwork_repair_cas_topology(
    response: Response,
    expected_leader: u64,
    expected_term: u64,
) -> Result<()> {
    match response {
        Response::Metrics {
            leader: Some(current_leader),
            current_term,
            ..
        } if current_leader == expected_leader && current_term == expected_term => Ok(()),
        Response::Metrics {
            leader,
            current_term,
            ..
        } => bail!(
            "artwork repair topology changed during the generation CAS: expected leader \
             {expected_leader} term {expected_term}, observed leader {leader:?} term {current_term}"
        ),
        response => bail!("unexpected post-CAS successor metrics response: {response:?}"),
    }
}

/// A potentially mutating repair claim is sent exactly once. In particular,
/// its typed routing ambiguity is terminal because the write may have applied.
async fn claim_artwork_repair_fence_once(
    cluster: &mut ClusterProcesses,
    node_id: u64,
    item_id: i64,
    lease_ms: u64,
    inject_leader_change: bool,
) -> Result<Option<ArtworkRepairFence>> {
    classify_artwork_repair_claim_response(
        cluster
            .request(
                node_id,
                Request::ClaimArtworkRepairFence {
                    item_id,
                    lease_ms,
                    inject_leader_change,
                },
            )
            .await?,
    )
}

/// Only an explicit no-fence response proves the mutating request reached no
/// generation CAS and is therefore safe for a bounded caller to retry.
fn classify_artwork_repair_claim_response(
    response: Response,
) -> Result<Option<ArtworkRepairFence>> {
    match response {
        Response::ArtworkRepairFence { fence } => Ok(fence),
        Response::MembershipLeaderChange { .. } => {
            bail!("mutating artwork repair claim was ambiguous and was not retried")
        }
        response => bail!("unexpected artwork repair fence response: {response:?}"),
    }
}

/// Prove the self-leave path that a remote removal deliberately refuses: the
/// current leader hands leadership to a survivor before it excludes itself.
/// This is a separate cluster because the membership lifecycle above must keep
/// exercising ordinary remote follower removal and its offline-work policy.
async fn run_leader_self_leave_case() -> Result<()> {
    let executable = harness_executable()?;
    let root = tempfile::tempdir().context("leader self-leave data root")?;
    let (mut cluster, _) = start_cluster_with_port_retry(&executable, root.path(), 3).await?;

    cluster.request(1, Request::Bootstrap).await?.require_ok()?;
    for node_id in 2..=3 {
        cluster
            .request(node_id, Request::Open)
            .await?
            .require_ok()?;
    }
    cluster.wait_for_voters(&[1, 2, 3]).await?;

    let departed = cluster.leader().await?;
    let local_follower = (1..=3)
        .find(|node_id| *node_id != departed)
        .context("choose local follower removal refusal")?;
    require_membership_error(
        cluster
            .request(
                local_follower,
                Request::RemoveVoter {
                    node_id: format!("node-{local_follower}"),
                },
            )
            .await?,
        "self_removal_requires_leave",
    )?;
    cluster
        .request(departed, Request::LeaveVoter)
        .await?
        .require_ok()?;
    cluster
        .request(
            departed,
            Request::HeartbeatPreservesTombstone {
                node_id: format!("node-{departed}"),
            },
        )
        .await?
        .require_ok()?;
    require_membership_error(
        cluster
            .request(
                departed,
                Request::ClaimArtworkRepair {
                    item_id: 9_002,
                    lease_ms: 100,
                },
            )
            .await?,
        "local_node_not_active",
    )?;
    let remaining = (1..=3)
        .filter(|node_id| *node_id != departed)
        .collect::<Vec<_>>();
    cluster.wait_for_voters(&remaining).await?;

    let successor = cluster.leader_among(&remaining).await?;
    if successor == departed || !remaining.contains(&successor) {
        bail!(
            "leader self-leave did not converge on a surviving successor: departed={departed}, successor={successor}, remaining={remaining:?}"
        );
    }
    let observer = remaining[0];
    cluster
        .wait_for_membership_roster(observer, &remaining, successor)
        .await?;

    cluster.shutdown_all().await?;
    Ok(())
}

/// Four voters still have a three-vote quorum with one nondeparting voter
/// down. The leader and two live survivors must commit the safer odd 4→3
/// membership before the survivors elect and confirm their successor.
async fn run_degraded_four_voter_leader_self_leave_case() -> Result<()> {
    let executable = harness_executable()?;
    let root = tempfile::tempdir().context("degraded four-voter leave data root")?;
    let (mut cluster, _) = start_cluster_with_port_retry(&executable, root.path(), 4).await?;

    cluster.request(1, Request::Bootstrap).await?.require_ok()?;
    for node_id in 2..=4 {
        cluster
            .request(node_id, Request::Open)
            .await?
            .require_ok()?;
    }
    cluster.wait_for_voters(&[1, 2, 3, 4]).await?;

    let departed = cluster.leader().await?;
    let remaining = (1..=4)
        .filter(|node_id| *node_id != departed)
        .collect::<Vec<_>>();
    let unavailable = remaining[0];
    cluster.kill(unavailable).await?;
    cluster
        .request(departed, Request::LeaveVoter)
        .await?
        .require_ok()?;
    cluster
        .request(
            departed,
            Request::HeartbeatPreservesTombstone {
                node_id: format!("node-{departed}"),
            },
        )
        .await?
        .require_ok()?;
    require_membership_error(
        cluster
            .request(
                departed,
                Request::ClaimArtworkRepair {
                    item_id: 9_004,
                    lease_ms: 100,
                },
            )
            .await?,
        "local_node_not_active",
    )?;
    cluster.wait_for_voters(&remaining).await?;

    let live_survivors = remaining
        .iter()
        .copied()
        .filter(|node_id| *node_id != unavailable)
        .collect::<Vec<_>>();
    let successor = cluster.leader_among(&live_survivors).await?;
    if successor == departed || successor == unavailable || !remaining.contains(&successor) {
        bail!(
            "degraded four-voter leave did not elect a live successor: departed={departed}, unavailable={unavailable}, successor={successor}, remaining={remaining:?}"
        );
    }
    let observer = remaining
        .iter()
        .copied()
        .find(|node_id| *node_id != unavailable)
        .context("choose live degraded-leave observer")?;
    cluster
        .wait_for_membership_roster(observer, &remaining, successor)
        .await?;
    cluster.shutdown_all().await?;
    Ok(())
}

/// M3d: real voter processes must converge on one logical name while exposing
/// distinct node records through both LAN discovery projections.
async fn prove_cluster_discovery(cluster: &mut ClusterProcesses) -> Result<()> {
    let seeds = ["Living Room", "wrong-node-two", "wrong-node-three"];
    let mut node_ids = BTreeSet::new();
    let mut mdns_names = BTreeSet::new();
    for (index, seed_name) in seeds.iter().enumerate() {
        let node = u64::try_from(index + 1)?;
        match cluster
            .request(
                node,
                Request::Advertisement {
                    seed_name: (*seed_name).to_owned(),
                },
            )
            .await?
        {
            Response::Advertisement {
                instance_id,
                node_id,
                name,
                gdm,
                mdns_name,
                mdns_instance_id,
                mdns_node_id,
            } => {
                if instance_id != INSTANCE_ID
                    || mdns_instance_id != INSTANCE_ID
                    || name != "Living Room"
                    || mdns_node_id != node_id
                    || !mdns_name.starts_with("Living Room · ")
                    || !gdm.contains(&format!("Resource-Identifier: {node_id}\r\n"))
                    || !gdm.contains(&format!("Logical-Identifier: {INSTANCE_ID}\r\n"))
                    || !gdm.contains(&format!("Node-Identifier: {node_id}\r\n"))
                    || !gdm.contains("Name: Living Room\r\n")
                {
                    bail!("voter {node} advertised inconsistent discovery identity");
                }
                mdns_names.insert(mdns_name);
                node_ids.insert(node_id);
            }
            response => bail!("unexpected voter {node} advertisement: {response:?}"),
        }
    }
    if node_ids.len() != 3 || mdns_names.len() != 3 {
        bail!(
            "three voters did not advertise three distinct node records: node_ids={node_ids:?}, mdns_names={mdns_names:?}"
        );
    }

    cluster
        .request(
            2,
            Request::RenameServer {
                name: "Cinema".to_owned(),
            },
        )
        .await?
        .require_ok()?;
    for node in 1..=3 {
        match cluster
            .request(
                node,
                Request::Advertisement {
                    seed_name: "ignored-after-bootstrap".to_owned(),
                },
            )
            .await?
        {
            Response::Advertisement {
                name,
                mdns_name,
                gdm,
                ..
            } if name == "Cinema"
                && mdns_name.starts_with("Cinema · ")
                && gdm.contains("Name: Cinema\r\n") => {}
            response => {
                bail!("renamed discovery identity did not converge on voter {node}: {response:?}")
            }
        }
    }
    Ok(())
}

async fn offline_summary(
    cluster: &mut ClusterProcesses,
    observer: u64,
    package_id: &str,
    user_id: i64,
) -> Result<Response> {
    cluster
        .request(
            observer,
            Request::OfflinePackageSummary {
                package_id: package_id.to_owned(),
                user_id,
            },
        )
        .await
}

/// The P6 learner contract, on real processes.
///
/// Every clause here needs a second operating-system process and would be
/// unfalsifiable without one. Hiqlite 0.14 keeps a node's TLS listener alive
/// until its runtime exits, so an in-process restart fails on the port before
/// it can prove catch-up; a stubbed job authority proves the stub, not
/// `acquire_cluster_job`; and a binary that does not implement the learner
/// protocol is a *binary*, not a flag on a struct. So this scenario runs three
/// real voters plus one real learner and asserts, in order:
///
/// - a live process whose heartbeat omits `learner_protocol_v5` blocks
///   activation, and the refusal names it;
/// - restarting that process on a binary that writes the capability unblocks
///   activation;
/// - the admitted learner enters committed Raft membership and never the voter
///   set, so quorum size stays three;
/// - the learner takes no cluster-wide job lease through plurxd's own
///   `acquire_cluster_job`, and the resource it was refused is left unheld;
/// - a learner that falls behind the retained log installs a snapshot on
///   restart, publishes a fresh zero-lag/read-ready proof, and still cannot
///   take a cluster-wide job;
/// - learner removal drains and fences the target, removes the non-voting
///   member without changing the three-voter quorum, and leaves a durable
///   tombstone;
/// - a newly admitted learner can be promoted only after its own readiness
///   and storage proofs cross a committed barrier; promotion immediately
///   grants voter job authority, and the promoted voter restarts with the
///   same authority from its durable role;
/// - the capacity projection never confuses a non-voting copy with voter
///   failure tolerance, and protocol rollback becomes safe after promotion.
async fn run_learner_membership_case() -> Result<failure_drills::LearnerDrillObservation> {
    /// The voter that starts on a binary predating the learner protocol.
    const OLD_BINARY_VOTER: u64 = 3;
    /// The non-voting member. Its raft id is not chosen: token issuance
    /// allocates one above every durable node and live token, and the
    /// assertion below is that it allocated exactly this.
    const LEARNER: u64 = 4;
    /// The replacement learner that proves the promotion path after node 4's
    /// complete removal. Keeping the identities distinct also proves token
    /// allocation does not reuse a tombstoned node id.
    const PROMOTED: u64 = 5;
    const CATCHUP_KEY: &str = "learner.catchup";
    const CATCHUP_VALUE: &str = "committed-while-the-learner-was-down";
    const SNAPSHOT_KEY: &str = "cluster.growth.compaction.learner-snapshot";
    /// The resource whose duplication P6 is most worried about, and the one
    /// `spawn_background_loops` starts on every node.
    const FIRST_JOB: &str = "provider:artwork";
    /// A second, never-contended resource, so the post-restart gate is proved
    /// against an unheld row rather than against the first job's owner.
    const SECOND_JOB: &str = "repair:probe";
    const POST_RESTART_JOB: &str = "repair:post-promotion-restart";

    let executable = harness_executable()?;
    let root = tempfile::tempdir().context("learner membership data root")?;
    let (mut cluster, specs, cluster_root) = with_port_retry(|attempt| {
        let executable = executable.clone();
        let attempt_root = root.path().join(format!("attempt-{attempt}"));
        async move {
            // Five addresses, three processes: both learner port sets are
            // allocated with the voters so their specs stay stable, and each
            // process starts only after the cluster has admitted it.
            let (listeners, all_specs) = allocate_nodes(5)?.into_inner();
            let cluster = ClusterProcesses::start_with_pre_learner_heartbeat(
                &executable,
                &attempt_root,
                PortReservation {
                    listeners,
                    specs: all_specs[..3].to_vec(),
                },
                OLD_BINARY_VOTER,
            )
            .await?;
            Ok((cluster, all_specs, attempt_root))
        }
    })
    .await?;
    cluster.request(1, Request::Bootstrap).await?.require_ok()?;
    for node_id in 2..=3 {
        cluster
            .request(node_id, Request::Open)
            .await?
            .require_ok()?;
    }
    cluster.wait_for_voters(&[1, 2, 3]).await?;
    let leader = cluster.leader().await?;

    // Installing this binary activates nothing. Three voters are running it,
    // one of them is not, and the cluster is still on the unactivated range.
    let pending =
        wait_for_protocol_pending(&mut cluster, leader, &[format!("node-{OLD_BINARY_VOTER}")])
            .await?;
    if (pending.active_min, pending.active_max) != (AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN)
        || (pending.binary_min, pending.binary_max) != (AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MAX)
        || pending.learner_protocol_active
    {
        bail!("a deployed binary moved the protocol range on its own: {pending:?}");
    }

    // Before activation there is no learner role to hand out at all.
    require_membership_error(
        cluster
            .request(leader, Request::IssueLearnerJoinToken { ttl_ms: 120_000 })
            .await?,
        "learner_protocol_inactive",
    )?;

    // The whole point of coupling the capability to the heartbeat: a second
    // process that heartbeats without proving protocol 5 blocks activation,
    // and the refusal is actionable because it names the node to upgrade.
    require_membership_error_message(
        cluster
            .request(leader, Request::ActivateLearnerProtocol)
            .await?,
        "learner_protocol_upgrade_required",
        &format!("node-{OLD_BINARY_VOTER}"),
    )?;

    // Upgrade that node the way an operator does: stop the process, start the
    // new binary. Quorum survives the restart because two voters remain.
    cluster.kill(OLD_BINARY_VOTER).await?;
    cluster
        .spawn_node(
            &executable,
            NodeLaunch::voter(OLD_BINARY_VOTER, cluster_root.clone(), specs[..3].to_vec()),
        )
        .await?;
    cluster
        .request(OLD_BINARY_VOTER, Request::Open)
        .await?
        .require_ok()
        .context("open upgraded learner-case voter")?;
    cluster.wait_for_voters(&[1, 2, 3]).await?;
    let leader = cluster.leader().await?;
    wait_for_protocol_pending(&mut cluster, leader, &[]).await?;

    let activated = match cluster
        .request(leader, Request::ActivateLearnerProtocol)
        .await?
    {
        Response::ProtocolChange { change } => change,
        response => bail!("unexpected activation response: {response:?}"),
    };
    if !activated.changed
        || !activated.protocol.learner_protocol_active
        || (activated.protocol.active_min, activated.protocol.active_max)
            != (AUTH_PROTOCOL_MAX, AUTH_PROTOCOL_MAX)
    {
        bail!("activation did not narrow the range onto protocol 5: {activated:?}");
    }

    let issued = match cluster
        .request(leader, Request::IssueLearnerJoinToken { ttl_ms: 120_000 })
        .await?
    {
        Response::IssuedJoinToken { token } => token,
        response => bail!("unexpected learner join-token response: {response:?}"),
    };
    if issued.raft_id != LEARNER {
        bail!(
            "learner token assigned raft id {}, expected {LEARNER}",
            issued.raft_id
        );
    }
    let learner_spec = specs[(LEARNER - 1) as usize].clone();
    cluster
        .request(
            leader,
            Request::RedeemLearnerJoin {
                request: RedeemJoinRequest {
                    token_digest: join_token_digest(&issued.token),
                    raft_id: issued.raft_id,
                    node_id: format!("node-{LEARNER}"),
                    hostname: format!("cluster-node-{LEARNER}"),
                    raft_address: learner_spec.raft.clone(),
                    api_address: learner_spec.api.clone(),
                    http_base: format!("http://127.0.0.1:{}", 33_000 + LEARNER),
                    schema_version: AUTH_SCHEMA_VERSION,
                    protocol_version: AUTH_PROTOCOL_MIN,
                    protocol_min: AUTH_PROTOCOL_MIN,
                    protocol_max: AUTH_PROTOCOL_MAX,
                },
            },
        )
        .await?
        .require_ok()?;

    // A real fourth process, started with Hiqlite's `learner_only` hint set
    // from its admitted role — the same thing the daemon does.
    cluster
        .spawn_node(
            &executable,
            NodeLaunch::voter(LEARNER, cluster_root.clone(), specs.clone()).as_learner(),
        )
        .await?;
    cluster
        .wait_for_members(1, &[1, 2, 3], &[1, 2, 3, LEARNER])
        .await?;
    cluster
        .request(LEARNER, Request::Open)
        .await?
        .require_ok()
        .context("open newly admitted learner")?;
    cluster
        .request(LEARNER, Request::ForceHeartbeat)
        .await?
        .require_ok()?;
    cluster
        .request(
            leader,
            Request::FinalizeLearnerJoin {
                request: FinalizeJoinRequest {
                    token_digest: join_token_digest(&issued.token),
                    raft_id: issued.raft_id,
                    node_id: format!("node-{LEARNER}"),
                },
            },
        )
        .await?
        .require_ok()?;
    cluster
        .request(LEARNER, Request::StartHeartbeatLoop)
        .await?
        .require_ok()?;

    // Quorum is still three. Every process agrees, including the learner: a
    // node that believed itself a voter would campaign.
    for node_id in [1, 2, 3, LEARNER] {
        cluster
            .wait_for_members(node_id, &[1, 2, 3], &[1, 2, 3, LEARNER])
            .await?;
    }
    let status = match cluster.request(leader, Request::MembershipStatus).await? {
        Response::MembershipStatus { status } => status,
        response => bail!("unexpected learner membership status: {response:?}"),
    };
    let learner_record = status
        .nodes
        .iter()
        .find(|node| node.node_id == format!("node-{LEARNER}"))
        .context("the admitted learner is missing from the roster")?;
    if status.nodes.len() != 4
        || status.availability != ClusterAvailability::HighAvailability
        || learner_record.role != NodeRole::Learner
        || learner_record.is_voter
        || learner_record.is_leader
        || !learner_record.reachable
        || status.nodes.iter().filter(|node| node.is_voter).count() != 3
        || status.capacity.voting_nodes != 3
        || status.capacity.voting_quorum != 2
        || status.capacity.voting_failure_tolerance != 1
        || status.capacity.non_voting_replicas != 1
    {
        bail!("the roster did not describe a three-voter cluster with one learner: {status:?}");
    }

    // The job gate, through plurxd's own `acquire_cluster_job`. The learner is
    // refused while the row is absent, so the refusal is the authority and not
    // a lease someone else already held.
    match cluster
        .request(
            LEARNER,
            Request::AcquireClusterJob {
                resource: FIRST_JOB.to_owned(),
            },
        )
        .await?
    {
        Response::ClusterJobAttempt {
            acquired: false,
            lease: None,
        } => {}
        response => bail!("a learner was not refused an uncontended cluster job: {response:?}"),
    }
    match cluster
        .request(
            leader,
            Request::AcquireClusterJob {
                resource: FIRST_JOB.to_owned(),
            },
        )
        .await?
    {
        Response::ClusterJobAttempt {
            acquired: true,
            lease: Some(lease),
        } if lease.owner_node_id == format!("node-{leader}") => {}
        response => bail!("a voter could not take the same cluster job: {response:?}"),
    }
    match cluster
        .request(
            LEARNER,
            Request::ReadClusterJobLease {
                resource: FIRST_JOB.to_owned(),
            },
        )
        .await?
    {
        Response::ClusterJobAttempt {
            lease: Some(lease), ..
        } if lease.owner_node_id == format!("node-{leader}") => {}
        response => bail!("the cluster job did not stay with its voter owner: {response:?}"),
    }

    // Kill the learner's own process. The cluster keeps committing without it,
    // which is the point of it holding no vote, and the write it misses is
    // what its restart has to catch up on. Read the key first, so "the
    // restarted process applied it" cannot be satisfied by a value that was
    // already there.
    match cluster
        .request(
            LEARNER,
            Request::ReadLocalSetting {
                key: CATCHUP_KEY.to_owned(),
            },
        )
        .await?
    {
        Response::Setting { value: None } => {}
        response => bail!("the catch-up key existed before the learner was stopped: {response:?}"),
    }
    cluster.kill(LEARNER).await?;
    cluster
        .request(
            leader,
            Request::PutSetting {
                key: CATCHUP_KEY.to_owned(),
                value: CATCHUP_VALUE.to_owned(),
            },
        )
        .await?
        .require_ok()?;
    // Compact past every log the stopped learner could fetch. Its restart can
    // recover CATCHUP_KEY only by installing the snapshot produced here; the
    // snapshot marker below proves that exact state machine image arrived.
    cluster
        .request(
            leader,
            Request::ForceCompaction {
                phase: "learner-snapshot".to_owned(),
            },
        )
        .await?
        .require_ok()?;
    cluster
        .wait_for_members(1, &[1, 2, 3], &[1, 2, 3, LEARNER])
        .await?;
    let catchup_index = match cluster.request(leader, Request::Metrics).await? {
        Response::Metrics {
            applied_index: Some(index),
            ..
        } => index,
        response => bail!("leader did not publish a catch-up index: {response:?}"),
    };
    cluster
        .spawn_node(
            &executable,
            NodeLaunch::voter(LEARNER, cluster_root.clone(), specs.clone()).as_learner(),
        )
        .await?;
    // Watch target-local Raft progress while SQLite is restoring. Querying the
    // state machine during restore can legitimately hold the harness request
    // longer than its protocol deadline; the applied-index watch is the
    // production catch-up source and stays responsive throughout the restore.
    let catchup_deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match cluster.request(LEARNER, Request::Metrics).await? {
            Response::Metrics {
                applied_index: Some(index),
                ..
            } if index >= catchup_index => break,
            Response::Metrics { .. } => {}
            response => bail!("unexpected learner catch-up metrics: {response:?}"),
        }
        if Instant::now() >= catchup_deadline {
            let learner_metrics = cluster.request(LEARNER, Request::Metrics).await?;
            let leader_metrics = cluster.request(leader, Request::Metrics).await?;
            let learner_raft = cluster.request(LEARNER, Request::RaftDebug).await?;
            let leader_raft = cluster.request(leader, Request::RaftDebug).await?;
            bail!(
                "the restarted learner never applied the write it missed; learner metrics: \
                 {learner_metrics:?}; leader metrics: {leader_metrics:?}; learner raft: \
                 {learner_raft:?}; leader raft: {leader_raft:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    cluster
        .request(LEARNER, Request::Open)
        .await?
        .require_ok()
        .context("open restarted learner after snapshot catch-up")?;
    let local_read_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = cluster
            .request(
                LEARNER,
                Request::ReadLocalSetting {
                    key: CATCHUP_KEY.to_owned(),
                },
            )
            .await?;
        match response {
            Response::Setting { value } if value.as_deref() == Some(CATCHUP_VALUE) => break,
            Response::Setting { .. } if Instant::now() < local_read_deadline => {}
            response => {
                bail!("the restarted learner did not apply the missed write: {response:?}")
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    match cluster
        .request(
            LEARNER,
            Request::ReadLocalSetting {
                key: SNAPSHOT_KEY.to_owned(),
            },
        )
        .await?
    {
        Response::Setting { value: Some(_) } => {}
        response => {
            bail!("the restarted learner did not install the compacted snapshot: {response:?}")
        }
    }
    cluster
        .wait_for_members(LEARNER, &[1, 2, 3], &[1, 2, 3, LEARNER])
        .await?;
    cluster
        .request(LEARNER, Request::StartHeartbeatLoop)
        .await?
        .require_ok()?;
    wait_for_learner_ready(&mut cluster, leader, LEARNER).await?;

    // A ready learner is useful read capacity only while its target-local
    // applied index stays inside the bounded-replica freshness contract.
    // Pause the real state-machine apply path, commit beyond it, and force the
    // production heartbeat projection to prove that rotation drops the
    // learner before the process itself is unhealthy.
    cluster
        .request(LEARNER, Request::PauseApply)
        .await?
        .require_ok()?;
    cluster
        .request(
            leader,
            Request::PutSetting {
                key: "learner.rotation-lag".to_owned(),
                value: "committed-past-paused-apply".to_owned(),
            },
        )
        .await?
        .require_ok()?;
    let pause_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match cluster
            .request(LEARNER, Request::ApplyPauseObserved)
            .await?
        {
            Response::ApplyPauseObserved { observed: true } => break,
            Response::ApplyPauseObserved { observed: false } => {}
            response => bail!("learner apply-pause observation returned {response:?}"),
        }
        if Instant::now() >= pause_deadline {
            bail!("learner did not block inside its SQLite apply path");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let rotation_deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    loop {
        cluster
            .request(LEARNER, Request::ForceHeartbeat)
            .await?
            .require_ok()?;
        let status = match cluster.request(leader, Request::MembershipStatus).await? {
            Response::MembershipStatus { status } => status,
            response => bail!("unexpected lagged learner status: {response:?}"),
        };
        let lagged = status.nodes.iter().any(|node| {
            node.raft_id == LEARNER
                && node.role == NodeRole::Learner
                && node.apply_lag_entries.is_some_and(|lag| lag > 0)
                && !node.bounded_read_ready
        });
        if lagged && status.capacity.ready_read_workers == 0 {
            break;
        }
        if Instant::now() >= rotation_deadline {
            bail!("lagged learner stayed in ready read-worker rotation: {status:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    cluster
        .request(LEARNER, Request::ResumeApply)
        .await?
        .require_ok()?;
    wait_for_learner_ready(&mut cluster, leader, LEARNER).await?;

    // The restart did not make it eligible either. Eligibility is re-derived
    // from committed membership on every call, so a fresh process is refused
    // for the same reason the old one was.
    match cluster
        .request(
            LEARNER,
            Request::AcquireClusterJob {
                resource: SECOND_JOB.to_owned(),
            },
        )
        .await?
    {
        Response::ClusterJobAttempt {
            acquired: false,
            lease: None,
        } => {}
        response => bail!("a restarted learner was not refused a cluster job: {response:?}"),
    }

    // Rollback is unavailable while a learner is a member, and the refusal
    // names what it would strand.
    require_membership_error_message(
        cluster
            .request(leader, Request::DeactivateLearnerProtocol)
            .await?,
        "learner_protocol_in_use",
        &format!("node-{LEARNER}"),
    )?;

    const LEARNER_MEDIA_SESSION: &str = "p6-learner-active-media";
    cluster
        .request(
            leader,
            Request::SeedActiveMediaSession {
                node_id: format!("node-{LEARNER}"),
                session_id: LEARNER_MEDIA_SESSION.to_owned(),
            },
        )
        .await?
        .require_ok()?;

    // Removal is a learner lifecycle operation, not a voter resize. The
    // target's production heartbeat crosses the durable route-fence barrier
    // before the leader asks Hiqlite to remove it from the member set.
    cluster
        .request(
            leader,
            Request::RemoveNode {
                node_id: format!("node-{LEARNER}"),
            },
        )
        .await?
        .require_ok()?;
    cluster
        .wait_for_members(leader, &[1, 2, 3], &[1, 2, 3])
        .await?;
    match cluster
        .request(
            leader,
            Request::ReadMediaSessionState {
                session_id: LEARNER_MEDIA_SESSION.to_owned(),
            },
        )
        .await?
    {
        Response::Setting { value: Some(state) } if state == "ended" => {}
        response => {
            bail!("learner removal did not supersede its active media ownership: {response:?}")
        }
    }
    cluster
        .request(
            LEARNER,
            Request::HeartbeatPreservesTombstone {
                node_id: format!("node-{LEARNER}"),
            },
        )
        .await?
        .require_ok()?;
    let removed_status = match cluster.request(leader, Request::MembershipStatus).await? {
        Response::MembershipStatus { status } => status,
        response => bail!("unexpected post-learner-removal status: {response:?}"),
    };
    if removed_status
        .nodes
        .iter()
        .any(|node| node.raft_id == LEARNER)
        || removed_status.capacity.voting_nodes != 3
        || removed_status.capacity.voting_quorum != 2
        || removed_status.capacity.voting_failure_tolerance != 1
        || removed_status.capacity.non_voting_replicas != 0
        || removed_status.capacity.ready_read_workers != 0
    {
        bail!("learner removal changed quorum or left replica capacity behind: {removed_status:?}");
    }
    cluster.kill(LEARNER).await?;

    // Admit a distinct replacement learner. The removed id stays reserved by
    // its tombstone, so token allocation must advance to node 5.
    let promoted_token = match cluster
        .request(leader, Request::IssueLearnerJoinToken { ttl_ms: 120_000 })
        .await?
    {
        Response::IssuedJoinToken { token } => token,
        response => bail!("unexpected replacement learner token response: {response:?}"),
    };
    if promoted_token.raft_id != PROMOTED {
        bail!(
            "replacement learner token assigned raft id {}, expected {PROMOTED}",
            promoted_token.raft_id
        );
    }
    let promoted_spec = specs[(PROMOTED - 1) as usize].clone();
    // A real replacement daemon is configured from the current roster, which
    // excludes tombstoned node 4. Feeding the removed endpoint to Hiqlite here
    // makes startup discovery spend its entire bounded wait on a peer the
    // scenario has already proved cannot return. Keep the durable Raft ids
    // sparse instead of turning a removed learner into a bootstrap peer.
    let promoted_specs = specs
        .iter()
        .filter(|spec| spec.id != LEARNER)
        .cloned()
        .collect::<Vec<_>>();
    cluster
        .request(
            leader,
            Request::RedeemLearnerJoin {
                request: RedeemJoinRequest {
                    token_digest: join_token_digest(&promoted_token.token),
                    raft_id: promoted_token.raft_id,
                    node_id: format!("node-{PROMOTED}"),
                    hostname: format!("cluster-node-{PROMOTED}"),
                    raft_address: promoted_spec.raft.clone(),
                    api_address: promoted_spec.api.clone(),
                    http_base: format!("http://127.0.0.1:{}", 33_000 + PROMOTED),
                    schema_version: AUTH_SCHEMA_VERSION,
                    protocol_version: AUTH_PROTOCOL_MIN,
                    protocol_min: AUTH_PROTOCOL_MIN,
                    protocol_max: AUTH_PROTOCOL_MAX,
                },
            },
        )
        .await?
        .require_ok()?;
    cluster
        .spawn_node(
            &executable,
            NodeLaunch::voter(PROMOTED, cluster_root.clone(), promoted_specs.clone()).as_learner(),
        )
        .await?;
    cluster
        .wait_for_members(leader, &[1, 2, 3], &[1, 2, 3, PROMOTED])
        .await?;
    cluster
        .request(PROMOTED, Request::Open)
        .await?
        .require_ok()
        .context("open replacement learner")?;
    cluster
        .request(PROMOTED, Request::ForceHeartbeat)
        .await?
        .require_ok()?;
    cluster
        .request(
            leader,
            Request::FinalizeLearnerJoin {
                request: FinalizeJoinRequest {
                    token_digest: join_token_digest(&promoted_token.token),
                    raft_id: promoted_token.raft_id,
                    node_id: format!("node-{PROMOTED}"),
                },
            },
        )
        .await?
        .require_ok()?;
    cluster
        .request(PROMOTED, Request::StartHeartbeatLoop)
        .await?
        .require_ok()?;
    wait_for_learner_ready(&mut cluster, leader, PROMOTED).await?;

    cluster
        .request(
            leader,
            Request::PromoteLearner {
                node_id: format!("node-{PROMOTED}"),
            },
        )
        .await?
        .require_ok()?;
    cluster
        .wait_for_members(leader, &[1, 2, 3, PROMOTED], &[1, 2, 3, PROMOTED])
        .await?;
    cluster
        .wait_for_members(PROMOTED, &[1, 2, 3, PROMOTED], &[1, 2, 3, PROMOTED])
        .await?;

    // The same process that was refused SECOND_JOB as a learner becomes its
    // voter owner immediately after committed promotion; no daemon restart or
    // boot-time role cache participates in the decision.
    match cluster
        .request(
            PROMOTED,
            Request::AcquireClusterJob {
                resource: SECOND_JOB.to_owned(),
            },
        )
        .await?
    {
        Response::ClusterJobAttempt {
            acquired: true,
            lease: Some(lease),
        } if lease.owner_node_id == format!("node-{PROMOTED}") => {}
        response => bail!("a promoted learner did not gain voter job authority: {response:?}"),
    }
    let promoted_status = match cluster.request(leader, Request::MembershipStatus).await? {
        Response::MembershipStatus { status } => status,
        response => bail!("unexpected promoted learner status: {response:?}"),
    };
    let promoted_record = promoted_status
        .nodes
        .iter()
        .find(|node| node.raft_id == PROMOTED)
        .context("the promoted learner is missing from the roster")?;
    if promoted_record.role != NodeRole::Voter
        || !promoted_record.is_voter
        || promoted_status.capacity.voting_nodes != 4
        || promoted_status.capacity.voting_quorum != 3
        || promoted_status.capacity.voting_failure_tolerance != 1
        || promoted_status.capacity.non_voting_replicas != 0
        || promoted_status.capacity.ready_read_workers != 0
    {
        bail!("promotion did not become four-voter capacity: {promoted_status:?}");
    }

    // Production rewrites the promoted node's two local boot records only
    // after it observes the committed vote. Restart it as the resulting voter
    // record dictates and prove both Raft admission and singleton authority
    // survive the process boundary before declaring protocol rollback safe.
    cluster.kill(PROMOTED).await?;
    cluster
        .spawn_node(
            &executable,
            NodeLaunch::voter(PROMOTED, cluster_root.clone(), promoted_specs),
        )
        .await?;
    cluster
        .request(PROMOTED, Request::Open)
        .await?
        .require_ok()
        .context("open promoted voter after restart")?;
    cluster
        .request(PROMOTED, Request::StartHeartbeatLoop)
        .await?
        .require_ok()?;
    cluster
        .wait_for_members(PROMOTED, &[1, 2, 3, PROMOTED], &[1, 2, 3, PROMOTED])
        .await?;
    match cluster
        .request(
            PROMOTED,
            Request::AcquireClusterJob {
                resource: POST_RESTART_JOB.to_owned(),
            },
        )
        .await?
    {
        Response::ClusterJobAttempt {
            acquired: true,
            lease: Some(lease),
        } if lease.owner_node_id == format!("node-{PROMOTED}") => {}
        response => bail!("a restarted promoted voter did not retain job authority: {response:?}"),
    }

    let deactivated = match cluster
        .request(leader, Request::DeactivateLearnerProtocol)
        .await?
    {
        Response::ProtocolChange { change } => change,
        response => bail!("unexpected learner protocol deactivation response: {response:?}"),
    };
    if !deactivated.changed
        || deactivated.protocol.learner_protocol_active
        || (
            deactivated.protocol.active_min,
            deactivated.protocol.active_max,
        ) != (AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN)
    {
        bail!("protocol rollback did not become safe after promotion: {deactivated:?}");
    }

    cluster.assert_running().await?;
    cluster.kill_all().await;
    Ok(failure_drills::LearnerDrillObservation {
        voting_nodes: 3,
        voting_quorum: 2,
        non_voting_replicas: 1,
        lagged_learner_left_rotation: true,
        learner_reentered_rotation_after_catchup: true,
    })
}

/// Wait for the target's own passive quorum sample, local applied index, and
/// voter-filesystem preflight to arrive in one fresh heartbeat. The controller
/// forces heartbeats only to shorten the test; every field is populated by the
/// production heartbeat transaction.
async fn wait_for_learner_ready(
    cluster: &mut ClusterProcesses,
    observer: u64,
    learner: u64,
) -> Result<MembershipStatus> {
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    loop {
        // Do not create another replicated heartbeat while the target-local
        // sampler is still acquiring a fresh quorum watermark. Advancing the
        // log on every poll can keep the proof and heartbeat loops in a stable
        // phase where every heartbeat samples just before the next proof.
        let passive = passive_raft_observation(cluster, learner).await?;
        if passive.valid
            && passive.watermark_valid
            && passive.local_reads_supported
            && passive.apply_lag_entries == Some(0)
        {
            cluster
                .request(learner, Request::ForceHeartbeat)
                .await?
                .require_ok()?;
        }
        let status = match cluster.request(observer, Request::MembershipStatus).await? {
            Response::MembershipStatus { status } => status,
            response => bail!("unexpected learner readiness status: {response:?}"),
        };
        let ready = status.nodes.iter().any(|node| {
            node.raft_id == learner
                && node.role == NodeRole::Learner
                && !node.is_voter
                && node.reachable
                && node.bounded_read_ready
                && node.apply_lag_entries == Some(0)
                && node.voter_storage_ready
                && node.storage_headroom_bytes.is_some_and(|bytes| bytes > 0)
        }) && status.capacity.ready_read_workers == 1;
        if ready {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            bail!(
                "learner node {learner} never published a ready proof: {status:?}; target-local \
                 passive metrics: {passive:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Wait until `observer` reports exactly `expected` as the nodes whose running
/// binary has not proven the learner protocol, and return the projection.
///
/// The projection is an applied local read — deliberately, so the roster keeps
/// answering during quorum loss — so a scenario that read it once could observe
/// a heartbeat that has committed but not yet applied here.
async fn wait_for_protocol_pending(
    cluster: &mut ClusterProcesses,
    observer: u64,
    expected: &[String],
) -> Result<ClusterProtocolStatus> {
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    loop {
        let status = match cluster.request(observer, Request::ProtocolStatus).await? {
            Response::ProtocolStatus { status } => status,
            response => bail!("unexpected protocol status response: {response:?}"),
        };
        if status.learner_protocol_pending == expected {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            bail!("protocol readiness did not converge on pending {expected:?}: {status:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn require_membership_error(response: Response, expected: &str) -> Result<()> {
    match response {
        Response::MembershipError { code, .. } if code == expected => Ok(()),
        response => bail!("expected membership error {expected}, got {response:?}"),
    }
}

fn require_membership_error_message(
    response: Response,
    expected_code: &str,
    expected_message: &str,
) -> Result<()> {
    match response {
        Response::MembershipError { code, message }
            if code == expected_code && message.contains(expected_message) =>
        {
            Ok(())
        }
        response => bail!(
            "expected membership error {expected_code} containing {expected_message:?}, got {response:?}"
        ),
    }
}

async fn compacted_growth_gate(root: Option<PathBuf>) -> Result<()> {
    println!("cluster-check: post-coalescer compacted growth");
    install_crypto_provider();
    let _ = ServerTlsConfig::server_config_self_signed("127.0.0.1").await;

    let owned_root = root
        .is_none()
        .then(|| tempfile::tempdir().context("compacted-growth data root"))
        .transpose()?;
    let root = root.unwrap_or_else(|| {
        owned_root
            .as_ref()
            .expect("missing owned compacted-growth root")
            .path()
            .to_path_buf()
    });
    let reservation = allocate_nodes(1)?;
    let (listeners, specs) = reservation.into_inner();
    let launch = NodeLaunch::voter(1, root, specs);
    // The reservation is dropped here: hiqlite binds its own sockets from the
    // address strings, so we must release the port before it can bind it. A
    // collision is returned synchronously and the binary maps it to the
    // retryable bind-failure exit status.
    drop(listeners);
    let mut config = node_config(&launch)?;
    config.filename_db = Cow::Borrowed("growth.db");
    config.raft_config = NodeConfig::default_raft_config(GROWTH_COMPACTION_LOGS);
    let client = hiqlite::start_node(config)
        .await
        .context("start compacted-growth voter")?;
    tokio::time::timeout(START_TIMEOUT, client.wait_until_healthy_db())
        .await
        .context("compacted-growth voter health timed out")?;
    let metrics_client = client.clone();
    let snapshot_metrics = metrics_client
        .local_db_snapshot_metrics()
        .context("obtain local snapshot instrumentation")?;
    let snapshot_metrics_before = snapshot_metrics.snapshot();
    let telemetry_path = launch.root.join("node-1").join("telemetry-growth.db");
    let store = Arc::new(
        HiqliteAuthStore::bootstrap(client, "compacted-growth-check", &telemetry_path)
            .await
            .context("bootstrap compacted-growth store")?,
    );

    let (users, item_id) = growth_fixture(store.as_ref()).await?;
    let initial_snapshot = snapshot_index(&metrics_client).await?;
    let warm_snapshot = ensure_compaction_after(
        &metrics_client,
        store.as_ref(),
        initial_snapshot,
        "baseline warm-up",
    )
    .await?;
    // The first snapshot also lets SQLite settle its state-machine WAL. Take a
    // second compacted baseline so its one-time checkpoint is not mistaken for
    // negative progress growth in the measured cycle.
    let baseline_snapshot = ensure_compaction_after(
        &metrics_client,
        store.as_ref(),
        Some(warm_snapshot),
        "baseline settle",
    )
    .await?;
    settle_post_snapshot_tail(
        &metrics_client,
        store.as_ref(),
        baseline_snapshot,
        "baseline",
    )
    .await?;
    let data_dir = launch.root.join("node-1");
    let before_bytes = stable_directory_bytes(&data_dir).await?;

    let applied_before = applied_index(&metrics_client).await?;
    // Put the deterministic clock well ahead of wall time so the background
    // flush workers cannot race this accelerated, beat-by-beat load. Each
    // request still runs the production due/commit path against a real store.
    let logical_start = TokioInstant::now() + Duration::from_secs(24 * 60 * 60);
    let logical_now = Arc::new(RwLock::new(logical_start));
    let coalescer: Arc<ProgressCoalescer> = ProgressCoalescer::with_time_source(store.clone(), {
        let logical_now = Arc::clone(&logical_now);
        move || {
            *logical_now
                .read()
                .expect("compacted-growth logical clock poisoned")
        }
    });
    let beats_per_stream = GROWTH_INCOMING_BEATS / GROWTH_ACTIVE_STREAMS;
    let logical_span_seconds = beats_per_stream
        .saturating_sub(1)
        .saturating_mul(GROWTH_BEAT_INTERVAL_SECS);
    let expected_commits_per_stream = logical_span_seconds / GROWTH_COMMIT_WINDOW_SECS + 1;
    let expected_progress_commits = GROWTH_ACTIVE_STREAMS * expected_commits_per_stream;
    let mut synchronous_commits = 0_u64;
    for beat in 0..beats_per_stream {
        *logical_now
            .write()
            .expect("compacted-growth logical clock poisoned") =
            logical_start + Duration::from_secs(beat.saturating_mul(GROWTH_BEAT_INTERVAL_SECS));
        let mut tasks = tokio::task::JoinSet::new();
        for user_id in users.iter().copied() {
            let coalescer = Arc::clone(&coalescer);
            tasks.spawn(async move {
                let update = coalescer
                    .put(
                        user_id,
                        item_id,
                        i64::try_from((beat + 1) * 1_000)?,
                        Some(10_000_000),
                    )
                    .await?;
                Ok::<u64, anyhow::Error>(u64::from(update.committed))
            });
        }
        while let Some(result) = tasks.join_next().await {
            synchronous_commits += result.context("coalesced stream task panicked")??;
        }
    }
    let drained = u64::try_from(coalescer.drain().await.context("drain coalescer")?)?;
    let physical_progress_commits = synchronous_commits.saturating_add(drained);
    if physical_progress_commits != expected_progress_commits || drained != 0 {
        bail!(
            "coalescer produced {synchronous_commits} synchronous and {drained} trailing commits; expected {expected_progress_commits} synchronous and no trailing commits"
        );
    }
    let applied_after = applied_index(&metrics_client).await?;
    let applied_index_delta = applied_after.saturating_sub(applied_before);
    let measured_snapshot = ensure_compaction_after(
        &metrics_client,
        store.as_ref(),
        Some(baseline_snapshot),
        "coalesced load",
    )
    .await?;
    // hiqlite's retained WAL segment alternates allocation across adjacent
    // compactions. Compare equally settled, two-cycle states so that rollover
    // is not reported as durable progress growth (or as a negative delta).
    let settled_snapshot = ensure_compaction_after(
        &metrics_client,
        store.as_ref(),
        Some(measured_snapshot),
        "coalesced settle",
    )
    .await?;
    settle_post_snapshot_tail(
        &metrics_client,
        store.as_ref(),
        settled_snapshot,
        "coalesced",
    )
    .await?;
    let after_bytes = stable_directory_bytes(&data_dir).await?;
    let report = CompactedGrowthReport {
        incoming_beats: GROWTH_INCOMING_BEATS,
        active_streams: GROWTH_ACTIVE_STREAMS,
        logical_span_seconds,
        physical_progress_commits,
        applied_index_delta,
        before_bytes,
        after_bytes,
        compacted_growth_bytes: after_bytes.saturating_sub(before_bytes),
    };
    validate_compacted_growth(&report)?;

    let raw_before_bytes = after_bytes;
    let raw_snapshot = snapshot_index(&metrics_client).await?;
    let raw_applied_before = applied_index(&metrics_client).await?;
    let raw_value = "x".repeat(GROWTH_RAW_VALUE_BYTES);
    for beat in 0..GROWTH_INCOMING_BEATS {
        store
            .put_setting(&format!("cluster-check.raw-growth.{beat:05}"), &raw_value)
            .await
            .context("raw induced-regression retained-state write")?;
    }
    let raw_applied_after = applied_index(&metrics_client).await?;
    let raw_measured_snapshot =
        ensure_compaction_after(&metrics_client, store.as_ref(), raw_snapshot, "raw control")
            .await?;
    let raw_settled_snapshot = ensure_compaction_after(
        &metrics_client,
        store.as_ref(),
        Some(raw_measured_snapshot),
        "raw settle",
    )
    .await?;
    settle_post_snapshot_tail(&metrics_client, store.as_ref(), raw_settled_snapshot, "raw").await?;
    let snapshot_metrics_after = snapshot_metrics.snapshot();
    let build_ok_delta = snapshot_metrics_after
        .build_ok
        .count
        .saturating_sub(snapshot_metrics_before.build_ok.count);
    if build_ok_delta < 6 {
        bail!(
            "six observed real compactions produced only {build_ok_delta} successful snapshot build metrics"
        );
    }
    for (name, before, after) in [
        (
            "build error",
            snapshot_metrics_before.build_error.count,
            snapshot_metrics_after.build_error.count,
        ),
        (
            "install success",
            snapshot_metrics_before.install_ok.count,
            snapshot_metrics_after.install_ok.count,
        ),
        (
            "install error",
            snapshot_metrics_before.install_error.count,
            snapshot_metrics_after.install_error.count,
        ),
    ] {
        if after != before {
            bail!(
                "compacted-growth build-only flow unexpectedly changed {name} snapshot metrics from {before} to {after}"
            );
        }
    }
    let raw_after_bytes = stable_directory_bytes(&data_dir).await?;
    let raw_report = CompactedGrowthReport {
        incoming_beats: GROWTH_INCOMING_BEATS,
        active_streams: GROWTH_ACTIVE_STREAMS,
        logical_span_seconds,
        physical_progress_commits: GROWTH_INCOMING_BEATS,
        applied_index_delta: raw_applied_after.saturating_sub(raw_applied_before),
        before_bytes: raw_before_bytes,
        after_bytes: raw_after_bytes,
        compacted_growth_bytes: raw_after_bytes.saturating_sub(raw_before_bytes),
    };
    let raw_rejection = validate_compacted_growth(&raw_report)
        .expect_err("bypassing the coalescer must fail the growth gate");
    let raw_rejection = format!("{raw_rejection:#}");
    for required in ["physical progress commits", "compacted growth"] {
        if !raw_rejection.contains(required) {
            bail!("raw control did not exercise the {required} budget: {raw_rejection}");
        }
    }

    println!(
        "CLUSTER_GROWTH incoming_beats={} active_streams={} beat_interval_seconds={} \
         logical_span_seconds={} physical_commits={} applied_index_delta={} commit_budget={} \
         before_bytes={} after_bytes={} compacted_growth_bytes={} \
         bytes_per_beat={:.6} budget_bytes_per_beat={} \
         raw_control_physical_commits={} raw_control_applied_index_delta={} \
         raw_control_growth_bytes={} raw_control_bytes_per_beat={:.6} \
         snapshot_build_ok_delta={} raw_control_rejected={}",
        report.incoming_beats,
        report.active_streams,
        GROWTH_BEAT_INTERVAL_SECS,
        report.logical_span_seconds,
        report.physical_progress_commits,
        report.applied_index_delta,
        report.active_streams * (expected_commits_per_stream + GROWTH_COMMIT_HEADROOM_PER_STREAM),
        report.before_bytes,
        report.after_bytes,
        report.compacted_growth_bytes,
        report.compacted_growth_bytes as f64 / report.incoming_beats as f64,
        GROWTH_BYTES_PER_BEAT_BUDGET,
        raw_report.physical_progress_commits,
        raw_report.applied_index_delta,
        raw_report.compacted_growth_bytes,
        raw_report.compacted_growth_bytes as f64 / raw_report.incoming_beats as f64,
        build_ok_delta,
        raw_rejection,
    );
    Ok(())
}

async fn growth_fixture(store: &HiqliteAuthStore) -> Result<(Vec<i64>, i64)> {
    let library = store
        .create_library(&NewLibrary {
            name: "Compacted growth".to_owned(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/cluster-growth")],
            anime: false,
        })
        .await?;
    let item_id = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Progress load".to_owned(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await?;
    let mut users = Vec::with_capacity(usize::try_from(GROWTH_ACTIVE_STREAMS)?);
    for ordinal in 0..GROWTH_ACTIVE_STREAMS {
        users.push(
            store
                .create_user(&format!("growth-{ordinal}"), "hash", false)
                .await?
                .id,
        );
    }
    Ok((users, item_id))
}

async fn applied_index(client: &Client) -> Result<u64> {
    client
        .metrics_db()
        .await?
        .last_applied
        .map(|log| log.index)
        .context("replicated store has no applied log index")
}

async fn snapshot_index(client: &Client) -> Result<Option<u64>> {
    Ok(client.metrics_db().await?.snapshot.map(|log| log.index))
}

async fn ensure_compaction_after(
    client: &Client,
    store: &HiqliteAuthStore,
    previous_snapshot: Option<u64>,
    phase: &str,
) -> Result<u64> {
    let metrics = client.metrics_db().await?;
    if let Some(snapshot) = metrics
        .snapshot
        .filter(|log| previous_snapshot.is_none_or(|previous| log.index > previous))
    {
        return wait_for_purge(client, snapshot.index, phase).await;
    }

    // OpenRaft's LogsSinceLast policy triggers from the committed index, not
    // from publication of the asynchronously built snapshot. Continuing to
    // write until the snapshot metric appears creates a scheduler-dependent
    // tail and contaminates the fixed-tail size comparison. Quorum-anchor the
    // starting point, submit exactly enough writes to reach the trigger, then
    // stop all writes while the snapshot builds and publishes.
    let committed = client
        .db_quorum_watermark()
        .await
        .with_context(|| format!("{phase} obtain pre-compaction commit watermark"))?
        .committed_index;
    let writes = snapshot_trigger_plan(previous_snapshot, committed)?;
    let marker = format!("cluster.growth.compaction.{phase}");
    for ordinal in 0..writes {
        store.put_setting(&marker, &ordinal.to_string()).await?;
    }
    let final_committed = client
        .db_quorum_watermark()
        .await
        .with_context(|| format!("{phase} confirm exact compaction trigger"))?
        .committed_index;
    let expected_final = committed
        .checked_add(writes)
        .context("compaction trigger commit index overflowed")?;
    if final_committed != expected_final {
        bail!(
            "{phase} compaction trigger was contaminated by concurrent writes: \
             committed {committed} + {writes} planned writes reached {final_committed}, \
             expected exact {expected_final}"
        );
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let metrics = client.metrics_db().await?;
        if let Some(snapshot) = metrics
            .snapshot
            .filter(|log| previous_snapshot.is_none_or(|previous| log.index > previous))
        {
            return wait_for_purge(client, snapshot.index, phase).await;
        }
        if Instant::now() >= deadline {
            bail!("{phase} did not create a snapshot after the bounded write load");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn snapshot_trigger_plan(previous_snapshot: Option<u64>, committed: u64) -> Result<u64> {
    if previous_snapshot.is_some_and(|snapshot| committed < snapshot) {
        bail!("compaction commit {committed} precedes the observed snapshot {previous_snapshot:?}");
    }
    let snapshot_next = previous_snapshot
        .map(|snapshot| {
            snapshot
                .checked_add(1)
                .context("compaction snapshot next-index overflowed")
        })
        .transpose()?
        .unwrap_or(0);
    let target_next = snapshot_next
        .checked_add(GROWTH_COMPACTION_LOGS)
        .context("compaction snapshot trigger next-index overflowed")?;
    let committed_next = committed
        .checked_add(1)
        .context("compaction commit next-index overflowed")?;
    Ok(target_next.saturating_sub(committed_next))
}

async fn wait_for_purge(client: &Client, snapshot: u64, phase: &str) -> Result<u64> {
    let deadline = Instant::now() + Duration::from_secs(30);
    // The production raft config deliberately retains one log that is already
    // represented by the snapshot. Everything before it must be gone.
    let required_purge = snapshot.saturating_sub(1);
    loop {
        let metrics = client.metrics_db().await?;
        if metrics
            .purged
            .is_some_and(|log| log.index >= required_purge)
        {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            bail!("{phase} snapshot {snapshot} was not followed by purge through {required_purge}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn settle_post_snapshot_tail(
    client: &Client,
    store: &HiqliteAuthStore,
    snapshot: u64,
    phase: &str,
) -> Result<()> {
    let target = snapshot.saturating_add(GROWTH_SETTLED_LOG_TAIL);
    // A write acknowledgement and OpenRaft's metrics-watch publication are
    // distinct completion paths. Anchor both ends with a quorum-confirmed
    // commit watermark so a temporarily stale `last_applied` sample cannot
    // make the harness submit one extra settling write.
    let applied = wait_for_quorum_applied(client, phase).await?;
    if applied > target {
        bail!(
            "{phase} snapshot was observed with a {}-entry tail, beyond the fixed {}-entry settling boundary",
            applied.saturating_sub(snapshot),
            GROWTH_SETTLED_LOG_TAIL
        );
    }
    let marker = "cluster.growth.post_snapshot_tail";
    for ordinal in applied..target {
        store
            .put_setting(marker, &ordinal.saturating_sub(snapshot).to_string())
            .await?;
    }
    let applied = wait_for_quorum_applied(client, phase).await?;
    if applied != target {
        bail!("{phase} post-snapshot tail settled at {applied}, expected exact index {target}");
    }
    Ok(())
}

async fn wait_for_quorum_applied(client: &Client, phase: &str) -> Result<u64> {
    let committed = client
        .db_quorum_watermark()
        .await
        .with_context(|| format!("{phase} obtain settling commit watermark"))?
        .committed_index;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let applied = applied_index(client).await?;
        if applied >= committed {
            return Ok(applied);
        }
        if Instant::now() >= deadline {
            bail!("{phase} applied index did not reach quorum-confirmed commit {committed}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn stable_directory_bytes(root: &Path) -> Result<u64> {
    // Snapshot and WAL cleanup runs after the Raft purge metric advances.
    // Let that task begin, then require three seconds of an unchanged total.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut previous = None;
    let mut stable_samples = 0_u8;
    loop {
        let current = directory_bytes(root)?;
        if previous == Some(current) {
            stable_samples += 1;
            if stable_samples >= 30 {
                return Ok(current);
            }
        } else {
            previous = Some(current);
            stable_samples = 0;
        }
        if Instant::now() >= deadline {
            bail!("compacted directory size did not settle under {root:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn directory_bytes(root: &Path) -> Result<u64> {
    let mut total = 0_u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

#[derive(Clone, Copy, Debug)]
enum FailureTarget {
    Leader,
    Follower,
}

async fn run_failure_case(
    target: FailureTarget,
) -> Result<failure_drills::FailureDrillObservation> {
    let executable = harness_executable()?;
    let root = tempfile::tempdir().context("cluster-check data root")?;
    let (mut cluster, specs) = start_cluster_with_port_retry(&executable, root.path(), 3).await?;

    cluster.request(1, Request::Bootstrap).await?.require_ok()?;
    cluster
        .request(1, Request::RejectIdentityDrift)
        .await?
        .require_ok()?;
    for node_id in 2..=3 {
        cluster
            .request(node_id, Request::Open)
            .await?
            .require_ok()?;
    }
    cluster.wait_for_voters(&[1, 2, 3]).await?;
    for node_id in 1..=3 {
        let status = cluster
            .wait_for_replication_health(node_id, ReplicationHealth::InSync)
            .await?;
        if !status.clustered
            || status.last_applied_term.is_none()
            || status.last_applied_index.is_none()
            || status.last_converged_at.is_none()
        {
            bail!("voter {node_id} in-sync status omitted its convergence point: {status:?}");
        }
    }
    prove_passive_raft_observer(&mut cluster).await?;
    prove_local_telemetry_sidecars(&mut cluster).await?;

    // Every voter exercises an immediate cache read after its acknowledged
    // write. Record the current leader so the harness explicitly proves that
    // at least one of those call paths ran through a non-leader.
    let leader = cluster.leader().await?;
    let mut exercised_follower = false;
    for ordinal in 1..=3 {
        exercised_follower |= ordinal != leader;
        cluster
            .request(ordinal, Request::Exercise { ordinal })
            .await?
            .require_ok()?;
    }
    if !exercised_follower {
        bail!("cache read-after-write proof did not exercise a follower");
    }
    cluster.wait_for_equal_dumps().await?;
    let catalog = cluster.wait_for_equal_catalog_views().await?;
    if matches!(target, FailureTarget::Follower) {
        prove_local_fts_rebuild(&mut cluster, &catalog, 2).await?;
    }

    let leader = cluster.leader().await?;
    let target_id = match target {
        FailureTarget::Leader => leader,
        FailureTarget::Follower => (1..=3)
            .find(|node_id| *node_id != leader)
            .context("choose follower")?,
    };
    let failure_name = format!("{target:?}").to_ascii_lowercase();
    let survivor = (1..=3)
        .find(|node_id| *node_id != target_id)
        .context("choose survivor")?;
    println!(
        "CLUSTER_FAILURE_START target={failure_name} initial_leader={leader} failed_node={target_id} request_target={survivor}"
    );
    let mut loss_started = None;
    let mut recovery_millis = None;
    let mut request_errors = 0_u64;
    let mut raw_write_latency_millis = Vec::with_capacity(usize::try_from(
        failure_drills::FAILURE_DRILL_WRITE_OPERATIONS,
    )?);

    // Keep one fixed-cadence workload running across the process loss. There
    // is deliberately no readiness wait between kill and write 33: the first
    // surviving request absorbs the election, and its acknowledged completion
    // is the recovery point retained in the artifact.
    for ordinal in 0..failure_drills::FAILURE_DRILL_WRITE_OPERATIONS {
        if ordinal == failure_drills::FAILURE_DRILL_WRITE_OPERATIONS / 2 {
            loss_started = Some(Instant::now());
            cluster.kill(target_id).await?;
        }
        let request_target = if loss_started.is_some() {
            survivor
        } else {
            leader
        };
        let value = format!("failure-drill-{failure_name}-{ordinal:02}");
        let attempt_started = Instant::now();
        let outcome = cluster
            .request(request_target, Request::TopologyWrite { ordinal, value })
            .await
            .and_then(Response::require_ok);
        raw_write_latency_millis.push(u64::try_from(attempt_started.elapsed().as_millis())?);
        if let Err(error) = outcome {
            request_errors = request_errors.saturating_add(1);
            eprintln!("cluster-check: {failure_name} workload write {ordinal} failed: {error:#}");
        } else if recovery_millis.is_none() {
            if let Some(started) = loss_started {
                recovery_millis = Some(u64::try_from(started.elapsed().as_millis())?);
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    if request_errors != 0 {
        let recovered_leader = cluster.leader().await?;
        bail!(
            "{failure_name} loss produced {request_errors} failed requests in the fixed 64-write workload; initial leader {leader}, failed node {target_id}, request target {survivor}, recovered leader {recovered_leader}"
        );
    }
    let recovery_millis = recovery_millis.context("post-loss workload never recovered")?;

    cluster.wait_for_ready(survivor).await?;
    cluster
        .request(survivor, Request::VerifyProof)
        .await?
        .require_ok()?;
    cluster
        .request(
            survivor,
            Request::PostLossWrite {
                target: failure_name.clone(),
                position_ms: 60_000,
            },
        )
        .await?
        .require_ok()?;
    let current_leader = cluster.leader().await?;
    let first_degraded = cluster
        .wait_for_replication_health(current_leader, ReplicationHealth::Degraded)
        .await?;
    let first_lag = first_degraded
        .behind_by
        .filter(|lag| *lag > 0)
        .context("first degraded status omitted its known lag")?;
    let converged_at = first_degraded
        .last_converged_at
        .context("first degraded status omitted its prior convergence")?;

    let repeated_degraded = cluster
        .wait_for_replication_health(current_leader, ReplicationHealth::Degraded)
        .await?;
    if repeated_degraded
        .behind_by
        .is_none_or(|lag| lag < first_lag)
        || repeated_degraded.last_converged_at != Some(converged_at)
    {
        bail!(
            "repeated degraded sample forgot known lag or prior convergence: first={first_degraded:?}, repeated={repeated_degraded:?}"
        );
    }

    cluster
        .request(
            survivor,
            Request::PostLossWrite {
                target: format!("{failure_name}-later"),
                position_ms: 90_000,
            },
        )
        .await?
        .require_ok()?;
    let current_leader = cluster.leader().await?;
    let advanced_degraded = cluster
        .wait_for_replication_health(current_leader, ReplicationHealth::Degraded)
        .await?;
    if advanced_degraded
        .behind_by
        .is_none_or(|lag| lag <= first_lag)
        || advanced_degraded.last_converged_at != Some(converged_at)
    {
        bail!(
            "degraded status did not grow its lag after another acknowledged watch write: first={first_degraded:?}, advanced={advanced_degraded:?}"
        );
    }
    cluster.wait_for_equal_dumps().await?;
    let current_leader = cluster.leader().await?;
    let follower = cluster
        .node_ids()
        .into_iter()
        .find(|node_id| *node_id != current_leader)
        .context("choose surviving follower for replication status")?;
    let follower_status = cluster
        .wait_for_replication_health(follower, ReplicationHealth::InSync)
        .await?;
    if !follower_status
        .explanation
        .contains("other nodes is visible on the leader")
        || follower_status.explanation.contains("every reporting peer")
    {
        bail!(
            "follower replication status claimed peer visibility it does not have: {follower_status:?}"
        );
    }
    let post_loss_key = format!("post_loss.{failure_name}");
    let post_loss_dump = match cluster.request(survivor, Request::Dump).await? {
        Response::Dump { digest, dump } => {
            if digest != hex::encode(Sha256::digest(dump.as_bytes())) {
                bail!("post-loss dump digest is not anchored to the returned rows");
            }
            dump
        }
        response => bail!("unexpected post-loss dump response: {response:?}"),
    };
    require_dump_setting(&post_loss_dump, &post_loss_key, "acknowledged")?;
    for ordinal in 0..failure_drills::FAILURE_DRILL_WRITE_OPERATIONS {
        require_dump_setting(
            &post_loss_dump,
            &format!("cluster.topology.write.{ordinal:04}"),
            &format!("failure-drill-{failure_name}-{ordinal:02}"),
        )?;
    }
    let writes_preserved = true;
    if cluster.wait_for_equal_catalog_views().await?.search.len() != 3 {
        bail!("post-loss catalogue/search proof lost rows");
    }

    if matches!(target, FailureTarget::Follower) {
        let voters_before = match cluster.request(survivor, Request::Metrics).await? {
            Response::Metrics { voters, .. } => voters,
            response => bail!("unexpected pre-refusal metrics response: {response:?}"),
        };
        let refused = run_incompatible_preflight(&executable, &specs).await?;
        if !refused.contains("incompatible with voter schema") {
            bail!("old-schema voter was not refused: {refused}");
        }
        let voters_after = match cluster.request(survivor, Request::Metrics).await? {
            Response::Metrics { voters, .. } => voters,
            response => bail!("unexpected post-refusal metrics response: {response:?}"),
        };
        if voters_after != voters_before {
            bail!(
                "rejected preflight changed raft membership: {voters_before:?} -> {voters_after:?}"
            );
        }
    }

    // Retain the actual current leader for the quorum-loss proof. Its latest
    // commit watermark must be current and locally applied before the second
    // process loss; this rules out a test that merely starts with stale data.
    let quorum_loss_survivor = cluster.leader().await?;
    let watermark_deadline = Instant::now() + Duration::from_secs(5);
    let before_quorum_loss = loop {
        let sample = passive_raft_observation(&mut cluster, quorum_loss_survivor).await?;
        if sample.valid
            && sample.watermark_valid
            && sample.watermark_age_millis.is_some_and(|age| age < 1_000)
            && sample.committed_index == Some(sample.applied_index)
            && sample.apply_lag_entries == Some(0)
        {
            break sample;
        }
        if Instant::now() >= watermark_deadline {
            bail!(
                "current leader {quorum_loss_survivor} never published a fully applied pre-loss watermark: {sample:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let direct_before_loss =
        quorum_watermark_observation(&mut cluster, quorum_loss_survivor).await?;
    if direct_before_loss.leader_id != quorum_loss_survivor
        || direct_before_loss.term != before_quorum_loss.current_term
        || direct_before_loss.committed_index < before_quorum_loss.applied_index
    {
        bail!(
            "direct pre-loss watermark did not describe retained leader {quorum_loss_survivor}: passive={before_quorum_loss:?}, direct={direct_before_loss:?}"
        );
    }

    // One more loss removes quorum. The former leader remains alive and its
    // local applied state remains fresh, but it must be unable to renew the
    // original one-second proof. At and beyond that proof's exact deadline,
    // the retained commit stays diagnostic-only and lag disappears.
    let second_loss = (1..=3)
        .find(|node_id| *node_id != target_id && *node_id != quorum_loss_survivor)
        .context("choose second loss")?;
    cluster.kill(second_loss).await?;
    let expiry_deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let sample = passive_raft_observation(&mut cluster, quorum_loss_survivor).await?;
        if sample.watermark_age_millis.is_some_and(|age| age >= 1_000) {
            if !sample.valid
                || sample.watermark_valid
                || sample.applied_index != before_quorum_loss.applied_index
                || sample.committed_index != before_quorum_loss.committed_index
                || sample.apply_lag_entries.is_some()
            {
                bail!(
                    "former leader renewed or misreported an expired quorum proof: before={before_quorum_loss:?}, after={sample:?}"
                );
            }
            break;
        }
        if Instant::now() >= expiry_deadline {
            bail!(
                "former leader {quorum_loss_survivor} did not expose the original watermark expiry"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    loop {
        let sample = passive_raft_observation(&mut cluster, quorum_loss_survivor).await?;
        if sample.watermark_errors > before_quorum_loss.watermark_errors {
            break;
        }
        if Instant::now() >= expiry_deadline {
            bail!(
                "former leader {quorum_loss_survivor} expired its proof but did not report a failed renewal"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for request in [
        Request::QuorumWatermark,
        Request::Ping,
        Request::ReadWithoutQuorum,
        Request::WriteWithoutQuorum,
    ] {
        let response = cluster.request(quorum_loss_survivor, request).await?;
        require_quorum_error(response)?;
    }
    cluster.assert_running().await?;

    let request_attempts = u64::try_from(raw_write_latency_millis.len())?;
    let observation = failure_drills::FailureDrillObservation {
        target: failure_name,
        initial_leader: leader,
        failed_node: target_id,
        replacement_leader: current_leader,
        write_operations: failure_drills::FAILURE_DRILL_WRITE_OPERATIONS,
        request_attempts,
        request_errors,
        raw_write_latency_millis,
        recovery_millis,
        writes_preserved,
    };
    cluster.kill_all().await;
    Ok(observation)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeSpec {
    pub id: u64,
    pub raft: String,
    pub api: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeLaunch {
    pub node_id: u64,
    pub root: PathBuf,
    pub nodes: Vec<NodeSpec>,
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    /// Local read-only connection pool, carried to the voter that is actually
    /// measured. Without it every arm of a 4/8/16 comparison would launch on
    /// the same default pool and the run would validate whichever number the
    /// report happened to claim.
    #[serde(default = "default_read_pool_size")]
    pub read_pool_size: usize,
    /// P2f control-arm switch. This field reaches a validation-only hook in
    /// plurx-core; production plurxd builds have no corresponding switch.
    #[serde(default = "instrument_store_operations_by_default")]
    pub instrument_store_operations: bool,
    #[serde(default)]
    pub emulate_old_watermark_handler: bool,
    #[serde(default)]
    pub emulate_p3a_watermark_handler: bool,
    /// What this process starts as. A learner sets Hiqlite's `learner_only`
    /// hint, so it adds itself to Raft as a learner and then stops, rather than
    /// asking the leader to promote it — exactly what the daemon does with the
    /// role its join token admitted it under.
    #[serde(default)]
    pub role: ClusterRole,
    /// Start a binary that heartbeats without proving the learner protocol.
    /// Env-plumbed like the two watermark emulations beside it, and read on the
    /// far side only by a `plurx-core` compiled with `cluster-validation`.
    #[serde(default)]
    pub emulate_pre_learner_heartbeat: bool,
}

/// A voter launch with no emulation, which is what almost every call site
/// wants. Written out rather than derived so a new field is a compile error at
/// the one place that decides its default, not silently `false` everywhere.
impl NodeLaunch {
    pub fn voter(node_id: u64, root: PathBuf, nodes: Vec<NodeSpec>) -> Self {
        Self {
            node_id,
            root,
            nodes,
            listen_addr: default_listen_addr(),
            read_pool_size: default_read_pool_size(),
            instrument_store_operations: true,
            emulate_old_watermark_handler: false,
            emulate_p3a_watermark_handler: false,
            role: ClusterRole::Voter,
            emulate_pre_learner_heartbeat: false,
        }
    }

    /// The same launch for a node admitted as a non-voting learner.
    #[must_use]
    pub fn as_learner(mut self) -> Self {
        self.role = ClusterRole::Learner;
        self
    }

    fn startup_timeout(&self) -> Duration {
        if self.role.is_learner() {
            LEARNER_START_TIMEOUT
        } else {
            START_TIMEOUT
        }
    }
}

fn default_listen_addr() -> String {
    LISTEN_ADDR.to_owned()
}

fn instrument_store_operations_by_default() -> bool {
    true
}

/// The daemon's own default, read from the daemon's own config type so the two
/// cannot drift.
pub fn default_read_pool_size() -> usize {
    plurx_core::config::ClusterConfig::default().read_pool_size
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preflight {
    pub addresses: Vec<String>,
    pub compatibility: ClusterCompatibility,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ServingLaunch {
    proxy_addresses: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ServingReady {
    http_address: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// Recreate the origin/main M3 table shape and its former shared join-URL
    /// rows before MembershipManager starts, proving the rolling upgrade path
    /// rather than only the schema a fresh binary would create.
    SeedLegacyArtworkUrls,
    /// Recreate the previous rolling binary's crash window: its first write
    /// reserved the token, while the node/staging transaction never ran.
    SeedLegacyPartialRedemption {
        token_digest: String,
        node_id: String,
    },
    SeedHistoricalHttpDuplicate {
        node_id: String,
        public_http_url: String,
    },
    Bootstrap,
    RejectIdentityDrift,
    Open,
    IssueJoinToken {
        ttl_ms: u64,
    },
    /// Mint a token that admits a non-voting learner. The role lives in the
    /// coordinator's token record, never in the joiner's redeem request, so
    /// this is the only place a learner can be asked for.
    IssueLearnerJoinToken {
        ttl_ms: u64,
    },
    /// The operator-facing protocol projection: the cluster's active range,
    /// this binary's range, and the nodes whose running binary is the reason
    /// activation would be refused.
    ProtocolStatus,
    ActivateLearnerProtocol,
    DeactivateLearnerProtocol,
    /// Commit a heartbeat past the write-coalescing window, so a scenario that
    /// has just restarted a process can refresh its capability proof without
    /// waiting one production interval.
    ForceHeartbeat,
    /// Start the production heartbeat loop on this process. The normal
    /// harness keeps writes explicit so compaction assertions have no
    /// background traffic; learner lifecycle cases need the real periodic
    /// target-local proof while another process waits on a promotion or
    /// removal barrier.
    StartHeartbeatLoop,
    /// Take one cluster-wide singleton job through plurxd's own
    /// `acquire_cluster_job`, including its eligibility gate. The harness
    /// compiles that function from plurxd's source, so a learner refused here
    /// is refused by the code the daemon runs.
    AcquireClusterJob {
        resource: String,
    },
    /// Read the durable lease row for `resource`, so a refusal can be told
    /// apart from a lease someone else already held.
    ReadClusterJobLease {
        resource: String,
    },
    RedeemJoin {
        request: RedeemJoinRequest,
    },
    /// Exercise the wire-distinct learner redemption path. Keeping this a
    /// separate harness request prevents the validation process from silently
    /// routing a learner credential through the legacy voter protocol.
    RedeemLearnerJoin {
        request: RedeemJoinRequest,
    },
    FinalizeJoin {
        request: FinalizeJoinRequest,
    },
    /// Exercise the wire-distinct learner finalization path.
    FinalizeLearnerJoin {
        request: FinalizeJoinRequest,
    },
    MembershipStatus,
    /// Probe the target-local replicated membership schema without requiring
    /// the production membership manager to have opened already.
    MembershipSchemaReady,
    Advertisement {
        seed_name: String,
    },
    RenameServer {
        name: String,
    },
    SignActivityRequest {
        target_node_id: String,
        timestamp_ms: i64,
    },
    AuthorizeActivityRequest {
        auth: ActivityPeerAuth,
    },
    SignInternalPeerRequest {
        target_node_id: String,
        timestamp_ms: i64,
        nonce: String,
        method: String,
        path: String,
        body: Vec<u8>,
    },
    AuthorizeInternalPeerRequest {
        auth: InternalPeerAuth,
        method: String,
        path: String,
        body: Vec<u8>,
    },
    ActivityPeers,
    ArtworkPeerUrls,
    RejectDuplicateArtworkUrl {
        public_http_url: String,
    },
    ReuseRemovedArtworkUrl {
        removed_node_id: String,
        public_http_url: String,
    },
    ArtworkPeerAuth {
        filename: String,
    },
    VerifyArtworkPeer {
        filename: String,
        auth: ArtworkPeerAuth,
    },
    TombstoneOfflineFence {
        node_id: String,
    },
    HeartbeatPreservesTombstone {
        node_id: String,
    },
    Heartbeat,
    ObserveArtworkRepair {
        item_id: i64,
        inject_leader_change: bool,
    },
    ReadArtworkRepairFence {
        item_id: i64,
    },
    ClaimArtworkRepair {
        item_id: i64,
        lease_ms: u64,
    },
    ClaimArtworkRepairAfterPause {
        item_id: i64,
        lease_ms: u64,
        pause_ms: u64,
    },
    SeedArtworkRepairFenceItem {
        ordinal: u64,
    },
    ClaimArtworkRepairFence {
        item_id: i64,
        lease_ms: u64,
        inject_leader_change: bool,
    },
    ApplyArtworkRepairFence {
        target_item_id: i64,
        fence: ArtworkRepairFence,
        title: String,
        stale_job_lease: bool,
        stale_book_snapshot: bool,
    },
    AcquireJobLease {
        resource: String,
        owner_node_id: String,
    },
    SeedExhaustedJobLease {
        resource: String,
        owner_node_id: String,
    },
    AcquireJobLeaseLegacy {
        resource: String,
        owner_node_id: String,
    },
    ClearJobOwnerFence {
        owner_node_id: String,
    },
    ApplyJobLease {
        key: String,
        lease: Lease,
        observed_at_ms: Option<i64>,
    },
    StartSingletonProbe {
        provider_url: String,
    },
    ReadSingletonLease,
    SingletonProbeStatus,
    ReplaySingletonLease {
        lease: Lease,
    },
    ReadSingletonValue {
        local: bool,
    },
    PutSetting {
        key: String,
        value: String,
    },
    ReadLocalSetting {
        key: String,
    },
    ForceCompaction {
        phase: String,
    },
    ReleaseArtworkRepair {
        fence: ArtworkRepairFence,
    },
    TriggerElection,
    RemoveVoter {
        node_id: String,
    },
    RemoveNode {
        node_id: String,
    },
    PromoteLearner {
        node_id: String,
    },
    SeedActiveMediaSession {
        node_id: String,
        session_id: String,
    },
    ReadMediaSessionState {
        session_id: String,
    },
    LeaveVoter,
    SeedOfflineRemovalWork {
        node_id: String,
        media_dir: String,
    },
    /// Request a download on this node `delay_ms` from now and answer
    /// immediately, so the package lands while a removal started right after
    /// this call is still resolving. Real operators do exactly this: the
    /// departing node keeps serving its API until the membership change
    /// commits.
    SeedOfflineWorkDuringRemoval {
        node_id: String,
        media_dir: String,
        user_id: i64,
        delay_ms: u64,
    },
    DeleteOfflinePackage {
        package_id: String,
        user_id: i64,
    },
    OfflinePackageSummary {
        package_id: String,
        user_id: i64,
    },
    ClaimNextOfflinePackage {
        node_id: String,
    },
    PublishOfflinePackage {
        package_id: String,
        node_id: String,
    },
    ResetContractState,
    RecordLocalTelemetry {
        marker: String,
    },
    CountLocalTelemetry {
        marker: String,
    },
    Exercise {
        ordinal: u64,
    },
    TopologyWrite {
        ordinal: u64,
        value: String,
    },
    TopologySeedCatalogue {
        rows: u64,
    },
    TopologyCatalogueReads {
        item_ids: Vec<i64>,
        operations: u64,
        concurrency: u64,
    },
    TopologyResources {
        hardware: String,
        storage_device: String,
        network_path: String,
        reset_max_rss: bool,
    },
    StoreInstrumentationStatus,
    PostLossWrite {
        target: String,
        position_ms: i64,
    },
    VerifyProof,
    Dump,
    CatalogView,
    RebuildSearch,
    Metrics,
    /// Full OpenRaft metrics rendered for failure diagnostics. The ordinary
    /// metrics response stays intentionally stable for harness consumers.
    RaftDebug,
    PassiveRaftMetrics,
    QuorumWatermark,
    PauseApply,
    ApplyPauseObserved,
    ResumeApply,
    SetRaftPartitioned {
        partitioned: bool,
    },
    BoundedCatalogueGetItem {
        item_id: i64,
    },
    ProveOldWatermarkStreamCompatibility,
    ProveP3aWatermarkCompatibility,
    ReplicationStatus,
    Ping,
    ReadWithoutQuorum,
    WriteWithoutQuorum,
}

impl Request {
    fn response_timeout(&self) -> Duration {
        match self {
            Self::ForceCompaction { .. } => COMPACTION_RESPONSE_TIMEOUT,
            Self::WriteWithoutQuorum => WRITE_WITHOUT_QUORUM_RESPONSE_TIMEOUT,
            Self::RemoveVoter { .. }
            | Self::RemoveNode { .. }
            | Self::PromoteLearner { .. }
            | Self::LeaveVoter => CONVERGENCE_TIMEOUT,
            _ => REQUEST_TIMEOUT,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Ready {
        node_id: u64,
    },
    Ok,
    Flag {
        value: bool,
    },
    ActivityAuth {
        auth: ActivityPeerAuth,
    },
    InternalPeerAuth {
        auth: InternalPeerAuth,
    },
    ActivityPeers {
        peers: Vec<(String, Option<String>, bool)>,
    },
    SeededOfflineRemovalWork {
        user_id: i64,
    },
    /// Just enough of a package to assert a §6.7 outcome. Deliberately does
    /// not carry `source_path`: a media path has no business crossing a
    /// harness wire any more than it does a log line.
    OfflinePackageSummary {
        state: String,
        node_id: String,
        error_code: Option<String>,
        reserved_bytes: i64,
        actual_bytes: Option<i64>,
    },
    ClaimedOfflinePackage {
        package_id: Option<String>,
    },
    TelemetryCount {
        count: usize,
    },
    TopologyResources {
        sample: ResourceSample,
    },
    StoreInstrumentationStatus {
        enabled: bool,
        recorded_operations_total: u64,
    },
    TopologyCatalogueSeeded {
        item_ids: Vec<i64>,
    },
    TopologyCatalogueReads {
        raw_round_trip_us: Vec<u64>,
        errors: u64,
        consistent_query_calls: u64,
        non_consistent_query_calls: u64,
    },
    Dump {
        digest: String,
        dump: String,
    },
    CatalogView {
        view: CatalogView,
    },
    Metrics {
        leader: Option<u64>,
        current_term: u64,
        voters: Vec<u64>,
        /// Every committed member, voting or not. `members` minus `voters` is
        /// the learner set, and it is derived from the committed Raft
        /// configuration rather than from replicated SQL because Raft is what
        /// decides a member's role.
        members: Vec<u64>,
        applied_index: Option<u64>,
        quorum_acknowledged: bool,
    },
    RaftDebug {
        summary: String,
    },
    PassiveRaftMetrics {
        valid: bool,
        age_seconds: Option<u64>,
        errors: u64,
        leader_changes: u64,
        current_term: Option<u64>,
        applied_index: Option<u64>,
        leader_known: Option<bool>,
        is_leader: Option<bool>,
        watermark_valid: bool,
        local_reads_supported: bool,
        watermark_age_millis: Option<u64>,
        watermark_errors: u64,
        committed_index: Option<u64>,
        apply_lag_entries: Option<u64>,
    },
    QuorumWatermark {
        term: u64,
        leader_id: u64,
        committed_index: u64,
    },
    ApplyPauseObserved {
        observed: bool,
    },
    BoundedCatalogueRead {
        title: Option<String>,
        error: Option<String>,
        consistent_query_calls: u64,
        non_consistent_query_calls: u64,
        watermark_valid: bool,
        watermark_age_millis: Option<u64>,
        local_reads_supported: bool,
        apply_lag_entries: Option<u64>,
        serving_ready: bool,
    },
    ReplicationStatus {
        status: ReplicationStatus,
    },
    IssuedJoinToken {
        token: IssuedJoinToken,
    },
    MembershipStatus {
        status: MembershipStatus,
    },
    Advertisement {
        instance_id: String,
        node_id: String,
        name: String,
        gdm: String,
        mdns_name: String,
        mdns_instance_id: String,
        mdns_node_id: String,
    },
    ProtocolStatus {
        status: ClusterProtocolStatus,
    },
    ProtocolChange {
        change: ProtocolChange,
    },
    ClusterJobAttempt {
        acquired: bool,
        lease: Option<Lease>,
    },
    ArtworkPeerUrls {
        urls: Vec<String>,
    },
    ArtworkPeerAuth {
        auth: ArtworkPeerAuth,
    },
    ItemId {
        item_id: i64,
    },
    ArtworkRepairFence {
        fence: Option<ArtworkRepairFence>,
    },
    JobLease {
        lease: Option<Lease>,
    },
    SingletonProbeStart {
        acquired: bool,
        lease: Lease,
    },
    SingletonLease {
        lease: Option<Lease>,
    },
    SingletonProbeStatus {
        outcome: String,
        cleanup_done: bool,
        cleanup_error: Option<String>,
    },
    SingletonValue {
        value: Option<String>,
    },
    Setting {
        value: Option<String>,
    },
    ArtworkFenceApply {
        setting: bool,
        metadata: bool,
        book: bool,
    },
    MembershipError {
        code: String,
        message: String,
    },
    /// Harness-only preservation of a typed Hiqlite routing verdict. This is
    /// never accepted after a potentially mutating request.
    MembershipLeaderChange {
        message: String,
    },
    Error {
        message: String,
    },
}

/// Prove playback telemetry stays node-local: only the voter that recorded the
/// marker can count it, and the count survives that voter reopening its store.
pub async fn prove_local_telemetry_sidecars(cluster: &mut ClusterProcesses) -> Result<()> {
    let marker = "voter-1-restart-proof".to_owned();
    cluster
        .request(
            1,
            Request::RecordLocalTelemetry {
                marker: marker.clone(),
            },
        )
        .await?
        .require_ok()?;
    for node_id in cluster.node_ids() {
        let expected = usize::from(node_id == 1);
        match cluster
            .request(
                node_id,
                Request::CountLocalTelemetry {
                    marker: marker.clone(),
                },
            )
            .await?
        {
            Response::TelemetryCount { count } if count == expected => {}
            response => {
                bail!("voter {node_id} local telemetry count was not {expected}: {response:?}")
            }
        }
    }
    cluster.request(1, Request::Open).await?.require_ok()?;
    match cluster
        .request(1, Request::CountLocalTelemetry { marker })
        .await?
    {
        Response::TelemetryCount { count: 1 } => Ok(()),
        response => bail!("voter-1 telemetry did not survive reopen: {response:?}"),
    }
}

#[derive(Clone, Copy, Debug)]
struct PassiveRaftObservation {
    valid: bool,
    age_seconds: Option<u64>,
    errors: u64,
    leader_changes: u64,
    current_term: u64,
    applied_index: u64,
    leader_known: bool,
    is_leader: bool,
    watermark_valid: bool,
    local_reads_supported: bool,
    watermark_age_millis: Option<u64>,
    watermark_errors: u64,
    committed_index: Option<u64>,
    apply_lag_entries: Option<u64>,
}

async fn passive_raft_observation(
    cluster: &mut ClusterProcesses,
    node_id: u64,
) -> Result<PassiveRaftObservation> {
    match cluster
        .request(node_id, Request::PassiveRaftMetrics)
        .await?
    {
        Response::PassiveRaftMetrics {
            valid,
            age_seconds,
            errors,
            leader_changes,
            current_term: Some(current_term),
            applied_index: Some(applied_index),
            leader_known: Some(leader_known),
            is_leader: Some(is_leader),
            watermark_valid,
            local_reads_supported,
            watermark_age_millis,
            watermark_errors,
            committed_index,
            apply_lag_entries,
        } => Ok(PassiveRaftObservation {
            valid,
            age_seconds,
            errors,
            leader_changes,
            current_term,
            applied_index,
            leader_known,
            is_leader,
            watermark_valid,
            local_reads_supported,
            watermark_age_millis,
            watermark_errors,
            committed_index,
            apply_lag_entries,
        }),
        response => bail!("voter {node_id} passive Raft sample was absent: {response:?}"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QuorumWatermarkObservation {
    term: u64,
    leader_id: u64,
    committed_index: u64,
}

async fn quorum_watermark_observation(
    cluster: &mut ClusterProcesses,
    node_id: u64,
) -> Result<QuorumWatermarkObservation> {
    match cluster.request(node_id, Request::QuorumWatermark).await? {
        Response::QuorumWatermark {
            term,
            leader_id,
            committed_index,
        } => Ok(QuorumWatermarkObservation {
            term,
            leader_id,
            committed_index,
        }),
        response => bail!("voter {node_id} quorum watermark was absent: {response:?}"),
    }
}

/// Prove the production passive observer follows real three-voter apply and
/// election events without a Store or management request on its sample path.
async fn prove_passive_raft_observer(cluster: &mut ClusterProcesses) -> Result<()> {
    let initial_deadline = Instant::now() + Duration::from_secs(10);
    let initial = loop {
        let mut samples = Vec::new();
        let mut ready = true;
        for node_id in 1..=3 {
            let sample = passive_raft_observation(cluster, node_id).await?;
            ready &= sample.valid
                && sample.leader_known
                && sample.errors == 0
                && sample.watermark_valid
                && sample.watermark_age_millis.is_some()
                && sample.committed_index.is_some()
                && sample.apply_lag_entries.is_some();
            samples.push(sample);
        }
        if ready {
            break samples;
        }
        if Instant::now() >= initial_deadline {
            bail!("passive Raft observers did not publish fresh initial samples: {samples:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    let leader = cluster.leader().await?;
    cluster
        .request(
            leader,
            Request::TopologyWrite {
                ordinal: 9_001,
                value: "passive-observer-proof".to_owned(),
            },
        )
        .await?
        .require_ok()?;
    let apply_deadline = Instant::now() + Duration::from_secs(10);
    for node_id in 1..=3 {
        let baseline = initial[(node_id - 1) as usize].applied_index;
        loop {
            let sample = passive_raft_observation(cluster, node_id).await?;
            if sample.valid && sample.applied_index > baseline {
                break;
            }
            if Instant::now() >= apply_deadline {
                bail!(
                    "voter {node_id} passive applied index did not advance beyond {baseline}: {sample:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    let mut proofs = Vec::new();
    for node_id in 1..=3 {
        let proof = quorum_watermark_observation(cluster, node_id).await?;
        let local = passive_raft_observation(cluster, node_id).await?;
        if proof.committed_index < local.applied_index {
            bail!(
                "voter {node_id} quorum watermark {} did not cover local applied {}",
                proof.committed_index,
                local.applied_index
            );
        }
        proofs.push(proof);
    }
    if proofs
        .iter()
        .any(|proof| proof.term != proofs[0].term || proof.leader_id != proofs[0].leader_id)
    {
        bail!("three voters disagreed on the quorum proof: {proofs:?}");
    }
    if proofs[0].leader_id != leader {
        bail!(
            "quorum proof named leader {} but cluster leader is {leader}",
            proofs[0].leader_id
        );
    }

    let before_renewal = [
        passive_raft_observation(cluster, 1).await?.applied_index,
        passive_raft_observation(cluster, 2).await?.applied_index,
        passive_raft_observation(cluster, 3).await?.applied_index,
    ];
    let before_watermark_errors = [
        initial[0].watermark_errors,
        initial[1].watermark_errors,
        initial[2].watermark_errors,
    ];
    for _ in 0..3 {
        for node_id in 1..=3 {
            let renewed = quorum_watermark_observation(cluster, node_id).await?;
            if renewed.term != proofs[0].term
                || renewed.leader_id != proofs[0].leader_id
                || renewed.committed_index != proofs[0].committed_index
            {
                bail!("stable-term quorum renewal changed the proof: {renewed:?}");
            }
        }
    }
    for node_id in 1..=3 {
        let after = passive_raft_observation(cluster, node_id).await?;
        let before = before_renewal[(node_id - 1) as usize];
        if after.applied_index != before {
            bail!(
                "quorum watermark renewals consumed Raft entries on voter {node_id}: {before} -> {}",
                after.applied_index
            );
        }
        if after.watermark_errors < before_watermark_errors[(node_id - 1) as usize] {
            bail!("voter {node_id} quorum watermark error counter regressed");
        }
    }

    let before_election = [
        passive_raft_observation(cluster, 1).await?,
        passive_raft_observation(cluster, 2).await?,
        passive_raft_observation(cluster, 3).await?,
    ];
    let election_target = (1..=3)
        .find(|node_id| *node_id != leader)
        .context("choose passive-observer election target")?;
    cluster
        .request(election_target, Request::TriggerElection)
        .await?
        .require_ok()?;
    let election_deadline = Instant::now() + Duration::from_secs(10);
    let new_leader = loop {
        let candidate = cluster.leader().await?;
        if candidate != leader {
            break candidate;
        }
        if Instant::now() >= election_deadline {
            bail!("passive-observer election did not replace leader {leader}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    let mut observed_local_leaders = 0;
    let observation_deadline = Instant::now() + Duration::from_secs(10);
    for node_id in 1..=3 {
        let before = before_election[(node_id - 1) as usize];
        loop {
            let sample = passive_raft_observation(cluster, node_id).await?;
            if sample.valid
                && sample.leader_known
                && sample.current_term > before.current_term
                && sample.leader_changes > before.leader_changes
            {
                if sample.is_leader {
                    observed_local_leaders += 1;
                    if node_id != new_leader {
                        bail!("voter {node_id} claimed leadership but leader is {new_leader}");
                    }
                }
                if sample.age_seconds.is_none() || sample.errors != 0 {
                    bail!("voter {node_id} published an invalid passive sample: {sample:?}");
                }
                break;
            }
            if Instant::now() >= observation_deadline {
                bail!("voter {node_id} passive observer missed the live election: {sample:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    if observed_local_leaders != 1 {
        bail!("passive observer reported {observed_local_leaders} local leaders after election");
    }

    let watermark_recovery_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut recovered = Vec::new();
        for node_id in 1..=3 {
            recovered.push(passive_raft_observation(cluster, node_id).await?);
        }
        if recovered.iter().all(|sample| {
            sample.watermark_valid
                && sample.current_term > proofs[0].term
                && sample.watermark_age_millis.is_some()
                && sample.committed_index.is_some()
                && sample.apply_lag_entries.is_some()
        }) {
            break;
        }
        if Instant::now() >= watermark_recovery_deadline {
            bail!("background quorum samplers did not recover after idle election: {recovered:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let mut successor_proofs = Vec::new();
    for node_id in 1..=3 {
        let successor = quorum_watermark_observation(cluster, node_id).await?;
        if successor.term <= proofs[0].term || successor.leader_id != new_leader {
            bail!(
                "voter {node_id} did not replace quorum generation {:?}: {successor:?}",
                proofs[0]
            );
        }
        successor_proofs.push(successor);
    }
    if successor_proofs.iter().any(|proof| {
        proof.term != successor_proofs[0].term
            || proof.leader_id != successor_proofs[0].leader_id
            || proof.committed_index != successor_proofs[0].committed_index
    }) {
        bail!("voters disagreed after idle election: {successor_proofs:?}");
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogView {
    pub authoritative_digest: String,
    pub full_digest: String,
    pub search: Vec<i64>,
}

impl Response {
    pub fn require_ok(self) -> Result<()> {
        match self {
            Self::Ok => Ok(()),
            Self::Error { message } => Err(anyhow!(message)),
            other => bail!("expected OK response, got {other:?}"),
        }
    }
}

/// Accept only a store failure that names the lost quorum. An ordinary success
/// or an unrelated error both mean the readiness contract was not proved.
pub fn require_quorum_error(response: Response) -> Result<()> {
    let Response::Error { message } = response else {
        bail!("ordinary store operation succeeded without quorum: {response:?}");
    };
    let normalized = message.to_ascii_lowercase();
    if !normalized.contains("quorum")
        && !normalized.contains("timed out")
        && !normalized.contains("raft leader")
    {
        bail!("store failed without quorum for an unrelated reason: {message}");
    }
    Ok(())
}

/// Require one `key=value` settings row inside a voter's local dump JSON.
pub fn require_dump_setting(dump: &str, key: &str, expected: &str) -> Result<()> {
    let dump: serde_json::Value = serde_json::from_str(dump)?;
    let settings = dump
        .get("settings")
        .and_then(serde_json::Value::as_array)
        .context("local dump has no settings rows")?;
    if !settings.iter().any(|row| {
        row.get("key").and_then(serde_json::Value::as_str) == Some(key)
            && row.get("value").and_then(serde_json::Value::as_str) == Some(expected)
    }) {
        bail!("local dump is missing expected setting {key}={expected}");
    }
    Ok(())
}

pub struct NodeProcess {
    id: u64,
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl NodeProcess {
    pub fn spawn(executable: &Path, launch: &NodeLaunch) -> Result<Self> {
        let mut command = Command::new(executable);
        command
            .arg("node")
            .arg(serde_json::to_string(launch)?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if launch.emulate_old_watermark_handler {
            command.env(OLD_WATERMARK_HANDLER_ENV, "1");
        }
        if launch.emulate_p3a_watermark_handler {
            command.env(P3A_WATERMARK_HANDLER_ENV, "1");
        }
        if launch.emulate_pre_learner_heartbeat {
            command.env(PRE_LEARNER_HEARTBEAT_ENV, "1");
        }
        let mut child = command.spawn().context("spawn cluster voter")?;
        let input = child.stdin.take().context("voter stdin")?;
        let output = BufReader::new(child.stdout.take().context("voter stdout")?);
        Ok(Self {
            id: launch.node_id,
            child,
            input,
            output,
        })
    }

    pub async fn wait_ready(&mut self) -> Result<()> {
        self.wait_ready_with_timeout(START_TIMEOUT + START_RESPONSE_GRACE)
            .await
    }

    async fn wait_ready_with_timeout(&mut self, timeout: Duration) -> Result<()> {
        let response = self
            .read_response(timeout)
            .await
            .with_context(|| format!("voter {} startup response", self.id))?;
        match response {
            Response::Ready { node_id } if node_id == self.id => Ok(()),
            response => bail!("voter {} failed startup: {response:?}", self.id),
        }
    }

    pub async fn request(&mut self, request: &Request) -> Result<Response> {
        let mut bytes = serde_json::to_vec(request)?;
        bytes.push(b'\n');
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        self.read_response(request.response_timeout())
            .await
            .with_context(|| format!("voter {} request {request:?}", self.id))
    }

    async fn read_response(&mut self, timeout: Duration) -> Result<Response> {
        let mut line = String::new();
        let bytes = tokio::time::timeout(timeout, self.output.read_line(&mut line))
            .await
            .context("voter response timed out")??;
        if bytes == 0 {
            let status = self.child.try_wait()?;
            bail!("voter {} closed its protocol stream ({status:?})", self.id);
        }
        serde_json::from_str(line.trim()).with_context(|| {
            format!(
                "decode voter {} response line {:?}",
                self.id,
                line.trim_end()
            )
        })
    }

    /// Close the request stream and wait for the voter to exit on its own.
    ///
    /// `kill` is what proves process loss; this is its orderly counterpart, and
    /// it is how a caller stops a voter it is finished with rather than one it
    /// is trying to destroy. It also makes an instrumented voter observable: a
    /// SIGKILLed process never runs its atexit handlers, so a voter that is
    /// only ever killed writes no coverage profile and reads as never executed.
    pub async fn shutdown(self) -> Result<()> {
        let Self {
            id,
            mut child,
            input,
            output,
        } = self;
        // Closing stdin ends the voter's request loop, which returns from
        // `node` and exits the process normally.
        drop(input);
        drop(output);
        let status = tokio::time::timeout(START_TIMEOUT, child.wait())
            .await
            .with_context(|| format!("voter {id} did not exit after its stream closed"))??;
        if !status.success() {
            bail!("voter {id} exited with {status} after an orderly shutdown");
        }
        Ok(())
    }

    pub async fn kill(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill().await?;
            let status = self.child.wait().await?;
            if status.success() {
                bail!(
                    "voter {} exited successfully after an intentional kill",
                    self.id
                );
            }
            #[cfg(unix)]
            if status.signal() != Some(9) {
                bail!(
                    "voter {} exited with unexpected status after kill: {status}",
                    self.id
                );
            }
        }
        Ok(())
    }

    fn pause(&mut self) -> Result<PausedProcessGuard> {
        let pid = self
            .child
            .id()
            .with_context(|| format!("voter {} has no process id", self.id))?;
        send_process_signal(pid, libc::SIGSTOP, "SIGSTOP")?;
        Ok(PausedProcessGuard { pid, armed: true })
    }
}

struct PausedProcessGuard {
    pid: u32,
    armed: bool,
}

impl PausedProcessGuard {
    fn resume(mut self) -> Result<()> {
        send_process_signal(self.pid, libc::SIGCONT, "SIGCONT")?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for PausedProcessGuard {
    fn drop(&mut self) {
        if self.armed {
            // Best effort in Drop: the owning assertion error is more useful
            // than a second panic, but CI must never retain a stopped child.
            let _ = send_process_signal(self.pid, libc::SIGCONT, "SIGCONT cleanup");
        }
    }
}

fn send_process_signal(pid: u32, signal: libc::c_int, label: &str) -> Result<()> {
    let pid = libc::pid_t::try_from(pid).context("voter process id overflowed pid_t")?;
    // SAFETY: `pid` came from the live child handle and `signal` is one of the
    // two fixed POSIX stop/continue constants supplied by the caller above.
    let result = unsafe { libc::kill(pid, signal) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("send {label}"));
    }
    Ok(())
}

fn kill_process_group(pid: u32, label: &str) -> Result<()> {
    let process_group = libc::pid_t::try_from(pid).context("process group id overflowed pid_t")?;
    // SAFETY: the singleton-attempt controller created a new process group
    // whose id is its child pid. A negative pid targets that exact group, so
    // timeout cleanup includes the voter grandchildren it spawned.
    let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error).with_context(|| format!("send SIGKILL for {label}"));
        }
    }
    Ok(())
}

pub struct ClusterProcesses {
    nodes: Vec<Option<NodeProcess>>,
    root: PathBuf,
    convergence_timeout: Duration,
}

impl ClusterProcesses {
    /// Start one voter process per spec from `executable` and wait for each to
    /// announce readiness. The executable is explicit rather than
    /// `current_exe()` so a test binary can start real harness voters.
    pub async fn start(
        executable: &Path,
        root: &Path,
        reservation: PortReservation,
    ) -> Result<Self> {
        Self::start_inner(executable, root, reservation, None, None, None).await
    }

    async fn start_with_old_watermark_handler(
        executable: &Path,
        root: &Path,
        reservation: PortReservation,
        old_handler_node: u64,
    ) -> Result<Self> {
        Self::start_inner(
            executable,
            root,
            reservation,
            Some(old_handler_node),
            None,
            None,
        )
        .await
    }

    async fn start_with_p3a_watermark_handler(
        executable: &Path,
        root: &Path,
        reservation: PortReservation,
        p3a_handler_node: u64,
    ) -> Result<Self> {
        Self::start_inner(
            executable,
            root,
            reservation,
            None,
            Some(p3a_handler_node),
            None,
        )
        .await
    }

    /// Start a cluster in which exactly one voter runs a binary that predates
    /// the learner protocol: it heartbeats, so it stays an active member, and
    /// it never writes the `learner_protocol_v5` capability row. That is the
    /// only node in the cluster whose presence must refuse activation.
    async fn start_with_pre_learner_heartbeat(
        executable: &Path,
        root: &Path,
        reservation: PortReservation,
        pre_learner_node: u64,
    ) -> Result<Self> {
        Self::start_inner(
            executable,
            root,
            reservation,
            None,
            None,
            Some(pre_learner_node),
        )
        .await
    }

    async fn start_inner(
        executable: &Path,
        root: &Path,
        reservation: PortReservation,
        old_handler_node: Option<u64>,
        p3a_handler_node: Option<u64>,
        pre_learner_heartbeat_node: Option<u64>,
    ) -> Result<Self> {
        let (_listeners, specs) = reservation.into_inner();
        // Listeners are dropped here: the child process must bind the same
        // ports, so we cannot hold them across the spawn. The window between
        // releasing the port and the child binding it is the residual race
        // that synchronous listener binding plus the retry wrapper handle.
        drop(_listeners);
        let mut nodes = Vec::with_capacity(specs.len());
        for node_id in 1..=specs.len() as u64 {
            let launch = NodeLaunch {
                emulate_old_watermark_handler: old_handler_node == Some(node_id),
                emulate_p3a_watermark_handler: p3a_handler_node == Some(node_id),
                emulate_pre_learner_heartbeat: pre_learner_heartbeat_node == Some(node_id),
                ..NodeLaunch::voter(node_id, root.to_path_buf(), specs.clone())
            };
            nodes.push(Some(NodeProcess::spawn(executable, &launch)?));
        }
        let mut cluster = Self {
            nodes,
            root: root.to_path_buf(),
            convergence_timeout: CONVERGENCE_TIMEOUT,
        };
        for node_id in 1..=specs.len() as u64 {
            cluster.node_mut(node_id)?.wait_ready().await?;
        }
        Ok(cluster)
    }

    /// Shorten how long the `wait_*` and `leader` helpers keep retrying before
    /// they give up.
    ///
    /// The controller keeps the default: [`CONVERGENCE_TIMEOUT`] is sized for a
    /// three-voter raft election on a loaded machine. Tests that assert the
    /// give-up path itself need a state that never converges, and waiting the
    /// full production timeout for each one would dominate the suite.
    #[must_use]
    pub fn with_convergence_timeout(mut self, timeout: Duration) -> Self {
        self.convergence_timeout = timeout;
        self
    }

    fn node_mut(&mut self, node_id: u64) -> Result<&mut NodeProcess> {
        self.nodes
            .get_mut((node_id - 1) as usize)
            .and_then(Option::as_mut)
            .with_context(|| format!("voter {node_id} is not running"))
    }

    /// The voters still running, in ascending id order.
    pub fn node_ids(&self) -> Vec<u64> {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| node.as_ref().map(|_| index as u64 + 1))
            .collect()
    }

    pub async fn request(&mut self, node_id: u64, request: Request) -> Result<Response> {
        self.node_mut(node_id)?.request(&request).await
    }

    async fn request_all_concurrently(
        &mut self,
        requests: Vec<Request>,
    ) -> Result<Vec<(u64, Response)>> {
        if requests.len() != self.nodes.len() || self.nodes.iter().any(Option::is_none) {
            bail!("concurrent all-voter request requires every configured voter");
        }
        let mut calls = Vec::with_capacity(requests.len());
        for (index, (slot, request)) in self.nodes.iter_mut().zip(requests).enumerate() {
            let node_id = u64::try_from(index)? + 1;
            let node = slot
                .as_mut()
                .with_context(|| format!("voter {node_id} is not running"))?;
            calls.push(async move { Ok((node_id, node.request(&request).await?)) });
        }
        futures_util::future::try_join_all(calls).await
    }

    async fn request_pair_concurrently(
        &mut self,
        first_id: u64,
        first_request: Request,
        second_id: u64,
        second_request: Request,
    ) -> Result<(Response, Response)> {
        if first_id == second_id {
            bail!("concurrent requests require two distinct voters");
        }
        let first_index = usize::try_from(first_id.saturating_sub(1))?;
        let second_index = usize::try_from(second_id.saturating_sub(1))?;
        let (first, second) = if first_index < second_index {
            let (before_second, from_second) = self.nodes.split_at_mut(second_index);
            (
                before_second
                    .get_mut(first_index)
                    .and_then(Option::as_mut)
                    .with_context(|| format!("voter {first_id} is not running"))?,
                from_second
                    .first_mut()
                    .and_then(Option::as_mut)
                    .with_context(|| format!("voter {second_id} is not running"))?,
            )
        } else {
            let (before_first, from_first) = self.nodes.split_at_mut(first_index);
            (
                from_first
                    .first_mut()
                    .and_then(Option::as_mut)
                    .with_context(|| format!("voter {first_id} is not running"))?,
                before_first
                    .get_mut(second_index)
                    .and_then(Option::as_mut)
                    .with_context(|| format!("voter {second_id} is not running"))?,
            )
        };
        let (first_response, second_response) = tokio::join!(
            first.request(&first_request),
            second.request(&second_request)
        );
        Ok((first_response?, second_response?))
    }

    /// Add one real voter process to an already-running cluster.
    pub async fn spawn_node(&mut self, executable: &Path, launch: NodeLaunch) -> Result<()> {
        if launch.root != self.root {
            bail!("dynamic voter root did not match the running cluster");
        }
        let index = usize::try_from(launch.node_id.saturating_sub(1))?;
        if self.nodes.len() <= index {
            self.nodes.resize_with(index + 1, || None);
        }
        if self.nodes[index].is_some() {
            bail!("voter {} is already running", launch.node_id);
        }
        let response_timeout = launch.startup_timeout() + START_RESPONSE_GRACE;
        let mut process = NodeProcess::spawn(executable, &launch)?;
        process.wait_ready_with_timeout(response_timeout).await?;
        self.nodes[index] = Some(process);
        Ok(())
    }

    pub async fn kill(&mut self, node_id: u64) -> Result<()> {
        let slot = self
            .nodes
            .get_mut((node_id - 1) as usize)
            .with_context(|| format!("unknown voter {node_id}"))?;
        if let Some(mut node) = slot.take() {
            node.kill().await?;
        }
        Ok(())
    }

    fn pause(&mut self, node_id: u64) -> Result<PausedProcessGuard> {
        self.node_mut(node_id)?.pause()
    }

    /// Stop every remaining voter in an orderly way. See
    /// [`NodeProcess::shutdown`]; `kill_all` is the destructive counterpart.
    pub async fn shutdown_all(&mut self) -> Result<()> {
        for slot in &mut self.nodes {
            if let Some(node) = slot.take() {
                node.shutdown().await?;
            }
        }
        Ok(())
    }

    pub async fn kill_all(&mut self) {
        for node in &mut self.nodes {
            if let Some(node) = node.as_mut() {
                let _ = node.kill().await;
            }
            *node = None;
        }
    }

    pub async fn assert_running(&mut self) -> Result<()> {
        for node in self.nodes.iter_mut().flatten() {
            if let Some(status) = node.child.try_wait()? {
                bail!("voter {} exited unexpectedly with {status}", node.id);
            }
        }
        Ok(())
    }

    pub async fn leader(&mut self) -> Result<u64> {
        let eligible = self.node_ids();
        self.leader_among(&eligible).await
    }

    /// Wait for a leader reported and confirmed inside one eligible voter
    /// set. A graceful self-leave harness keeps the removed protocol process
    /// alive long enough to inspect its tombstone, so its stale self-report
    /// must not satisfy successor convergence.
    pub async fn leader_among(&mut self, eligible: &[u64]) -> Result<u64> {
        let deadline = Instant::now() + self.convergence_timeout;
        loop {
            for node_id in eligible.iter().copied() {
                if self
                    .nodes
                    .get((node_id - 1) as usize)
                    .is_none_or(Option::is_none)
                {
                    continue;
                }
                if let Ok(Response::Metrics {
                    leader: Some(leader),
                    ..
                }) = self.request(node_id, Request::Metrics).await
                {
                    if eligible.contains(&leader)
                        && self.nodes.get((leader - 1) as usize).is_some_and(Option::is_some)
                        && self
                            .request(leader, Request::Metrics)
                            .await
                            .is_ok_and(|response| {
                                matches!(response, Response::Metrics { leader: Some(id), .. } if id == leader)
                            })
                    {
                        return Ok(leader);
                    }
                }
            }
            if Instant::now() >= deadline {
                bail!("eligible voters {eligible:?} did not report a leader");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Wait until the operator-facing roster catches up with the already
    /// confirmed Raft leader. Metrics and the replicated heartbeat roster are
    /// separate observations, so an immediate status read may legitimately
    /// land between them during the successor's first heartbeat.
    pub async fn wait_for_membership_roster(
        &mut self,
        observer: u64,
        expected_voters: &[u64],
        expected_leader: u64,
    ) -> Result<MembershipStatus> {
        let expected = expected_voters.iter().copied().collect::<BTreeSet<_>>();
        let deadline = Instant::now() + self.convergence_timeout;
        let mut last_status = None;
        loop {
            if let Ok(Response::MembershipStatus { status }) =
                self.request(observer, Request::MembershipStatus).await
            {
                let roster = status
                    .nodes
                    .iter()
                    .map(|node| node.raft_id)
                    .collect::<BTreeSet<_>>();
                let leader_count = status.nodes.iter().filter(|node| node.is_leader).count();
                if roster == expected
                    && leader_count == 1
                    && status
                        .nodes
                        .iter()
                        .any(|node| node.raft_id == expected_leader && node.is_leader)
                {
                    return Ok(status);
                }
                last_status = Some(status);
            }
            if Instant::now() >= deadline {
                bail!(
                    "membership roster did not converge on voters {expected:?} with leader \
                     {expected_leader}: {last_status:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Wait until a surviving voter reports exactly `expected` as the raft
    /// membership. The graceful-leave scenario deliberately removes the
    /// original leader, which is commonly voter 1; a removed process is not
    /// required to learn entries committed after its removal.
    pub async fn wait_for_voters(&mut self, expected: &[u64]) -> Result<()> {
        let observer = expected
            .iter()
            .copied()
            .find(|node_id| {
                self.nodes
                    .get((*node_id - 1) as usize)
                    .is_some_and(Option::is_some)
            })
            .context("cannot observe membership without a running expected voter")?;
        let deadline = Instant::now() + self.convergence_timeout;
        loop {
            if let Ok(Response::Metrics { voters, .. }) =
                self.request(observer, Request::Metrics).await
            {
                if voters == expected {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                bail!("cluster did not converge to voters {expected:?}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Wait until `observer` reports exactly `voters` as the voting set and
    /// exactly `members` as the whole committed configuration.
    ///
    /// [`Self::wait_for_voters`] cannot express a learner: a node that joins as
    /// one never enters the voter set at all, so a scenario that only waited on
    /// voters would pass the instant the cluster ignored the join entirely.
    pub async fn wait_for_members(
        &mut self,
        observer: u64,
        voters: &[u64],
        members: &[u64],
    ) -> Result<()> {
        let deadline = Instant::now() + self.convergence_timeout;
        let mut last = None;
        loop {
            if let Ok(Response::Metrics {
                voters: observed_voters,
                members: observed_members,
                ..
            }) = self.request(observer, Request::Metrics).await
            {
                if observed_voters == voters && observed_members == members {
                    return Ok(());
                }
                last = Some((observed_voters, observed_members));
            }
            if Instant::now() >= deadline {
                bail!(
                    "voter {observer} did not converge on voters {voters:?} and members \
                     {members:?}: {last:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn wait_for_ready(&mut self, node_id: u64) -> Result<()> {
        let deadline = Instant::now() + self.convergence_timeout;
        loop {
            if self
                .request(node_id, Request::Ping)
                .await
                .is_ok_and(|response| matches!(response, Response::Ok))
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("surviving voter {node_id} did not regain quorum readiness");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Wait for the exact production status projection on one voter.
    pub async fn wait_for_replication_health(
        &mut self,
        node_id: u64,
        expected: ReplicationHealth,
    ) -> Result<ReplicationStatus> {
        let deadline = Instant::now() + self.convergence_timeout;
        loop {
            if let Ok(Response::ReplicationStatus { status }) =
                self.request(node_id, Request::ReplicationStatus).await
            {
                if status.health == expected {
                    return Ok(status);
                }
            }
            if Instant::now() >= deadline {
                bail!("voter {node_id} did not report replication health {expected:?}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn wait_for_equal_dumps(&mut self) -> Result<()> {
        let deadline = Instant::now() + self.convergence_timeout;
        loop {
            let mut dumps = Vec::new();
            for node_id in 1..=self.nodes.len() as u64 {
                if self.nodes[(node_id - 1) as usize].is_none() {
                    continue;
                }
                match self.request(node_id, Request::Dump).await? {
                    Response::Dump { digest, dump } => {
                        let computed = hex::encode(Sha256::digest(dump.as_bytes()));
                        if digest != computed {
                            bail!("local dump digest is not anchored to the returned rows");
                        }
                        validate_known_dump(&serde_json::from_str(&dump)?)?;
                        dumps.push((digest, dump));
                    }
                    Response::Error { message } => return Err(anyhow!(message)),
                    response => bail!("unexpected dump response: {response:?}"),
                }
            }
            if dumps.windows(2).all(|pair| pair[0] == pair[1]) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("local auth table dumps did not converge byte-for-byte");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn wait_for_equal_catalog_views(&mut self) -> Result<CatalogView> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let mut views = Vec::new();
            for node_id in 1..=self.nodes.len() as u64 {
                if self.nodes[(node_id - 1) as usize].is_none() {
                    continue;
                }
                match self.request(node_id, Request::CatalogView).await? {
                    Response::CatalogView { view } => views.push(view),
                    Response::Error { message } => return Err(anyhow!(message)),
                    response => bail!("unexpected catalog response: {response:?}"),
                }
            }
            if views.windows(2).all(|pair| pair[0] == pair[1]) {
                return views
                    .into_iter()
                    .next()
                    .context("catalog view set was empty");
            }
            if Instant::now() >= deadline {
                bail!("browse/search views did not converge on all three voters");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// Delete `voter`'s derived FTS rows behind hiqlite's back and prove the search
/// index is local: replicated truth is unchanged, the full digest moves, search
/// goes empty, and a rebuild restores the baseline view exactly.
///
/// The voter is a parameter because the search index is node-local on every
/// voter, so the proof is the same wherever it runs; the three-voter controller
/// passes 2 exactly as it always did.
pub async fn prove_local_fts_rebuild(
    cluster: &mut ClusterProcesses,
    baseline: &CatalogView,
    voter: u64,
) -> Result<()> {
    let path = cluster
        .root
        .join(format!("node-{voter}"))
        .join("state_machine")
        .join("db")
        .join("auth.db");
    let connection = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open voter-{voter} local database at {}", path.display()))?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .context("set voter validation busy timeout")?;
    connection
        .execute("DELETE FROM items_fts", [])
        .context("delete voter derived FTS rows")?;
    let empty = match cluster.request(voter, Request::CatalogView).await? {
        Response::CatalogView { view } => view,
        response => bail!("unexpected post-delete catalog response: {response:?}"),
    };
    if empty.authoritative_digest != baseline.authoritative_digest
        || empty.full_digest == baseline.full_digest
        || !empty.search.is_empty()
    {
        bail!("deleting voter-{voter} FTS changed truth, escaped the digest, or left search rows");
    }
    drop(connection);
    cluster
        .request(1, Request::RebuildSearch)
        .await?
        .require_ok()?;
    let rebuilt = cluster.wait_for_equal_catalog_views().await?;
    if &rebuilt != baseline {
        bail!("voter-{voter} FTS rebuild did not restore the baseline view");
    }
    Ok(())
}

/// The path of the running harness executable, used to start voter children.
pub fn harness_executable() -> Result<PathBuf> {
    std::env::current_exe().context("cluster-check executable")
}

/// Retry a cluster start whose only failure was a port taken between
/// allocation and bind.
///
/// `attempt` is handed the attempt number so it can allocate fresh ports and a
/// fresh data root each time. Only [`is_port_collision`] errors are retried;
/// every other failure is a verdict and is returned immediately, which is the
/// whole point of classifying the collision rather than matching a message
/// here.
pub async fn with_port_retry<T, F, Fut>(mut attempt: F) -> Result<T>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut last_error = None;
    for number in 1..=PORT_RETRY_ATTEMPTS {
        match attempt(number).await {
            Ok(value) => return Ok(value),
            Err(error) if is_port_collision(&error) => last_error = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last_error.with_context(|| {
        format!("cluster ports stayed occupied across {PORT_RETRY_ATTEMPTS} allocations")
    })?)
}

/// Allocate ports and start `voters` voter processes, retrying the whole
/// allocation when a port was taken between binding it and using it.
pub async fn start_cluster_with_port_retry(
    executable: &Path,
    root: &Path,
    voters: u64,
) -> Result<(ClusterProcesses, Vec<NodeSpec>)> {
    start_cluster_with_port_retry_using(
        executable,
        root,
        voters,
        |_| allocate_nodes(voters),
        |_, _| Ok(()),
    )
    .await
}

/// Start a cluster with injectable allocation and pre-start seams.
///
/// This is public only so the integration harness can deterministically return
/// a classified collision before the first start and prove the production
/// retry wrapper performs a fresh allocation. The reservation remains held
/// during that injection, so the regression does not recreate the
/// release-to-bind race this wrapper exists to handle. Production callers
/// should use [`start_cluster_with_port_retry`].
#[doc(hidden)]
pub async fn start_cluster_with_port_retry_using<A, B>(
    executable: &Path,
    root: &Path,
    voters: u64,
    mut allocate: A,
    mut before_start: B,
) -> Result<(ClusterProcesses, Vec<NodeSpec>)>
where
    A: FnMut(u32) -> Result<PortReservation>,
    B: FnMut(u32, &PortReservation) -> Result<()>,
{
    with_port_retry(|attempt| {
        let reservation = allocate(attempt);
        let before_start = match reservation.as_ref() {
            Ok(reservation) => before_start(attempt, reservation),
            Err(_) => Ok(()),
        };
        async move {
            let reservation = reservation?;
            before_start?;
            if reservation.specs.len() != voters as usize {
                bail!(
                    "cluster allocator returned {} voters, expected {voters}",
                    reservation.specs.len()
                );
            }
            let specs = reservation.specs.clone();
            let attempt_root = root.join(format!("attempt-{attempt}"));
            let cluster = ClusterProcesses::start(executable, &attempt_root, reservation).await?;
            Ok((cluster, specs))
        }
    })
    .await
}

/// Assert a voter's local dump carries every row the harness wrote: the
/// cluster identity, one acknowledged setting per voter, and exactly the three
/// surviving users, tokens, and API keys with their expected field values.
pub fn validate_known_dump(dump: &serde_json::Value) -> Result<()> {
    let rows = |name: &str| -> Result<&Vec<serde_json::Value>> {
        dump.get(name)
            .and_then(serde_json::Value::as_array)
            .with_context(|| format!("local dump has no {name} rows"))
    };

    let settings = rows("settings")?;
    let expected_settings = [
        ("instance.id", INSTANCE_ID),
        ("proof.node.1", "acknowledged"),
        ("proof.node.2", "acknowledged"),
        ("proof.node.3", "acknowledged"),
    ];
    for (key, value) in expected_settings {
        if !settings.iter().any(|row| {
            row.get("key").and_then(serde_json::Value::as_str) == Some(key)
                && row.get("value").and_then(serde_json::Value::as_str) == Some(value)
        }) {
            bail!("local dump is missing expected setting {key}={value}");
        }
    }

    let users = rows("users")?;
    if users.len() != 3 {
        bail!(
            "local dump expected 3 surviving users, found {}",
            users.len()
        );
    }
    for ordinal in 1..=3 {
        let username = format!("survivor-{ordinal}");
        let password = format!("hash-v2-{ordinal}");
        if !users.iter().any(|row| {
            row.get("username").and_then(serde_json::Value::as_str) == Some(username.as_str())
                && row.get("password_hash").and_then(serde_json::Value::as_str)
                    == Some(password.as_str())
                && row.get("is_admin").and_then(serde_json::Value::as_i64) == Some(1)
        }) {
            bail!("local dump has incorrect user proof for {username}");
        }
    }

    let tokens = rows("tokens")?;
    if tokens.len() != 3 {
        bail!(
            "local dump expected 3 surviving tokens, found {}",
            tokens.len()
        );
    }
    for ordinal in 1..=3 {
        let token = format!("survive-token-{ordinal}");
        if !tokens.iter().any(|row| {
            row.get("token_hash").and_then(serde_json::Value::as_str) == Some(token.as_str())
                && row.get("device").and_then(serde_json::Value::as_str) == Some("cluster-check")
        }) {
            bail!("local dump has incorrect token proof for {token}");
        }
    }

    let keys = rows("api_keys")?;
    if keys.len() != 3 {
        bail!(
            "local dump expected 3 surviving API keys, found {}",
            keys.len()
        );
    }
    for ordinal in 1..=3 {
        let key_hash = format!("survive-key-{ordinal}");
        if !keys.iter().any(|row| {
            row.get("key_hash").and_then(serde_json::Value::as_str) == Some(key_hash.as_str())
                && row.get("scopes").and_then(serde_json::Value::as_str)
                    == Some(r#"["scan:trigger"]"#)
                && row.get("disabled").and_then(serde_json::Value::as_i64) == Some(0)
        }) {
            bail!("local dump has incorrect API-key proof for {key_hash}");
        }
    }
    Ok(())
}

struct SingletonProbe {
    outcome: Arc<RwLock<String>>,
    cleanup_done: Arc<AtomicBool>,
    cleanup_error: Arc<RwLock<Option<String>>>,
}

#[derive(Default)]
struct NodeMutableState {
    store: Option<Arc<HiqliteAuthStore>>,
    catalogue_store: Option<Arc<HiqliteAuthStore>>,
    catalogue: Option<CatalogueReader>,
    membership: Option<MembershipManager>,
    singleton_probe: Option<SingletonProbe>,
    /// A cluster-wide job lease this process won through the production
    /// acquisition path, kept alive so its renewal loop keeps running.
    cluster_job: Option<ActiveJobLease>,
}

/// Run one embedded voter: start hiqlite, announce readiness, then serve the
/// line-delimited request protocol until stdin closes.
pub async fn node(launch: NodeLaunch) -> Result<()> {
    let node_started = Instant::now();
    let startup_timeout = launch.startup_timeout();
    let startup = tokio::time::timeout(startup_timeout, async {
        install_crypto_provider();
        plurx_core::store::validation_set_store_operation_instrumentation(
            launch.instrument_store_operations,
        );
        let listeners = voter_listen_addrs(&launch)?;
        let _ = ServerTlsConfig::server_config_self_signed(&launch.listen_addr).await;
        let client = hiqlite::start_node(node_config(&launch)?)
            .await
            .context("start hiqlite voter")?;
        client.wait_until_healthy_db().await;
        prove_listeners_bound(&listeners).await?;
        Ok::<_, anyhow::Error>(client)
    })
    .await;
    let client = match startup {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => {
            write_response(&Response::Error {
                message: format!("{error:#}"),
            })
            .await?;
            return Err(error);
        }
        Err(_) => {
            let message = format!(
                "voter startup timed out after {} seconds",
                startup_timeout.as_secs()
            );
            write_response(&Response::Error {
                message: message.clone(),
            })
            .await?;
            bail!(message);
        }
    };
    let replication = ReplicationMonitor::replicated(client.clone());
    let (passive_shutdown, passive_shutdown_signal) = tokio::sync::oneshot::channel();
    tokio::spawn(replication.clone().passive_metrics_loop(async move {
        let _ = passive_shutdown_signal.await;
    }));
    let serving = ServingFence::new(replication.metrics_handle());
    let serving_shutdown = tokio_util::sync::CancellationToken::new();
    tokio::spawn(serving.clone().monitor_loop(serving_shutdown.clone()));

    write_response(&Response::Ready {
        node_id: launch.node_id,
    })
    .await?;

    let telemetry_path = launch
        .root
        .join(format!("node-{}", launch.node_id))
        .join("telemetry.db");
    let request_context = NodeRequestContext {
        client: &client,
        replication: &replication,
        serving: &serving,
        launch: &launch,
        telemetry_path: &telemetry_path,
        node_started,
    };
    let mut state = NodeMutableState::default();
    let stdin = tokio::io::stdin();
    let mut input = BufReader::new(stdin).lines();
    while let Some(line) = input.next_line().await? {
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => handle_request(request, &request_context, &mut state).await,
            Err(error) => Err(error.into()),
        };
        match response {
            Ok(response) => write_response(&response).await?,
            Err(error) => {
                write_response(&Response::Error {
                    message: format!("{error:#}"),
                })
                .await?
            }
        }
    }
    let _ = passive_shutdown.send(());
    serving_shutdown.cancel();
    Ok(())
}

struct NodeRequestContext<'a> {
    client: &'a Client,
    replication: &'a ReplicationMonitor,
    serving: &'a ServingFence,
    launch: &'a NodeLaunch,
    telemetry_path: &'a Path,
    node_started: Instant,
}

async fn handle_request(
    request: Request,
    context: &NodeRequestContext<'_>,
    state: &mut NodeMutableState,
) -> Result<Response> {
    let &NodeRequestContext {
        client,
        replication,
        serving,
        launch,
        telemetry_path,
        node_started,
    } = context;
    let NodeMutableState {
        store,
        catalogue_store,
        catalogue,
        membership,
        singleton_probe,
        cluster_job,
    } = state;
    match request {
        Request::SeedLegacyArtworkUrls => {
            client
                .execute(
                    "CREATE TABLE IF NOT EXISTS cluster_node_http (\
                       node_id TEXT PRIMARY KEY, \
                       public_http_url TEXT NOT NULL) STRICT",
                    hiqlite::macros::params!(),
                )
                .await?;
            for node_id in 1..=3 {
                client
                    .execute(
                        "INSERT INTO cluster_node_http (node_id, public_http_url) \
                         VALUES ($1, $2)",
                        hiqlite::macros::params!(
                            format!("node-{node_id}"),
                            "https://shared-join-lb.invalid"
                        ),
                    )
                    .await?;
            }
            Ok(Response::Ok)
        }
        Request::SeedLegacyPartialRedemption {
            token_digest,
            node_id,
        } => {
            let changed = client
                .execute(
                    "UPDATE cluster_join_tokens SET state = 'redeeming', node_id = $1 \
                     WHERE token_hash = $2 AND state = 'issued'",
                    hiqlite::macros::params!(node_id.as_str(), token_digest.as_str()),
                )
                .await?;
            if changed != 1 {
                bail!("could not seed the legacy partial redemption");
            }
            client
                .execute(
                    "DELETE FROM cluster_node_http WHERE node_id = $1",
                    hiqlite::macros::params!(node_id),
                )
                .await?;
            Ok(Response::Ok)
        }
        Request::SeedHistoricalHttpDuplicate {
            node_id,
            public_http_url,
        } => {
            client
                .execute(
                    "INSERT INTO cluster_node_http (node_id, public_http_url) VALUES ($1, $2) \
                     ON CONFLICT(node_id) DO UPDATE SET public_http_url = excluded.public_http_url",
                    hiqlite::macros::params!(node_id, public_http_url),
                )
                .await?;
            Ok(Response::Ok)
        }
        Request::Bootstrap => {
            let opened = Arc::new(
                HiqliteAuthStore::bootstrap((*client).clone(), INSTANCE_ID, telemetry_path).await?,
            );
            // Keep exact catalogue routing counters isolated from membership's
            // background probes while sharing this node's real replicated DB.
            let opened_catalogue_store = Arc::new(
                HiqliteAuthStore::open(
                    (*client).clone(),
                    &telemetry_path.with_extension("catalogue-validation.db"),
                )
                .await?,
            );
            let authority: Arc<dyn plurx_core::store::Store> = opened_catalogue_store.clone();
            let opened_catalogue = CatalogueReader::validation_replicated(
                authority,
                Arc::clone(&opened_catalogue_store),
                replication.metrics_handle(),
                0,
            );
            let opened_membership =
                membership_manager(client, replication, opened.clone(), launch).await?;
            tokio::spawn(opened_membership.clone().offline_source_probe_loop());
            *membership = Some(opened_membership);
            *catalogue = Some(opened_catalogue);
            *catalogue_store = Some(opened_catalogue_store);
            *store = Some(opened);
            Ok(Response::Ok)
        }
        Request::RejectIdentityDrift => {
            match HiqliteAuthStore::bootstrap(
                (*client).clone(),
                "wrong-instance-id",
                telemetry_path,
            )
            .await
            {
                Err(error) if error.to_string().contains("refusing bootstrap") => Ok(Response::Ok),
                Err(error) => bail!("identity drift failed for the wrong reason: {error}"),
                Ok(_) => bail!("bootstrap overwrote the immutable cluster identity"),
            }
        }
        Request::Open => {
            let opened = Arc::new(HiqliteAuthStore::open((*client).clone(), telemetry_path).await?);
            let opened_catalogue_store = Arc::new(
                HiqliteAuthStore::open(
                    (*client).clone(),
                    &telemetry_path.with_extension("catalogue-validation.db"),
                )
                .await?,
            );
            let authority: Arc<dyn plurx_core::store::Store> = opened_catalogue_store.clone();
            let opened_catalogue = CatalogueReader::validation_replicated(
                authority,
                Arc::clone(&opened_catalogue_store),
                replication.metrics_handle(),
                0,
            );
            let opened_membership =
                membership_manager(client, replication, opened.clone(), launch).await?;
            tokio::spawn(opened_membership.clone().offline_source_probe_loop());
            *membership = Some(opened_membership);
            *catalogue = Some(opened_catalogue);
            *catalogue_store = Some(opened_catalogue_store);
            *store = Some(opened);
            Ok(Response::Ok)
        }
        Request::IssueJoinToken { ttl_ms } => membership_ref(membership)?
            .issue_token(Duration::from_millis(ttl_ms))
            .await
            .map(|token| Response::IssuedJoinToken { token })
            .or_else(|error| Ok(membership_error_response(error))),
        Request::IssueLearnerJoinToken { ttl_ms } => membership_ref(membership)?
            .issue_learner_token(Duration::from_millis(ttl_ms))
            .await
            .map(|token| Response::IssuedJoinToken { token })
            .or_else(|error| Ok(membership_error_response(error))),
        Request::ProtocolStatus => membership_ref(membership)?
            .protocol_status()
            .await
            .map(|status| Response::ProtocolStatus { status })
            .or_else(|error| Ok(membership_error_response(error))),
        Request::ActivateLearnerProtocol => membership_ref(membership)?
            .activate_learner_protocol()
            .await
            .map(|change| Response::ProtocolChange { change })
            .or_else(|error| Ok(membership_error_response(error))),
        Request::DeactivateLearnerProtocol => membership_ref(membership)?
            .deactivate_learner_protocol()
            .await
            .map(|change| Response::ProtocolChange { change })
            .or_else(|error| Ok(membership_error_response(error))),
        Request::ForceHeartbeat => {
            membership_ref(membership)?
                .validation_force_heartbeat()
                .await?;
            Ok(Response::Ok)
        }
        Request::StartHeartbeatLoop => {
            let membership = membership_ref(membership)?.clone();
            tokio::spawn(membership.heartbeat_loop());
            Ok(Response::Ok)
        }
        Request::AcquireClusterJob { ref resource } => {
            let opened = store.clone().context("node store is not open")?;
            let coordinator = StoreCoordinator::new(
                opened as Arc<dyn plurx_core::store::Store>,
                format!("node-{}", launch.node_id),
            )?;
            // The production function, compiled from plurxd's own source, with
            // the production authority: this node's live membership manager.
            let acquired = production_job_lease::acquire_cluster_job(
                &coordinator,
                membership_ref(membership)?,
                resource.clone(),
            )
            .await?;
            let held = acquired.is_some();
            // Retained rather than dropped, so the durable row the caller reads
            // next is one a live lease is actually renewing.
            *cluster_job = acquired;
            Ok(Response::ClusterJobAttempt {
                acquired: held,
                lease: read_job_lease(client, resource).await?,
            })
        }
        Request::ReadClusterJobLease { ref resource } => Ok(Response::ClusterJobAttempt {
            acquired: false,
            lease: read_job_lease(client, resource).await?,
        }),
        Request::RedeemJoin { request } => membership_ref(membership)?
            .redeem(&request)
            .await
            .map(|()| Response::Ok)
            .or_else(|error| Ok(membership_error_response(error))),
        Request::RedeemLearnerJoin { request } => membership_ref(membership)?
            .redeem_learner(&request)
            .await
            .map(|()| Response::Ok)
            .or_else(|error| Ok(membership_error_response(error))),
        Request::FinalizeJoin { request } => membership_ref(membership)?
            .finalize(&request)
            .await
            .map(|()| Response::Ok)
            .or_else(|error| Ok(membership_error_response(error))),
        Request::FinalizeLearnerJoin { request } => membership_ref(membership)?
            .finalize_learner(&request)
            .await
            .map(|()| Response::Ok)
            .or_else(|error| Ok(membership_error_response(error))),
        Request::MembershipStatus => membership_ref(membership)?
            .status()
            .await
            .map(|status| Response::MembershipStatus { status })
            .or_else(|error| Ok(membership_error_response(error))),
        Request::MembershipSchemaReady => {
            let _ = client
                .query_map::<MembershipTombstoneRow, _>(
                    "SELECT removed_at FROM cluster_nodes LIMIT 1",
                    params!(),
                )
                .await?;
            Ok(Response::Flag { value: true })
        }
        Request::Advertisement { seed_name } => {
            let name = store_ref(store)?
                .get_or_init_setting(plurx_core::store::keys::SERVER_NAME, &seed_name)
                .await?;
            let node_id = format!("node-{}", launch.node_id);
            let advertisement = plurx_compat_plex::gdm::Advertisement {
                instance_id: INSTANCE_ID,
                name: &name,
                node_id: Some(&node_id),
            };
            let gdm = String::from_utf8(plurx_compat_plex::gdm::response_for(
                &advertisement,
                "0.2.7",
                32400,
            ))?;
            Ok(Response::Advertisement {
                instance_id: INSTANCE_ID.to_owned(),
                node_id: node_id.clone(),
                name: name.clone(),
                gdm,
                mdns_name: format!("{name} · node{}", launch.node_id),
                mdns_instance_id: INSTANCE_ID.to_owned(),
                mdns_node_id: node_id,
            })
        }
        Request::RenameServer { name } => {
            store_ref(store)?
                .put_setting(plurx_core::store::keys::SERVER_NAME, &name)
                .await?;
            Ok(Response::Ok)
        }
        Request::SignActivityRequest {
            target_node_id,
            timestamp_ms,
        } => Ok(Response::ActivityAuth {
            auth: membership_ref(membership)?
                .sign_activity_request(&target_node_id, timestamp_ms)?,
        }),
        Request::AuthorizeActivityRequest { auth } => Ok(Response::Flag {
            value: membership_ref(membership)?
                .authorize_activity_request(&auth)
                .await?,
        }),
        Request::SignInternalPeerRequest {
            target_node_id,
            timestamp_ms,
            nonce,
            method,
            path,
            body,
        } => Ok(Response::InternalPeerAuth {
            auth: membership_ref(membership)?.sign_internal_peer_request(
                &target_node_id,
                timestamp_ms,
                &nonce,
                &method,
                &path,
                &body,
            )?,
        }),
        Request::AuthorizeInternalPeerRequest {
            auth,
            method,
            path,
            body,
        } => Ok(Response::Flag {
            value: membership_ref(membership)?
                .authorize_internal_peer_request(&auth, &method, &path, &body)
                .await?,
        }),
        Request::ActivityPeers => Ok(Response::ActivityPeers {
            peers: membership_ref(membership)?
                .activity_peers()
                .await?
                .into_iter()
                .map(|peer| (peer.node_id, peer.http_base, peer.reachable))
                .collect(),
        }),
        Request::ArtworkPeerUrls => membership_ref(membership)?
            .reachable_peer_http_urls()
            .await
            .map(|urls| Response::ArtworkPeerUrls { urls })
            .or_else(|error| Ok(membership_error_response(error))),
        Request::RejectDuplicateArtworkUrl { public_http_url } => {
            membership_manager_with_artwork_url(
                client,
                replication,
                store
                    .as_ref()
                    .cloned()
                    .context("auth store has not been opened")?,
                launch,
                public_http_url,
            )
            .await
            .map(|_| Response::Ok)
            .or_else(|error| Ok(membership_error_response(error)))
        }
        Request::ReuseRemovedArtworkUrl {
            removed_node_id,
            public_http_url,
        } => {
            let rejoined_node_id = format!("rejoined-{removed_node_id}");
            let rejoined_raft_id = launch
                .nodes
                .iter()
                .map(|node| node.id)
                .max()
                .unwrap_or_default()
                .saturating_add(100);
            match membership_manager_with_identity_artwork_url(
                client,
                replication,
                store
                    .as_ref()
                    .cloned()
                    .context("auth store has not been opened")?,
                launch,
                rejoined_node_id.clone(),
                rejoined_raft_id,
                public_http_url,
            )
            .await
            {
                Ok(_) => {
                    for statement in [
                        "DELETE FROM cluster_node_hostnames WHERE node_id = $1",
                        "DELETE FROM cluster_node_http WHERE node_id = $1",
                        "DELETE FROM cluster_node_activity_keys WHERE node_id = $1",
                        "DELETE FROM cluster_nodes WHERE node_id = $1",
                    ] {
                        client
                            .execute(
                                statement,
                                hiqlite::macros::params!(rejoined_node_id.as_str()),
                            )
                            .await?;
                    }
                    Ok(Response::Ok)
                }
                Err(error) => Ok(membership_error_response(error)),
            }
        }
        Request::ArtworkPeerAuth { ref filename } => membership_ref(membership)?
            .artwork_peer_auth(filename)
            .map(|auth| Response::ArtworkPeerAuth { auth })
            .or_else(|error| Ok(membership_error_response(error))),
        Request::VerifyArtworkPeer {
            ref filename,
            ref auth,
        } => membership_ref(membership)?
            .verify_artwork_peer_auth(filename, auth)
            .await
            .map(|value| Response::Flag { value })
            .or_else(|error| Ok(membership_error_response(error))),
        Request::TombstoneOfflineFence { ref node_id } => {
            let store = store_ref(store)?;
            let now = unix_now()?;
            let package = NewOfflinePackage {
                id: format!("tombstone-fence-{node_id}"),
                request_id: format!("fence-{node_id}-{now}"),
                user_id: 1,
                file_id: 0,
                node_id: node_id.clone(),
                source_path: "/dev/null/tombstone-proof".to_owned(),
                source_size: 10,
                source_mtime: 1_700_000_000,
                effective_rate_control: "vbr".to_owned(),
                target_height: 720,
                output_width: Some(1280),
                output_height: Some(720),
                audio_index: None,
                audio_offset_ms: 0,
                subtitle_index: None,
                subtitle_language: None,
                subtitle_mode: "none".to_owned(),
                estimated_bytes: 700,
                reserved_bytes: 900,
                expires_at: now.saturating_add(3_600),
            };
            match store
                .create_offline_package(&package, 10, 100_000, 1_000_000)
                .await
            {
                Ok(OfflineCreateOutcome::NodeIsTombstone) => Ok(Response::Ok),
                Ok(other) => bail!(
                    "removed node was not refused as tombstone: expected \
                     NodeIsTombstone, got {other:?}"
                ),
                Err(error) => bail!("tombstone offline fence failed: {error:#}"),
            }
        }
        Request::HeartbeatPreservesTombstone { node_id } => {
            membership_ref(membership)?
                .validation_force_heartbeat()
                .await?;
            let rows = client
                .query_consistent_map::<MembershipTombstoneRow, _>(
                    "SELECT COALESCE(removed_at, (SELECT started_at \
                       FROM cluster_node_removals WHERE node_id = $1)) AS removed_at \
                     FROM cluster_nodes WHERE node_id = $1",
                    hiqlite::macros::params!(node_id),
                )
                .await?;
            if rows.first().and_then(|row| row.removed_at).is_none() {
                bail!("removed node heartbeat cleared its durable tombstone");
            }
            Ok(Response::Ok)
        }
        Request::Heartbeat => {
            membership_ref(membership)?.heartbeat().await?;
            Ok(Response::Ok)
        }
        Request::ObserveArtworkRepair {
            item_id,
            inject_leader_change,
        } => {
            if inject_leader_change {
                Ok(membership_error_response(MembershipError::LeaderChanged(
                    "injected before the read-only repair observation".to_owned(),
                )))
            } else {
                membership_ref(membership)?
                    .observe_artwork_source_repair(item_id)
                    .await
                    .map(|value| Response::Flag { value })
                    .or_else(|error| Ok(membership_error_response(error)))
            }
        }
        Request::ReadArtworkRepairFence { item_id } => {
            let rows = client
                .query_consistent_map::<HarnessArtworkRepairRow, _>(
                    "SELECT owner_node_id, leader_term, generation \
                     FROM cluster_artwork_repairs WHERE item_id = $1",
                    params!(item_id),
                )
                .await;
            let mut rows = match rows {
                Ok(rows) => rows,
                Err(error) if error.is_forward_to_leader().is_some() => {
                    return Ok(Response::MembershipLeaderChange {
                        message: error.to_string(),
                    });
                }
                Err(error) => return Err(error.into()),
            };
            if rows.len() > 1 {
                bail!("artwork repair primary key returned multiple rows");
            }
            Ok(Response::ArtworkRepairFence {
                fence: rows.pop().map(|row| ArtworkRepairFence {
                    item_id,
                    owner_node_id: row.owner_node_id,
                    leader_term: row.leader_term,
                    generation: row.generation,
                }),
            })
        }
        Request::ClaimArtworkRepair { item_id, lease_ms } => membership_ref(membership)?
            .claim_artwork_source_repair(item_id, Duration::from_millis(lease_ms))
            .await
            .map(|claim| Response::Flag {
                value: claim.is_some(),
            })
            .or_else(|error| Ok(membership_error_response(error))),
        Request::ClaimArtworkRepairAfterPause {
            item_id,
            lease_ms,
            pause_ms,
        } => {
            let claim = membership_ref(membership)?
                .claim_artwork_source_repair(item_id, Duration::from_millis(lease_ms))
                .await?;
            tokio::time::sleep(Duration::from_millis(pause_ms)).await;
            Ok(Response::Flag {
                value: claim.is_some_and(|claim| claim.deadline() > Instant::now()),
            })
        }
        Request::SeedArtworkRepairFenceItem { ordinal } => {
            let store = store_ref(store)?;
            let library = store
                .create_library(&NewLibrary {
                    name: format!("Artwork fence {}-{ordinal}", unix_now()?),
                    kind: LibraryKind::Books,
                    paths: vec![PathBuf::from("/cluster-artwork-fence")],
                    anime: false,
                })
                .await?;
            let item_id = store
                .insert_item(&NewItem {
                    library_id: library.id,
                    kind: ItemKind::Book,
                    parent_id: None,
                    title: "Original artwork fence title".to_owned(),
                    year: None,
                    season_number: None,
                    episode_number: None,
                })
                .await?;
            Ok(Response::ItemId { item_id })
        }
        Request::ClaimArtworkRepairFence {
            item_id,
            lease_ms,
            inject_leader_change,
        } => {
            if inject_leader_change {
                // Execute the real potentially mutating path, then discard its
                // acknowledgement. This deterministically models the exact
                // client ambiguity that must be fatal to the controller.
                membership_ref(membership)?
                    .claim_artwork_source_repair(item_id, Duration::from_millis(lease_ms))
                    .await?;
                Ok(membership_error_response(MembershipError::LeaderChanged(
                    "injected after applying a potentially mutating repair claim".to_owned(),
                )))
            } else {
                membership_ref(membership)?
                    .claim_artwork_source_repair(item_id, Duration::from_millis(lease_ms))
                    .await
                    .map(|claim| Response::ArtworkRepairFence {
                        fence: claim.map(|claim| claim.fence().clone()),
                    })
                    .or_else(|error| Ok(membership_error_response(error)))
            }
        }
        Request::ApplyArtworkRepairFence {
            target_item_id,
            ref fence,
            ref title,
            stale_job_lease,
            stale_book_snapshot,
        } => {
            let store = store_ref(store)?;
            let now_ms = i64::try_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .context("cluster-check clock precedes unix epoch")?
                    .as_millis(),
            )
            .context("cluster-check unix millisecond clock overflowed")?;
            let resource = format!("cluster-check:artwork-fence:{target_item_id}:{title}");
            let mut lease = match store
                .acquire_lease(&resource, "cluster-check-artwork", now_ms, now_ms + 10_000)
                .await?
            {
                LeaseClaim::Acquired(lease) => lease,
                held => bail!("cluster-check artwork publication lease was held: {held:?}"),
            };
            if stale_job_lease {
                store
                    .renew_lease(&lease, now_ms + 1, now_ms + 20_000)
                    .await?
                    .context("cluster-check artwork publication lease was not renewable")?;
            }
            let replacement = lease.publication_successor()?;
            let setting = match store
                .put_setting_if_absent_if_artwork_repair_current_fenced(
                    &format!("cluster-check.artwork-fence.{target_item_id}.{title}"),
                    title,
                    target_item_id,
                    fence,
                    &lease,
                    &replacement,
                )
                .await
            {
                Ok(setting) => {
                    lease = replacement;
                    setting
                }
                Err(plurx_core::error::StoreError::FenceRejected { .. }) => false,
                Err(error) => return Err(error.into()),
            };
            let replacement = lease.publication_successor()?;
            let metadata = match store
                .apply_metadata_if_artwork_repair_current_fenced(
                    target_item_id,
                    &MetadataPatch {
                        title: Some(title.clone()),
                        ..Default::default()
                    },
                    fence,
                    &lease,
                    &replacement,
                )
                .await
            {
                Ok(metadata) => {
                    lease = replacement;
                    metadata
                }
                Err(plurx_core::error::StoreError::FenceRejected { .. }) => false,
                Err(error) => return Err(error.into()),
            };
            let expected = store
                .get_item(target_item_id)
                .await?
                .context("cluster-check artwork fence target disappeared")?;
            if stale_book_snapshot {
                let replacement = lease.publication_successor()?;
                store
                    .apply_metadata_fenced(
                        target_item_id,
                        &MetadataPatch {
                            title: Some(format!("{title} successor")),
                            ..Default::default()
                        },
                        &lease,
                        &replacement,
                    )
                    .await?;
                lease = replacement;
            }
            let replacement = lease.publication_successor()?;
            let book = match store
                .apply_book_metadata_if_current_fenced(
                    &expected,
                    &BookMetadataPatch {
                        title: None,
                        author: Some(format!("{title} author")),
                        work_id: Some(format!("{title} work")),
                        edition_id: Some(format!("{title} edition")),
                        poster_path: None,
                        source: BookMetadataSource::Curator,
                        required_origin: None,
                    },
                    Some(fence),
                    &lease,
                    &replacement,
                )
                .await
            {
                Ok(book) => book,
                Err(plurx_core::error::StoreError::FenceRejected { .. }) => false,
                Err(error) => return Err(error.into()),
            };
            Ok(Response::ArtworkFenceApply {
                setting,
                metadata,
                book,
            })
        }
        Request::AcquireJobLease {
            ref resource,
            ref owner_node_id,
        } => {
            let now_ms = i64::try_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .context("cluster-check clock precedes unix epoch")?
                    .as_millis(),
            )
            .context("cluster-check unix millisecond clock overflowed")?;
            match store_ref(store)?
                .acquire_lease(resource, owner_node_id, now_ms, now_ms + 600_000)
                .await
            {
                Ok(LeaseClaim::Acquired(lease)) => Ok(Response::JobLease { lease: Some(lease) }),
                Ok(LeaseClaim::Held { .. }) => Ok(Response::JobLease { lease: None }),
                Err(plurx_core::error::StoreError::Task(message))
                    if message.contains("has been removed") =>
                {
                    Ok(Response::JobLease { lease: None })
                }
                Err(error) => Err(error.into()),
            }
        }
        Request::SeedExhaustedJobLease {
            ref resource,
            ref owner_node_id,
        } => {
            let now_ms = i64::try_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .context("cluster-check clock precedes unix epoch")?
                    .as_millis(),
            )
            .context("cluster-check unix millisecond clock overflowed")?;
            let expires_at_unix_ms = now_ms - 100;
            client
                .execute(
                    "INSERT INTO job_leases \
                       (resource, owner_node_id, fence, revision, expires_at_ms, updated_at_ms) \
                     VALUES ($1, $2, 1, 9223372036854775807, $3, $4)",
                    params!(resource, owner_node_id, expires_at_unix_ms, now_ms),
                )
                .await?;
            Ok(Response::JobLease {
                lease: Some(Lease {
                    resource: resource.clone(),
                    owner_node_id: owner_node_id.clone(),
                    fence: 1,
                    revision: i64::MAX as u64,
                    expires_at_unix_ms,
                }),
            })
        }
        Request::AcquireJobLeaseLegacy {
            ref resource,
            ref owner_node_id,
        } => {
            let now_ms = i64::try_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .context("cluster-check clock precedes unix epoch")?
                    .as_millis(),
            )
            .context("cluster-check unix millisecond clock overflowed")?;
            let legacy_sql = "INSERT INTO job_leases \
                (resource, owner_node_id, fence, revision, expires_at_ms, updated_at_ms) \
                VALUES ($1, $2, 1, 1, $3, $4) \
                ON CONFLICT(resource) DO UPDATE SET \
                    owner_node_id = excluded.owner_node_id, \
                    fence = job_leases.fence + 1, \
                    revision = job_leases.revision + 1, \
                    expires_at_ms = excluded.expires_at_ms, \
                    updated_at_ms = excluded.updated_at_ms \
                WHERE job_leases.expires_at_ms <= $4 \
                  AND job_leases.fence < 9223372036854775807 \
                  AND job_leases.revision < 9223372036854775807";
            match client
                .execute(
                    legacy_sql,
                    params!(resource, owner_node_id, now_ms + 600_000, now_ms),
                )
                .await
            {
                Ok(changed) => Ok(Response::Flag {
                    value: changed == 1,
                }),
                Err(error) if error.to_string().contains("owner has been removed") => {
                    Ok(Response::Flag { value: false })
                }
                Err(error) => Err(error.into()),
            }
        }
        Request::ClearJobOwnerFence { ref owner_node_id } => {
            client
                .execute(
                    "DELETE FROM settings \
                     WHERE key = 'internal.cluster_job_owner_removed.' || $1",
                    params!(owner_node_id),
                )
                .await?;
            Ok(Response::Ok)
        }
        Request::ApplyJobLease {
            ref key,
            ref lease,
            observed_at_ms,
        } => {
            let now_ms = i64::try_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .context("cluster-check clock precedes unix epoch")?
                    .as_millis(),
            )
            .context("cluster-check unix millisecond clock overflowed")?;
            let observed_at_ms = observed_at_ms.unwrap_or(now_ms);
            if observed_at_ms >= lease.expires_at_unix_ms {
                return Ok(Response::Flag { value: false });
            }
            let replacement = match lease.publication_successor() {
                Ok(replacement) => replacement,
                Err(plurx_core::error::StoreError::FenceRejected { .. }) => {
                    return Ok(Response::Flag { value: false });
                }
                Err(error) => return Err(error.into()),
            };
            match store_ref(store)?
                .put_setting_fenced(key, "removed-owner-write", lease, &replacement)
                .await
            {
                Ok(()) => Ok(Response::Flag { value: true }),
                Err(plurx_core::error::StoreError::FenceRejected { .. }) => {
                    Ok(Response::Flag { value: false })
                }
                Err(error) => Err(error.into()),
            }
        }
        Request::StartSingletonProbe { provider_url } => {
            if singleton_probe.is_some() {
                bail!("this voter already owns a singleton probe generation");
            }
            let opened = store.clone().context("node store is not open")?;
            let coordinator = StoreCoordinator::new(
                opened.clone() as Arc<dyn plurx_core::store::Store>,
                format!("node-{}", launch.node_id),
            )?;
            match coordinator
                .acquire(SINGLETON_RESOURCE, SINGLETON_LEASE_TTL)
                .await?
            {
                LeaseClaim::Held { .. } => Ok(Response::SingletonProbeStart {
                    acquired: false,
                    lease: read_job_lease(client, SINGLETON_RESOURCE)
                        .await?
                        .context("held singleton lease row disappeared")?,
                }),
                LeaseClaim::Acquired(lease) => {
                    let active = ActiveJobLease::start_with_policy(
                        coordinator,
                        lease.clone(),
                        SINGLETON_LEASE_TTL,
                        SINGLETON_HEARTBEAT,
                    )?;
                    let outcome = Arc::new(RwLock::new("provider_pending".to_owned()));
                    let cleanup_done = Arc::new(AtomicBool::new(false));
                    let cleanup_error = Arc::new(RwLock::new(None));
                    *singleton_probe = Some(SingletonProbe {
                        outcome: Arc::clone(&outcome),
                        cleanup_done: Arc::clone(&cleanup_done),
                        cleanup_error: Arc::clone(&cleanup_error),
                    });
                    let probe_store = opened as Arc<dyn plurx_core::store::Store>;
                    let _probe_task = tokio::spawn(async move {
                        let loss = active.loss_token();
                        let client = reqwest::Client::builder()
                            .no_proxy()
                            .connect_timeout(Duration::from_secs(5))
                            .timeout(Duration::from_secs(30))
                            .build();
                        let terminal = match client {
                            Err(error) => format!("failed:http_client:{error}"),
                            Ok(client) => {
                                let provider = async {
                                    let response = client.get(provider_url).send().await?;
                                    let response = response.error_for_status()?;
                                    response.text().await
                                };
                                tokio::pin!(provider);
                                tokio::select! {
                                    _ = loss.cancelled() => "lease_lost".to_owned(),
                                    result = &mut provider => match result {
                                        Err(error) => format!("failed:provider:{error}"),
                                        Ok(marker) => {
                                            *outcome.write().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                                                "publishing".to_owned();
                                            match active.publisher(probe_store.as_ref())
                                                .put_setting(SINGLETON_PROOF_KEY, &marker)
                                                .await
                                            {
                                                Ok(()) => "published".to_owned(),
                                                Err(StoreError::FenceRejected { .. }) =>
                                                    "fence_rejected".to_owned(),
                                                Err(error) => format!("failed:publish:{error}"),
                                            }
                                        }
                                    }
                                }
                            }
                        };
                        *outcome
                            .write()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) = terminal;
                        // Publish the terminal verdict before cleanup so a
                        // stuck retirement reports the exact completed phase.
                        // The controller separately waits for cleanup before
                        // sampling the bounded Raft-entry delta.
                        if let Err(error) = active.release().await {
                            *cleanup_error
                                .write()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                                Some(error.to_string());
                        }
                        cleanup_done.store(true, AtomicOrdering::Release);
                    });
                    Ok(Response::SingletonProbeStart {
                        acquired: true,
                        lease,
                    })
                }
            }
        }
        Request::ReadSingletonLease => Ok(Response::SingletonLease {
            lease: read_job_lease(client, SINGLETON_RESOURCE).await?,
        }),
        Request::SingletonProbeStatus => {
            let probe = singleton_probe
                .as_ref()
                .context("this voter has no acquired singleton probe")?;
            Ok(Response::SingletonProbeStatus {
                outcome: probe
                    .outcome
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone(),
                cleanup_done: probe.cleanup_done.load(AtomicOrdering::Acquire),
                cleanup_error: probe
                    .cleanup_error
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone(),
            })
        }
        Request::ReplaySingletonLease { ref lease } => {
            let replacement = lease.publication_successor()?;
            match store_ref(store)?
                .put_setting_fenced(SINGLETON_PROOF_KEY, "raw-stale-owner", lease, &replacement)
                .await
            {
                Ok(()) => Ok(Response::Flag { value: true }),
                Err(StoreError::FenceRejected { .. }) => Ok(Response::Flag { value: false }),
                Err(error) => Err(error.into()),
            }
        }
        Request::ReadSingletonValue { local } => {
            let value = if local {
                read_local_setting(client, SINGLETON_PROOF_KEY).await?
            } else {
                store_ref(store)?.get_setting(SINGLETON_PROOF_KEY).await?
            };
            Ok(Response::SingletonValue { value })
        }
        Request::PutSetting { ref key, ref value } => {
            store_ref(store)?.put_setting(key, value).await?;
            Ok(Response::Ok)
        }
        Request::ReadLocalSetting { ref key } => Ok(Response::Setting {
            value: read_local_setting(client, key).await?,
        }),
        Request::ForceCompaction { ref phase } => {
            let previous = snapshot_index(client).await?;
            ensure_compaction_after(client, store_ref(store)?, previous, phase).await?;
            Ok(Response::Ok)
        }
        Request::ReleaseArtworkRepair { ref fence } => membership_ref(membership)?
            .retire_artwork_source_repair(fence)
            .await
            .and_then(|retired| {
                retired.then_some(Response::Ok).ok_or_else(|| {
                    MembershipError::Internal(
                        "current artwork repair generation was not retired".to_owned(),
                    )
                })
            })
            .or_else(|error| Ok(membership_error_response(error))),
        Request::TriggerElection => {
            membership_ref(membership)?.trigger_local_election().await?;
            Ok(Response::Ok)
        }
        Request::RemoveVoter { node_id } => membership_ref(membership)?
            .remove_voter(&node_id)
            .await
            .map(|_| Response::Ok)
            .or_else(|error| Ok(membership_error_response(error))),
        Request::RemoveNode { node_id } => membership_ref(membership)?
            .remove_node(&node_id)
            .await
            .map(|_| Response::Ok)
            .or_else(|error| Ok(membership_error_response(error))),
        Request::PromoteLearner { node_id } => membership_ref(membership)?
            .promote_learner(&node_id)
            .await
            .map(|_| Response::Ok)
            .or_else(|error| Ok(membership_error_response(error))),
        Request::SeedActiveMediaSession {
            ref node_id,
            ref session_id,
        } => {
            let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
            client
                .execute(
                    "INSERT INTO media_sessions \
                     (incarnation_id, session_id, user_id, playback_id, request_fingerprint, \
                      owner_node_id, owner_epoch, lease_expires_at_ms, state, recipe_json, \
                      response_json, updated_at_ms) \
                     VALUES ($1, $1, 1, $1, $1, $2, 1, $3, 'active', '{}', '{}', $4)",
                    params!(session_id, node_id, now + 60_000, now),
                )
                .await?;
            Ok(Response::Ok)
        }
        Request::ReadMediaSessionState { ref session_id } => {
            let value = client
                .query_map::<SingletonSettingRow, _>(
                    "SELECT state AS value FROM media_sessions WHERE session_id = $1",
                    params!(session_id),
                )
                .await?
                .into_iter()
                .next()
                .map(|row| row.value);
            Ok(Response::Setting { value })
        }
        Request::LeaveVoter => membership_ref(membership)?
            .leave_voter()
            .await
            .map(|()| Response::Ok)
            .or_else(|error| Ok(membership_error_response(error))),
        Request::SeedOfflineRemovalWork { node_id, media_dir } => {
            let user_id =
                seed_offline_removal_work(store_ref(store)?, &node_id, Path::new(&media_dir))
                    .await?;
            Ok(Response::SeededOfflineRemovalWork { user_id })
        }
        Request::SeedOfflineWorkDuringRemoval {
            node_id,
            media_dir,
            user_id,
            delay_ms,
        } => {
            // Answered before the package exists on purpose. The caller starts
            // the removal next, so the request lands mid-resolution the way a
            // real one would, rather than at a moment the harness chose.
            let store = store.clone().context("node store is not open")?;
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                if let Err(error) = seed_offline_work_during_removal(
                    &store,
                    &node_id,
                    Path::new(&media_dir),
                    user_id,
                )
                .await
                {
                    eprintln!("cluster-check: mid-removal offline seed failed: {error:#}");
                }
            });
            Ok(Response::Ok)
        }
        Request::DeleteOfflinePackage {
            package_id,
            user_id,
        } => Ok(Response::Flag {
            value: store_ref(store)?
                .delete_offline_package(&package_id, user_id)
                .await?,
        }),
        Request::OfflinePackageSummary {
            package_id,
            user_id,
        } => {
            let package = store_ref(store)?
                .offline_package_for_user(&package_id, user_id)
                .await?
                .with_context(|| format!("offline package {package_id} disappeared"))?;
            Ok(Response::OfflinePackageSummary {
                state: package.state,
                node_id: package.node_id,
                error_code: package.error_code,
                reserved_bytes: package.reserved_bytes,
                actual_bytes: package.actual_bytes,
            })
        }
        Request::ClaimNextOfflinePackage { node_id } => Ok(Response::ClaimedOfflinePackage {
            package_id: store_ref(store)?
                .claim_next_offline_package(&node_id)
                .await?
                .map(|package| package.id),
        }),
        Request::PublishOfflinePackage {
            package_id,
            node_id,
        } => Ok(Response::Flag {
            value: store_ref(store)?
                .mark_offline_package_ready(&package_id, &node_id, "rehomed-recipe", 900, 90_000)
                .await?,
        }),
        Request::ResetContractState => {
            store_ref(store)?.validation_reset_contract_state().await?;
            Ok(Response::Ok)
        }
        Request::RecordLocalTelemetry { marker } => {
            store_ref(store)?
                .record_playback_event(&PlaybackEvent {
                    at_unix_ms: 1_700_000_000_000,
                    event: "cluster_sidecar_marker".to_owned(),
                    detail: Some(marker),
                    ..PlaybackEvent::default()
                })
                .await?;
            Ok(Response::Ok)
        }
        Request::CountLocalTelemetry { marker } => {
            let count = store_ref(store)?
                .playback_events(&PlaybackEventQuery {
                    event: Some("cluster_sidecar_marker".to_owned()),
                    limit: 100,
                    ..PlaybackEventQuery::default()
                })
                .await?
                .into_iter()
                .filter(|event| event.detail.as_deref() == Some(marker.as_str()))
                .count();
            Ok(Response::TelemetryCount { count })
        }
        Request::Exercise { ordinal } => {
            exercise(store_ref(store)?, ordinal).await?;
            Ok(Response::Ok)
        }
        Request::TopologyWrite { ordinal, ref value } => {
            store_ref(store)?
                .put_setting(&format!("cluster.topology.write.{ordinal:04}"), value)
                .await?;
            Ok(Response::Ok)
        }
        Request::TopologySeedCatalogue { rows } => Ok(Response::TopologyCatalogueSeeded {
            item_ids: seed_topology_catalogue(store_ref(store)?, rows).await?,
        }),
        Request::TopologyCatalogueReads {
            item_ids,
            operations,
            concurrency,
        } => {
            run_topology_catalogue_reads(
                catalogue_ref(catalogue)?.clone(),
                store_ref(catalogue_store)?,
                item_ids,
                operations,
                concurrency,
            )
            .await
        }
        Request::TopologyResources {
            hardware,
            storage_device,
            network_path,
            reset_max_rss,
        } => Ok(Response::TopologyResources {
            sample: capture_topology_resources(
                launch.node_id,
                node_started.elapsed(),
                hardware,
                storage_device,
                network_path,
                reset_max_rss,
            )?,
        }),
        Request::StoreInstrumentationStatus => Ok(Response::StoreInstrumentationStatus {
            enabled: plurx_core::store::validation_store_operation_instrumentation_enabled(),
            recorded_operations_total: plurx_core::store::validation_store_operation_metric_count(),
        }),
        Request::PostLossWrite {
            target,
            position_ms,
        } => {
            let store = store_ref(store)?;
            store
                .put_setting(&format!("post_loss.{target}"), "acknowledged")
                .await?;
            if store
                .get_setting(&format!("post_loss.{target}"))
                .await?
                .as_deref()
                != Some("acknowledged")
            {
                bail!("post-loss acknowledged write was not readable");
            }
            // The lag signal must be driven by the user-visible state this
            // feature describes, not by an unrelated synthetic Raft entry.
            let user = store
                .get_user_by_username("survivor-1")
                .await?
                .context("post-loss watch user is missing")?;
            let movie = store
                .search_items("Replicated Catalog Proof 1", 10)
                .await?
                .into_iter()
                .find(|item| item.item.title == "Replicated Catalog Proof 1")
                .context("post-loss watch item is missing")?;
            let watch = store
                .put_progress(user.id, movie.item.id, position_ms, Some(120_000))
                .await?;
            if watch.position_ms != position_ms {
                bail!("post-loss watch progress was not acknowledged");
            }
            Ok(Response::Ok)
        }
        Request::VerifyProof => {
            verify_proof(store_ref(store)?).await?;
            Ok(Response::Ok)
        }
        Request::Dump => {
            let store = store_ref(store)?;
            Ok(Response::Dump {
                digest: store.local_dump_digest().await?,
                dump: store.validation_local_dump().await?,
            })
        }
        Request::CatalogView => Ok(Response::CatalogView {
            view: catalog_view(store_ref(store)?).await?,
        }),
        Request::RebuildSearch => {
            store_ref(store)?.rebuild_search_index().await?;
            Ok(Response::Ok)
        }
        Request::Metrics => {
            let metrics = client.metrics_db().await?;
            let mut voters = metrics.membership_config.voter_ids().collect::<Vec<_>>();
            voters.sort_unstable();
            let mut members = metrics
                .membership_config
                .nodes()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            members.sort_unstable();
            members.dedup();
            Ok(Response::Metrics {
                leader: metrics.current_leader,
                current_term: metrics.current_term,
                voters,
                members,
                applied_index: metrics.last_applied.as_ref().map(|log| log.index),
                quorum_acknowledged: metrics
                    .millis_since_quorum_ack
                    .is_some_and(|age| age <= 1_000),
            })
        }
        Request::RaftDebug => Ok(Response::RaftDebug {
            summary: client.metrics_db().await?.to_string(),
        }),
        Request::PassiveRaftMetrics => {
            let view = replication.metrics_handle().snapshot();
            Ok(Response::PassiveRaftMetrics {
                valid: view.valid,
                age_seconds: view.age_seconds,
                errors: view.errors,
                leader_changes: view.leader_changes,
                current_term: view.sample.map(|sample| sample.current_term),
                applied_index: view.sample.and_then(|sample| sample.last_applied_index),
                leader_known: view.sample.map(|sample| sample.leader_known),
                is_leader: view.sample.map(|sample| sample.is_leader),
                watermark_valid: view.watermark_valid,
                local_reads_supported: view.watermark_local_reads_supported,
                watermark_age_millis: view.watermark_age_millis,
                watermark_errors: view.watermark_errors,
                committed_index: view.watermark.map(|sample| sample.committed_index),
                apply_lag_entries: view.watermark.and_then(|sample| sample.apply_lag_entries),
            })
        }
        Request::QuorumWatermark => {
            let watermark = client.db_quorum_watermark().await?;
            Ok(Response::QuorumWatermark {
                term: watermark.term,
                leader_id: watermark.leader_id,
                committed_index: watermark.committed_index,
            })
        }
        Request::PauseApply => {
            hiqlite::validation_pause_apply();
            Ok(Response::Ok)
        }
        Request::ApplyPauseObserved => Ok(Response::ApplyPauseObserved {
            observed: hiqlite::validation_apply_pause_observed(),
        }),
        Request::ResumeApply => {
            hiqlite::validation_resume_apply();
            Ok(Response::Ok)
        }
        Request::SetRaftPartitioned { partitioned } => {
            hiqlite::validation_set_raft_partitioned(partitioned);
            Ok(Response::Ok)
        }
        Request::BoundedCatalogueGetItem { item_id } => {
            let store = catalogue_store_ref(catalogue_store)?;
            store.validation_reset_operation_counts();
            let result = catalogue_ref(catalogue)?.get_item(item_id).await;
            let counts = store.validation_operation_counts();
            let view = replication.metrics_handle().snapshot();
            let (title, error) = match result {
                Ok(item) => (item.map(|item| item.title), None),
                Err(error) => (None, Some(error.to_string())),
            };
            Ok(Response::BoundedCatalogueRead {
                title,
                error,
                consistent_query_calls: counts.consistent_query_calls,
                non_consistent_query_calls: counts.non_consistent_query_calls,
                watermark_valid: view.watermark_valid,
                watermark_age_millis: view.watermark_age_millis,
                local_reads_supported: view.watermark_local_reads_supported,
                apply_lag_entries: view
                    .watermark
                    .and_then(|watermark| watermark.apply_lag_entries),
                serving_ready: serving.is_ready(),
            })
        }
        Request::ProveOldWatermarkStreamCompatibility => {
            let error = client
                .db_quorum_watermark()
                .await
                .expect_err("old handler must reject the reserved marker as SQL");
            if !error.to_string().to_ascii_lowercase().contains("syntax") {
                bail!("old handler rejected the watermark marker unexpectedly: {error}");
            }
            let mut rows = client
                .query_consistent(WATERMARK_STREAM_COMPAT_PROBE, params!())
                .await?;
            if rows.len() != 1
                || rows
                    .swap_remove(0)
                    .get::<i64>("hiqlite_watermark_stream_compat_v1")
                    != 1
            {
                bail!("ordinary consistent query returned the wrong rolling-compatibility proof");
            }
            Ok(Response::Ok)
        }
        Request::ProveP3aWatermarkCompatibility => {
            let watermark = client.db_quorum_watermark().await?;
            if watermark.local_read_protocol_version != 0 {
                bail!(
                    "P3a three-column watermark advertised local-read protocol {}",
                    watermark.local_read_protocol_version
                );
            }

            let fence = ServingFence::new(replication.metrics_handle());
            let shutdown = tokio_util::sync::CancellationToken::new();
            let monitor = tokio::spawn(fence.clone().monitor_loop(shutdown.clone()));
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let view = replication.metrics_handle().snapshot();
                if view.watermark_valid && !view.watermark_local_reads_supported && fence.is_ready()
                {
                    break;
                }
                if Instant::now() >= deadline {
                    shutdown.cancel();
                    let _ = monitor.await;
                    bail!(
                        "P3a watermark did not preserve readiness while local reads stayed disabled: valid={}, local_reads_supported={}, ready={}",
                        view.watermark_valid,
                        view.watermark_local_reads_supported,
                        fence.is_ready()
                    );
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            shutdown.cancel();
            monitor.await.context("join compatibility serving fence")?;
            Ok(Response::Ok)
        }
        Request::ReplicationStatus => Ok(Response::ReplicationStatus {
            status: replication.status().await,
        }),
        Request::Ping => {
            store_ref(store)?.ping().await?;
            Ok(Response::Ok)
        }
        Request::ReadWithoutQuorum => {
            store_ref(store)?.get_setting("instance.id").await?;
            Ok(Response::Ok)
        }
        Request::WriteWithoutQuorum => {
            store_ref(store)?
                .put_setting("no-quorum.write", "must-not-ack")
                .await?;
            Ok(Response::Ok)
        }
    }
}

#[cfg(target_os = "linux")]
fn capture_topology_resources(
    node_id: u64,
    wall: Duration,
    hardware: String,
    storage_device: String,
    network_path: String,
    reset_max_rss: bool,
) -> Result<ResourceSample> {
    let stat = std::fs::read_to_string("/proc/self/stat").context("read process CPU counters")?;
    let after_name = stat
        .rsplit_once(')')
        .map(|(_, fields)| fields)
        .context("parse process stat command name")?;
    // Fields after the command name begin at proc(5) field 3. utime/stime are
    // fields 14/15, hence offsets 11/12 in this suffix.
    let fields = after_name.split_whitespace().collect::<Vec<_>>();
    let user_ticks = fields
        .get(11)
        .context("process stat omitted user ticks")?
        .parse::<u64>()?;
    let system_ticks = fields
        .get(12)
        .context("process stat omitted system ticks")?
        .parse::<u64>()?;
    // SAFETY: sysconf is a read-only libc query with a fixed selector.
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks_per_second <= 0 {
        bail!("Linux reported an invalid process clock tick rate");
    }

    let status = std::fs::read_to_string("/proc/self/status")
        .context("read process resident-memory counters")?;
    let max_rss_kib = proc_key_u64(&status, "VmHWM:")
        .or_else(|| proc_key_u64(&status, "VmRSS:"))
        .context("process status omitted VmHWM and VmRSS")?;
    let io = std::fs::read_to_string("/proc/self/io").context("read process I/O counters")?;
    let storage_read_bytes =
        proc_key_u64(&io, "read_bytes:").context("process I/O omitted read_bytes")?;
    let storage_write_bytes =
        proc_key_u64(&io, "write_bytes:").context("process I/O omitted write_bytes")?;
    let network =
        std::fs::read_to_string("/proc/net/dev").context("read container network counters")?;
    let (network_receive_bytes, network_transmit_bytes) = parse_network_bytes(&network)?;

    if reset_max_rss {
        // Linux supports clear_refs=5 as a process-local high-water reset. The
        // post-workload VmHWM therefore belongs to the measured interval, not
        // bootstrap, schema migration, or leader election.
        std::fs::write("/proc/self/clear_refs", b"5\n")
            .context("reset process peak resident memory for topology window")?;
    }

    Ok(ResourceSample {
        node_id,
        hardware,
        storage_device,
        network_path,
        cpu_seconds: Some((user_ticks + system_ticks) as f64 / ticks_per_second as f64),
        wall_seconds: Some(wall.as_secs_f64()),
        max_rss_bytes: Some(max_rss_kib.saturating_mul(1024)),
        storage_read_bytes: Some(storage_read_bytes),
        storage_write_bytes: Some(storage_write_bytes),
        network_receive_bytes: Some(network_receive_bytes),
        network_transmit_bytes: Some(network_transmit_bytes),
    })
}

#[cfg(not(target_os = "linux"))]
fn capture_topology_resources(
    _node_id: u64,
    _wall: Duration,
    _hardware: String,
    _storage_device: String,
    _network_path: String,
    _reset_max_rss: bool,
) -> Result<ResourceSample> {
    bail!("named topology resource capture requires Linux voters")
}

#[cfg(target_os = "linux")]
fn proc_key_u64(contents: &str, key: &str) -> Option<u64> {
    contents.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.trim();
        value.split_whitespace().next()?.parse().ok()
    })
}

#[cfg(target_os = "linux")]
fn parse_network_bytes(contents: &str) -> Result<(u64, u64)> {
    let mut receive = 0_u64;
    let mut transmit = 0_u64;
    let mut interfaces = 0_u64;
    for line in contents.lines().skip(2) {
        let Some((name, counters)) = line.split_once(':') else {
            continue;
        };
        if name.trim() == "lo" {
            continue;
        }
        let counters = counters.split_whitespace().collect::<Vec<_>>();
        if counters.len() < 16 {
            bail!("network interface counters were truncated");
        }
        receive = receive.saturating_add(counters[0].parse::<u64>()?);
        transmit = transmit.saturating_add(counters[8].parse::<u64>()?);
        interfaces += 1;
    }
    if interfaces == 0 {
        bail!("container has no non-loopback network interface");
    }
    Ok((receive, transmit))
}

#[derive(Debug)]
struct SingletonLeaseRow {
    resource: String,
    owner_node_id: String,
    fence: i64,
    revision: i64,
    expires_at_unix_ms: i64,
}

impl From<&mut Row<'_>> for SingletonLeaseRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            resource: row.get("resource"),
            owner_node_id: row.get("owner_node_id"),
            fence: row.get("fence"),
            revision: row.get("revision"),
            expires_at_unix_ms: row.get("expires_at_ms"),
        }
    }
}

impl SingletonLeaseRow {
    fn into_lease(self) -> Result<Lease> {
        Ok(Lease {
            resource: self.resource,
            owner_node_id: self.owner_node_id,
            fence: u64::try_from(self.fence).context("singleton fence was not positive")?,
            revision: u64::try_from(self.revision)
                .context("singleton revision was not positive")?,
            expires_at_unix_ms: self.expires_at_unix_ms,
        })
    }
}

struct SingletonSettingRow {
    value: String,
}

impl From<&mut Row<'_>> for SingletonSettingRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            value: row.get("value"),
        }
    }
}

struct HarnessArtworkRepairRow {
    owner_node_id: String,
    leader_term: i64,
    generation: i64,
}

impl From<&mut Row<'_>> for HarnessArtworkRepairRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            owner_node_id: row.get("owner_node_id"),
            leader_term: row.get("leader_term"),
            generation: row.get("generation"),
        }
    }
}

async fn read_job_lease(client: &Client, resource: &str) -> Result<Option<Lease>> {
    let mut rows = client
        .query_consistent_map::<SingletonLeaseRow, _>(
            "SELECT resource, owner_node_id, fence, revision, expires_at_ms \
             FROM job_leases WHERE resource = $1",
            params!(resource),
        )
        .await?;
    if rows.len() > 1 {
        bail!("singleton lease primary key returned multiple rows");
    }
    rows.pop().map(SingletonLeaseRow::into_lease).transpose()
}

async fn read_local_setting(client: &Client, key: &str) -> Result<Option<String>> {
    let mut rows = client
        .query_map::<SingletonSettingRow, _>(
            "SELECT value FROM settings WHERE key = $1",
            params!(key),
        )
        .await?;
    if rows.len() > 1 {
        bail!("singleton setting primary key returned multiple rows");
    }
    Ok(rows.pop().map(|row| row.value))
}

struct MembershipTombstoneRow {
    removed_at: Option<i64>,
}

impl From<&mut Row<'_>> for MembershipTombstoneRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            removed_at: row.get("removed_at"),
        }
    }
}

fn store_ref(store: &Option<Arc<HiqliteAuthStore>>) -> Result<&HiqliteAuthStore> {
    store.as_deref().context("auth store has not been opened")
}

fn catalogue_ref(catalogue: &Option<CatalogueReader>) -> Result<&CatalogueReader> {
    catalogue
        .as_ref()
        .context("catalogue reader has not been opened")
}

fn catalogue_store_ref(store: &Option<Arc<HiqliteAuthStore>>) -> Result<&Arc<HiqliteAuthStore>> {
    store
        .as_ref()
        .context("catalogue validation store has not been opened")
}

fn membership_ref(membership: &Option<MembershipManager>) -> Result<&MembershipManager> {
    membership
        .as_ref()
        .context("membership manager has not been opened")
}

fn membership_error_response(error: plurx_core::cluster::membership::MembershipError) -> Response {
    match error {
        MembershipError::LeaderChanged(message) => Response::MembershipLeaderChange { message },
        error => Response::MembershipError {
            code: error.code().to_owned(),
            message: error.to_string(),
        },
    }
}

/// Reproduce the removed-node exploit from the shared-HMAC design: a daemon
/// that retained the cluster API secret freshly claims a surviving sender.
/// The per-node Ed25519 verifier must reject this proof; the former verifier
/// accepted it because every voter possessed the same key.
fn shared_secret_activity_proof(
    claimed_node_id: &str,
    target_node_id: &str,
    timestamp_ms: i64,
) -> Result<ActivityPeerAuth> {
    let mut message = b"plurx-internal-activity-v1".to_vec();
    for value in [claimed_node_id, target_node_id] {
        message.extend_from_slice(&u64::try_from(value.len())?.to_be_bytes());
        message.extend_from_slice(value.as_bytes());
    }
    message.extend_from_slice(&timestamp_ms.to_be_bytes());
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(API_SECRET.as_bytes())
        .map_err(|error| anyhow!("construct retained shared-secret proof: {error}"))?;
    mac.update(&message);
    Ok(ActivityPeerAuth {
        node_id: claimed_node_id.to_owned(),
        target_node_id: target_node_id.to_owned(),
        timestamp_ms,
        signature: hex::encode(mac.finalize().into_bytes()),
    })
}

/// Build an artwork proof without the production signer so this fixture
/// remains a golden compatibility check for the established v4 HMAC wire.
fn shared_secret_artwork_proof(
    node_id: &str,
    filename: &str,
    timestamp_ms: i64,
) -> Result<ArtworkPeerAuth> {
    let message = format!("plurx-artwork-v1\n{node_id}\n{timestamp_ms}\n{filename}");
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(API_SECRET.as_bytes())
        .map_err(|error| anyhow!("construct legacy artwork proof: {error}"))?;
    mac.update(message.as_bytes());
    Ok(ArtworkPeerAuth {
        node_id: node_id.to_owned(),
        timestamp_ms,
        signature: hex::encode(mac.finalize().into_bytes()),
    })
}

/// Verify a production proof without the production verifier. Together with
/// `shared_secret_artwork_proof`, this checks both rolling-upgrade directions.
fn legacy_artwork_proof_is_valid(filename: &str, auth: &ArtworkPeerAuth) -> Result<bool> {
    let signature = match hex::decode(&auth.signature) {
        Ok(signature) => signature,
        Err(_) => return Ok(false),
    };
    let message = format!(
        "plurx-artwork-v1\n{}\n{}\n{filename}",
        auth.node_id, auth.timestamp_ms
    );
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(API_SECRET.as_bytes())
        .map_err(|error| anyhow!("construct legacy artwork verifier: {error}"))?;
    mac.update(message.as_bytes());
    Ok(mac.verify_slice(&signature).is_ok())
}

async fn membership_manager(
    client: &Client,
    replication: &ReplicationMonitor,
    store: Arc<HiqliteAuthStore>,
    launch: &NodeLaunch,
) -> Result<MembershipManager> {
    membership_manager_with_artwork_url(
        client,
        replication,
        store,
        launch,
        format!("http://127.0.0.1:{}", 33_000 + launch.node_id),
    )
    .await
    .map_err(Into::into)
}

async fn membership_manager_with_artwork_url(
    client: &Client,
    replication: &ReplicationMonitor,
    store: Arc<HiqliteAuthStore>,
    launch: &NodeLaunch,
    artwork_http: String,
) -> std::result::Result<MembershipManager, plurx_core::cluster::membership::MembershipError> {
    membership_manager_with_identity_artwork_url(
        client,
        replication,
        store,
        launch,
        format!("node-{}", launch.node_id),
        launch.node_id,
        artwork_http,
    )
    .await
}

async fn membership_manager_with_identity_artwork_url(
    client: &Client,
    replication: &ReplicationMonitor,
    store: Arc<HiqliteAuthStore>,
    launch: &NodeLaunch,
    node_id: String,
    raft_id: u64,
    artwork_http: String,
) -> std::result::Result<MembershipManager, plurx_core::cluster::membership::MembershipError> {
    let local = launch
        .nodes
        .iter()
        .find(|node| node.id == launch.node_id)
        .ok_or_else(|| {
            plurx_core::cluster::membership::MembershipError::Internal(format!(
                "voter {} has no local node spec",
                launch.node_id
            ))
        })?;
    MembershipManager::replicated(
        client.clone(),
        replication.clone(),
        store,
        ClusterIdentity {
            cluster_id: INSTANCE_ID.to_owned(),
            node_id: node_id.clone(),
            raft_id,
        },
        ClusterPeer {
            raft_id: local.id,
            raft_address: local.raft.clone(),
            api_address: local.api.clone(),
        },
        "https://shared-join-lb.invalid".to_owned(),
        artwork_http,
        JoinSecrets {
            raft: RAFT_SECRET.to_owned(),
            api: API_SECRET.to_owned(),
            credential_key: "00".repeat(32),
        },
        ActivitySigningKey::from_seed_hex(&hex::encode(Sha256::digest(format!(
            "plurx-cluster-check-activity-key:{node_id}:{raft_id}"
        ))))?,
        ActivationMarker {
            marker_version: 1,
            cluster_id: INSTANCE_ID.to_owned(),
            source_backup_sha256: "00".repeat(32),
            source_schema_version: AUTH_SCHEMA_VERSION,
            replicated_schema_version: AUTH_SCHEMA_VERSION,
            imported_rows: 0,
            table_hashes: Vec::new(),
            admitted_role: Some(launch.role),
        },
        launch.role,
        launch.root.join(format!("node-{}", launch.node_id)),
    )
    .await
}

/// Package ids the removal scenario asserts on. Each names the §6.7 outcome
/// it is there to pin down.
const MOVABLE_PACKAGE: &str = "offline-movable";
const STRANDED_PACKAGE: &str = "offline-stranded";
const READY_PACKAGE: &str = "offline-ready";
const TRANSFER_PACKAGE: &str = "offline-transfer";
/// Requested after the removal has already drawn its plan.
const LATE_PACKAGE: &str = "offline-late";
/// Long enough to land after the removal snapshots the work it plans to
/// resolve, and well inside the bounded probe wait that follows it.
const LATE_PACKAGE_DELAY_MS: u64 = 250;

/// Seed the four packages a node removal has to deal with, backed by real
/// files on a real filesystem.
///
/// The sources are genuinely written to disk (and genuinely absent, for the
/// stranded one) because the whole contract under test is that a survivor
/// proves it can *read* the bytes. A fixture that only wrote rows would prove
/// the requeue mechanics and skip the part §6.7 actually cares about.
async fn seed_offline_removal_work(
    store: &HiqliteAuthStore,
    node_id: &str,
    media_dir: &Path,
) -> Result<i64> {
    std::fs::create_dir_all(media_dir).context("offline removal fixture media directory")?;
    let user = store
        .create_user(&format!("membership-offline-{node_id}"), "hash", false)
        .await?;
    let library = store
        .create_library(&NewLibrary {
            name: format!("Membership Offline {node_id}"),
            kind: LibraryKind::Movies,
            paths: vec![media_dir.to_path_buf()],
            anime: false,
        })
        .await?;
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: format!("Membership Offline {node_id}"),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await?;

    // Created and transitioned one at a time so exactly one package is
    // claimable at each step: the fairness ordering in the queue is not what
    // this scenario is proving.
    for (package_id, present, target_state) in [
        (MOVABLE_PACKAGE, true, "preparing"),
        (READY_PACKAGE, true, "ready"),
        (TRANSFER_PACKAGE, true, "ready"),
        (STRANDED_PACKAGE, false, "queued"),
    ] {
        let source_path = media_dir.join(format!("{package_id}.mkv"));
        let (source_size, source_mtime) = if present {
            std::fs::write(&source_path, vec![0_u8; 4_096])
                .with_context(|| format!("write offline fixture source for {package_id}"))?;
            let metadata = std::fs::metadata(&source_path)?;
            let mtime = i64::try_from(metadata.modified()?.duration_since(UNIX_EPOCH)?.as_secs())?;
            (i64::try_from(metadata.len())?, mtime)
        } else {
            // Nothing is written here on purpose: no node can prove a source
            // that does not exist, which is exactly the case that must resolve
            // to `node_removed` instead of a hopeful reassignment.
            (4_096, 1_700_000_000)
        };
        let source = source_path.to_string_lossy().to_string();
        let file = store
            .upsert_file(
                item,
                &source,
                source_size,
                source_mtime,
                &ProbeResult::default(),
            )
            .await?;
        let package = NewOfflinePackage {
            id: format!("{package_id}-{node_id}"),
            request_id: format!("{package_id}-request-{node_id}"),
            user_id: user.id,
            file_id: file,
            node_id: node_id.to_owned(),
            source_path: source,
            source_size,
            source_mtime,
            effective_rate_control: "vbr".to_owned(),
            target_height: 720,
            output_width: Some(1280),
            output_height: Some(720),
            audio_index: None,
            audio_offset_ms: 0,
            subtitle_index: None,
            subtitle_language: None,
            subtitle_mode: "none".to_owned(),
            estimated_bytes: 700,
            reserved_bytes: 900,
            expires_at: unix_now()?.saturating_add(3_600),
        };
        if !matches!(
            store
                .create_offline_package(&package, 10, 100_000, 1_000_000)
                .await?,
            OfflineCreateOutcome::Created(_)
        ) {
            bail!("membership offline fixture {package_id} was not admitted");
        }
        if target_state == "queued" {
            continue;
        }
        if store
            .claim_next_offline_package(node_id)
            .await?
            .is_none_or(|claimed| claimed.id != package.id)
        {
            bail!("membership offline fixture {package_id} did not claim in order");
        }
        if target_state == "ready"
            && !store
                .mark_offline_package_ready(&package.id, node_id, package_id, 900, 90_000)
                .await?
        {
            bail!("membership offline fixture {package_id} did not publish");
        }
    }

    // One downloader is mid-transfer. A lease touched this recently is what
    // the activity surface calls "sending", and removal refuses rather than
    // cutting it off.
    let expires_at = unix_now()?.saturating_add(3_600);
    if !matches!(
        store
            .put_offline_lease(
                &format!("{TRANSFER_PACKAGE}-{node_id}"),
                user.id,
                &format!("{:x}", Sha256::digest(b"membership-offline-transfer")),
                expires_at,
            )
            .await?,
        OfflineLeaseOutcome::Created(_) | OfflineLeaseOutcome::Renewed(_)
    ) {
        bail!("membership offline transfer lease was not admitted");
    }
    Ok(user.id)
}

/// Request one more download on the departing node, after that node's removal
/// has already snapshotted the work it planned to resolve.
///
/// The source is a real, readable file so a survivor can genuinely prove it.
/// What this fixture is about is whether the removal looks *again* before it
/// commits — the probe protocol itself is already covered by the four packages
/// seeded up front.
async fn seed_offline_work_during_removal(
    store: &HiqliteAuthStore,
    node_id: &str,
    media_dir: &Path,
    user_id: i64,
) -> Result<()> {
    let dir = media_dir.join("late");
    std::fs::create_dir_all(&dir).context("mid-removal offline fixture directory")?;
    let library = store
        .create_library(&NewLibrary {
            name: format!("Membership Offline Late {node_id}"),
            kind: LibraryKind::Movies,
            paths: vec![dir.clone()],
            anime: false,
        })
        .await?;
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: format!("Membership Offline Late {node_id}"),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await?;
    let source_path = dir.join(format!("{LATE_PACKAGE}.mkv"));
    std::fs::write(&source_path, vec![0_u8; 4_096])
        .context("mid-removal offline fixture source")?;
    let metadata = std::fs::metadata(&source_path)?;
    let source_size = i64::try_from(metadata.len())?;
    let source_mtime = i64::try_from(metadata.modified()?.duration_since(UNIX_EPOCH)?.as_secs())?;
    let source = source_path.to_string_lossy().to_string();
    let file = store
        .upsert_file(
            item,
            &source,
            source_size,
            source_mtime,
            &ProbeResult::default(),
        )
        .await?;
    let package = NewOfflinePackage {
        id: format!("{LATE_PACKAGE}-{node_id}"),
        request_id: format!("{LATE_PACKAGE}-request-{node_id}"),
        user_id,
        file_id: file,
        node_id: node_id.to_owned(),
        source_path: source,
        source_size,
        source_mtime,
        effective_rate_control: "vbr".to_owned(),
        target_height: 720,
        output_width: Some(1280),
        output_height: Some(720),
        audio_index: None,
        audio_offset_ms: 0,
        subtitle_index: None,
        subtitle_language: None,
        subtitle_mode: "none".to_owned(),
        estimated_bytes: 700,
        reserved_bytes: 900,
        expires_at: unix_now()?.saturating_add(3_600),
    };
    if !matches!(
        store
            .create_offline_package(&package, 10, 100_000, 1_000_000)
            .await?,
        OfflineCreateOutcome::Created(_)
    ) {
        bail!("the download requested during the removal was not admitted");
    }
    Ok(())
}

async fn seed_topology_catalogue(store: &HiqliteAuthStore, rows: u64) -> Result<Vec<i64>> {
    if rows == 0 || rows > 256 {
        bail!("topology catalogue row count must be between 1 and 256");
    }
    let library = store
        .create_library(&NewLibrary {
            name: "Topology Read Pool".to_owned(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/cluster/topology-read-pool")],
            anime: false,
        })
        .await?;
    let mut item_ids = Vec::with_capacity(usize::try_from(rows)?);
    for ordinal in 0..rows {
        item_ids.push(
            store
                .insert_item(&NewItem {
                    library_id: library.id,
                    kind: ItemKind::Movie,
                    parent_id: None,
                    title: format!("{} {ordinal:04}", topology::TOPOLOGY_CATALOGUE_TITLE_PREFIX),
                    year: None,
                    season_number: None,
                    episode_number: None,
                })
                .await?,
        );
    }
    Ok(item_ids)
}

async fn run_topology_catalogue_reads(
    catalogue: CatalogueReader,
    store: &HiqliteAuthStore,
    item_ids: Vec<i64>,
    operations: u64,
    concurrency: u64,
) -> Result<Response> {
    if item_ids.is_empty() {
        bail!("topology catalogue read workload has no corpus");
    }
    if operations == 0 || operations > 16_384 {
        bail!("topology catalogue read operations must be between 1 and 16384");
    }
    if concurrency == 0 || concurrency > 256 || concurrency > operations {
        bail!("topology catalogue read concurrency must be inside 1..=min(256, operations)");
    }

    store.validation_reset_operation_counts();
    let item_count = u64::try_from(item_ids.len())?;
    let read_ids = (0..operations)
        .map(|ordinal| {
            let index = usize::try_from(ordinal % item_count)?;
            Ok(item_ids[index])
        })
        .collect::<Result<Vec<_>>>()?;
    let samples = futures_util::stream::iter(read_ids)
        .map(|item_id| {
            let catalogue = catalogue.clone();
            async move {
                let started = Instant::now();
                let result = catalogue.get_item(item_id).await;
                let elapsed = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
                let failed = result
                    .map(|item| item.is_none_or(|item| item.id != item_id))
                    .unwrap_or(true);
                (elapsed, failed)
            }
        })
        .buffer_unordered(usize::try_from(concurrency)?)
        .collect::<Vec<_>>()
        .await;
    let counts = store.validation_operation_counts();
    Ok(Response::TopologyCatalogueReads {
        raw_round_trip_us: samples.iter().map(|(elapsed, _)| *elapsed).collect(),
        errors: u64::try_from(samples.iter().filter(|(_, failed)| *failed).count())?,
        consistent_query_calls: counts.consistent_query_calls,
        non_consistent_query_calls: counts.non_consistent_query_calls,
    })
}

async fn exercise(store: &HiqliteAuthStore, ordinal: u64) -> Result<()> {
    store.ping().await?;
    if store.instance_id().await? != INSTANCE_ID {
        bail!("logical instance id drifted");
    }

    let suffix = ordinal.to_string();
    let setting = format!("proof.node.{suffix}");
    store.put_setting(&setting, "acknowledged").await?;
    if store.get_setting(&setting).await?.as_deref() != Some("acknowledged") {
        bail!("setting did not round-trip");
    }

    let username = format!("survivor-{suffix}");
    let initial_admin = ordinal.is_multiple_of(2);
    let user = store
        .create_user(&username, "hash-v1", initial_admin)
        .await
        .context("create proof user")?;
    if user.password_hash != "hash-v1" || user.is_admin != initial_admin {
        bail!("create user did not preserve password/admin fields");
    }
    if store
        .get_user(user.id)
        .await?
        .is_none_or(|row| row.password_hash != "hash-v1" || row.is_admin != initial_admin)
    {
        bail!("user id lookup did not preserve password/admin fields");
    }
    if store
        .get_user_by_username(&username.to_ascii_uppercase())
        .await?
        .map(|row| row.id)
        != Some(user.id)
    {
        bail!("case-insensitive username lookup failed");
    }
    let replacement = format!("hash-v2-{suffix}");
    let password_changed = store.set_password(user.id, &replacement).await?;
    let password_after = store.get_user(user.id).await?.map(|row| row.password_hash);
    if !password_changed || password_after.as_deref() != Some(replacement.as_str()) {
        bail!("password replacement failed: changed={password_changed}, value={password_after:?}");
    }
    if !store.set_admin(user.id, false).await?
        || store
            .get_user(user.id)
            .await?
            .is_none_or(|row| row.is_admin)
        || !store.set_admin(user.id, true).await?
    {
        bail!("admin mutation failed");
    }
    if store.count_users().await? < ordinal as i64
        || store.count_admins().await? < ordinal as i64
        || !store
            .list_users()
            .await?
            .iter()
            .any(|row| row.id == user.id)
    {
        bail!("user list/count contract failed");
    }

    let token = format!("survive-token-{suffix}");
    store
        .create_token(&token, user.id, Some("cluster-check"))
        .await?;
    if store.user_for_token(&token).await?.map(|row| row.id) != Some(user.id) {
        bail!("proof token lookup failed");
    }
    let temporary_token = format!("temporary-token-{suffix}");
    store.create_token(&temporary_token, user.id, None).await?;
    if !store.delete_token(&temporary_token).await?
        || store.user_for_token(&temporary_token).await?.is_some()
    {
        bail!("token deletion failed");
    }
    for token_index in 1..=2 {
        store
            .create_token(&format!("bulk-token-{suffix}-{token_index}"), user.id, None)
            .await?;
    }
    if store.delete_tokens_for_user(user.id).await? != 3 {
        bail!("bulk token revocation did not include every token");
    }
    store
        .create_token(&token, user.id, Some("cluster-check"))
        .await?;

    let disposable = store
        .create_user(&format!("disposable-{suffix}"), "hash", false)
        .await?;
    let disposable_token = format!("disposable-token-{suffix}");
    store
        .create_token(&disposable_token, disposable.id, None)
        .await?;
    if !store.delete_user(disposable.id).await?
        || store.get_user(disposable.id).await?.is_some()
        || store.user_for_token(&disposable_token).await?.is_some()
    {
        bail!("user deletion/cascade failed");
    }

    let key_hash = format!("survive-key-{suffix}");
    let key = store
        .create_api_key(
            &format!("node-{suffix}"),
            &key_hash,
            &["scan:trigger".to_owned()],
        )
        .await?;
    if !key.allows("scan:trigger")
        || store
            .api_key_for_hash(&key_hash)
            .await?
            .is_none_or(|row| row.id != key.id || !row.allows("scan:trigger"))
        || !store
            .list_api_keys()
            .await?
            .iter()
            .any(|row| row.id == key.id)
    {
        bail!("API key lookup/list failed");
    }
    store.touch_api_key(key.id).await?;
    if store
        .api_key_for_hash(&key_hash)
        .await?
        .is_none_or(|row| row.last_used_at.is_none())
        || !store.set_api_key_disabled(key.id, true).await?
        || store
            .api_key_for_hash(&key_hash)
            .await?
            .is_none_or(|row| !row.disabled || row.allows("scan:trigger"))
        || !store.set_api_key_disabled(key.id, false).await?
    {
        bail!("API key touch/disable contract failed");
    }
    let temporary_key = store
        .create_api_key(
            &format!("temporary-{suffix}"),
            &format!("temporary-key-{suffix}"),
            &[],
        )
        .await?;
    if !store.delete_api_key(temporary_key.id).await?
        || store
            .api_key_for_hash(&format!("temporary-key-{suffix}"))
            .await?
            .is_some()
    {
        bail!("API key deletion failed");
    }

    let library = store
        .create_library(&NewLibrary {
            name: format!("Cluster Movies {suffix}"),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from(format!("/cluster/media/{suffix}"))],
            anime: false,
        })
        .await?;
    let movie = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: format!("Replicated Catalog Proof {suffix}"),
            year: Some(2000 + ordinal as i32),
            season_number: None,
            episode_number: None,
        })
        .await?;
    store
        .apply_metadata(
            movie,
            &MetadataPatch {
                overview: Some(format!("replicated browse search voter {suffix}")),
                tmdb_id: Some(10_000 + ordinal as i64),
                genres: Some(vec!["Science Fiction".to_owned()]),
                enriched: true,
                ..MetadataPatch::default()
            },
        )
        .await?;
    let file = store
        .upsert_file(
            movie,
            &format!("/cluster/media/{suffix}/proof-{suffix}.mkv"),
            1_000 + ordinal as i64,
            1_700_000_000 + ordinal as i64,
            &ProbeResult {
                duration_ms: Some(120_000),
                container: Some("mkv".to_owned()),
                video_codec: Some("hevc".to_owned()),
                width: Some(3840),
                height: Some(2160),
                raw_json: Some(format!(r#"{{"voter":{ordinal}}}"#)),
                ..ProbeResult::default()
            },
        )
        .await?;
    let fingerprint = format!("root-fingerprint-{suffix}");
    if store
        .ensure_library_root_fingerprint(library.id, &fingerprint, true)
        .await?
        != RootFingerprintStatus::Established
    {
        bail!("new library root fingerprint was not established");
    }
    if !matches!(
        store
            .reconcile_library(library.id, "stale-root", &[file], 1)
            .await?,
        ReconcileOutcome::RefusedRoot { .. }
    ) || store.get_file(file).await?.is_none()
    {
        bail!("stale-root reconciliation committed a prune");
    }
    if store
        .reconcile_library(library.id, &fingerprint, &[file], 0)
        .await?
        != (ReconcileOutcome::RefusedPrune {
            requested: 1,
            limit: 0,
        })
        || store.get_file(file).await?.is_none()
    {
        bail!("over-budget reconciliation committed a prune");
    }
    let disposable_item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: format!("Disposable Search Trigger Proof {suffix}"),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await?;
    let disposable_file = store
        .upsert_file(
            disposable_item,
            &format!("/cluster/media/{suffix}/disposable-{suffix}.mkv"),
            42,
            1_700_000_100 + ordinal as i64,
            &ProbeResult::default(),
        )
        .await?;
    if !matches!(
        store
            .reconcile_library(library.id, &fingerprint, &[disposable_file], 1)
            .await?,
        ReconcileOutcome::Applied {
            deleted_files: 1,
            pruned_items: 1
        }
    ) || store.get_item(disposable_item).await?.is_some()
    {
        bail!("item deletion did not remove the file/item through the FTS trigger");
    }
    let state = store
        .put_progress(user.id, movie, 30_000, Some(10_000))
        .await?;
    if state.position_ms != 30_000 || state.duration_ms != Some(120_000) || state.watched {
        bail!("replicated watch progress did not prefer the probed duration");
    }

    // Trakt is the one durable credential plurx replays rather than verifies,
    // so what raft carries here must be ciphertext (CLUSTERING-PLAN.md §3.2).
    // The key is node-local and never replicated; this harness shares one
    // across the voters exactly as a real cluster distributes it out of band.
    let key = CredentialKey::generate();
    let sealed_refresh = key.seal_trakt(user.id, &format!("trakt-refresh-{suffix}"))?;
    let trakt = TraktAuth {
        user_id: user.id,
        access_token: key.seal_trakt(user.id, &format!("trakt-access-{suffix}"))?,
        refresh_token: sealed_refresh.clone(),
        expires_at: 4_000_000_000,
        trakt_username: Some(format!("trakt-{suffix}")),
        connected_at: 1_700_000_000 + ordinal as i64,
        last_sync_at: 0,
        last_activities: None,
    };
    store.put_trakt_auth(&trakt).await?;
    let trakt_sync_at = 1_700_000_100 + ordinal as i64;
    store
        .set_trakt_sync(user.id, trakt_sync_at, Some("{}"))
        .await?;
    // The compare-and-set operand is the replicated envelope, so the winner is
    // decided on bytes every voter already agrees on without holding the key.
    if !store
        .update_trakt_tokens(
            user.id,
            &sealed_refresh,
            &key.seal_trakt(user.id, &format!("trakt-access-new-{suffix}"))?,
            &key.seal_trakt(user.id, &format!("trakt-refresh-new-{suffix}"))?,
            4_000_000_001,
        )
        .await?
        || store
            .update_trakt_tokens(
                user.id,
                &sealed_refresh,
                &key.seal_trakt(user.id, &format!("trakt-access-loser-{suffix}"))?,
                &key.seal_trakt(user.id, &format!("trakt-refresh-loser-{suffix}"))?,
                4_000_000_002,
            )
            .await?
    {
        bail!("replicated Trakt token compare-and-set failed");
    }
    if store.get_trakt_auth(user.id).await?.is_none_or(|auth| {
        let replicated_cleartext = auth.access_token.as_stored().contains("trakt-access")
            || auth.refresh_token.as_stored().contains("trakt-refresh");
        let access = auth
            .reveal_access_token(&key)
            .ok()
            .is_none_or(|token| token.expose() != format!("trakt-access-new-{suffix}"));
        let refresh = auth
            .reveal_refresh_token(&key)
            .ok()
            .is_none_or(|token| token.expose() != format!("trakt-refresh-new-{suffix}"));
        replicated_cleartext
            || access
            || refresh
            || auth.expires_at != 4_000_000_001
            || auth.last_sync_at != trakt_sync_at
            || auth.last_activities.as_deref() != Some("{}")
    }) || !store
        .list_trakt_auth()
        .await?
        .iter()
        .any(|auth| auth.user_id == user.id)
        || !store
            .trakt_sync_candidates(user.id)
            .await?
            .iter()
            .any(|candidate| candidate.item_id == movie)
    {
        bail!("replicated Trakt link/candidate join failed");
    }
    if store
        .delete_trakt_auth_if_current(user.id, &sealed_refresh)
        .await?
        || store.get_trakt_auth(user.id).await?.is_none()
    {
        bail!("stale replicated Trakt unlink deleted a refreshed credential");
    }
    let unlink_user = store
        .create_user(&format!("trakt-unlink-{suffix}"), "hash", false)
        .await?;
    let mut unlink = trakt.clone();
    unlink.user_id = unlink_user.id;
    store.put_trakt_auth(&unlink).await?;
    store.delete_trakt_auth(unlink_user.id).await?;
    if store.get_trakt_auth(unlink_user.id).await?.is_some()
        || store.get_user(unlink_user.id).await?.is_none()
    {
        bail!("replicated Trakt unlink failed");
    }
    if !store.delete_user(unlink_user.id).await? {
        bail!("replicated Trakt unlink fixture user cleanup failed");
    }

    let outbox_id = store
        .enqueue_watched(&format!(r#"{{"item":{movie}}}"#))
        .await?;
    let mut outbox = store
        .due_watched(100)
        .await?
        .into_iter()
        .find(|entry| entry.id == outbox_id)
        .context("replicated outbox entry was not due")?;
    outbox.attempts = 1;
    outbox.status = "ok".to_owned();
    store.settle_watched(&outbox).await?;
    if store.watched_outbox_counts().await? != (0, ordinal as i64, 0) {
        bail!("replicated watched-outbox status counts drifted");
    }

    let node_id = format!("node-{suffix}");
    let recipe_hash = format!("recipe-{suffix}");
    if !store
        .claim_cache_entry(&recipe_hash, file, 1, &node_id, &format!("cache/{suffix}"))
        .await?
    {
        bail!("first replicated cache claim was not accepted");
    }
    if store
        .claim_cache_entry(
            &recipe_hash,
            file,
            1,
            &node_id,
            &format!("cache/moved-{suffix}"),
        )
        .await?
    {
        bail!("second replicated cache claim moved an existing owner");
    }
    store
        .complete_cache_entry(&recipe_hash, &node_id, 800 + ordinal as i64)
        .await?;
    store.touch_cache_entry(&recipe_hash, &node_id).await?;
    if store.cache_hit(&recipe_hash, &node_id).await?.is_none()
        || store.cache_hit(&recipe_hash, "other-node").await?.is_some()
        || !store
            .cache_by_age(&node_id, 100)
            .await?
            .iter()
            .any(|row| row.recipe_hash == recipe_hash)
        || !store
            .all_cache_rows(&node_id)
            .await?
            .iter()
            .any(|row| row.recipe_hash == recipe_hash)
    {
        bail!("replicated cache location lost node ownership");
    }
    let unpinned = format!("unpinned-recipe-{suffix}");
    if !store
        .claim_cache_entry(
            &unpinned,
            file,
            1,
            &node_id,
            &format!("cache/unpinned-{suffix}"),
        )
        .await?
    {
        bail!("replicated unpinned cache claim was not accepted");
    }
    store.complete_cache_entry(&unpinned, &node_id, 123).await?;
    let expected_cache_bytes = 923 + ordinal as i64;
    let cache_bytes = store.cache_bytes(&node_id).await?;
    if cache_bytes != expected_cache_bytes {
        bail!(
            "replicated cache read-after-write on voter {ordinal} expected \
             {expected_cache_bytes} bytes, found {cache_bytes}"
        );
    }
    let abandoned = format!("abandoned-recipe-{suffix}");
    if !store
        .claim_cache_entry(
            &abandoned,
            file,
            1,
            &node_id,
            &format!("cache/abandoned-{suffix}"),
        )
        .await?
    {
        bail!("replicated abandoned cache claim was not accepted");
    }
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let stale_cutoff = unix_now()?;
    if !store
        .stale_cache_claims(&node_id, stale_cutoff)
        .await?
        .iter()
        .any(|row| row.recipe_hash == abandoned)
    {
        bail!("replicated cache claim did not become stale before its heartbeat");
    }
    store.touch_cache_claim(&abandoned, &node_id).await?;
    let heartbeat_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if !store
            .stale_cache_claims(&node_id, stale_cutoff)
            .await?
            .iter()
            .any(|row| row.recipe_hash == abandoned)
        {
            break;
        }
        if Instant::now() >= heartbeat_deadline {
            bail!("replicated cache heartbeat did not refresh the stale cutoff");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !store
        .stale_cache_claims(&node_id, i64::MAX)
        .await?
        .iter()
        .any(|row| row.recipe_hash == abandoned)
    {
        bail!("replicated stale cache claim was not visible");
    }
    store
        .forget_cache_entry(&abandoned, &node_id, "local")
        .await?;
    if store.cache_hit(&abandoned, &node_id).await?.is_some() {
        bail!("replicated cache forget left a serveable row");
    }

    let outsider = store
        .create_user(&format!("offline-outsider-{suffix}"), "hash", false)
        .await?;
    let offline = NewOfflinePackage {
        id: format!("offline-{suffix}"),
        request_id: format!("offline-request-{suffix}"),
        user_id: user.id,
        file_id: file,
        node_id: node_id.clone(),
        source_path: format!("/cluster/media/{suffix}/proof-{suffix}.mkv"),
        source_size: 1_000 + ordinal as i64,
        source_mtime: 1_700_000_000 + ordinal as i64,
        effective_rate_control: "vbr".to_owned(),
        target_height: 720,
        output_width: Some(1280),
        output_height: Some(720),
        audio_index: Some(1),
        audio_offset_ms: 0,
        subtitle_index: None,
        subtitle_language: None,
        subtitle_mode: "none".to_owned(),
        estimated_bytes: 700,
        reserved_bytes: 900,
        expires_at: 4_000_000_000,
    };
    if !matches!(
        store
            .create_offline_package(&offline, 10, 100_000, 1_000_000)
            .await?,
        OfflineCreateOutcome::Created(_)
    ) {
        bail!("replicated offline admission was not created");
    }
    if !matches!(
        store
            .create_offline_package(&offline, 10, 100_000, 1_000_000)
            .await?,
        OfflineCreateOutcome::Existing(_)
    ) {
        bail!("replicated offline admission was not idempotent");
    }
    let mut conflict = offline.clone();
    conflict.target_height = 1080;
    if store
        .create_offline_package(&conflict, 10, 100_000, 1_000_000)
        .await?
        != OfflineCreateOutcome::RequestConflict
    {
        bail!("replicated offline request conflict was not detected");
    }
    if store
        .offline_package_for_user(&offline.id, user.id)
        .await?
        .is_none_or(|package| {
            package.target_height != offline.target_height
                || package.estimated_bytes != offline.estimated_bytes
                || package.reserved_bytes != offline.reserved_bytes
                || package.expires_at != offline.expires_at
        })
    {
        bail!("replicated request conflict mutated the accepted package");
    }
    let mut rejected = offline.clone();
    rejected.id = format!("offline-rejected-{suffix}");
    rejected.request_id = format!("offline-rejected-request-{suffix}");
    if !matches!(
        store
            .create_offline_package(&rejected, 0, 100_000, 1_000_000)
            .await?,
        OfflineCreateOutcome::RowLimit { .. }
    ) || !matches!(
        store
            .create_offline_package(&rejected, 10, 1, 1_000_000)
            .await?,
        OfflineCreateOutcome::ByteLimit { .. }
    ) || !matches!(
        store
            .create_offline_package(&rejected, 10, 100_000, 1)
            .await?,
        OfflineCreateOutcome::GlobalByteLimit { .. }
    ) {
        bail!("replicated offline quota refusal contract failed");
    }
    if store
        .offline_package_for_user(&rejected.id, user.id)
        .await?
        .is_some()
    {
        bail!("replicated quota refusal left a package row behind");
    }
    let wrong_node = format!("wrong-node-{suffix}");
    let wrong_stats = store.offline_package_stats(&wrong_node, 1).await?;
    if store
        .claim_next_offline_package(&wrong_node)
        .await?
        .is_some()
        || store
            .reset_interrupted_offline_packages(&wrong_node)
            .await?
            != 0
        || !store
            .offline_activity_packages(&wrong_node, 1, 0, 100)
            .await?
            .is_empty()
        || wrong_stats.queued != 0
        || wrong_stats.preparing != 0
        || wrong_stats.ready != 0
        || wrong_stats.failed != 0
        || store.cache_bytes(&wrong_node).await? != 0
        || !store.cache_by_age(&wrong_node, 100).await?.is_empty()
        || !store.all_cache_rows(&wrong_node).await?.is_empty()
    {
        bail!("replicated node ownership predicates accepted another node's state");
    }
    if store
        .offline_package_for_user(&offline.id, outsider.id)
        .await?
        .is_some()
        || store
            .renew_offline_package_for_user(&offline.id, outsider.id, 4_000_000_003)
            .await?
            .is_some()
        || store
            .delete_offline_package(&offline.id, outsider.id)
            .await?
    {
        bail!("replicated offline owner predicates exposed another user's package");
    }
    if store
        .claim_next_offline_package(&node_id)
        .await?
        .is_none_or(|package| package.id != offline.id)
        || !store
            .set_offline_package_recipe(&offline.id, &recipe_hash)
            .await?
        || !store
            .update_offline_progress(&offline.id, &node_id, "video", 500)
            .await?
    {
        bail!("replicated offline preparation state machine failed");
    }
    // The producer fence, asserted while the package is genuinely
    // mid-preparation: its writes belong to whichever node owns it *now*.
    // Removal re-homes a package while the departing node's encoder may still
    // be running, and an unfenced yield would knock the survivor's claimed
    // work back to `queued` while its progress flapped.
    if store
        .requeue_offline_package(&offline.id, &wrong_node)
        .await?
        || store
            .update_offline_progress(&offline.id, &wrong_node, "stolen", 999)
            .await?
        || store
            .offline_package_for_user(&offline.id, user.id)
            .await?
            .is_none_or(|package| package.phase != "video" || package.progress_millis != 500)
    {
        bail!("replicated offline producer writes were not fenced to the owning node");
    }
    if !store
        .mark_offline_package_ready(&offline.id, &node_id, &recipe_hash, 800, 120_000)
        .await?
    {
        bail!("replicated offline preparation state machine failed");
    }
    if store
        .mark_offline_package_ready(&offline.id, &node_id, &recipe_hash, 801, 120_001)
        .await?
        || store.requeue_offline_package(&offline.id, &node_id).await?
        || store
            .set_offline_package_recipe(&offline.id, "wrong-recipe")
            .await?
        || store
            .update_offline_progress(&offline.id, &node_id, "wrong-state", 999)
            .await?
        || store
            .fail_offline_package(&offline.id, &node_id, "wrong-state", "wrong", "wrong")
            .await?
    {
        bail!("replicated offline terminal-state guards accepted a late mutation");
    }
    if store
        .offline_package_for_user(&offline.id, user.id)
        .await?
        .is_none_or(|package| {
            package.state != "ready"
                || package.recipe_hash.as_deref() != Some(recipe_hash.as_str())
                || package.actual_bytes != Some(800)
                || package.duration_ms != Some(120_000)
        })
    {
        bail!("replicated rejected state transitions mutated the ready package");
    }
    if !matches!(
        store
            .put_offline_lease(
                &offline.id,
                user.id,
                &format!("offline-token-{suffix}"),
                4_000_000_000,
            )
            .await?,
        OfflineLeaseOutcome::Created(_)
    ) || store.offline_package_stats(&node_id, 1).await?.ready != 1
        || store.cache_bytes(&node_id).await? != 123
    {
        bail!("replicated offline lease/pinned-cache accounting failed");
    }
    let wrong_lease = format!("offline-wrong-user-token-{suffix}");
    if store
        .put_offline_lease(&offline.id, outsider.id, &wrong_lease, 4_000_000_005)
        .await?
        != OfflineLeaseOutcome::PackageNotReady
        || store
            .offline_package_for_lease(&wrong_lease, 1, 4_000_000_005)
            .await?
            .is_some()
    {
        bail!("replicated lease ownership predicates accepted another user");
    }
    if !matches!(
        store
            .put_offline_lease(
                &offline.id,
                user.id,
                &format!("offline-token-{suffix}"),
                4_000_000_001,
            )
            .await?,
        OfflineLeaseOutcome::Renewed(_)
    ) || store
        .put_offline_lease(
            &offline.id,
            user.id,
            &format!("offline-token-conflict-{suffix}"),
            4_000_000_001,
        )
        .await?
        != OfflineLeaseOutcome::TokenConflict
        || store
            .renew_offline_package_for_user(&offline.id, user.id, 4_000_000_002)
            .await?
            .is_none()
        || !store
            .offline_activity_packages(&node_id, 1, 0, 100)
            .await?
            .iter()
            .any(|row| row.package.id == offline.id && row.lease_active)
    {
        bail!("replicated offline renewal/activity contract failed");
    }

    let mut work = offline.clone();
    work.id = format!("offline-work-{suffix}");
    work.request_id = format!("offline-work-request-{suffix}");
    store
        .create_offline_package(&work, 10, 100_000, 1_000_000)
        .await?;
    if store
        .claim_next_offline_package(&node_id)
        .await?
        .is_none_or(|package| package.id != work.id)
        || store.reset_interrupted_offline_packages(&node_id).await? != 1
        || store
            .claim_next_offline_package(&node_id)
            .await?
            .is_none_or(|package| package.id != work.id)
        || !store.requeue_offline_package(&work.id, &node_id).await?
        || store
            .claim_next_offline_package(&node_id)
            .await?
            .is_none_or(|package| package.id != work.id)
        || !store
            .fail_offline_package(&work.id, &node_id, "video", "proof", "expected")
            .await?
        || !store.delete_offline_package(&work.id, user.id).await?
    {
        bail!("replicated offline recovery/failure/delete contract failed");
    }

    let mut expired = offline.clone();
    expired.id = format!("offline-expired-{suffix}");
    expired.request_id = format!("offline-expired-request-{suffix}");
    expired.expires_at = 1;
    store
        .create_offline_package(&expired, 10, 100_000, 1_000_000)
        .await?;
    if store.expire_offline_packages(2).await? == 0 {
        bail!("replicated offline expiry did not remove an expired package");
    }
    if !store.delete_user(outsider.id).await? {
        bail!("replicated offline outsider fixture cleanup failed");
    }
    Ok(())
}

async fn catalog_view(store: &HiqliteAuthStore) -> Result<CatalogView> {
    let mut search = store
        .search_items("replicated browse", 100)
        .await?
        .into_iter()
        .map(|row| row.item.id)
        .collect::<Vec<_>>();
    search.sort_unstable();
    Ok(CatalogView {
        authoritative_digest: store.validation_local_catalog_truth_digest().await?,
        full_digest: store.local_dump_digest().await?,
        search,
    })
}

async fn verify_proof(store: &HiqliteAuthStore) -> Result<()> {
    for ordinal in 1..=3 {
        let suffix = ordinal.to_string();
        if store
            .get_setting(&format!("proof.node.{suffix}"))
            .await?
            .as_deref()
            != Some("acknowledged")
        {
            bail!("lost acknowledged setting from node {ordinal}");
        }
        let user = store
            .get_user_by_username(&format!("survivor-{suffix}"))
            .await?
            .with_context(|| format!("lost acknowledged user from node {ordinal}"))?;
        if store
            .user_for_token(&format!("survive-token-{suffix}"))
            .await?
            .map(|row| row.id)
            != Some(user.id)
        {
            bail!("lost acknowledged token from node {ordinal}");
        }
        if store
            .api_key_for_hash(&format!("survive-key-{suffix}"))
            .await?
            .is_none()
        {
            bail!("lost acknowledged API key from node {ordinal}");
        }
        let library = store
            .list_libraries()
            .await?
            .into_iter()
            .find(|library| library.name == format!("Cluster Movies {suffix}"))
            .with_context(|| format!("lost acknowledged library from node {ordinal}"))?;
        let page = store
            .list_top_items(library.id, ItemSort::Title, 0, 10)
            .await?;
        if page.items.len() != 1
            || store.files_for_item(page.items[0].id).await?.len() != 1
            || store
                .watch_state(user.id, page.items[0].id)
                .await?
                .is_none()
        {
            bail!("lost acknowledged media/watch state from node {ordinal}");
        }
        if store.get_trakt_auth(user.id).await?.is_none_or(|auth| {
            // `exercise` sealed these under a key it did not persist, so this
            // pass proves the durable form and not the cleartext: what came
            // back through raft is still an envelope, and still not the bearer
            // token. A voter disk alone is not enough to use the account.
            !auth.is_wrapped()
                || auth.access_token.as_stored().contains("trakt-access")
                || auth.refresh_token.as_stored().contains("trakt-refresh")
                || auth.expires_at != 4_000_000_001
                || auth.last_sync_at != 1_700_000_100 + ordinal as i64
                || auth.last_activities.as_deref() != Some("{}")
        }) || store
            .offline_package_for_user(&format!("offline-{suffix}"), user.id)
            .await?
            .is_none_or(|package| package.state != "ready")
            || store
                .offline_package_for_lease(&format!("offline-token-{suffix}"), 1, 4_000_000_000)
                .await?
                .is_none()
            || store
                .cache_hit(&format!("recipe-{suffix}"), &format!("node-{suffix}"))
                .await?
                .is_none()
            || store
                .cache_hit(
                    &format!("unpinned-recipe-{suffix}"),
                    &format!("node-{suffix}"),
                )
                .await?
                .is_none()
            || store.cache_bytes(&format!("node-{suffix}")).await? != 123
        {
            bail!("lost acknowledged Trakt/cache/offline state from node {ordinal}");
        }
    }
    let counts = store.watched_outbox_counts().await?;
    if counts != (0, 3, 0) {
        bail!("lost acknowledged watched-outbox rows after voter loss: {counts:?}");
    }
    if catalog_view(store).await?.search.len() != 3 {
        bail!("lost local FTS search rows after voter loss");
    }
    Ok(())
}

pub async fn write_response(response: &Response) -> Result<()> {
    let mut output = tokio::io::stdout();
    let mut bytes = serde_json::to_vec(response)?;
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    output.flush().await?;
    Ok(())
}

/// Run the joining-voter compatibility preflight in its own process, exiting
/// 42 when the cluster refuses this candidate.
pub async fn preflight_voter(preflight: Preflight) -> Result<()> {
    install_crypto_provider();
    let remote = Client::remote(
        preflight.addresses,
        true,
        true,
        API_SECRET.to_owned(),
        false,
        None,
    )
    .await?;
    match HiqliteAuthStore::preflight_voter(&remote, preflight.compatibility).await {
        Ok(()) => {
            println!("compatible");
            Ok(())
        }
        Err(error) => {
            println!("{error}");
            std::process::exit(42);
        }
    }
}

/// Start a candidate voter one schema version behind and return the refusal it
/// printed. Any other exit status is itself a failure.
pub async fn run_incompatible_preflight(executable: &Path, specs: &[NodeSpec]) -> Result<String> {
    run_preflight(
        executable,
        specs,
        ClusterCompatibility {
            schema_version: AUTH_SCHEMA_VERSION - 1,
            ..ClusterCompatibility::CURRENT
        },
        Some(42),
    )
    .await
}

/// Start a candidate voter that implements only the pre-P6 protocol. Against a
/// cluster that has not activated protocol 5 this must succeed; against an
/// activated one it must refuse. The caller says which it expects.
pub async fn run_previous_release_preflight(
    executable: &Path,
    specs: &[NodeSpec],
    expect_refusal: bool,
) -> Result<String> {
    run_preflight(
        executable,
        specs,
        ClusterCompatibility {
            schema_version: AUTH_SCHEMA_VERSION,
            protocol_min: AUTH_PROTOCOL_VERSION,
            protocol_max: AUTH_PROTOCOL_VERSION,
        },
        expect_refusal.then_some(42),
    )
    .await
}

/// Run one candidate voter's compatibility preflight in its own process and
/// require the exit status the caller expects (`None` meaning success).
async fn run_preflight(
    executable: &Path,
    specs: &[NodeSpec],
    compatibility: ClusterCompatibility,
    expected_exit: Option<i32>,
) -> Result<String> {
    let input = Preflight {
        addresses: specs.iter().map(|node| node.api.clone()).collect(),
        compatibility,
    };
    let output = tokio::time::timeout(
        REQUEST_TIMEOUT,
        Command::new(executable)
            .arg("preflight")
            .arg(serde_json::to_string(&input)?)
            .output(),
    )
    .await
    .context("candidate voter preflight timed out")??;
    let expected = expected_exit.unwrap_or(0);
    if output.status.code() != Some(expected) {
        bail!(
            "candidate voter exited {:?}, expected {expected} for schema {} protocol {}..={}: {}",
            output.status.code(),
            compatibility.schema_version,
            compatibility.protocol_min,
            compatibility.protocol_max,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// Build the hiqlite configuration for one voter and create its data dir.
pub fn node_config(launch: &NodeLaunch) -> Result<NodeConfig> {
    let data_dir = launch.root.join(format!("node-{}", launch.node_id));
    std::fs::create_dir_all(&data_dir)?;
    Ok(NodeConfig {
        node_id: launch.node_id,
        nodes: launch
            .nodes
            .iter()
            .map(|node| Node {
                id: node.id,
                addr_raft: node.raft.clone(),
                addr_api: node.api.clone(),
            })
            .collect(),
        listen_addr_api: Cow::Owned(launch.listen_addr.clone()),
        listen_addr_raft: Cow::Owned(launch.listen_addr.clone()),
        data_dir: Cow::Owned(data_dir.to_string_lossy().into_owned()),
        filename_db: Cow::Borrowed("auth.db"),
        secret_raft: RAFT_SECRET.to_owned(),
        secret_api: API_SECRET.to_owned(),
        tls_raft: Some(ServerTlsConfig::TlsAutoCertificates),
        tls_api: Some(ServerTlsConfig::TlsAutoCertificates),
        // The same hint, for the same reason, as the daemon sets from the role
        // its join token admitted it under: Hiqlite reads it once, to decide
        // whether this process asks the leader to promote it during startup
        // reconciliation. It is what makes this a real learner process rather
        // than a voter the harness merely calls one.
        learner_only: launch.role.is_learner(),
        // Raft, WAL, and read-pool settings come from the daemon's own builder
        // rather than a second copy here, so a harness run cannot measure a
        // configuration production never runs.
        ..production_hiqlite_defaults_with_read_pool(launch.read_pool_size)
    })
}

/// A reserved set of ports whose listeners stay alive so no other process
/// can claim them before the intended voter binds.
///
/// Drop the reservation to release the ports. Pass it to
/// [`ClusterProcesses::start`] so the cluster holds the ports until every
/// voter has announced readiness, or to any call site that needs the
/// guarantee that these ports will not be reallocated while the caller
/// decides.
pub struct PortReservation {
    /// Held open so nothing else claims the port between allocation and bind.
    #[allow(dead_code)]
    listeners: Vec<TcpListener>,
    /// The voter specs describing the reserved ports.
    pub specs: Vec<NodeSpec>,
}

impl PortReservation {
    /// An empty reservation that holds no ports. Useful for tests that
    /// create a cluster with no voters.
    pub fn empty() -> Self {
        Self {
            listeners: Vec::new(),
            specs: Vec::new(),
        }
    }
    /// The [`NodeSpec`] entries allocated for each voter.
    pub fn specs(&self) -> &[NodeSpec] {
        &self.specs
    }

    /// Consume the reservation and return only the specs, releasing the ports.
    pub fn into_specs(self) -> Vec<NodeSpec> {
        self.specs
    }

    /// Recreate an allocation after releasing its reservations.
    ///
    /// This deliberately provides no allocation guarantee and exists only for
    /// the deterministic collision regression that takes one of these ports
    /// before the voter binds it.
    #[doc(hidden)]
    pub fn unreserved(specs: Vec<NodeSpec>) -> Self {
        Self {
            listeners: Vec::new(),
            specs,
        }
    }

    /// Returns the listeners, consuming the reservation.
    pub fn into_inner(self) -> (Vec<TcpListener>, Vec<NodeSpec>) {
        (self.listeners, self.specs)
    }
}

/// Reserve a raft and an API port for each of `count` voters, holding every
/// listener open so another process cannot claim the port between allocation
/// and bind.
///
/// Each port is bound to [`LISTEN_ADDR`] on a kernel-assigned port, the port
/// number is recorded in a [`NodeSpec`], and the listener is kept alive until
/// the [`PortReservation`] is consumed. Pass the reservation to
/// [`ClusterProcesses::start`] so the ports stay reserved until every voter
/// has announced readiness.
pub fn allocate_nodes(count: u64) -> Result<PortReservation> {
    let mut listeners = Vec::with_capacity(count as usize * 2);
    for _ in 0..count * 2 {
        listeners.push(TcpListener::bind((LISTEN_ADDR, 0)).context("reserve a harness port")?);
    }
    let mut ports = listeners
        .iter()
        .map(|listener| Ok(listener.local_addr()?.port()))
        .collect::<Result<Vec<_>>>()?
        .into_iter();
    let specs = (1..=count)
        .map(|id| {
            Ok(NodeSpec {
                id,
                raft: format!("{LISTEN_ADDR}:{}", ports.next().context("raft port")?),
                api: format!("{LISTEN_ADDR}:{}", ports.next().context("api port")?),
            })
        })
        .collect::<Result<_>>()?;
    Ok(PortReservation { listeners, specs })
}

/// Observe one free port.
///
/// The listener is released as the expression ends, so this reports a port
/// that *was* free rather than one this process holds. Nothing that starts a
/// voter may treat the result as a reservation; see [`allocate_nodes`].
///
/// # Correct use
///
/// `free_port` is for observation, not allocation. Use it to find a port for
/// a squatter in a collision test, or to assert that the OS assigned a
/// non-zero port. Never use it to choose a port a voter will later bind:
/// that is what [`allocate_nodes`] / [`PortReservation`] are for.
pub fn free_port() -> Result<u16> {
    Ok(TcpListener::bind((LISTEN_ADDR, 0))?.local_addr()?.port())
}

/// Is this error a port taken between allocation and bind, rather than
/// anything the cluster contract asserts?
///
/// A port is only ever observed free and then released, so a voter is always
/// started on a port another process may have claimed in the meantime. That is
/// an environment fact, not a durable-state fault. Classifying it here — once,
/// by name — is what keeps "the port moved" from being read as "the store is
/// un-migrated".
pub fn is_port_collision(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}");
    text.contains(PORT_COLLISION) || text.contains("Address already in use")
}

/// The addresses this voter's hiqlite node will bind.
///
/// hiqlite builds each listener from `listen_addr_*` and the port half of this
/// node's own entry (`hiqlite-0.14.0/src/start.rs:318`), so these are the two
/// addresses a collision can be a collision *with*.
pub fn voter_listen_addrs(launch: &NodeLaunch) -> Result<Vec<String>> {
    let node = launch
        .nodes
        .iter()
        .find(|node| node.id == launch.node_id)
        .with_context(|| format!("voter {} has no spec of its own", launch.node_id))?;
    [&node.raft, &node.api]
        .into_iter()
        .map(|address| {
            let (_, port) = address
                .rsplit_once(':')
                .with_context(|| format!("voter address {address} has no port"))?;
            Ok(format!("{}:{port}", launch.listen_addr))
        })
        .collect()
}

/// Prove both of a voter's listeners accept before it announces readiness.
///
/// `hiqlite::start_node` now pre-binds both sockets before returning, which is
/// the identity proof: another process cannot own either address after that
/// successful return. These bounded connects are only the readiness half,
/// proving both spawned servers are accepting before the voter announces
/// itself to the controller.
async fn prove_listeners_bound(addresses: &[String]) -> Result<()> {
    for address in addresses {
        let connection_address = address
            .strip_prefix("0.0.0.0:")
            .map(|port| format!("127.0.0.1:{port}"));
        let connection_address = connection_address.as_deref().unwrap_or(address);
        let deadline = TokioInstant::now() + LISTENER_PROOF_TIMEOUT;
        loop {
            match tokio::net::TcpStream::connect(connection_address).await {
                Ok(_) => break,
                Err(error) if TokioInstant::now() >= deadline => {
                    return Err(anyhow!(error)).with_context(|| {
                        format!("voter listener {address} never accepted a connection")
                    });
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
    }
    Ok(())
}

pub fn unix_now() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?)
}

pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use plurx_core::cluster::migration::HIQLITE_WAL_SIZE_BYTES;

    use super::*;

    #[test]
    fn inspect_wal_arguments_are_explicit_and_order_independent() {
        let parsed = parse_inspect_wal_args(&[
            "--output".to_owned(),
            "report.json".to_owned(),
            "--hiqlite-dir".to_owned(),
            "forensic/hiqlite".to_owned(),
        ])
        .expect("valid inspect-wal arguments");
        assert_eq!(parsed.output, PathBuf::from("report.json"));
        assert_eq!(parsed.hiqlite_dir, PathBuf::from("forensic/hiqlite"));
        assert!(parse_inspect_wal_args(&["--output".to_owned()]).is_err());
        assert!(parse_inspect_wal_args(&[
            "--output".to_owned(),
            "a".to_owned(),
            "--output".to_owned(),
            "b".to_owned(),
        ])
        .is_err());
    }

    #[test]
    fn state_machine_inspection_includes_a_committed_sqlite_wal_sidecar() {
        let root = tempfile::tempdir().expect("state-machine fixture");
        let path = root.path().join("plurx.db");
        let connection = Connection::open(&path).expect("open fixture database");
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL mode");
        connection
            .pragma_update(None, "wal_autocheckpoint", 0)
            .expect("retain committed WAL pages");
        connection
            .execute(
                "CREATE TABLE _metadata (key TEXT PRIMARY KEY, data BLOB NOT NULL)",
                [],
            )
            .expect("metadata table");
        let expected = InspectedLogId {
            term: 11,
            node_id: 4,
            index: 42,
        };
        let mut encoded = vec![1_u8];
        encoded.extend_from_slice(&expected.term.to_le_bytes());
        encoded.extend_from_slice(&expected.node_id.to_le_bytes());
        encoded.extend_from_slice(&expected.index.to_le_bytes());
        connection
            .execute(
                "INSERT INTO _metadata (key, data) VALUES ('meta', ?1)",
                rusqlite::params![encoded],
            )
            .expect("committed metadata in WAL");
        assert!(sqlite_sidecar_path(&path, "-wal").is_file());

        let (present, wal_present, applied) =
            read_local_state_machine_boundary(&path).expect("inspect source through private copy");
        assert!(present);
        assert!(wal_present);
        assert_eq!(applied, Some(expected));
    }

    fn test_media_child(admission_id: u64) -> MediaChild {
        let mut command = Command::new("sh");
        command
            .args(["-c", "cat >/dev/null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().expect("spawn test media child");
        let input = child.stdin.take().expect("retain test child stdin");
        MediaChild {
            child,
            _input: input,
            admitted_generation: 7,
            admission_id,
        }
    }

    #[tokio::test]
    async fn delayed_media_teardown_cannot_take_or_hide_its_replacement() {
        let media = Arc::new(tokio::sync::Mutex::new(Some(test_media_child(1))));

        let delayed_old = take_media_child_if_id(&media, 1)
            .await
            .expect("old admission owns its child");
        *media.lock().await = Some(test_media_child(2));

        assert!(take_media_child_if_id(&media, 1).await.is_none());
        reap_media_child(delayed_old).await;

        let mut slot = media.lock().await;
        let replacement = slot.as_mut().expect("replacement remains registered");
        assert_eq!(replacement.admission_id, 2);
        assert!(matches!(replacement.child.try_wait(), Ok(None)));
        drop(slot);

        kill_media_child_if_id(&media, 2).await;
        assert!(media.lock().await.is_none());
    }

    #[test]
    fn singleton_retry_requires_the_explicit_trial_marker() {
        assert!(singleton_trial_was_unstable(
            b"startup\nCLUSTER_SINGLETON_UNSTABLE stage=after-resume\n"
        ));
        assert!(!singleton_trial_was_unstable(
            b"Error: singleton proof changed leader/term during its measured interval\n"
        ));
        assert!(!singleton_trial_was_unstable(
            b"diagnostic mentioned CLUSTER_SINGLETON_UNSTABLE but was not a verdict\n"
        ));
    }

    #[test]
    fn membership_harness_retries_only_typed_leader_changes() {
        assert!(matches!(
            membership_error_response(MembershipError::LeaderChanged("routing".to_owned())),
            Response::MembershipLeaderChange { message } if message == "routing"
        ));
        assert!(matches!(
            membership_error_response(MembershipError::Internal(
                "LeaderChange text from an unrelated semantic error".to_owned()
            )),
            Response::MembershipError { code, .. } if code == "membership_internal"
        ));
    }

    #[test]
    fn read_only_repair_repeat_retries_only_stale_or_rerouted_observations() {
        assert!(
            repeatable_artwork_observation_succeeded(Response::Flag { value: true })
                .expect("a successful read-only repeat is classified")
        );
        assert!(
            !repeatable_artwork_observation_succeeded(Response::Flag { value: false })
                .expect("a stale read-only repeat is classified")
        );
        assert!(
            !repeatable_artwork_observation_succeeded(Response::MembershipLeaderChange {
                message: "election".to_owned(),
            })
            .expect("a rerouted read-only repeat is classified")
        );
        assert!(repeatable_artwork_observation_succeeded(Response::Ok).is_err());
    }

    #[test]
    fn post_cas_repair_proof_distinguishes_quorum_age_from_topology_change() {
        let metrics = |leader, current_term, quorum_acknowledged| Response::Metrics {
            leader,
            current_term,
            voters: vec![1, 2, 3],
            members: vec![1, 2, 3],
            applied_index: Some(41),
            quorum_acknowledged,
        };

        require_artwork_repair_cas_topology(metrics(Some(2), 9, true), 2, 9)
            .expect("fresh matching topology is valid");
        require_artwork_repair_cas_topology(metrics(Some(2), 9, false), 2, 9)
            .expect("aged quorum freshness does not rewrite committed topology");
        assert!(require_artwork_repair_cas_topology(metrics(Some(3), 9, true), 2, 9).is_err());
        assert!(require_artwork_repair_cas_topology(metrics(Some(2), 10, true), 2, 9).is_err());
        assert!(require_artwork_repair_cas_topology(metrics(None, 9, false), 2, 9).is_err());
    }

    #[test]
    fn read_only_repair_evidence_preserves_forward_to_leader_as_typed() {
        let source = include_str!("lib.rs");
        let production = source
            .rsplit_once("\n#[cfg(test)]\nmod tests {")
            .expect("test module boundary")
            .0;
        let handler = production
            .rsplit_once("Request::ReadArtworkRepairFence { item_id } => {")
            .expect("repair evidence request handler")
            .1
            .split_once("Request::ClaimArtworkRepair")
            .expect("next request handler")
            .0;
        assert!(handler.contains("error.is_forward_to_leader().is_some()"));
        assert!(handler.contains("Response::MembershipLeaderChange"));
    }

    #[test]
    fn mutating_artwork_claim_helper_has_exactly_one_dispatch() {
        let source = include_str!("lib.rs");
        let helper = source
            .split_once("async fn claim_artwork_repair_fence_once")
            .expect("single-dispatch claim helper")
            .1
            .split_once("async fn run_leader_self_leave_case")
            .expect("helper boundary")
            .0;
        assert_eq!(helper.matches(".request(").count(), 1);
        assert!(helper.contains("Response::MembershipLeaderChange"));
        assert!(helper.contains("was ambiguous and was not retried"));
    }

    #[test]
    fn mutating_artwork_claim_retries_only_a_confirmed_noop() {
        let fence = ArtworkRepairFence {
            item_id: 17,
            owner_node_id: "node-2".to_owned(),
            leader_term: 9,
            generation: 3,
        };
        assert_eq!(
            classify_artwork_repair_claim_response(Response::ArtworkRepairFence {
                fence: Some(fence.clone()),
            })
            .expect("a committed repair claim is classified"),
            Some(fence)
        );
        assert!(
            classify_artwork_repair_claim_response(Response::ArtworkRepairFence { fence: None })
                .expect("an explicit no-op repair claim is classified")
                .is_none()
        );
        assert!(
            classify_artwork_repair_claim_response(Response::MembershipLeaderChange {
                message: "acknowledgement lost after submission".to_owned(),
            })
            .is_err()
        );
        assert!(classify_artwork_repair_claim_response(Response::Ok).is_err());
    }

    #[test]
    fn stable_term_rerepair_reestablishes_quorum_before_the_cas() {
        let source = include_str!("lib.rs");
        let production = source
            .rsplit_once("\n#[cfg(test)]\nmod tests {")
            .expect("test module boundary")
            .0;
        let proof = production
            .split_once("let stable_claim_deadline")
            .expect("stable-term repair proof")
            .1
            .split_once("let same_term_after")
            .expect("stable-term repair proof boundary")
            .0;
        assert!(proof.contains("CONVERGENCE_TIMEOUT"));
        assert!(proof.contains("quorum_acknowledged: true"));
        assert!(proof.contains("claim_artwork_repair_fence_once"));
        assert!(!proof.contains("Request::ClaimArtworkRepairFence"));
    }

    #[test]
    fn compaction_plan_stops_writing_at_the_exact_openraft_trigger() {
        assert_eq!(
            snapshot_trigger_plan(None, 9_998).expect("plan before initial trigger"),
            1
        );
        assert_eq!(
            snapshot_trigger_plan(None, 9_999).expect("plan initial trigger"),
            0
        );
        assert_eq!(
            snapshot_trigger_plan(Some(0), 9_999).expect("plan before index-zero trigger"),
            1
        );
        assert_eq!(
            snapshot_trigger_plan(Some(0), 10_000).expect("plan index-zero trigger"),
            0
        );
        assert_eq!(
            snapshot_trigger_plan(Some(1_000), 1_512).expect("plan partial tail"),
            9_488
        );
        assert_eq!(
            snapshot_trigger_plan(Some(1_000), 11_609).expect("plan in-flight snapshot"),
            0
        );
        assert!(snapshot_trigger_plan(Some(1_000), 999).is_err());
        assert!(snapshot_trigger_plan(Some(u64::MAX), u64::MAX).is_err());
    }

    #[test]
    fn compaction_request_outlives_snapshot_publication_and_purge_bounds() {
        let request = Request::ForceCompaction {
            phase: "response-timeout-contract".to_owned(),
        };
        assert!(request.response_timeout() > Duration::from_secs(60));
        assert_eq!(Request::Metrics.response_timeout(), REQUEST_TIMEOUT);
        assert_eq!(
            Request::WriteWithoutQuorum.response_timeout(),
            WRITE_WITHOUT_QUORUM_RESPONSE_TIMEOUT
        );
    }

    fn test_launch(root: &Path, read_pool_size: usize) -> NodeLaunch {
        NodeLaunch {
            node_id: 1,
            root: root.to_path_buf(),
            nodes: vec![NodeSpec {
                id: 1,
                raft: "127.0.0.1:19001".to_owned(),
                api: "127.0.0.1:19002".to_owned(),
            }],
            listen_addr: default_listen_addr(),
            read_pool_size,
            instrument_store_operations: true,
            emulate_old_watermark_handler: false,
            emulate_p3a_watermark_handler: false,
            role: ClusterRole::Voter,
            emulate_pre_learner_heartbeat: false,
        }
    }

    #[test]
    fn voter_config_uses_the_production_wal_size() {
        let root = tempfile::tempdir().expect("config test root");
        let launch = test_launch(root.path(), default_read_pool_size());

        let config = node_config(&launch).expect("build the voter config");
        assert_eq!(config.wal_size, HIQLITE_WAL_SIZE_BYTES);
        assert_eq!(config.health_check_delay_secs, 0);
    }

    /// A read-pool comparison is only evidence if the voter under measurement
    /// actually runs the pool the arm claims. The harness built its own
    /// `NodeConfig` and never set this, so 4, 8, and 16 all measured Hiqlite's
    /// default pool.
    #[test]
    fn the_launched_voter_runs_the_read_pool_it_was_given() {
        let root = tempfile::tempdir().expect("config test root");
        for size in [4, 8, 16] {
            let config =
                node_config(&test_launch(root.path(), size)).expect("build the voter config");
            assert_eq!(config.read_pool_size, size);
            // The extracted defaults must arrive with it, not be traded for it.
            assert_eq!(config.wal_size, HIQLITE_WAL_SIZE_BYTES);
        }
    }

    /// A launch written by an older controller carries no pool size, and must
    /// land on the daemon's default rather than Hiqlite's.
    #[test]
    fn a_launch_without_a_read_pool_size_uses_the_daemon_default() {
        let launch: NodeLaunch = serde_json::from_str(
            r#"{"node_id":1,"root":"/data","nodes":[{"id":1,"raft":"127.0.0.1:19001","api":"127.0.0.1:19002"}]}"#,
        )
        .expect("decode a legacy launch");
        assert_eq!(launch.read_pool_size, default_read_pool_size());
        assert_eq!(launch.read_pool_size, 4);
        assert!(launch.instrument_store_operations);
    }
}
