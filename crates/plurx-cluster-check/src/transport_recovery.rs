//! Linux-only, separate-process snapshot recovery qualification.
//!
//! The campaign deliberately uses the harness node binary, Hiqlite's real TLS
//! Raft transport, and a detached remote client using the production API TLS
//! WebSocket path.  The only acceleration is a validation-only snapshot policy
//! on [`NodeLaunch`]; production configuration keeps its normal threshold.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::future::Future;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use hiqlite::macros::params;
use hiqlite::{Client, Row, SnapshotTransportDirection, SnapshotTransportPhase};
use plurx_core::cluster::membership::{join_token_digest, FinalizeJoinRequest, RedeemJoinRequest};
use plurx_core::store::{AUTH_PROTOCOL_MAX, AUTH_PROTOCOL_MIN, AUTH_SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use super::topology::{resolve_build_sha, unix_ms};
use super::{
    allocate_nodes, harness_executable, wait_for_protocol_pending, with_port_retry,
    ClusterProcesses, NodeLaunch, NodeProcess, NodeSpec, Request, Response, API_SECRET,
};
use crate::transport_recovery_diagnostics::{
    classify_failure, CleanupSummary, DiagnosticContracts, DiagnosticOutcome, FailureClass,
    ProcessIdentity, RecoveryDiagnostics, CLEANUP_BUDGET,
};

/// Version 2 replaced the per-cycle resource ceiling and its three zero
/// margins with the campaign envelope comparison and its allowances. No version 1
/// artifact was ever produced: the per-cycle contract never completed a
/// campaign.
pub const TRANSPORT_RECOVERY_ARTIFACT_SCHEMA_VERSION: u32 = 2;
pub const TRANSPORT_RECOVERY_ROLE_REPORT_SCHEMA_VERSION: u32 = 1;
pub const TRANSPORT_RECOVERY_CYCLES_PER_ROLE: u32 = 20;
pub const TRANSPORT_RECOVERY_SMALL_IMAGE_BYTES: u64 = 88_559_616;
pub const TRANSPORT_RECOVERY_LARGE_IMAGE_BYTES: u64 = 177_119_232;
pub const TRANSPORT_RECOVERY_DEFAULT_VOTER_SMOKE_CYCLES: u32 = 4;
pub const TRANSPORT_RECOVERY_DEFAULT_LEARNER_SMOKE_CYCLES: u32 = 4;
/// Low enough for qualification, while leaving room for image setup before a
/// cycle begins. This value is not reachable from plurxd configuration.
pub const TRANSPORT_RECOVERY_SNAPSHOT_LOGS_SINCE_LAST: u64 = 256;

const EVIDENCE_SCOPE: &str = "linux_separate_process_recovery";
const ROLE_REPORT_KIND: &str = "transport_recovery_role";
const TARGET_NODE: u64 = 4;
const IMAGE_PADDING_BYTES: u64 = 8 * 1024 * 1024;
const IMAGE_ROW_BYTES: u64 = 4 * 1024 * 1024;
const WRITER_INTERVAL_MILLIS: u64 = 1_000;
const WRITER_MAX_OPERATIONS: u64 = 4_096;
const WRITER_READY_TIMEOUT: Duration = Duration::from_secs(60);
const MIN_ACKNOWLEDGED_WRITES_DURING_RECOVERY: usize = 2;
const WRITER_MAX_ACK_GAP: Duration = Duration::from_secs(35);
const WRITER_PROGRESS_POLL: Duration = Duration::from_millis(100);
const RECOVERY_DEADLINE: Duration = Duration::from_secs(1_500);
const RESOURCE_CLEANUP_HORIZON: Duration = Duration::from_secs(60);
const RESOURCE_SAMPLE_INTERVAL: Duration = Duration::from_secs(3);
const RESOURCE_STABLE_SAMPLES: usize = 2;
// Transport event ages are sampled independently in the source and target
// processes and rounded to whole milliseconds before the parent compares
// them. Preserve causal ordering while allowing only that quantization edge.
const CROSS_PROCESS_TIMESTAMP_TOLERANCE_MILLIS: i64 = 2;
const RESOURCE_BASELINE_WARMUP_CYCLES: u32 = 1;
/// How far a node's resource *envelope* may rise between the opening and the
/// closing half of a campaign before it is a leak.
///
/// The campaign does not assert that any single cycle stays under a ceiling:
/// sockets and owned async tasks alternate between two states one recovery
/// apart (one connection and one owned task per peer, still open when
/// `operation_owns_work` clears), and thread counts jitter on their own with
/// no recovery in flight. What it asserts is that the band a resource moved
/// in over the closing half of the campaign — its lowest and its highest
/// sample — does not sit above the band it moved in over the opening half by
/// more than this, at either edge. A leak that starts early adds every cycle
/// and lifts the closing floor by ten or more; a leak that starts late lifts
/// the closing ceiling past anything the opening half showed. The drain
/// cannot do either: it visits both of its states within a few cycles, so a
/// window of ten or eleven samples holds both. Sockets and owned tasks get no
/// allowance — one per-peer transport that stops being released is exactly
/// the leak this exists to catch. Threads get two, the widest swing measured
/// with nothing in flight (`docs/cluster/TRANSPORT-RECOVERY-RESOURCE-BASELINE.md`
/// §3); a thread that leaks every recovery still shows as +10 at the floor,
/// and one that leaks every third recovery as +3. One every fourth or slower
/// hides inside the allowance.
const THREAD_ENVELOPE_ALLOWANCE: u64 = 2;
const SOCKET_ENVELOPE_ALLOWANCE: u64 = 0;
const OWNED_ASYNC_TASK_ENVELOPE_ALLOWANCE: u64 = 0;
/// The envelope is asserted only when the closing window holds at least this
/// many samples. The statistics above are for the twenty-cycle campaign's
/// ten-sample closing window; a shorter smoke records and prints its envelope
/// but does not fail on it, because a window of two or three samples is the
/// same coin flip the per-cycle ceiling lost.
const RESOURCE_ENVELOPE_WINDOW_SAMPLES: usize = 10;
/// The closing ceiling is the fourth-highest sample of the closing window,
/// not its highest. The floor is a resting state and one sample of it is
/// evidence; the ceiling is whatever was still in flight when the sampler
/// looked, and the measurement shows it is not one value (node 1's owned
/// tasks sat at 35 on one high-side cycle and 34 on the next). A count that
/// three samples reached and seven did not is a spike — a replacement
/// connection during one recovery, a slower drain on one cycle, a blocking
/// thread that had not idled out — and a leak is not a spike: one that
/// starts at cycle 13 puts eight samples above the opening ceiling, and one
/// connection leaked at cycle 15 puts six.
const RESOURCE_CEILING_SUSTAINED_SAMPLES: usize = 4;
const RECOVERY_WRITE_SQL: &str =
    "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, $3)";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryRole {
    Learner,
    Voter,
}

impl RecoveryRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::Learner => "learner",
            Self::Voter => "voter",
        }
    }

    fn minimum_sqlite_bytes(self) -> u64 {
        match self {
            Self::Learner => TRANSPORT_RECOVERY_SMALL_IMAGE_BYTES,
            Self::Voter => TRANSPORT_RECOVERY_LARGE_IMAGE_BYTES,
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "learner" => Ok(Self::Learner),
            "voter" => Ok(Self::Voter),
            _ => bail!("transport-recovery role must be voter or learner"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryRolePlan {
    pub role: RecoveryRole,
    pub cycles_required: u32,
    pub execution_id: String,
    pub output: PathBuf,
    pub diagnostics_dir: PathBuf,
    pub max_runtime_seconds: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryAssemblePlan {
    pub execution_id: String,
    pub voter: PathBuf,
    pub learner: PathBuf,
    pub output: PathBuf,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryRoleReportScope {
    Smoke,
    QualificationRole,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransportRecoveryRoleReport {
    pub schema_version: u32,
    pub kind: String,
    pub execution_id: String,
    pub build_sha: String,
    pub platform: String,
    pub build_profile: String,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub scope: RecoveryRoleReportScope,
    pub resource_envelope_asserted: bool,
    pub snapshot_policy: RecoverySnapshotPolicy,
    pub transport: RecoveryTransportContract,
    pub resource_thread_envelope_allowance: u64,
    pub resource_socket_envelope_allowance: u64,
    pub resource_owned_async_task_envelope_allowance: u64,
    pub resource_cleanup_horizon_millis: u64,
    pub resource_stable_samples: usize,
    pub campaign: RecoveryRoleCampaign,
}

pub(crate) fn smoke_cycles(arguments: &[String], role: RecoveryRole) -> Result<u32> {
    if arguments.len() > 1 {
        bail!(
            "transport-recovery-{}-smoke accepts at most one cycle count",
            role.label()
        );
    }
    let cycles_required = arguments
        .first()
        .map(|value| {
            value
                .parse::<u32>()
                .with_context(|| format!("parse {} smoke cycle count", role.label()))
        })
        .transpose()?
        .unwrap_or(match role {
            RecoveryRole::Learner => TRANSPORT_RECOVERY_DEFAULT_LEARNER_SMOKE_CYCLES,
            RecoveryRole::Voter => TRANSPORT_RECOVERY_DEFAULT_VOTER_SMOKE_CYCLES,
        });
    if !(1..=TRANSPORT_RECOVERY_CYCLES_PER_ROLE).contains(&cycles_required) {
        bail!(
            "transport-recovery {} smoke requires 1..={TRANSPORT_RECOVERY_CYCLES_PER_ROLE} cycles",
            role.label()
        );
    }
    Ok(cycles_required)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverySnapshotPolicy {
    pub validation_only: bool,
    pub logs_since_last: u64,
    pub production_default_unchanged: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryTransportContract {
    pub raft_transport: String,
    pub api_writer_transport: String,
    pub tls_enabled: bool,
    pub tls_certificate_verification_disabled_for_self_signed_harness: bool,
    pub separate_node_processes: bool,
    pub separate_writer_process: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessResourceCount {
    pub threads: u64,
    pub sockets: u64,
    pub owned_async_tasks: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryResourceNodeKind {
    PersistentSourceVoter,
    RestartedTarget,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryNodeResourceEvidence {
    pub node_id: u64,
    pub kind: RecoveryResourceNodeKind,
    pub baseline: ProcessResourceCount,
    pub post_quiescence: ProcessResourceCount,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryImageEvidence {
    pub marker: String,
    pub minimum_sqlite_bytes: u64,
    pub rows: u64,
    pub payload_bytes: u64,
    pub content_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotFileEvidence {
    pub snapshot_id: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcknowledgedRecoveryWrite {
    pub key: String,
    pub value_sha256: String,
    pub acknowledged_at_unix_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOutboundTransportEvidence {
    pub observing_node_id: u64,
    pub target_node_id: u64,
    pub attempt_id: u64,
    pub attempt_started_at_unix_ms: i64,
    pub completed_at_unix_ms: i64,
    pub attempted_bytes: u64,
    pub acknowledged_bytes: u64,
    pub total_bytes: u64,
    pub attempts: u64,
    pub reconnect_count: u64,
    pub retry_count: u64,
    pub elapsed_millis: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TargetInboundTransportEvidence {
    pub observing_node_id: u64,
    pub source_node_id: u64,
    pub attempt_id: u64,
    pub receive_started_at_unix_ms: i64,
    pub last_local_receive_at_unix_ms: i64,
    pub install_started_at_unix_ms: i64,
    pub install_completed_at_unix_ms: i64,
    pub received_bytes: u64,
    pub total_bytes: u64,
    pub transfer_millis: u64,
    pub install_millis: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryCycleEvidence {
    pub role: RecoveryRole,
    pub cycle: u32,
    pub recovery_started_at_unix_ms: i64,
    pub recovery_finished_at_unix_ms: i64,
    pub snapshot_index: u64,
    pub purged_index: u64,
    pub applied_index: u64,
    pub source_snapshot: SnapshotFileEvidence,
    pub installed_snapshot: SnapshotFileEvidence,
    pub image: RecoveryImageEvidence,
    pub source_outbound: SourceOutboundTransportEvidence,
    pub target_inbound: TargetInboundTransportEvidence,
    pub recovery_millis: u64,
    pub acknowledged_writes: Vec<AcknowledgedRecoveryWrite>,
    pub acknowledged_write_digest: String,
    pub target_write_digest: String,
    pub node_resources: Vec<RecoveryNodeResourceEvidence>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransportRecoveryWorstDurations {
    pub recovery_millis: u64,
    pub transfer_millis: u64,
    pub install_millis: u64,
}

/// The band a node's resources moved in over one half of a campaign: the
/// lowest and the highest count of each resource, taken per resource, not per
/// sample — the threads floor and the sockets floor may come from different
/// cycles.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceBand {
    pub floor: ProcessResourceCount,
    pub ceiling: ProcessResourceCount,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeResourceEnvelope {
    pub node_id: u64,
    pub kind: RecoveryResourceNodeKind,
    pub opening: ResourceBand,
    pub closing: ResourceBand,
}

/// The campaign's resource assertion, recorded so the artifact proves it.
///
/// The series for each node is the warmup baseline (cycle 0) followed by
/// every recorded cycle's post-quiescence sample. The opening window is the
/// first half of that series rounded up — cycles `0..=opening_window_last_cycle`
/// — and the closing window is the rest. The campaign passes when no node's
/// closing band sits above its opening band, at the floor or at the ceiling,
/// by more than the artifact's recorded allowance for that resource.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryResourceEnvelopes {
    pub opening_window_last_cycle: u32,
    pub closing_window_samples: u32,
    /// The closing band's `ceiling` is this-highest sample of the closing
    /// window, not its highest; the opening band's is the plain highest of
    /// its cycles. Recorded so the file says what the number is.
    pub closing_ceiling_sustained_samples: u32,
    pub nodes: Vec<NodeResourceEnvelope>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRoleCampaign {
    pub role: RecoveryRole,
    pub cycles_required: u32,
    pub minimum_sqlite_bytes: u64,
    pub cycles: Vec<RecoveryCycleEvidence>,
    pub worst_durations: TransportRecoveryWorstDurations,
    pub resource_envelopes: RecoveryResourceEnvelopes,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterTransportRecoveryArtifact {
    pub schema_version: u32,
    pub evidence_scope: String,
    pub build_sha: String,
    pub platform: String,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub snapshot_policy: RecoverySnapshotPolicy,
    pub transport: RecoveryTransportContract,
    pub resource_thread_envelope_allowance: u64,
    pub resource_socket_envelope_allowance: u64,
    pub resource_owned_async_task_envelope_allowance: u64,
    pub resource_cleanup_horizon_millis: u64,
    pub resource_stable_samples: usize,
    pub learner: RecoveryRoleCampaign,
    pub voter: RecoveryRoleCampaign,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecoveryWriterConfig {
    addresses: Vec<String>,
    prefix: String,
    ready_path: PathBuf,
    progress_path: PathBuf,
    start_path: PathBuf,
    stop_path: PathBuf,
    interval_millis: u64,
    max_operations: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecoveryWriterReport {
    writes: Vec<AcknowledgedRecoveryWrite>,
    digest: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RecoveryWriterProgress {
    acknowledged_writes: usize,
    last_acknowledged_at_unix_ms: i64,
}

struct RecoveryWriter {
    child: Child,
    stdout: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
    identity: Option<ProcessIdentity>,
}

impl Drop for RecoveryWriter {
    fn drop(&mut self) {
        if let Some(stdout) = self.stdout.take() {
            stdout.abort();
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRuntimeStatus {
    pub snapshot_index: Option<u64>,
    pub purged_index: Option<u64>,
    pub applied_index: Option<u64>,
    pub install_ok_count: u64,
    pub last_install_observed_at_unix_ms: Option<u64>,
    pub last_install_elapsed_nanos: Option<u64>,
    pub transport: hiqlite::SnapshotTransportStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryWriteDigest {
    pub count: u64,
    pub sha256: String,
}

struct RecoveryImageRow {
    ordinal: i64,
    marker: String,
    payload: Vec<u8>,
}

impl From<&mut Row<'_>> for RecoveryImageRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            ordinal: row.get("ordinal"),
            marker: row.get("marker"),
            payload: row.get("payload"),
        }
    }
}

struct RecoveryWriteRow {
    key: String,
    value: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryWriteResolution {
    Exact,
    Missing,
}

impl From<&mut Row<'_>> for RecoveryWriteRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            key: row.get("key"),
            value: row.get("value"),
        }
    }
}

pub(crate) fn parse_recovery_role_plan(arguments: &[String]) -> Result<RecoveryRolePlan> {
    let mut role = None;
    let mut cycles_required = None;
    let mut execution_id = None;
    let mut output = None;
    let mut diagnostics_dir = None;
    let mut max_runtime_seconds = None;
    let mut seen = BTreeSet::new();
    let mut index = 0;
    while index < arguments.len() {
        let flag = arguments[index].as_str();
        if !matches!(
            flag,
            "--role"
                | "--cycles"
                | "--execution-id"
                | "--output"
                | "--diagnostics-dir"
                | "--max-runtime-seconds"
        ) {
            bail!("unknown transport-recovery-role flag {flag:?}");
        }
        if !seen.insert(flag.to_owned()) {
            bail!("duplicate transport-recovery-role flag {flag}");
        }
        let value = arguments
            .get(index + 1)
            .with_context(|| format!("transport-recovery-role {flag} requires a value"))?;
        match flag {
            "--role" => role = Some(RecoveryRole::parse(value)?),
            "--cycles" => {
                let cycles = value
                    .parse::<u32>()
                    .context("parse transport-recovery role cycle count")?;
                if !(1..=TRANSPORT_RECOVERY_CYCLES_PER_ROLE).contains(&cycles) {
                    bail!(
                        "transport-recovery role requires 1..={TRANSPORT_RECOVERY_CYCLES_PER_ROLE} cycles"
                    );
                }
                cycles_required = Some(cycles);
            }
            "--execution-id" => {
                validate_execution_id(value)?;
                execution_id = Some(value.clone());
            }
            "--output" => output = Some(PathBuf::from(value)),
            "--diagnostics-dir" => diagnostics_dir = Some(PathBuf::from(value)),
            "--max-runtime-seconds" => {
                let seconds = value
                    .parse::<u64>()
                    .context("parse transport-recovery role maximum runtime")?;
                if seconds == 0 {
                    bail!("transport-recovery role maximum runtime must be positive");
                }
                max_runtime_seconds = Some(seconds);
            }
            _ => unreachable!("known transport-recovery-role flag"),
        }
        index += 2;
    }
    let plan = RecoveryRolePlan {
        role: role.context("transport-recovery-role requires --role")?,
        cycles_required: cycles_required.context("transport-recovery-role requires --cycles")?,
        execution_id: execution_id.context("transport-recovery-role requires --execution-id")?,
        output: output.context("transport-recovery-role requires --output")?,
        diagnostics_dir: diagnostics_dir
            .context("transport-recovery-role requires --diagnostics-dir")?,
        max_runtime_seconds,
    };
    validate_role_plan_paths(&plan)?;
    Ok(plan)
}

pub(crate) fn parse_recovery_assemble_plan(arguments: &[String]) -> Result<RecoveryAssemblePlan> {
    let mut execution_id = None;
    let mut voter = None;
    let mut learner = None;
    let mut output = None;
    let mut seen = BTreeSet::new();
    let mut index = 0;
    while index < arguments.len() {
        let flag = arguments[index].as_str();
        if !matches!(
            flag,
            "--execution-id" | "--voter" | "--learner" | "--output"
        ) {
            bail!("unknown transport-recovery-assemble flag {flag:?}");
        }
        if !seen.insert(flag.to_owned()) {
            bail!("duplicate transport-recovery-assemble flag {flag}");
        }
        let value = arguments
            .get(index + 1)
            .with_context(|| format!("transport-recovery-assemble {flag} requires a value"))?;
        match flag {
            "--execution-id" => {
                validate_execution_id(value)?;
                execution_id = Some(value.clone());
            }
            "--voter" => voter = Some(PathBuf::from(value)),
            "--learner" => learner = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            _ => unreachable!("known transport-recovery-assemble flag"),
        }
        index += 2;
    }
    let plan = RecoveryAssemblePlan {
        execution_id: execution_id
            .context("transport-recovery-assemble requires --execution-id")?,
        voter: voter.context("transport-recovery-assemble requires --voter")?,
        learner: learner.context("transport-recovery-assemble requires --learner")?,
        output: output.context("transport-recovery-assemble requires --output")?,
    };
    validate_assemble_plan_paths(&plan)?;
    Ok(plan)
}

fn validate_execution_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        bail!(
            "transport-recovery execution ID must contain 1..=128 ASCII letters, digits, dots, underscores, or hyphens"
        );
    }
    Ok(())
}

fn lexical_absolute(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("resolve current directory for transport-recovery path")?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

fn paths_collide(first: &Path, second: &Path) -> Result<bool> {
    let first = lexical_absolute(first)?;
    let second = lexical_absolute(second)?;
    Ok(first == second || first.starts_with(&second) || second.starts_with(&first))
}

fn validate_role_plan_paths(plan: &RecoveryRolePlan) -> Result<()> {
    if paths_collide(&plan.output, &plan.diagnostics_dir)? {
        bail!("transport-recovery role output and diagnostics paths collide");
    }
    Ok(())
}

fn validate_assemble_plan_paths(plan: &RecoveryAssemblePlan) -> Result<()> {
    if paths_collide(&plan.voter, &plan.learner)?
        || paths_collide(&plan.output, &plan.voter)?
        || paths_collide(&plan.output, &plan.learner)?
    {
        bail!("transport-recovery assembly input and output paths collide");
    }
    Ok(())
}

fn fixed_snapshot_policy() -> RecoverySnapshotPolicy {
    RecoverySnapshotPolicy {
        validation_only: true,
        logs_since_last: TRANSPORT_RECOVERY_SNAPSHOT_LOGS_SINCE_LAST,
        production_default_unchanged: true,
    }
}

fn fixed_transport_contract() -> RecoveryTransportContract {
    RecoveryTransportContract {
        raft_transport: "hiqlite_production_tls_chunked_snapshot".to_owned(),
        api_writer_transport: "hiqlite_production_tls_websocket".to_owned(),
        tls_enabled: true,
        tls_certificate_verification_disabled_for_self_signed_harness: true,
        separate_node_processes: true,
        separate_writer_process: true,
    }
}

fn build_profile_descriptor() -> String {
    if cfg!(debug_assertions) {
        "debug_assertions_on".to_owned()
    } else {
        "debug_assertions_off".to_owned()
    }
}

fn local_execution_id() -> Result<String> {
    Ok(format!("local-{}-{}", unix_ms()?, std::process::id()))
}

pub async fn run_transport_recovery_campaign(output: &Path) -> Result<()> {
    prepare_artifact_output(output)?;
    let execution_id = local_execution_id()?;
    let role_root = artifact_parent(output)
        .join("transport-recovery-roles")
        .join(&execution_id);
    let voter_plan = RecoveryRolePlan {
        role: RecoveryRole::Voter,
        cycles_required: TRANSPORT_RECOVERY_CYCLES_PER_ROLE,
        execution_id: execution_id.clone(),
        output: role_root.join("voter-role.json"),
        diagnostics_dir: role_root.join("voter-diagnostics"),
        max_runtime_seconds: None,
    };
    let learner_plan = RecoveryRolePlan {
        role: RecoveryRole::Learner,
        cycles_required: TRANSPORT_RECOVERY_CYCLES_PER_ROLE,
        execution_id: execution_id.clone(),
        output: role_root.join("learner-role.json"),
        diagnostics_dir: role_root.join("learner-diagnostics"),
        max_runtime_seconds: None,
    };
    print_role_paths(&voter_plan)?;
    print_role_paths(&learner_plan)?;
    let results = run_independent_roles(voter_plan.clone(), learner_plan.clone()).await;
    match require_independent_roles(results) {
        Ok((_voter, _learner)) => assemble_transport_recovery_reports(RecoveryAssemblePlan {
            execution_id,
            voter: voter_plan.output,
            learner: learner_plan.output,
            output: output.to_path_buf(),
        }),
        Err(error) => Err(error),
    }
}

async fn run_independent_roles(
    voter: RecoveryRolePlan,
    learner: RecoveryRolePlan,
) -> (
    Result<TransportRecoveryRoleReport>,
    Result<TransportRecoveryRoleReport>,
) {
    run_role_sequence(voter, learner, run_transport_recovery_role).await
}

async fn run_role_sequence<F, Fut>(
    voter: RecoveryRolePlan,
    learner: RecoveryRolePlan,
    mut runner: F,
) -> (
    Result<TransportRecoveryRoleReport>,
    Result<TransportRecoveryRoleReport>,
)
where
    F: FnMut(RecoveryRolePlan) -> Fut,
    Fut: Future<Output = Result<TransportRecoveryRoleReport>>,
{
    let voter = runner(voter).await;
    if voter.as_ref().err().is_some_and(is_recovery_cancelled) {
        return (
            voter,
            Err(anyhow::Error::new(RecoveryCancelled {
                signal: "parent invocation already cancelled".to_owned(),
            })),
        );
    }
    let learner = runner(learner).await;
    (voter, learner)
}

#[derive(Debug)]
struct RecoveryCancelled {
    signal: String,
}

impl std::fmt::Display for RecoveryCancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "transport-recovery cancelled by {}", self.signal)
    }
}

impl std::error::Error for RecoveryCancelled {}

fn is_recovery_cancelled(error: &anyhow::Error) -> bool {
    error.downcast_ref::<RecoveryCancelled>().is_some()
}

fn require_independent_roles(
    results: (
        Result<TransportRecoveryRoleReport>,
        Result<TransportRecoveryRoleReport>,
    ),
) -> Result<(TransportRecoveryRoleReport, TransportRecoveryRoleReport)> {
    match results {
        (Ok(voter), Ok(learner)) => Ok((voter, learner)),
        (Err(voter), Ok(_)) => Err(voter).context("voter transport-recovery role failed"),
        (Ok(_), Err(learner)) => Err(learner).context("learner transport-recovery role failed"),
        (Err(voter), Err(learner)) => bail!(
            "voter transport-recovery role failed: {voter:#}; learner transport-recovery role failed: {learner:#}"
        ),
    }
}

pub(crate) async fn run_transport_recovery_smoke_alias(
    role: RecoveryRole,
    cycles_required: u32,
) -> Result<()> {
    let execution_id = local_execution_id()?;
    let root = PathBuf::from("target/validation/transport-recovery-smoke").join(&execution_id);
    let plan = RecoveryRolePlan {
        role,
        cycles_required,
        execution_id,
        output: root.join(format!("{}-role.json", role.label())),
        diagnostics_dir: root.join(format!("{}-diagnostics", role.label())),
        max_runtime_seconds: None,
    };
    print_role_paths(&plan)?;
    run_transport_recovery_role(plan).await.map(|_| ())
}

fn print_role_paths(plan: &RecoveryRolePlan) -> Result<()> {
    println!(
        "cluster-check: transport-recovery execution ID {}",
        plan.execution_id
    );
    println!(
        "cluster-check: {} role report {}",
        plan.role.label(),
        lexical_absolute(&plan.output)?.display()
    );
    println!(
        "cluster-check: {} diagnostics {}",
        plan.role.label(),
        lexical_absolute(&plan.diagnostics_dir)?.display()
    );
    Ok(())
}

pub async fn run_transport_recovery_role(
    plan: RecoveryRolePlan,
) -> Result<TransportRecoveryRoleReport> {
    validate_execution_id(&plan.execution_id)?;
    if !(1..=TRANSPORT_RECOVERY_CYCLES_PER_ROLE).contains(&plan.cycles_required) {
        bail!("transport-recovery role requires 1..={TRANSPORT_RECOVERY_CYCLES_PER_ROLE} cycles");
    }
    validate_role_plan_paths(&plan)?;
    prepare_role_destinations(&plan)?;
    if std::env::consts::OS != "linux" {
        bail!("transport-recovery role requires Linux /proc evidence");
    }
    let build_sha = resolve_build_sha()
        .context("resolve transport-recovery candidate SHA before role start")?;
    let started_at_unix_ms = unix_ms()?;
    let build_profile = build_profile_descriptor();
    let diagnostics = RecoveryDiagnostics::create(
        &plan,
        &build_sha,
        &build_profile,
        diagnostic_contracts(plan.role),
        started_at_unix_ms,
    )
    .context("initialize transport-recovery diagnostic evidence I/O")?;
    let role = plan.role;
    let cycles_required = plan.cycles_required;
    let output = plan.output.clone();
    let mut cancelled = false;
    let result = {
        let run = async {
            let executable = harness_executable()?;
            let root = tempfile::tempdir().context("transport-recovery role cluster root")?;
            println!(
                "cluster-check: {cycles_required}-cycle {} snapshot recovery role",
                role.label()
            );
            let campaign = run_role_campaign(
                &executable,
                root.path(),
                role,
                role.minimum_sqlite_bytes(),
                cycles_required,
                &diagnostics,
            )
            .await?;
            let report = TransportRecoveryRoleReport {
                schema_version: TRANSPORT_RECOVERY_ROLE_REPORT_SCHEMA_VERSION,
                kind: ROLE_REPORT_KIND.to_owned(),
                execution_id: plan.execution_id.clone(),
                build_sha,
                platform: "linux".to_owned(),
                build_profile,
                started_at_unix_ms,
                finished_at_unix_ms: unix_ms()?,
                scope: role_report_scope(cycles_required),
                resource_envelope_asserted: envelope_is_asserted(&campaign.resource_envelopes),
                snapshot_policy: fixed_snapshot_policy(),
                transport: fixed_transport_contract(),
                resource_thread_envelope_allowance: THREAD_ENVELOPE_ALLOWANCE,
                resource_socket_envelope_allowance: SOCKET_ENVELOPE_ALLOWANCE,
                resource_owned_async_task_envelope_allowance: OWNED_ASYNC_TASK_ENVELOPE_ALLOWANCE,
                resource_cleanup_horizon_millis: duration_millis(RESOURCE_CLEANUP_HORIZON),
                resource_stable_samples: RESOURCE_STABLE_SAMPLES,
                campaign,
            };
            validate_transport_recovery_role_report(&report)?;
            Ok::<_, anyhow::Error>(report)
        };
        tokio::pin!(run);
        let cancellation = wait_for_recovery_cancellation();
        tokio::pin!(cancellation);
        match plan.max_runtime_seconds {
            Some(seconds) => {
                let timeout = tokio::time::sleep(Duration::from_secs(seconds));
                tokio::pin!(timeout);
                tokio::select! {
                    result = &mut run => result,
                    signal = &mut cancellation => {
                        cancelled = true;
                        let signal = signal.unwrap_or_else(|error| format!("signal handler failed: {error:#}"));
                        Err(anyhow::Error::new(RecoveryCancelled { signal }))
                    }
                    _ = &mut timeout => {
                        let (phase, cycle) = diagnostics.active_phase()?;
                        bail!(
                            "{} transport-recovery role exceeded its {seconds}-second orchestration limit in phase {} cycle {}",
                            role.label(),
                            phase.as_deref().unwrap_or("between_phases"),
                            cycle.unwrap_or(0)
                        )
                    }
                }
            }
            None => tokio::select! {
                result = &mut run => result,
                signal = &mut cancellation => {
                    cancelled = true;
                    let signal = signal.unwrap_or_else(|error| format!("signal handler failed: {error:#}"));
                    Err(anyhow::Error::new(RecoveryCancelled { signal }))
                }
            },
        }
    };
    let result = match result {
        Ok(report) => Ok(report),
        Err(primary) => {
            let message = format!("{primary:#}");
            let failure_class = if cancelled {
                None
            } else {
                Some(classify_failure(&message))
            };
            match diagnostics.record_terminal_observation(
                if cancelled {
                    DiagnosticOutcome::Cancelled
                } else {
                    DiagnosticOutcome::Failed
                },
                failure_class,
                Some(&message),
            ) {
                Ok(()) => Err(primary),
                Err(diagnostic_error) => Err(primary.context(format!(
                    "also failed to record primary diagnostic failure: {diagnostic_error:#}"
                ))),
            }
        }
    };
    let cleanup_result = diagnostics.cleanup_registered(CLEANUP_BUDGET).await;
    let (cleanup, cleanup_error) = match cleanup_result {
        Ok(cleanup) => (cleanup, None),
        Err(error) => {
            let surviving_processes = diagnostics.registered_processes().unwrap_or_default();
            (
                CleanupSummary {
                    attempted: true,
                    budget_millis: duration_millis(CLEANUP_BUDGET),
                    registered_processes: surviving_processes.len(),
                    reaped_processes: 0,
                    surviving_processes,
                },
                Some(error.context("bounded transport-recovery cleanup")),
            )
        }
    };
    let result = match (result, cleanup_error) {
        (Ok(_), Some(cleanup_error)) => Err(cleanup_error),
        (Err(primary), Some(cleanup_error)) => {
            Err(primary.context(format!("cleanup also failed: {cleanup_error:#}")))
        }
        (result, None) => result,
    };
    let result = if cleanup.surviving_processes.is_empty() {
        result
    } else {
        let cleanup_error = anyhow::anyhow!(
            "transport-recovery cleanup left {} owned process(es)",
            cleanup.surviving_processes.len()
        );
        match result {
            Ok(_) => Err(cleanup_error),
            Err(primary) => Err(primary.context(cleanup_error)),
        }
    };
    match result {
        Ok(report) => {
            let publish = (|| -> Result<()> {
                let mut bytes = serde_json::to_vec_pretty(&report)?;
                bytes.push(b'\n');
                publish_artifact_atomically(&output, &bytes)
            })();
            if let Err(error) = publish {
                let message = format!("publish passing role report: {error:#}");
                let _ = diagnostics.write_summary(
                    DiagnosticOutcome::Failed,
                    false,
                    Some(FailureClass::EvidenceIo),
                    Some(&message),
                    cleanup,
                );
                return Err(error).context("publish passing transport-recovery role report");
            }
            let summary = match diagnostics.write_summary(
                DiagnosticOutcome::Passed,
                report.resource_envelope_asserted,
                None,
                None,
                cleanup,
            ) {
                Ok(summary) => summary,
                Err(error) => {
                    let _ = std::fs::remove_file(&output);
                    return Err(error)
                        .context("publish passing transport-recovery diagnostic summary");
                }
            };
            println!(
                "cluster-check: {cycles_required}/{cycles_required} {} recoveries passed for {}; resource envelope asserted: {}",
                role.label(),
                report.build_sha,
                report.resource_envelope_asserted
            );
            println!(
                "cluster-check: {} end-of-role timing {} (wall={}ms)",
                role.label(),
                describe_timing_statistics(&summary.phase_timings),
                summary.total_wall_millis
            );
            Ok(report)
        }
        Err(error) => {
            let message = format!("{error:#}");
            let summary_result = diagnostics.write_summary(
                if cancelled {
                    DiagnosticOutcome::Cancelled
                } else {
                    DiagnosticOutcome::Failed
                },
                false,
                if cancelled {
                    None
                } else {
                    Some(classify_failure(&message))
                },
                Some(&message),
                cleanup,
            );
            match summary_result {
                Ok(_) => Err(error),
                Err(summary_error) => Err(error.context(format!(
                    "also failed to publish diagnostic summary: {summary_error:#}"
                ))),
            }
        }
    }
}

fn diagnostic_contracts(role: RecoveryRole) -> DiagnosticContracts {
    DiagnosticContracts {
        minimum_sqlite_bytes: role.minimum_sqlite_bytes(),
        snapshot_logs_since_last: TRANSPORT_RECOVERY_SNAPSHOT_LOGS_SINCE_LAST,
        recovery_deadline_millis: duration_millis(RECOVERY_DEADLINE),
        resource_cleanup_horizon_millis: duration_millis(RESOURCE_CLEANUP_HORIZON),
        resource_sample_interval_millis: duration_millis(RESOURCE_SAMPLE_INTERVAL),
        resource_stable_samples: RESOURCE_STABLE_SAMPLES,
        thread_envelope_allowance: THREAD_ENVELOPE_ALLOWANCE,
        socket_envelope_allowance: SOCKET_ENVELOPE_ALLOWANCE,
        owned_async_task_envelope_allowance: OWNED_ASYNC_TASK_ENVELOPE_ALLOWANCE,
    }
}

pub(crate) async fn wait_for_recovery_cancellation() -> Result<String> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("install SIGTERM handler")?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                result.context("wait for SIGINT")?;
                Ok("SIGINT".to_owned())
            }
            _ = terminate.recv() => Ok("SIGTERM".to_owned()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .context("wait for cancellation")?;
        Ok("interrupt".to_owned())
    }
}

async fn observed_phase<T, F>(
    diagnostics: &RecoveryDiagnostics,
    phase: &str,
    cycle: u32,
    parent_phase: Option<&str>,
    future: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let token = diagnostics
        .phase_start(phase, cycle, parent_phase)
        .context("write transport-recovery diagnostic phase start")?;
    let result = future.await;
    let diagnostic_result = result
        .as_ref()
        .map(|_| ())
        .map_err(|error| anyhow::anyhow!("{error:#}"));
    if let Err(diagnostic_error) = diagnostics.phase_end(token, &diagnostic_result) {
        return match result {
            Ok(_) => Err(diagnostic_error).context("write transport-recovery diagnostic phase end"),
            Err(primary) => Err(primary.context(format!(
                "also failed to write transport-recovery diagnostic phase end: {diagnostic_error:#}"
            ))),
        };
    }
    result
}

fn role_report_scope(cycles_required: u32) -> RecoveryRoleReportScope {
    if cycles_required == TRANSPORT_RECOVERY_CYCLES_PER_ROLE {
        RecoveryRoleReportScope::QualificationRole
    } else {
        RecoveryRoleReportScope::Smoke
    }
}

fn prepare_role_destinations(plan: &RecoveryRolePlan) -> Result<()> {
    prepare_artifact_output(&plan.output)?;
    let diagnostics_parent = artifact_parent(&plan.diagnostics_dir);
    std::fs::create_dir_all(diagnostics_parent).with_context(|| {
        format!("create transport-recovery diagnostics parent {diagnostics_parent:?}")
    })?;
    std::fs::create_dir(&plan.diagnostics_dir).with_context(|| {
        format!(
            "create new transport-recovery diagnostics directory {:?}",
            plan.diagnostics_dir
        )
    })?;
    Ok(())
}

async fn run_role_campaign(
    executable: &Path,
    root: &Path,
    role: RecoveryRole,
    minimum_sqlite_bytes: u64,
    cycles_required: u32,
    diagnostics: &RecoveryDiagnostics,
) -> Result<RecoveryRoleCampaign> {
    let role_root = root.join(role.label());
    let (mut cluster, specs, cluster_root) = observed_phase(
        diagnostics,
        "cluster_start",
        0,
        None,
        start_recovery_cluster(executable, &role_root, role, diagnostics),
    )
    .await?;
    let runtime = RecoveryRoleRuntime {
        executable,
        specs: &specs,
        cluster_root: &cluster_root,
        role,
        minimum_sqlite_bytes,
        cycles_required,
        diagnostics,
    };
    let result = exercise_role_campaign(&mut cluster, runtime).await;
    match result {
        Ok(campaign) => {
            observed_phase(diagnostics, "shutdown", 0, None, async {
                let identities = diagnostics.registered_processes()?;
                let result = cluster.shutdown_all().await;
                for identity in identities
                    .iter()
                    .filter(|identity| identity.process_kind == "node")
                {
                    diagnostics.process_action("reap", identity, &result)?;
                }
                result
            })
            .await?;
            Ok(campaign)
        }
        Err(error) => {
            cluster.kill_all().await;
            Err(error)
        }
    }
}

async fn start_recovery_cluster(
    executable: &Path,
    root: &Path,
    role: RecoveryRole,
    diagnostics: &RecoveryDiagnostics,
) -> Result<(ClusterProcesses, Vec<NodeSpec>, PathBuf)> {
    with_port_retry(|attempt| {
        let reservation = allocate_nodes(TARGET_NODE);
        let attempt_root = root.join(format!("attempt-{attempt}"));
        async move {
            let reservation = reservation?;
            let (listeners, specs) = reservation.into_inner();
            drop(listeners);
            let initial_processes = if role == RecoveryRole::Learner {
                TARGET_NODE - 1
            } else {
                TARGET_NODE
            };
            let mut nodes = Vec::with_capacity(TARGET_NODE as usize);
            for node_id in 1..=initial_processes {
                let launch_specs = if role == RecoveryRole::Learner {
                    &specs[..3]
                } else {
                    specs.as_slice()
                };
                let launch =
                    recovery_launch(node_id, &attempt_root, launch_specs, RecoveryRole::Voter);
                let process = NodeProcess::spawn(executable, &launch)?;
                diagnostics.register_process(process.pid()?, "node", Some(node_id))?;
                nodes.push(Some(process));
            }
            nodes.resize_with(TARGET_NODE as usize, || None);
            let mut cluster = ClusterProcesses {
                nodes,
                root: attempt_root.clone(),
                convergence_timeout: RECOVERY_DEADLINE,
            };
            for node_id in 1..=initial_processes {
                cluster
                    .node_mut(node_id)?
                    .wait_ready_with_timeout(RECOVERY_DEADLINE)
                    .await?;
            }
            cluster.request(1, Request::Bootstrap).await?.require_ok()?;
            for node_id in 2..=initial_processes {
                cluster
                    .request(node_id, Request::Open)
                    .await?
                    .require_ok()?;
            }
            cluster
                .wait_for_voters(&(1..=initial_processes).collect::<Vec<_>>())
                .await?;
            if role == RecoveryRole::Learner {
                observed_phase(
                    diagnostics,
                    "learner_admission",
                    0,
                    Some("cluster_start"),
                    admit_learner(executable, &mut cluster, &specs, &attempt_root, diagnostics),
                )
                .await?;
            }
            Ok((cluster, specs, attempt_root))
        }
    })
    .await
}

async fn admit_learner(
    executable: &Path,
    cluster: &mut ClusterProcesses,
    specs: &[NodeSpec],
    cluster_root: &Path,
    diagnostics: &RecoveryDiagnostics,
) -> Result<()> {
    let leader = cluster.leader().await?;
    observed_phase(
        diagnostics,
        "admission_activation",
        0,
        Some("learner_admission"),
        async {
            wait_for_protocol_pending(cluster, leader, &[]).await?;
            match cluster
                .request(leader, Request::ActivateLearnerProtocol)
                .await?
            {
                Response::ProtocolChange { change }
                    if change.protocol.learner_protocol_active
                        && change.protocol.active_min == AUTH_PROTOCOL_MAX
                        && change.protocol.active_max == AUTH_PROTOCOL_MAX =>
                {
                    Ok(())
                }
                response => bail!("learner protocol activation failed: {response:?}"),
            }
        },
    )
    .await?;
    let issued = observed_phase(
        diagnostics,
        "admission_token_issue",
        0,
        Some("learner_admission"),
        async {
            match cluster
                .request(leader, Request::IssueLearnerJoinToken { ttl_ms: 120_000 })
                .await?
            {
                Response::IssuedJoinToken { token } => Ok(token),
                response => bail!("unexpected learner token response: {response:?}"),
            }
        },
    )
    .await?;
    if issued.raft_id != TARGET_NODE {
        bail!(
            "learner admission allocated raft id {}, expected {TARGET_NODE}",
            issued.raft_id
        );
    }
    let target = &specs[(TARGET_NODE - 1) as usize];
    observed_phase(
        diagnostics,
        "admission_token_redeem",
        0,
        Some("learner_admission"),
        async {
            cluster
                .request(
                    leader,
                    Request::RedeemLearnerJoin {
                        request: RedeemJoinRequest {
                            token_digest: join_token_digest(&issued.token),
                            raft_id: TARGET_NODE,
                            node_id: format!("node-{TARGET_NODE}"),
                            hostname: format!("cluster-node-{TARGET_NODE}"),
                            raft_address: target.raft.clone(),
                            api_address: target.api.clone(),
                            http_base: format!("http://127.0.0.1:{}", 33_000 + TARGET_NODE),
                            schema_version: AUTH_SCHEMA_VERSION,
                            protocol_version: AUTH_PROTOCOL_MIN,
                            protocol_min: AUTH_PROTOCOL_MIN,
                            protocol_max: AUTH_PROTOCOL_MAX,
                            live_tv_v1: true,
                        },
                    },
                )
                .await?
                .require_ok()
        },
    )
    .await?;
    observed_phase(
        diagnostics,
        "admission_learner_ready",
        0,
        Some("learner_admission"),
        spawn_recovery_node(
            executable,
            cluster,
            recovery_launch(TARGET_NODE, cluster_root, specs, RecoveryRole::Learner),
            Instant::now() + RECOVERY_DEADLINE,
            diagnostics,
        ),
    )
    .await?;
    observed_phase(
        diagnostics,
        "admission_member_convergence",
        0,
        Some("learner_admission"),
        async {
            cluster
                .wait_for_members(1, &[1, 2, 3], &[1, 2, 3, TARGET_NODE])
                .await
        },
    )
    .await?;
    observed_phase(
        diagnostics,
        "admission_open",
        0,
        Some("learner_admission"),
        async {
            cluster
                .request(TARGET_NODE, Request::Open)
                .await?
                .require_ok()
        },
    )
    .await?;
    observed_phase(
        diagnostics,
        "admission_forced_heartbeat",
        0,
        Some("learner_admission"),
        async {
            cluster
                .request(TARGET_NODE, Request::ForceHeartbeat)
                .await?
                .require_ok()
        },
    )
    .await?;
    observed_phase(
        diagnostics,
        "admission_finalize",
        0,
        Some("learner_admission"),
        async {
            cluster
                .request(
                    leader,
                    Request::FinalizeLearnerJoin {
                        request: FinalizeJoinRequest {
                            token_digest: join_token_digest(&issued.token),
                            raft_id: TARGET_NODE,
                            node_id: format!("node-{TARGET_NODE}"),
                        },
                    },
                )
                .await?
                .require_ok()
        },
    )
    .await?;
    observed_phase(
        diagnostics,
        "admission_loop_start",
        0,
        Some("learner_admission"),
        async {
            cluster
                .request(TARGET_NODE, Request::StartHeartbeatLoop)
                .await?
                .require_ok()
        },
    )
    .await?;
    Ok(())
}

fn recovery_launch(
    node_id: u64,
    root: &Path,
    specs: &[NodeSpec],
    role: RecoveryRole,
) -> NodeLaunch {
    let launch = NodeLaunch::voter(node_id, root.to_path_buf(), specs.to_vec())
        .with_snapshot_logs_since_last(TRANSPORT_RECOVERY_SNAPSHOT_LOGS_SINCE_LAST);
    if role == RecoveryRole::Learner {
        launch.as_learner()
    } else {
        launch
    }
}

struct RecoveryRoleRuntime<'a> {
    executable: &'a Path,
    specs: &'a [NodeSpec],
    cluster_root: &'a Path,
    role: RecoveryRole,
    minimum_sqlite_bytes: u64,
    cycles_required: u32,
    diagnostics: &'a RecoveryDiagnostics,
}

async fn exercise_role_campaign(
    cluster: &mut ClusterProcesses,
    runtime: RecoveryRoleRuntime<'_>,
) -> Result<RecoveryRoleCampaign> {
    let RecoveryRoleRuntime {
        executable,
        specs,
        cluster_root,
        role,
        minimum_sqlite_bytes,
        cycles_required,
        diagnostics,
    } = runtime;
    let leader = cluster.leader_among(&[1, 2, 3]).await?;
    let marker = format!("transport-recovery-{}-image-v1", role.label());
    let expected_image = observed_phase(diagnostics, "image_seed", 0, None, async {
        let evidence = match cluster
            .request(
                leader,
                Request::SeedRecoveryImage {
                    minimum_bytes: minimum_sqlite_bytes,
                    marker: marker.clone(),
                },
            )
            .await?
        {
            Response::RecoveryImage { evidence } => evidence,
            response => bail!("unexpected recovery image seed response: {response:?}"),
        };
        validate_image(&evidence, minimum_sqlite_bytes, &marker)?;
        Ok(evidence)
    })
    .await?;
    let voters = if role == RecoveryRole::Learner {
        vec![1, 2, 3]
    } else {
        vec![1, 2, 3, TARGET_NODE]
    };
    observed_phase(diagnostics, "baseline_convergence", 0, None, async {
        cluster
            .wait_for_members(leader, &voters, &[1, 2, 3, TARGET_NODE])
            .await?;
        let seeded_index = request_status(cluster, leader)
            .await?
            .applied_index
            .context("seeded recovery image has no applied index")?;
        wait_for_applied_index(
            cluster,
            TARGET_NODE,
            seeded_index,
            Instant::now() + RECOVERY_DEADLINE,
        )
        .await?;
        let target_image = match cluster
            .request(
                TARGET_NODE,
                Request::RecoveryImageDigest {
                    minimum_bytes: minimum_sqlite_bytes,
                },
            )
            .await?
        {
            Response::RecoveryImage { evidence } => evidence,
            response => bail!("unexpected baseline recovery image response: {response:?}"),
        };
        if target_image != expected_image {
            bail!("target recovery image did not converge before resource baseline");
        }
        Ok(())
    })
    .await?;
    let mut baseline_resources: Option<Vec<(u64, ProcessResourceCount)>> = None;
    let mut cycles = Vec::with_capacity(cycles_required as usize);

    for cycle in 0..cycles_required.saturating_add(RESOURCE_BASELINE_WARMUP_CYCLES) {
        if cycle == 0 {
            println!(
                "cluster-check: {} resource-baseline warmup cycle",
                role.label()
            );
        } else {
            println!(
                "cluster-check: {} recovery cycle {cycle}/{}",
                role.label(),
                cycles_required
            );
        }
        let prefix = format!("cluster.transport-recovery.{}.{cycle:02}.", role.label());
        let (mut writer, writer_config, ready_acknowledged_at_unix_ms) =
            observed_phase(diagnostics, "writer_ready", cycle, None, async {
                let (mut writer, writer_config) =
                    spawn_writer(executable, cluster_root, specs, &prefix, diagnostics).await?;
                wait_for_writer_ready(&mut writer, &writer_config.ready_path, WRITER_READY_TIMEOUT)
                    .await?;
                let ready_acknowledged_at_unix_ms =
                    std::fs::read_to_string(&writer_config.ready_path)
                        .context("read recovery writer readiness timestamp")?
                        .trim()
                        .parse::<i64>()
                        .context("parse recovery writer readiness timestamp")?;
                Ok((writer, writer_config, ready_acknowledged_at_unix_ms))
            })
            .await?;
        let timestamp_deadline = Instant::now() + Duration::from_secs(1);
        let recovery_started_at_unix_ms = loop {
            let now = unix_ms()?;
            if now > ready_acknowledged_at_unix_ms {
                break now;
            }
            if let Some(status) = writer
                .child
                .try_wait()
                .context("inspect recovery writer before recovery start")?
            {
                bail!("recovery writer exited before recovery start with {status}");
            }
            if Instant::now() >= timestamp_deadline {
                bail!("wall clock did not advance after recovery writer readiness");
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        };
        std::fs::write(&writer_config.start_path, b"start\n")
            .context("signal recovery writer to begin recovery-window writes")?;
        let recovery_started = Instant::now();
        let recovery_deadline = recovery_started
            .checked_add(RECOVERY_DEADLINE)
            .context("recovery deadline overflow")?;
        let recovery = async {
            observed_phase(diagnostics, "target_reset", cycle, None, async {
                let identity = diagnostics
                    .registered_for_node(TARGET_NODE)?
                    .context("target process identity was not registered")?;
                let kill_result = cluster.kill(TARGET_NODE).await;
                let lifecycle_result = kill_result
                    .as_ref()
                    .map(|_| ())
                    .map_err(|error| anyhow::anyhow!("{error:#}"));
                diagnostics.process_action("kill", &identity, &lifecycle_result)?;
                if kill_result.is_ok() {
                    diagnostics.process_action("reap", &identity, &Ok(()))?;
                }
                kill_result?;
                delete_disposable_target(cluster_root, TARGET_NODE)?;
                Ok(())
            })
            .await?;
            let leader = cluster.leader_among(&[1, 2, 3]).await?;
            let purge_evidence = trigger_and_wait_for_snapshot(
                cluster,
                leader,
                role,
                cycle,
                recovery_deadline,
                diagnostics,
            )
            .await?;
            let snapshot_index = purge_evidence.snapshot_index;
            let purged_index = purge_evidence.purged_index;
            let source_snapshot =
                observed_phase(diagnostics, "source_snapshot_hash", cycle, None, async {
                    let source_snapshot = snapshot_file(cluster_root, leader)?;
                    if source_snapshot.bytes < minimum_sqlite_bytes {
                        bail!(
                            "{} cycle {cycle} snapshot was {} bytes, below {minimum_sqlite_bytes}",
                            role.label(),
                            source_snapshot.bytes
                        );
                    }
                    Ok(source_snapshot)
                })
                .await?;
            let recovery_status = async {
                observed_phase(diagnostics, "target_start_open", cycle, None, async {
                    spawn_recovery_node(
                        executable,
                        cluster,
                        recovery_launch(TARGET_NODE, cluster_root, specs, role),
                        recovery_deadline,
                        diagnostics,
                    )
                    .await?;
                    cluster
                        .request(TARGET_NODE, Request::Open)
                        .await?
                        .require_ok()?;
                    if role == RecoveryRole::Learner {
                        cluster
                            .request(TARGET_NODE, Request::StartHeartbeatLoop)
                            .await?
                            .require_ok()?;
                    }
                    Ok(())
                })
                .await?;
                let target_status = observed_phase(
                    diagnostics,
                    "target_install_observed",
                    cycle,
                    None,
                    wait_for_installed_snapshot(
                        cluster,
                        TARGET_NODE,
                        snapshot_index,
                        &source_snapshot.snapshot_id,
                        recovery_deadline,
                    ),
                )
                .await?;
                let source_status = observed_phase(
                    diagnostics,
                    "source_completion_observed",
                    cycle,
                    None,
                    wait_for_completed_source_outbound(
                        cluster,
                        leader,
                        TARGET_NODE,
                        &source_snapshot.snapshot_id,
                        recovery_deadline,
                    ),
                )
                .await?;
                Ok::<_, anyhow::Error>((target_status, source_status))
            };
            let installed_snapshot = observed_phase(
                diagnostics,
                "installed_snapshot_verify",
                cycle,
                None,
                wait_for_installed_snapshot_file(
                    cluster_root,
                    TARGET_NODE,
                    &source_snapshot,
                    recovery_deadline,
                ),
            );
            let ((target_status, source_status), installed_snapshot) =
                tokio::try_join!(recovery_status, installed_snapshot)?;
            Ok::<_, anyhow::Error>((
                leader,
                snapshot_index,
                purged_index,
                source_snapshot,
                target_status,
                source_status,
                installed_snapshot,
            ))
        };
        let (
            leader,
            snapshot_index,
            purged_index,
            source_snapshot,
            target_status,
            source_status,
            installed_snapshot,
        ) = supervise_recovery_writer(
            &mut writer,
            &writer_config,
            ready_acknowledged_at_unix_ms,
            recovery_deadline,
            recovery,
        )
        .await?;
        let source_outbound = source_outbound_evidence(
            &source_status,
            TARGET_NODE,
            &source_snapshot.snapshot_id,
            source_snapshot.bytes,
        )?;
        let target_inbound = target_inbound_evidence(
            &target_status,
            leader,
            &source_snapshot.snapshot_id,
            source_snapshot.bytes,
        )?;
        let recovery_finished_at_unix_ms = unix_ms()?;
        let recovery_millis = duration_millis(recovery_started.elapsed());
        let writer_report = observed_phase(diagnostics, "writer_stop", cycle, None, async {
            std::fs::write(&writer_config.stop_path, b"stop\n")
                .context("signal recovery writer to stop")?;
            wait_writer(&mut writer, diagnostics).await
        })
        .await?;

        let target_digest = observed_phase(
            diagnostics,
            "write_digest_verify",
            cycle,
            None,
            wait_for_write_digest(
                cluster,
                TARGET_NODE,
                &prefix,
                writer_report.writes.len(),
                &writer_report.digest,
            ),
        )
        .await?;
        let image = observed_phase(diagnostics, "image_digest_verify", cycle, None, async {
            let image = match cluster
                .request(
                    TARGET_NODE,
                    Request::RecoveryImageDigest {
                        minimum_bytes: minimum_sqlite_bytes,
                    },
                )
                .await?
            {
                Response::RecoveryImage { evidence } => evidence,
                response => bail!("unexpected recovered image response: {response:?}"),
            };
            if image != expected_image {
                bail!(
                    "{} cycle {cycle} recovered image digest drifted",
                    role.label()
                );
            }
            Ok(image)
        })
        .await?;
        let applied_index = target_status
            .applied_index
            .context("target did not publish applied index")?;
        validate_acknowledged_writes(
            &writer_report.writes,
            recovery_started_at_unix_ms,
            recovery_finished_at_unix_ms,
        )?;
        let resource_deadline = if baseline_resources.is_some() {
            Instant::now() + RESOURCE_CLEANUP_HORIZON
        } else {
            Instant::now() + RECOVERY_DEADLINE
        };
        let post_resources = observed_phase(
            diagnostics,
            "resource_settle",
            cycle,
            None,
            wait_for_stable_idle_resources(
                cluster,
                &[1, 2, 3, TARGET_NODE],
                resource_deadline,
                diagnostics,
                cycle,
            ),
        )
        .await?;
        match baseline_resources.as_deref() {
            None => println!(
                "cluster-check: {} baseline resources {}",
                role.label(),
                describe_resource_sample(&post_resources)
            ),
            Some(baseline) => println!(
                "cluster-check: {} cycle {cycle} resources {} (baseline {})",
                role.label(),
                describe_resource_sample(&post_resources),
                describe_resource_sample(baseline)
            ),
        }
        if cycle == 0 {
            baseline_resources = Some(post_resources);
            continue;
        }
        let baseline_resources = baseline_resources
            .as_deref()
            .context("resource baseline warmup did not complete")?;
        let node_resources = collect_node_resource_evidence(baseline_resources, &post_resources)?;
        let evidence = RecoveryCycleEvidence {
            role,
            cycle,
            recovery_started_at_unix_ms,
            recovery_finished_at_unix_ms,
            snapshot_index,
            purged_index,
            applied_index,
            source_snapshot,
            installed_snapshot,
            image,
            source_outbound,
            target_inbound,
            recovery_millis,
            acknowledged_writes: writer_report.writes,
            acknowledged_write_digest: writer_report.digest,
            target_write_digest: target_digest.sha256,
            node_resources,
        };
        diagnostics.checkpoint(leader, &evidence)?;
        let phase_durations = diagnostics.cycle_phase_durations(cycle)?;
        println!(
            "cluster-check: {} cycle {cycle} timing {}",
            role.label(),
            describe_phase_durations(&phase_durations)
        );
        cycles.push(evidence);
    }

    let resource_envelopes = resource_envelopes(&cycles)?;
    println!(
        "cluster-check: {} campaign resource envelopes {}",
        role.label(),
        describe_resource_envelopes(&resource_envelopes)
    );
    let rose = resource_envelopes_over_allowance(
        &resource_envelopes,
        ResourceEnvelopeAllowances {
            threads: THREAD_ENVELOPE_ALLOWANCE,
            sockets: SOCKET_ENVELOPE_ALLOWANCE,
            owned_async_tasks: OWNED_ASYNC_TASK_ENVELOPE_ALLOWANCE,
        },
    );
    if !rose.is_empty() {
        for line in describe_resource_series(&cycles) {
            println!("cluster-check: {} {line}", role.label());
        }
        if envelope_is_asserted(&resource_envelopes) {
            bail!(
                "{} campaign leaked resources: {}",
                role.label(),
                rose.join("; ")
            );
        }
        println!(
            "cluster-check: {} campaign resource envelope rose, not asserted with a closing \
             window of {} samples (the campaign asserts at {RESOURCE_ENVELOPE_WINDOW_SAMPLES}): {}",
            role.label(),
            resource_envelopes.closing_window_samples,
            rose.join("; ")
        );
    }

    Ok(RecoveryRoleCampaign {
        role,
        cycles_required,
        minimum_sqlite_bytes,
        worst_durations: worst_durations(&cycles),
        resource_envelopes,
        cycles,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SnapshotPurgeEvidence {
    snapshot_index: u64,
    purged_index: u64,
}

async fn trigger_and_wait_for_snapshot(
    cluster: &mut ClusterProcesses,
    leader: u64,
    role: RecoveryRole,
    cycle: u32,
    deadline: Instant,
    diagnostics: &RecoveryDiagnostics,
) -> Result<SnapshotPurgeEvidence> {
    let previous = request_status(cluster, leader).await?.snapshot_index;
    let mut status = observed_phase(diagnostics, "snapshot_trigger", cycle, None, async {
        for ordinal in 0..TRANSPORT_RECOVERY_SNAPSHOT_LOGS_SINCE_LAST.saturating_mul(3) {
            remaining_before(deadline, "snapshot trigger")?;
            cluster
                .request(
                    leader,
                    Request::PutSetting {
                        key: format!(
                            "cluster.transport-recovery.snapshot.{}.{cycle:02}.{ordinal:04}",
                            role.label()
                        ),
                        value: "snapshot-trigger".to_owned(),
                    },
                )
                .await?
                .require_ok()?;
            let status = request_status(cluster, leader).await?;
            if status
                .snapshot_index
                .is_some_and(|snapshot| previous.is_none_or(|old| snapshot > old))
            {
                return Ok(status);
            }
        }
        bail!("low-threshold workload did not publish a new snapshot")
    })
    .await?;
    let snapshot = status
        .snapshot_index
        .context("new snapshot index disappeared")?;
    status = observed_phase(diagnostics, "snapshot_purge", cycle, None, async {
        while status.purged_index.is_none_or(|purged| purged < snapshot) {
            if Instant::now() >= deadline {
                bail!("snapshot {snapshot} was not purged before recovery deadline");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            status = request_status(cluster, leader).await?;
        }
        Ok(status)
    })
    .await?;
    snapshot_purge_evidence(
        // Pin the snapshot whose purge boundary this function actually
        // awaited. The external writer can publish a newer current snapshot
        // between observations; substituting that later index here would
        // falsely claim the later boundary was also awaited.
        snapshot,
        status
            .purged_index
            .context("source purged index disappeared")?,
    )
}

fn snapshot_purge_evidence(
    snapshot_index: u64,
    purged_index: u64,
) -> Result<SnapshotPurgeEvidence> {
    if purged_index < snapshot_index {
        bail!("snapshot {snapshot_index} was not purged before recovery continued");
    }
    Ok(SnapshotPurgeEvidence {
        snapshot_index,
        purged_index,
    })
}

async fn wait_for_installed_snapshot(
    cluster: &mut ClusterProcesses,
    target: u64,
    snapshot_index: u64,
    snapshot_id: &str,
    deadline: Instant,
) -> Result<RecoveryRuntimeStatus> {
    loop {
        let status = request_status(cluster, target).await?;
        let complete = status
            .snapshot_index
            .is_some_and(|index| index >= snapshot_index)
            && status
                .applied_index
                .is_some_and(|index| index >= snapshot_index)
            && status.install_ok_count >= 1
            && status.transport.observations.iter().any(|observation| {
                observation.direction == SnapshotTransportDirection::Inbound
                    && observation.snapshot_id.as_deref() == Some(snapshot_id)
                    && observation.phase == SnapshotTransportPhase::Complete
            });
        if complete {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            bail!("target did not install snapshot {snapshot_id}/{snapshot_index}: {status:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_completed_source_outbound(
    cluster: &mut ClusterProcesses,
    source: u64,
    target: u64,
    snapshot_id: &str,
    deadline: Instant,
) -> Result<RecoveryRuntimeStatus> {
    loop {
        let status = request_status(cluster, source).await?;
        let complete = status.transport.observations.iter().any(|observation| {
            observation.observing_node_id == source
                && observation.peer_node_id == target
                && observation.direction == SnapshotTransportDirection::Outbound
                && observation.snapshot_id.as_deref() == Some(snapshot_id)
                && observation.phase == SnapshotTransportPhase::Complete
        });
        if complete {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            bail!("source {source} did not complete outbound snapshot {snapshot_id} to {target}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn source_outbound_evidence(
    status: &RecoveryRuntimeStatus,
    target_node_id: u64,
    snapshot_id: &str,
    snapshot_bytes: u64,
) -> Result<SourceOutboundTransportEvidence> {
    let observation = status
        .transport
        .observations
        .iter()
        .find(|observation| {
            observation.peer_node_id == target_node_id
                && observation.direction == SnapshotTransportDirection::Outbound
                && observation.snapshot_id.as_deref() == Some(snapshot_id)
                && observation.phase == SnapshotTransportPhase::Complete
        })
        .context("completed source outbound snapshot observation is missing")?;
    if observation.observing_node_id != status.transport.observing_node_id
        || observation.attempt_id == 0
        || observation.attempted_offset != Some(snapshot_bytes)
        || observation.acknowledged_offset != Some(snapshot_bytes)
        || observation.total_bytes != Some(snapshot_bytes)
        || observation.operation_owns_work
    {
        bail!("completed source outbound counters do not match snapshot: {observation:?}");
    }
    let attempt_age_ms = observation
        .attempt_age_ms
        .context("source outbound attempt timestamp is missing")?;
    let acknowledgement_age_ms = observation
        .last_acknowledgement_age_ms
        .context("source outbound completion timestamp is missing")?;
    let attempt_started_at_unix_ms = event_unix_ms(
        status.transport.observed_at_unix_ms,
        attempt_age_ms,
        "source outbound attempt",
    )?;
    let completed_at_unix_ms = event_unix_ms(
        status.transport.observed_at_unix_ms,
        acknowledgement_age_ms,
        "source outbound completion",
    )?;
    let elapsed_millis = elapsed_event_millis(
        attempt_started_at_unix_ms,
        completed_at_unix_ms,
        "source outbound transfer",
    )?;
    Ok(SourceOutboundTransportEvidence {
        observing_node_id: observation.observing_node_id,
        target_node_id,
        attempt_id: observation.attempt_id,
        attempt_started_at_unix_ms,
        completed_at_unix_ms,
        attempted_bytes: snapshot_bytes,
        acknowledged_bytes: snapshot_bytes,
        total_bytes: snapshot_bytes,
        attempts: observation.attempt_count,
        reconnect_count: observation.reconnect_count,
        retry_count: observation.retry_count,
        elapsed_millis,
    })
}

fn target_inbound_evidence(
    status: &RecoveryRuntimeStatus,
    source_node_id: u64,
    snapshot_id: &str,
    snapshot_bytes: u64,
) -> Result<TargetInboundTransportEvidence> {
    let observation = status
        .transport
        .observations
        .iter()
        .find(|observation| {
            observation.direction == SnapshotTransportDirection::Inbound
                && observation.snapshot_id.as_deref() == Some(snapshot_id)
                && observation.phase == SnapshotTransportPhase::Complete
        })
        .context("completed inbound snapshot observation is missing")?;
    if observation.observing_node_id != status.transport.observing_node_id
        || observation.peer_node_id != source_node_id
        || observation.attempt_id == 0
        || observation.locally_received_bytes != Some(snapshot_bytes)
        || observation.total_bytes != Some(snapshot_bytes)
        || observation.operation_owns_work
    {
        bail!("completed inbound transport counters do not match snapshot: {observation:?}");
    }
    let attempt_age_ms = observation
        .attempt_age_ms
        .context("target inbound receive timestamp is missing")?;
    let receive_age_ms = observation
        .last_local_receive_age_ms
        .context("target inbound completion timestamp is missing")?;
    let install_elapsed_nanos = status
        .last_install_elapsed_nanos
        .filter(|elapsed| *elapsed > 0)
        .context("target snapshot install timing is missing or zero")?;
    let install_millis = install_elapsed_nanos.div_ceil(1_000_000);
    let receive_started_at_unix_ms = event_unix_ms(
        status.transport.observed_at_unix_ms,
        attempt_age_ms,
        "target inbound receive",
    )?;
    let last_local_receive_at_unix_ms = event_unix_ms(
        status.transport.observed_at_unix_ms,
        receive_age_ms,
        "target inbound last local receive",
    )?;
    let install_completed_at_unix_ms = i64::try_from(
        status
            .last_install_observed_at_unix_ms
            .context("target snapshot install completion timestamp is missing")?,
    )?;
    let install_started_at_unix_ms = install_completed_at_unix_ms
        .checked_sub(i64::try_from(install_millis)?)
        .context("target install timestamp underflow")?;
    let transfer_millis = elapsed_event_millis(
        receive_started_at_unix_ms,
        install_started_at_unix_ms,
        "target inbound receive",
    )?;
    if last_local_receive_at_unix_ms < install_completed_at_unix_ms {
        bail!("target local receive timestamp precedes completed snapshot installation");
    }
    Ok(TargetInboundTransportEvidence {
        observing_node_id: observation.observing_node_id,
        source_node_id,
        attempt_id: observation.attempt_id,
        receive_started_at_unix_ms,
        last_local_receive_at_unix_ms,
        install_started_at_unix_ms,
        install_completed_at_unix_ms,
        received_bytes: snapshot_bytes,
        total_bytes: snapshot_bytes,
        transfer_millis,
        install_millis,
    })
}

fn event_unix_ms(observed_at_unix_ms: u64, age_ms: u64, label: &str) -> Result<i64> {
    let event = observed_at_unix_ms
        .checked_sub(age_ms)
        .with_context(|| format!("{label} age exceeds observation timestamp"))?;
    i64::try_from(event).with_context(|| format!("{label} timestamp exceeds i64"))
}

fn elapsed_event_millis(start: i64, finish: i64, label: &str) -> Result<u64> {
    let elapsed = finish
        .checked_sub(start)
        .filter(|elapsed| *elapsed > 0)
        .with_context(|| format!("{label} timestamps are missing or non-increasing"))?;
    u64::try_from(elapsed).with_context(|| format!("{label} duration exceeds u64"))
}

async fn request_status(
    cluster: &mut ClusterProcesses,
    node_id: u64,
) -> Result<RecoveryRuntimeStatus> {
    match cluster.request(node_id, Request::RecoveryStatus).await? {
        Response::RecoveryStatus { status } => Ok(status),
        response => bail!("unexpected recovery status response: {response:?}"),
    }
}

async fn request_resources(
    cluster: &mut ClusterProcesses,
    node_id: u64,
) -> Result<ProcessResourceCount> {
    match cluster.request(node_id, Request::ProcessResources).await? {
        Response::ProcessResources { resources } => Ok(resources),
        response => bail!("unexpected process-resource response: {response:?}"),
    }
}

async fn request_resources_for_nodes(
    cluster: &mut ClusterProcesses,
    node_ids: &[u64],
) -> Result<Vec<(u64, ProcessResourceCount)>> {
    let mut samples = Vec::with_capacity(node_ids.len());
    for &node_id in node_ids {
        samples.push((node_id, request_resources(cluster, node_id).await?));
    }
    Ok(samples)
}

async fn wait_for_applied_index(
    cluster: &mut ClusterProcesses,
    node_id: u64,
    expected_index: u64,
    deadline: Instant,
) -> Result<()> {
    loop {
        let status = request_status(cluster, node_id).await?;
        if status
            .applied_index
            .is_some_and(|applied| applied >= expected_index)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("node {node_id} did not apply seeded image index {expected_index}: {status:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Sample every node once it reports no transport work and its counts have
/// held still for [`RESOURCE_STABLE_SAMPLES`] consecutive samples.
///
/// This is a *sample*, not a verdict. It deliberately does not wait for the
/// counts to come under any ceiling: the per-peer transports a recovery used
/// are still open when `operation_owns_work` clears and drain tens of seconds
/// later, so a sample lands on one side or the other of that drain, and
/// holding the horizon open for the low side turned every high-side cycle
/// into a sixty-second expiry. The leak question is asked once per campaign,
/// by [`resource_envelopes_over_allowance`], where a drain cannot be mistaken
/// for growth.
async fn wait_for_stable_idle_resources(
    cluster: &mut ClusterProcesses,
    node_ids: &[u64],
    deadline: Instant,
    diagnostics: &RecoveryDiagnostics,
    cycle: u32,
) -> Result<Vec<(u64, ProcessResourceCount)>> {
    const PHASE: &str = "stable idle resource sampling";
    let mut previous = None;
    let mut stable_samples = 0_usize;
    let mut waiting_on: Option<String> = None;
    loop {
        let remaining = match remaining_before(deadline, PHASE) {
            Ok(remaining) => remaining,
            Err(error) => return Err(annotate_resource_wait(error, waiting_on.as_deref())),
        };
        let (busy_nodes, current) = tokio::time::timeout(remaining, async {
            let mut busy_nodes = Vec::new();
            for &node_id in node_ids {
                let status = request_status(cluster, node_id).await?;
                if status
                    .transport
                    .observations
                    .iter()
                    .any(|observation| observation.operation_owns_work)
                {
                    busy_nodes.push(node_id);
                }
            }
            Ok::<_, anyhow::Error>((
                busy_nodes,
                request_resources_for_nodes(cluster, node_ids).await?,
            ))
        })
        .await
        .with_context(|| {
            format!(
                "{PHASE} exceeded its absolute deadline, still waiting on {}",
                waiting_on.as_deref().unwrap_or("its first complete sample")
            )
        })??;
        let idle = busy_nodes.is_empty();
        if idle && previous.as_ref() == Some(&current) {
            stable_samples = stable_samples.saturating_add(1);
        } else if idle {
            stable_samples = 1;
        } else {
            stable_samples = 0;
        }
        diagnostics.record_resource_sample(
            cycle,
            &busy_nodes,
            &current,
            stable_samples,
            RESOURCE_STABLE_SAMPLES,
        )?;
        if stable_samples >= RESOURCE_STABLE_SAMPLES {
            return Ok(current);
        }
        waiting_on = Some(describe_resource_wait(
            &busy_nodes,
            &current,
            previous.as_deref(),
            stable_samples,
        ));
        previous = Some(current);
        let remaining = match remaining_before(deadline, PHASE) {
            Ok(remaining) => remaining,
            Err(error) => return Err(annotate_resource_wait(error, waiting_on.as_deref())),
        };
        tokio::time::sleep(RESOURCE_SAMPLE_INTERVAL.min(remaining)).await;
    }
}

/// Say what the sampler was still waiting for when its horizon ran out.
///
/// The bare message — "recovery deadline expired before stable idle resource
/// sampling" — cannot distinguish the two opposite things it means. A node
/// still owning transport work is a recovery that never finished, and must
/// never be waited out with a longer horizon. Every node already idle, with
/// only the repeat-sample requirement outstanding, is a loaded machine, and a
/// code change would be the wrong answer. This lane has expired here on
/// `main` and on three pull requests without either reader ever being able to
/// tell which it was.
fn annotate_resource_wait(error: anyhow::Error, waiting_on: Option<&str>) -> anyhow::Error {
    match waiting_on {
        Some(report) => error.context(format!("still waiting on {report}")),
        None => error.context("no resource sample completed inside the horizon"),
    }
}

fn describe_resource_wait(
    busy_nodes: &[u64],
    current: &[(u64, ProcessResourceCount)],
    previous: Option<&[(u64, ProcessResourceCount)]>,
    stable_samples: usize,
) -> String {
    if !busy_nodes.is_empty() {
        return format!(
            "node(s) {} still owning transport work",
            busy_nodes
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let moved = previous.map(|previous| sample_movement(previous, current));
    let movement = match moved.as_deref() {
        Some([]) => " (the last two samples were identical)".to_owned(),
        Some(moved) => format!(" (since the last sample: {})", moved.join("; ")),
        None => String::new(),
    };
    format!(
        "every node idle, with {stable_samples} of {RESOURCE_STABLE_SAMPLES} consecutive \
         repeated samples{movement}"
    )
}

/// What moved between two consecutive samples, one resource at a time.
///
/// When every node is idle and inside its baseline, the only thing left is the
/// repeat requirement, and then the whole question is whether the counts are
/// settling towards a value or jittering forever. A slow settle wants a longer
/// horizon; persistent jitter means exact equality is the wrong stability
/// proxy and no horizon will fix it. Naming the deltas is the difference
/// between those two, and it is the only thing the report still cannot say.
fn sample_movement(
    previous: &[(u64, ProcessResourceCount)],
    current: &[(u64, ProcessResourceCount)],
) -> Vec<String> {
    if previous.len() != current.len() {
        return vec![format!(
            "node count changed: {} then {}",
            previous.len(),
            current.len()
        )];
    }
    let mut moved = Vec::new();
    for ((previous_id, before), (current_id, after)) in previous.iter().zip(current) {
        if previous_id != current_id {
            moved.push(format!(
                "node order changed: {previous_id} then {current_id}"
            ));
            continue;
        }
        for (resource, before, after) in [
            ("threads", before.threads, after.threads),
            ("sockets", before.sockets, after.sockets),
            (
                "owned async tasks",
                before.owned_async_tasks,
                after.owned_async_tasks,
            ),
        ] {
            if before != after {
                moved.push(format!("node {previous_id} {resource} {before} -> {after}"));
            }
        }
    }
    moved
}

/// One resource sample as `node N t/s/a`, for the campaign's own log.
///
/// A failing campaign never writes its evidence file — it aborts before
/// `publish_artifact_atomically`, and the workflow's upload of
/// `cluster-transport-recovery.json` finds nothing — so the log is the only
/// channel a failure has. Without the per-cycle counts in it, deciding
/// whether a count is climbing or oscillating needs the campaign rebuilt and
/// re-run locally with the margins widened, which is what it cost the first
/// time. One line per cycle makes the next failure answer that from CI.
fn describe_resource_sample(sample: &[(u64, ProcessResourceCount)]) -> String {
    sample
        .iter()
        .map(|(node_id, counts)| {
            format!(
                "node {node_id} {}/{}/{}",
                counts.threads, counts.sockets, counts.owned_async_tasks
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe_phase_durations(durations: &BTreeMap<String, u64>) -> String {
    durations
        .iter()
        .map(|(phase, millis)| format!("{phase}={millis}ms"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe_timing_statistics(
    timings: &BTreeMap<String, crate::transport_recovery_diagnostics::TimingStatistic>,
) -> String {
    timings
        .iter()
        .map(|(phase, timing)| {
            format!(
                "{phase}[n={},median={}ms,max={}ms,total={}ms]",
                timing.sample_count,
                timing.median_millis,
                timing.maximum_millis,
                timing.total_millis
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The envelope allowances the campaign asserts against, as one value.
///
/// At run time these are the three constants; offline they are whatever the
/// artifact recorded, which its identity check pins back to the constants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResourceEnvelopeAllowances {
    threads: u64,
    sockets: u64,
    owned_async_tasks: u64,
}

impl ResourceBand {
    /// The band of `samples`: the floor is the elementwise minimum and the
    /// ceiling is the `sustained`-th highest value of each resource — the
    /// highest when `sustained` is one, the fourth-highest for the closing
    /// window, clamped to what a shorter window holds.
    ///
    /// Both edges are taken over recovery cycles alone. The warmup baseline is
    /// deliberately not a member of either window: it is sampled before the
    /// first recovery, while a node's pools are still cold, so it records a
    /// level the process leaves during warmup and never returns to. Admitting
    /// it to the opening floor would compare steady state against cold start
    /// and read one-time warmup growth as a leak.
    fn over(samples: &[ProcessResourceCount], sustained: usize) -> Option<Self> {
        let first = samples.first()?;
        let mut floor = first.clone();
        for sample in samples {
            floor.threads = floor.threads.min(sample.threads);
            floor.sockets = floor.sockets.min(sample.sockets);
            floor.owned_async_tasks = floor.owned_async_tasks.min(sample.owned_async_tasks);
        }
        let kth_highest = |pick: fn(&ProcessResourceCount) -> u64| {
            let mut values = samples.iter().map(pick).collect::<Vec<_>>();
            values.sort_unstable_by(|a, b| b.cmp(a));
            values[sustained.clamp(1, values.len()) - 1]
        };
        Some(Self {
            floor,
            ceiling: ProcessResourceCount {
                threads: kth_highest(|sample| sample.threads),
                sockets: kth_highest(|sample| sample.sockets),
                owned_async_tasks: kth_highest(|sample| sample.owned_async_tasks),
            },
        })
    }
}

/// Where the opening window ends for a series of `samples` observations.
///
/// The series is the warmup baseline followed by every recorded cycle, so the
/// count is one more than the cycle count. The opening window is the first
/// half rounded up: for the twenty-cycle campaign it ends at cycle 10, so ten
/// cycles face ten. For a three-cycle smoke it is cycle 1 against cycles 2
/// and 3.
///
/// The baseline is counted here only so that the split falls evenly; it is
/// not itself a member of either band. See [`ResourceBand::over`].
fn opening_window_last_cycle(samples: usize) -> u32 {
    let opening_len = samples.div_ceil(2);
    u32::try_from(opening_len.saturating_sub(1)).unwrap_or(u32::MAX)
}

/// Whether the closing window is long enough for the envelope to be a verdict
/// rather than a print. See [`RESOURCE_ENVELOPE_WINDOW_SAMPLES`].
fn envelope_is_asserted(envelopes: &RecoveryResourceEnvelopes) -> bool {
    usize::try_from(envelopes.closing_window_samples)
        .is_ok_and(|samples| samples >= RESOURCE_ENVELOPE_WINDOW_SAMPLES)
}

/// The per-node envelope of each half of the campaign, from the recorded
/// cycles.
///
/// The baseline is taken from the first cycle's record; the validator
/// separately refuses a campaign whose baseline differs between cycles, so
/// every cycle carries the same one. This refuses, rather than skips, a
/// cycle whose records are missing, reordered or mislabelled, and a cycle
/// numbered outside `1..=len` or twice: a window computed over a subset of
/// the cycles would prove nothing about the campaign. An empty campaign has
/// no envelopes.
fn resource_envelopes(cycles: &[RecoveryCycleEvidence]) -> Result<RecoveryResourceEnvelopes> {
    let Some(first) = cycles.first() else {
        bail!("a recovery campaign with no cycles has no resource envelope");
    };
    let mut seen = BTreeSet::new();
    for cycle in cycles {
        if cycle.cycle == 0 || cycle.cycle as usize > cycles.len() || !seen.insert(cycle.cycle) {
            bail!(
                "recovery cycle number {} is outside 1..={} or repeated",
                cycle.cycle,
                cycles.len()
            );
        }
        if cycle.node_resources.len() != first.node_resources.len() {
            bail!(
                "recovery cycle {} sampled {} nodes, cycle {} sampled {}",
                cycle.cycle,
                cycle.node_resources.len(),
                first.cycle,
                first.node_resources.len()
            );
        }
        for (expected, record) in first.node_resources.iter().zip(&cycle.node_resources) {
            if record.node_id != expected.node_id || record.kind != expected.kind {
                bail!(
                    "recovery cycle {} recorded node {} as {:?} where cycle {} recorded node {} as {:?}",
                    cycle.cycle,
                    record.node_id,
                    record.kind,
                    first.cycle,
                    expected.node_id,
                    expected.kind
                );
            }
        }
    }
    let opening_window_last_cycle = opening_window_last_cycle(cycles.len().saturating_add(1));
    let closing_window_samples = u32::try_from(cycles.len())
        .unwrap_or(u32::MAX)
        .saturating_sub(opening_window_last_cycle);
    let nodes = first
        .node_resources
        .iter()
        .enumerate()
        .map(|(position, sample)| {
            let (opening_samples, closing_samples): (Vec<_>, Vec<_>) = cycles
                .iter()
                .map(|cycle| {
                    (
                        cycle.cycle,
                        cycle.node_resources[position].post_quiescence.clone(),
                    )
                })
                .partition(|(cycle, _)| *cycle <= opening_window_last_cycle);
            let samples = |window: Vec<(u32, ProcessResourceCount)>| {
                window
                    .into_iter()
                    .map(|(_, sample)| sample)
                    .collect::<Vec<_>>()
            };
            let opening_samples = samples(opening_samples);
            let closing_samples = samples(closing_samples);
            // The opening window is empty only for a one-cycle campaign, whose
            // single cycle is the whole closing window; the baseline stands in
            // as that campaign's opening band because there is no cycle left to
            // form one. Such a campaign is far short of
            // RESOURCE_ENVELOPE_WINDOW_SAMPLES, so the envelope is a print
            // rather than a verdict and the stand-in decides nothing.
            let opening = ResourceBand::over(&opening_samples, 1).unwrap_or_else(|| ResourceBand {
                floor: sample.baseline.clone(),
                ceiling: sample.baseline.clone(),
            });
            // The last cycle is always past the opening window, so a
            // non-empty campaign always has a closing sample.
            let closing = ResourceBand::over(&closing_samples, RESOURCE_CEILING_SUSTAINED_SAMPLES)
                .context("closing window has no sample")?;
            Ok(NodeResourceEnvelope {
                node_id: sample.node_id,
                kind: sample.kind,
                opening,
                closing,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(RecoveryResourceEnvelopes {
        opening_window_last_cycle,
        closing_window_samples,
        closing_ceiling_sustained_samples: u32::try_from(RESOURCE_CEILING_SUSTAINED_SAMPLES)
            .unwrap_or(u32::MAX),
        nodes,
    })
}

/// Every node and resource whose closing band sits above its opening band by
/// more than the allowance, at either edge, named one at a time.
///
/// The single source of truth for the campaign's leak question: the run-time
/// check and the offline validator both ask it here, so the verdict and the
/// message it produces can never disagree about what counts as a leak.
fn resource_envelopes_over_allowance(
    envelopes: &RecoveryResourceEnvelopes,
    allowances: ResourceEnvelopeAllowances,
) -> Vec<String> {
    let mut over = Vec::new();
    for node in &envelopes.nodes {
        for (resource, opening, closing, allowance) in [
            (
                "threads",
                (node.opening.floor.threads, node.opening.ceiling.threads),
                (node.closing.floor.threads, node.closing.ceiling.threads),
                allowances.threads,
            ),
            (
                "sockets",
                (node.opening.floor.sockets, node.opening.ceiling.sockets),
                (node.closing.floor.sockets, node.closing.ceiling.sockets),
                allowances.sockets,
            ),
            (
                "owned async tasks",
                (
                    node.opening.floor.owned_async_tasks,
                    node.opening.ceiling.owned_async_tasks,
                ),
                (
                    node.closing.floor.owned_async_tasks,
                    node.closing.ceiling.owned_async_tasks,
                ),
                allowances.owned_async_tasks,
            ),
        ] {
            for (edge, before, after) in [
                ("floor", opening.0, closing.0),
                ("ceiling", opening.1, closing.1),
            ] {
                if after > before.saturating_add(allowance) {
                    over.push(format!(
                        "node {} {resource} {edge} rose {before} -> {after} (allowance {allowance})",
                        node.node_id
                    ));
                }
            }
        }
    }
    over
}

fn describe_band(band: &ResourceBand) -> String {
    format!(
        "{}-{}/{}-{}/{}-{}",
        band.floor.threads,
        band.ceiling.threads,
        band.floor.sockets,
        band.ceiling.sockets,
        band.floor.owned_async_tasks,
        band.ceiling.owned_async_tasks
    )
}

fn describe_resource_envelopes(envelopes: &RecoveryResourceEnvelopes) -> String {
    let nodes = envelopes
        .nodes
        .iter()
        .map(|node| {
            format!(
                "node {} {} -> {}",
                node.node_id,
                describe_band(&node.opening),
                describe_band(&node.closing)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "(floor-ceiling of threads/sockets/owned async tasks) opening window cycles 0..={} \
         against the closing {} samples{}: {nodes}",
        envelopes.opening_window_last_cycle,
        envelopes.closing_window_samples,
        if envelope_is_asserted(envelopes) {
            ""
        } else {
            ", recorded but not asserted"
        }
    )
}

/// The whole per-cycle series, one line per node, for the log of a campaign
/// whose envelope rose: the envelope says that a count grew, and this says at
/// which cycle. The snapshot source of each cycle is beside it, because the
/// leader carries the peers' connections and a leader that moved between the
/// halves is the one benign thing that looks exactly like a leak here.
fn describe_resource_series(cycles: &[RecoveryCycleEvidence]) -> Vec<String> {
    let Some(first) = cycles.first() else {
        return Vec::new();
    };
    let mut ordered = cycles.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|cycle| cycle.cycle);
    let cycles = ordered.as_slice();
    let sources = cycles
        .iter()
        .map(|cycle| cycle.source_outbound.observing_node_id.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let mut lines = vec![format!(
        "snapshot source per cycle (cycle 1 first) {sources}"
    )];
    lines.extend(
        first
            .node_resources
            .iter()
            .enumerate()
            .map(|(position, sample)| {
                let series = std::iter::once(&sample.baseline)
                    .chain(cycles.iter().filter_map(|cycle| {
                        cycle
                            .node_resources
                            .get(position)
                            .map(|record| &record.post_quiescence)
                    }))
                    .map(|counts| {
                        format!(
                            "{}/{}/{}",
                            counts.threads, counts.sockets, counts.owned_async_tasks
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("node {} series (cycle 0 first) {series}", sample.node_id)
            }),
    );
    lines
}

/// Record every node's sample beside the warmup baseline it will be read
/// against. Nothing here fails a cycle: a sample above the baseline is one
/// side of the per-peer transport drain as often as it is anything else, and
/// the campaign's leak question is asked over all the cycles at once.
fn collect_node_resource_evidence(
    baselines: &[(u64, ProcessResourceCount)],
    post_resources: &[(u64, ProcessResourceCount)],
) -> Result<Vec<RecoveryNodeResourceEvidence>> {
    if baselines.len() != post_resources.len() {
        bail!("resource baseline and post-quiescence node counts differ");
    }
    let mut evidence = Vec::with_capacity(baselines.len());
    for ((node_id, baseline), (post_node_id, post_quiescence)) in
        baselines.iter().zip(post_resources)
    {
        if node_id != post_node_id {
            bail!("resource baseline and post-quiescence node order differs");
        }
        evidence.push(RecoveryNodeResourceEvidence {
            node_id: *node_id,
            kind: if *node_id == TARGET_NODE {
                RecoveryResourceNodeKind::RestartedTarget
            } else {
                RecoveryResourceNodeKind::PersistentSourceVoter
            },
            baseline: baseline.clone(),
            post_quiescence: post_quiescence.clone(),
        });
    }
    Ok(evidence)
}

async fn spawn_writer(
    executable: &Path,
    root: &Path,
    specs: &[NodeSpec],
    prefix: &str,
    diagnostics: &RecoveryDiagnostics,
) -> Result<(RecoveryWriter, RecoveryWriterConfig)> {
    let control = root.join("transport-recovery-writers");
    std::fs::create_dir_all(&control).context("create recovery writer control directory")?;
    let stem = prefix.trim_end_matches('.').replace('.', "-");
    let config = RecoveryWriterConfig {
        addresses: specs[..3].iter().map(|node| node.api.clone()).collect(),
        prefix: prefix.to_owned(),
        ready_path: control.join(format!("{stem}.ready")),
        progress_path: control.join(format!("{stem}.progress.json")),
        start_path: control.join(format!("{stem}.start")),
        stop_path: control.join(format!("{stem}.stop")),
        interval_millis: WRITER_INTERVAL_MILLIS,
        max_operations: WRITER_MAX_OPERATIONS,
    };
    let config_path = control.join(format!("{stem}.json"));
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config)?)
        .context("write recovery writer config")?;
    let mut child = Command::new(executable)
        .arg("transport-recovery-writer")
        .arg(&config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .context("spawn separate recovery writer")?;
    let stdout = child.stdout.take().context("recovery writer stdout")?;
    let identity = diagnostics.register_process(
        child.id().context("recovery writer has no process id")?,
        "writer",
        None,
    )?;
    let stdout = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut stdout = stdout;
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await?;
        Ok(bytes)
    });
    Ok((
        RecoveryWriter {
            child,
            stdout: Some(stdout),
            identity: Some(identity),
        },
        config,
    ))
}

async fn supervise_recovery_writer<T, F>(
    writer: &mut RecoveryWriter,
    config: &RecoveryWriterConfig,
    ready_acknowledged_at_unix_ms: i64,
    recovery_deadline: Instant,
    recovery: F,
) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    tokio::pin!(recovery);
    let mut writer_exit = Box::pin(writer.child.wait());
    let mut progress_tick = tokio::time::interval(WRITER_PROGRESS_POLL);
    progress_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut acknowledged_writes = 1_usize;
    let mut last_acknowledged_at_unix_ms = ready_acknowledged_at_unix_ms;
    let mut last_progress = Instant::now();
    let deadline = tokio::time::Instant::from_std(recovery_deadline);

    loop {
        tokio::select! {
            biased;
            status = &mut writer_exit => {
                let status = status.context("wait for recovery writer during recovery")?;
                bail!("recovery writer exited before recovery completed with {status}");
            }
            _ = tokio::time::sleep_until(deadline) => {
                bail!("recovery exceeded its single absolute {} ms deadline", duration_millis(RECOVERY_DEADLINE));
            }
            result = &mut recovery => return result,
            _ = progress_tick.tick() => {
                if let Ok(progress) = read_writer_progress(&config.progress_path) {
                    if progress.acknowledged_writes < acknowledged_writes
                        || progress.last_acknowledged_at_unix_ms < last_acknowledged_at_unix_ms
                    {
                        bail!("recovery writer progress moved backwards: {progress:?}");
                    }
                    if progress.acknowledged_writes > acknowledged_writes {
                        acknowledged_writes = progress.acknowledged_writes;
                        last_acknowledged_at_unix_ms = progress.last_acknowledged_at_unix_ms;
                        last_progress = Instant::now();
                    }
                }
                if last_progress.elapsed() > WRITER_MAX_ACK_GAP {
                    bail!(
                        "recovery writer published no acknowledgement for more than {} ms",
                        duration_millis(WRITER_MAX_ACK_GAP)
                    );
                }
            }
        }
    }
}

fn read_writer_progress(path: &Path) -> Result<RecoveryWriterProgress> {
    serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("read recovery writer progress {path:?}"))?,
    )
    .context("decode recovery writer progress")
}

fn publish_writer_progress(
    config: &RecoveryWriterConfig,
    writes: &[AcknowledgedRecoveryWrite],
) -> Result<()> {
    let last = writes.last().context("publish empty writer progress")?;
    let progress = RecoveryWriterProgress {
        acknowledged_writes: writes.len(),
        last_acknowledged_at_unix_ms: last.acknowledged_at_unix_ms,
    };
    std::fs::write(&config.progress_path, serde_json::to_vec(&progress)?)
        .context("publish recovery writer progress")
}

async fn wait_writer(
    writer: &mut RecoveryWriter,
    diagnostics: &RecoveryDiagnostics,
) -> Result<RecoveryWriterReport> {
    let wait_result = tokio::time::timeout(Duration::from_secs(30), writer.child.wait())
        .await
        .context("recovery writer did not stop")?;
    let lifecycle_result = wait_result
        .as_ref()
        .map(|_| ())
        .map_err(|error| anyhow::anyhow!("{error}"));
    if let Some(identity) = writer.identity.as_ref() {
        diagnostics.process_action("reap", identity, &lifecycle_result)?;
    }
    let status = wait_result?;
    if !status.success() {
        bail!("recovery writer exited with {status}");
    }
    let bytes = writer
        .stdout
        .take()
        .context("recovery writer stdout reader missing")?
        .await
        .context("join recovery writer stdout reader")??;
    serde_json::from_slice(&bytes).context("decode recovery writer report")
}

async fn wait_for_path(path: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while !path.is_file() {
        if Instant::now() >= deadline {
            bail!("timed out waiting for {}", path.display());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(())
}

async fn wait_for_writer_ready(
    writer: &mut RecoveryWriter,
    path: &Path,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if path.is_file() {
            return Ok(());
        }
        if let Some(status) = writer
            .child
            .try_wait()
            .context("inspect recovery writer while waiting for readiness")?
        {
            bail!("recovery writer exited before publishing readiness with {status}");
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for recovery writer readiness at {}",
                path.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_write_digest(
    cluster: &mut ClusterProcesses,
    node_id: u64,
    prefix: &str,
    expected_count: usize,
    expected_digest: &str,
) -> Result<RecoveryWriteDigest> {
    let deadline = Instant::now() + RECOVERY_DEADLINE;
    loop {
        let digest = match cluster
            .request(
                node_id,
                Request::RecoveryWriteDigest {
                    prefix: prefix.to_owned(),
                },
            )
            .await?
        {
            Response::RecoveryWriteDigest { digest } => digest,
            response => bail!("unexpected recovery write digest response: {response:?}"),
        };
        if digest.count == u64::try_from(expected_count)? && digest.sha256 == expected_digest {
            return Ok(digest);
        }
        if Instant::now() >= deadline {
            bail!(
                "target write digest did not converge: expected {expected_count}/{expected_digest}, got {digest:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub async fn run_transport_recovery_writer(config_path: &Path) -> Result<()> {
    let config: RecoveryWriterConfig = serde_json::from_slice(
        &std::fs::read(config_path)
            .with_context(|| format!("read recovery writer config {config_path:?}"))?,
    )?;
    validate_writer_config(&config)?;
    let client = Client::remote(
        config.addresses.clone(),
        true,
        true,
        API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .context("connect recovery writer through production TLS stream")?;
    tokio::time::timeout(Duration::from_secs(30), client.wait_until_healthy_db())
        .await
        .context("recovery writer health deadline")?;
    let mut writes = Vec::new();
    for ordinal in 0..config.max_operations {
        if ordinal > 1 {
            tokio::time::sleep(Duration::from_millis(config.interval_millis)).await;
        }
        if ordinal > 1 && config.stop_path.is_file() {
            break;
        }
        let key = format!("{}{ordinal:06}", config.prefix);
        let value = format!("transport-recovery-ack-{ordinal:06}-{}", "x".repeat(128));
        let updated_at = unix_ms()? / 1_000;
        let execution = client
            .execute(
                RECOVERY_WRITE_SQL,
                params!(key.as_str(), value.as_str(), updated_at),
            )
            .await;
        if let Err(error) = execution {
            resolve_ambiguous_recovery_write(&client, &key, &value)
                .await
                .with_context(|| {
                    format!(
                        "recovery write {ordinal} returned {error}; its exact committed value could not be resolved"
                    )
                })?;
        }
        writes.push(AcknowledgedRecoveryWrite {
            key,
            value_sha256: hex::encode(Sha256::digest(value.as_bytes())),
            acknowledged_at_unix_ms: unix_ms()?,
        });
        publish_writer_progress(&config, &writes)?;
        if ordinal == 0 {
            let acknowledged_at = writes
                .last()
                .context("initial recovery writer acknowledgement")?
                .acknowledged_at_unix_ms;
            std::fs::write(&config.ready_path, format!("{acknowledged_at}\n"))
                .context("publish recovery writer readiness timestamp")?;
            wait_for_path(&config.start_path, Duration::from_secs(30)).await?;
        }
    }
    if !config.stop_path.is_file() {
        bail!("recovery writer exhausted its operation bound before stop signal");
    }
    let digest = acknowledged_write_digest(&writes);
    client.shutdown().await?;
    println!(
        "{}",
        serde_json::to_string(&RecoveryWriterReport { writes, digest })?
    );
    Ok(())
}

async fn resolve_ambiguous_recovery_write(client: &Client, key: &str, value: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match client
            .query_consistent_map::<RecoveryWriteRow, _>(
                "SELECT key, value FROM settings WHERE key = $1",
                params!(key),
            )
            .await
        {
            Ok(rows) => match classify_recovery_write(key, value, &rows)? {
                RecoveryWriteResolution::Exact => return Ok(()),
                RecoveryWriteResolution::Missing => {}
            },
            Err(error) if Instant::now() < deadline => {
                eprintln!("cluster-check: retry ambiguous write read for {key:?}: {error}");
            }
            Err(error) => {
                return Err(error).context("read ambiguous recovery write outcome consistently");
            }
        }
        if Instant::now() >= deadline {
            bail!("ambiguous recovery write {key:?} was not committed before resolution deadline");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn classify_recovery_write(
    key: &str,
    value: &str,
    rows: &[RecoveryWriteRow],
) -> Result<RecoveryWriteResolution> {
    match rows {
        [row] if row.key == key && row.value == value => Ok(RecoveryWriteResolution::Exact),
        [] => Ok(RecoveryWriteResolution::Missing),
        [row] => bail!(
            "ambiguous recovery write {key:?} resolved to unexpected key/value {:?}/{:?}",
            row.key,
            row.value
        ),
        _ => bail!(
            "ambiguous recovery write {key:?} resolved to {} rows",
            rows.len()
        ),
    }
}

fn validate_writer_config(config: &RecoveryWriterConfig) -> Result<()> {
    if config.addresses.len() != 3
        || config
            .addresses
            .iter()
            .any(|address| address.trim().is_empty())
        || config.prefix.is_empty()
        || config.interval_millis == 0
        || config.interval_millis > 1_000
        || config.max_operations < 2
        || config.max_operations > WRITER_MAX_OPERATIONS
        || config.progress_path == config.ready_path
        || config.progress_path == config.start_path
        || config.progress_path == config.stop_path
        || config.ready_path == config.stop_path
        || config.ready_path == config.start_path
        || config.start_path == config.stop_path
    {
        bail!("invalid transport-recovery writer configuration");
    }
    Ok(())
}

pub(super) async fn seed_recovery_image(
    client: &Client,
    minimum_bytes: u64,
    marker: &str,
) -> Result<RecoveryImageEvidence> {
    if !matches!(
        minimum_bytes,
        TRANSPORT_RECOVERY_SMALL_IMAGE_BYTES | TRANSPORT_RECOVERY_LARGE_IMAGE_BYTES
    ) || marker.trim().is_empty()
        || marker.len() > 128
    {
        bail!("recovery image request is outside the fixed campaign contract");
    }
    client
        .execute(
            "CREATE TABLE IF NOT EXISTS cluster_transport_recovery_image (\
             ordinal INTEGER PRIMARY KEY, marker TEXT NOT NULL, payload BLOB NOT NULL) STRICT",
            params!(),
        )
        .await?;
    client
        .execute("DELETE FROM cluster_transport_recovery_image", params!())
        .await?;
    let wanted = minimum_bytes.saturating_add(IMAGE_PADDING_BYTES);
    let rows = wanted.div_ceil(IMAGE_ROW_BYTES);
    for ordinal in 0..rows {
        client
            .execute(
                "INSERT INTO cluster_transport_recovery_image (ordinal, marker, payload) \
                 VALUES ($1, $2, zeroblob($3))",
                params!(
                    i64::try_from(ordinal)?,
                    marker,
                    i64::try_from(IMAGE_ROW_BYTES)?
                ),
            )
            .await?;
    }
    recovery_image_digest(client, minimum_bytes).await
}

pub(super) async fn recovery_image_digest(
    client: &Client,
    minimum_bytes: u64,
) -> Result<RecoveryImageEvidence> {
    let rows = client
        .query_map::<RecoveryImageRow, _>(
            "SELECT ordinal, marker, payload FROM cluster_transport_recovery_image ORDER BY ordinal",
            params!(),
        )
        .await?;
    let marker = rows
        .first()
        .map(|row| row.marker.clone())
        .context("recovery image is empty")?;
    let mut digest = Sha256::new();
    let mut payload_bytes = 0_u64;
    for (position, row) in rows.iter().enumerate() {
        if row.ordinal != i64::try_from(position)? || row.marker != marker {
            bail!("recovery image row order or marker drifted");
        }
        digest.update(row.ordinal.to_be_bytes());
        digest.update(u64::try_from(row.marker.len())?.to_be_bytes());
        digest.update(row.marker.as_bytes());
        digest.update(u64::try_from(row.payload.len())?.to_be_bytes());
        digest.update(&row.payload);
        payload_bytes = payload_bytes.saturating_add(u64::try_from(row.payload.len())?);
    }
    Ok(RecoveryImageEvidence {
        marker,
        minimum_sqlite_bytes: minimum_bytes,
        rows: u64::try_from(rows.len())?,
        payload_bytes,
        content_sha256: hex::encode(digest.finalize()),
    })
}

pub(super) async fn recovery_write_digest(
    client: &Client,
    prefix: &str,
) -> Result<RecoveryWriteDigest> {
    if prefix.is_empty() || prefix.len() > 128 || prefix.contains('%') || prefix.contains('_') {
        bail!("invalid recovery write prefix");
    }
    let rows = client
        .query_map::<RecoveryWriteRow, _>(
            "SELECT key, value FROM settings WHERE key LIKE $1 ORDER BY key",
            params!(format!("{prefix}%")),
        )
        .await?;
    let writes = rows
        .into_iter()
        .map(|row| AcknowledgedRecoveryWrite {
            key: row.key,
            value_sha256: hex::encode(Sha256::digest(row.value.as_bytes())),
            acknowledged_at_unix_ms: 0,
        })
        .collect::<Vec<_>>();
    Ok(RecoveryWriteDigest {
        count: u64::try_from(writes.len())?,
        sha256: acknowledged_write_digest(&writes),
    })
}

pub(super) async fn recovery_runtime_status(client: &Client) -> Result<RecoveryRuntimeStatus> {
    let metrics = client.metrics_db().await?;
    let snapshot_metrics = client.local_db_snapshot_metrics()?.snapshot();
    Ok(RecoveryRuntimeStatus {
        snapshot_index: metrics.snapshot.map(|log| log.index),
        purged_index: metrics.purged.map(|log| log.index),
        applied_index: metrics.last_applied.map(|log| log.index),
        install_ok_count: snapshot_metrics.install_ok.count,
        last_install_observed_at_unix_ms: snapshot_metrics
            .last_install
            .map(|last| last.observed_at_unix_ms),
        last_install_elapsed_nanos: snapshot_metrics.last_install.map(|last| last.elapsed_nanos),
        transport: client.local_snapshot_transport_status()?.snapshot(),
    })
}

pub(super) fn process_resources(_client: &Client) -> Result<ProcessResourceCount> {
    #[cfg(target_os = "linux")]
    {
        let threads = u64::try_from(std::fs::read_dir("/proc/self/task")?.count())?;
        let mut sockets = 0_u64;
        for entry in std::fs::read_dir("/proc/self/fd")? {
            let link = std::fs::read_link(entry?.path());
            if link
                .as_ref()
                .is_ok_and(|path| path.to_string_lossy().starts_with("socket:["))
            {
                sockets = sockets.saturating_add(1);
            }
        }
        let owned_async_tasks = _client
            .local_snapshot_transport_status()?
            .owned_async_task_count();
        Ok(ProcessResourceCount {
            threads,
            sockets,
            owned_async_tasks,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        bail!("transport-recovery process resource evidence requires Linux /proc")
    }
}

fn snapshot_file(root: &Path, node_id: u64) -> Result<SnapshotFileEvidence> {
    let snapshots = root
        .join(format!("node-{node_id}"))
        .join("state_machine/snapshots");
    let snapshot_id = std::fs::read_to_string(snapshots.join("current"))?
        .trim()
        .to_owned();
    snapshot_file_by_id(root, node_id, &snapshot_id)
}

fn snapshot_file_by_id(
    root: &Path,
    node_id: u64,
    snapshot_id: &str,
) -> Result<SnapshotFileEvidence> {
    if snapshot_id.is_empty()
        || snapshot_id.contains('/')
        || snapshot_id.contains('\\')
        || snapshot_id == "."
        || snapshot_id == ".."
    {
        bail!("invalid current snapshot pointer {snapshot_id:?}");
    }
    let snapshots = root
        .join(format!("node-{node_id}"))
        .join("state_machine/snapshots");
    let path = snapshots.join(snapshot_id);
    let mut file = File::open(&path).with_context(|| format!("open snapshot {path:?}"))?;
    let bytes = file.metadata()?.len();
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(SnapshotFileEvidence {
        snapshot_id: snapshot_id.to_owned(),
        bytes,
        sha256: hex::encode(digest.finalize()),
    })
}

async fn wait_for_installed_snapshot_file(
    root: &Path,
    node_id: u64,
    source_snapshot: &SnapshotFileEvidence,
    deadline: Instant,
) -> Result<SnapshotFileEvidence> {
    let mut last_mismatch = None;
    loop {
        match snapshot_file_by_id(root, node_id, &source_snapshot.snapshot_id) {
            Ok(installed_snapshot) if &installed_snapshot == source_snapshot => {
                return Ok(installed_snapshot);
            }
            Ok(installed_snapshot) => {
                last_mismatch = Some(format!(
                    "last observed {} bytes with sha256 {}",
                    installed_snapshot.bytes, installed_snapshot.sha256
                ));
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error).context("read installed snapshot file"),
        }
        if Instant::now() >= deadline {
            if let Some(last_mismatch) = last_mismatch {
                bail!(
                    "installed snapshot file {} did not match the source before the recovery deadline; {last_mismatch}",
                    source_snapshot.snapshot_id
                );
            }
            bail!(
                "installed snapshot file {} did not appear before the recovery deadline",
                source_snapshot.snapshot_id
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn delete_disposable_target(root: &Path, node_id: u64) -> Result<()> {
    let target = root.join(format!("node-{node_id}"));
    let expected_name = format!("node-{node_id}");
    if !root.is_absolute()
        || target.parent() != Some(root)
        || target.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str())
        || !target.starts_with(root)
    {
        bail!("refused unsafe recovery target deletion {target:?}");
    }
    if target.exists() {
        std::fs::remove_dir_all(&target)
            .with_context(|| format!("remove disposable recovery node {target:?}"))?;
    }
    Ok(())
}

async fn spawn_recovery_node(
    executable: &Path,
    cluster: &mut ClusterProcesses,
    launch: NodeLaunch,
    deadline: Instant,
    diagnostics: &RecoveryDiagnostics,
) -> Result<()> {
    if launch.root != cluster.root {
        bail!("recovery node root did not match the running cluster");
    }
    let index = usize::try_from(launch.node_id.saturating_sub(1))?;
    if cluster.nodes.len() <= index {
        cluster.nodes.resize_with(index + 1, || None);
    }
    if cluster.nodes[index].is_some() {
        bail!("recovery node {} is already running", launch.node_id);
    }
    let mut process = NodeProcess::spawn(executable, &launch)?;
    diagnostics.register_process(process.pid()?, "node", Some(launch.node_id))?;
    process
        .wait_ready_with_timeout(remaining_before(deadline, "recovery node readiness")?)
        .await?;
    cluster.nodes[index] = Some(process);
    Ok(())
}

fn acknowledged_write_digest(writes: &[AcknowledgedRecoveryWrite]) -> String {
    let mut digest = Sha256::new();
    for write in writes {
        digest.update(
            u64::try_from(write.key.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        digest.update(write.key.as_bytes());
        digest.update(write.value_sha256.as_bytes());
    }
    hex::encode(digest.finalize())
}

fn validate_acknowledged_writes(
    writes: &[AcknowledgedRecoveryWrite],
    recovery_started_at_unix_ms: i64,
    recovery_finished_at_unix_ms: i64,
) -> Result<()> {
    let unique_keys = writes
        .iter()
        .map(|write| write.key.as_str())
        .collect::<BTreeSet<_>>();
    if unique_keys.len() != writes.len()
        || writes
            .windows(2)
            .any(|pair| pair[0].acknowledged_at_unix_ms > pair[1].acknowledged_at_unix_ms)
    {
        bail!("recovery writer acknowledgements are duplicated or out of order");
    }
    let during_recovery = writes
        .iter()
        .filter(|write| {
            write.acknowledged_at_unix_ms >= recovery_started_at_unix_ms
                && write.acknowledged_at_unix_ms <= recovery_finished_at_unix_ms
        })
        .collect::<Vec<_>>();
    let cadence_millis = i64::try_from(WRITER_INTERVAL_MILLIS)?;
    let max_gap_millis = i64::try_from(duration_millis(WRITER_MAX_ACK_GAP))?;
    let first_in_window = during_recovery
        .first()
        .context("recovery writer did not publish an acknowledgement during recovery")?;
    let last_in_window = during_recovery
        .last()
        .context("recovery writer did not publish an acknowledgement during recovery")?;
    if during_recovery.len() < MIN_ACKNOWLEDGED_WRITES_DURING_RECOVERY
        || !during_recovery.windows(2).any(|pair| {
            pair[1]
                .acknowledged_at_unix_ms
                .saturating_sub(pair[0].acknowledged_at_unix_ms)
                >= cadence_millis
        })
        || during_recovery.windows(2).any(|pair| {
            pair[1]
                .acknowledged_at_unix_ms
                .saturating_sub(pair[0].acknowledged_at_unix_ms)
                > max_gap_millis
        })
        || first_in_window
            .acknowledged_at_unix_ms
            .saturating_sub(recovery_started_at_unix_ms)
            > max_gap_millis
        || recovery_finished_at_unix_ms.saturating_sub(last_in_window.acknowledged_at_unix_ms)
            > max_gap_millis
    {
        bail!(
            "recovery writer needs at least {MIN_ACKNOWLEDGED_WRITES_DURING_RECOVERY} in-window acknowledgements at {WRITER_INTERVAL_MILLIS} ms cadence with no gap or final tail above {} ms",
            duration_millis(WRITER_MAX_ACK_GAP)
        );
    }
    Ok(())
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn artifact_parent(output: &Path) -> &Path {
    output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn prepare_artifact_output(output: &Path) -> Result<()> {
    let parent = artifact_parent(output);
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create transport-recovery artifact directory {parent:?}"))?;
    match std::fs::remove_file(output) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("remove stale transport-recovery artifact {output:?}"));
        }
    }
    Ok(())
}

fn publish_artifact_atomically(output: &Path, bytes: &[u8]) -> Result<()> {
    let parent = artifact_parent(output);
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary transport-recovery artifact in {parent:?}"))?;
    temporary
        .as_file_mut()
        .write_all(bytes)
        .context("write temporary transport-recovery artifact")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync temporary transport-recovery artifact")?;
    temporary
        .persist(output)
        .map_err(|error| error.error)
        .with_context(|| format!("publish transport-recovery artifact {output:?}"))?;
    File::open(parent)
        .with_context(|| format!("open transport-recovery artifact directory {parent:?}"))?
        .sync_all()
        .with_context(|| format!("sync transport-recovery artifact directory {parent:?}"))?;
    Ok(())
}

fn remaining_before(deadline: Instant, phase: &str) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .with_context(|| format!("recovery deadline expired before {phase}"))
}

fn worst_durations(cycles: &[RecoveryCycleEvidence]) -> TransportRecoveryWorstDurations {
    TransportRecoveryWorstDurations {
        recovery_millis: cycles
            .iter()
            .map(|cycle| cycle.recovery_millis)
            .max()
            .unwrap_or(0),
        transfer_millis: cycles
            .iter()
            .map(|cycle| cycle.source_outbound.elapsed_millis)
            .max()
            .unwrap_or(0),
        install_millis: cycles
            .iter()
            .map(|cycle| cycle.target_inbound.install_millis)
            .max()
            .unwrap_or(0),
    }
}

fn validate_image(image: &RecoveryImageEvidence, minimum: u64, marker: &str) -> Result<()> {
    if image.marker != marker
        || image.minimum_sqlite_bytes != minimum
        || image.payload_bytes < minimum
        || image.rows == 0
        || !is_sha256(&image.content_sha256)
    {
        bail!("recovery SQLite image does not satisfy its fixed contract: {image:?}");
    }
    Ok(())
}

pub fn validate_transport_recovery_artifact(
    artifact: &ClusterTransportRecoveryArtifact,
) -> Result<()> {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../benchmarks/cluster-transport-recovery.schema.json"
    ))?;
    let validator = jsonschema::draft202012::new(&schema)
        .context("compile transport-recovery evidence schema")?;
    if !validator.is_valid(&serde_json::to_value(artifact)?) {
        bail!("transport-recovery artifact does not satisfy its closed schema");
    }
    if artifact.schema_version != TRANSPORT_RECOVERY_ARTIFACT_SCHEMA_VERSION
        || artifact.evidence_scope != EVIDENCE_SCOPE
        || artifact.platform != "linux"
        || !is_git_sha(&artifact.build_sha)
        || artifact.started_at_unix_ms > artifact.finished_at_unix_ms
        || artifact.snapshot_policy
            != (RecoverySnapshotPolicy {
                validation_only: true,
                logs_since_last: TRANSPORT_RECOVERY_SNAPSHOT_LOGS_SINCE_LAST,
                production_default_unchanged: true,
            })
        || !artifact.transport.tls_enabled
        || !artifact.transport.separate_node_processes
        || !artifact.transport.separate_writer_process
        || artifact.resource_thread_envelope_allowance != THREAD_ENVELOPE_ALLOWANCE
        || artifact.resource_socket_envelope_allowance != SOCKET_ENVELOPE_ALLOWANCE
        || artifact.resource_owned_async_task_envelope_allowance
            != OWNED_ASYNC_TASK_ENVELOPE_ALLOWANCE
        || artifact.resource_cleanup_horizon_millis != duration_millis(RESOURCE_CLEANUP_HORIZON)
        || artifact.resource_stable_samples != RESOURCE_STABLE_SAMPLES
    {
        bail!("transport-recovery artifact identity or execution contract drifted");
    }
    validate_role_campaign(
        &artifact.learner,
        RecoveryRole::Learner,
        TRANSPORT_RECOVERY_SMALL_IMAGE_BYTES,
        RoleValidationContext {
            cycles_required: TRANSPORT_RECOVERY_CYCLES_PER_ROLE,
            started_at_unix_ms: artifact.started_at_unix_ms,
            finished_at_unix_ms: artifact.finished_at_unix_ms,
            allowances: ResourceEnvelopeAllowances {
                threads: artifact.resource_thread_envelope_allowance,
                sockets: artifact.resource_socket_envelope_allowance,
                owned_async_tasks: artifact.resource_owned_async_task_envelope_allowance,
            },
            resource_envelope_asserted: true,
        },
    )?;
    validate_role_campaign(
        &artifact.voter,
        RecoveryRole::Voter,
        TRANSPORT_RECOVERY_LARGE_IMAGE_BYTES,
        RoleValidationContext {
            cycles_required: TRANSPORT_RECOVERY_CYCLES_PER_ROLE,
            started_at_unix_ms: artifact.started_at_unix_ms,
            finished_at_unix_ms: artifact.finished_at_unix_ms,
            allowances: ResourceEnvelopeAllowances {
                threads: artifact.resource_thread_envelope_allowance,
                sockets: artifact.resource_socket_envelope_allowance,
                owned_async_tasks: artifact.resource_owned_async_task_envelope_allowance,
            },
            resource_envelope_asserted: true,
        },
    )?;
    Ok(())
}

pub fn validate_transport_recovery_bytes(bytes: &[u8]) -> Result<()> {
    let artifact: ClusterTransportRecoveryArtifact =
        serde_json::from_slice(bytes).context("decode transport-recovery evidence")?;
    validate_transport_recovery_artifact(&artifact)
}

pub fn validate_transport_recovery_role_report(report: &TransportRecoveryRoleReport) -> Result<()> {
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../benchmarks/cluster-transport-recovery-role.schema.json"
    ))?;
    let validator = jsonschema::draft202012::new(&schema)
        .context("compile transport-recovery role-report schema")?;
    if !validator.is_valid(&serde_json::to_value(report)?) {
        bail!("transport-recovery role report does not satisfy its closed schema");
    }
    validate_execution_id(&report.execution_id)?;
    let cycles_required = report.campaign.cycles_required;
    if !(1..=TRANSPORT_RECOVERY_CYCLES_PER_ROLE).contains(&cycles_required) {
        bail!("transport-recovery role report cycle count is outside the supported range");
    }
    let recomputed_envelopes = resource_envelopes(&report.campaign.cycles)?;
    let expected_assertion = envelope_is_asserted(&recomputed_envelopes);
    if report.schema_version != TRANSPORT_RECOVERY_ROLE_REPORT_SCHEMA_VERSION
        || report.kind != ROLE_REPORT_KIND
        || report.platform != "linux"
        || !is_git_sha(&report.build_sha)
        || report.build_profile != build_profile_descriptor()
        || report.started_at_unix_ms > report.finished_at_unix_ms
        || report.scope != role_report_scope(cycles_required)
        || report.resource_envelope_asserted != expected_assertion
        || report.snapshot_policy != fixed_snapshot_policy()
        || report.transport != fixed_transport_contract()
        || report.resource_thread_envelope_allowance != THREAD_ENVELOPE_ALLOWANCE
        || report.resource_socket_envelope_allowance != SOCKET_ENVELOPE_ALLOWANCE
        || report.resource_owned_async_task_envelope_allowance
            != OWNED_ASYNC_TASK_ENVELOPE_ALLOWANCE
        || report.resource_cleanup_horizon_millis != duration_millis(RESOURCE_CLEANUP_HORIZON)
        || report.resource_stable_samples != RESOURCE_STABLE_SAMPLES
    {
        bail!("transport-recovery role report identity or execution contract drifted");
    }
    validate_role_campaign(
        &report.campaign,
        report.campaign.role,
        report.campaign.role.minimum_sqlite_bytes(),
        RoleValidationContext {
            cycles_required,
            started_at_unix_ms: report.started_at_unix_ms,
            finished_at_unix_ms: report.finished_at_unix_ms,
            allowances: ResourceEnvelopeAllowances {
                threads: report.resource_thread_envelope_allowance,
                sockets: report.resource_socket_envelope_allowance,
                owned_async_tasks: report.resource_owned_async_task_envelope_allowance,
            },
            resource_envelope_asserted: expected_assertion,
        },
    )
}

pub fn validate_transport_recovery_role_bytes(bytes: &[u8]) -> Result<()> {
    let report: TransportRecoveryRoleReport =
        serde_json::from_slice(bytes).context("decode transport-recovery role report")?;
    validate_transport_recovery_role_report(&report)
}

pub fn assemble_transport_recovery_reports(plan: RecoveryAssemblePlan) -> Result<()> {
    validate_execution_id(&plan.execution_id)?;
    validate_assemble_plan_paths(&plan)?;
    prepare_artifact_output(&plan.output)?;
    let voter_bytes = std::fs::read(&plan.voter)
        .with_context(|| format!("read voter transport-recovery role report {:?}", plan.voter))?;
    let learner_bytes = std::fs::read(&plan.learner).with_context(|| {
        format!(
            "read learner transport-recovery role report {:?}",
            plan.learner
        )
    })?;
    let voter: TransportRecoveryRoleReport = serde_json::from_slice(&voter_bytes)
        .context("decode voter transport-recovery role report")?;
    let learner: TransportRecoveryRoleReport = serde_json::from_slice(&learner_bytes)
        .context("decode learner transport-recovery role report")?;
    validate_transport_recovery_role_report(&voter)?;
    validate_transport_recovery_role_report(&learner)?;
    let build_sha =
        resolve_build_sha().context("resolve transport-recovery candidate SHA before assembly")?;
    if voter.campaign.role != RecoveryRole::Voter
        || learner.campaign.role != RecoveryRole::Learner
        || voter.execution_id != plan.execution_id
        || learner.execution_id != plan.execution_id
        || voter.build_sha != build_sha
        || learner.build_sha != build_sha
        || voter.build_profile != learner.build_profile
        || voter.scope != RecoveryRoleReportScope::QualificationRole
        || learner.scope != RecoveryRoleReportScope::QualificationRole
        || voter.campaign.cycles_required != TRANSPORT_RECOVERY_CYCLES_PER_ROLE
        || learner.campaign.cycles_required != TRANSPORT_RECOVERY_CYCLES_PER_ROLE
        || voter.snapshot_policy != learner.snapshot_policy
        || voter.transport != learner.transport
        || voter.resource_thread_envelope_allowance != learner.resource_thread_envelope_allowance
        || voter.resource_socket_envelope_allowance != learner.resource_socket_envelope_allowance
        || voter.resource_owned_async_task_envelope_allowance
            != learner.resource_owned_async_task_envelope_allowance
        || voter.resource_cleanup_horizon_millis != learner.resource_cleanup_horizon_millis
        || voter.resource_stable_samples != learner.resource_stable_samples
    {
        bail!("transport-recovery role reports do not form one qualified execution");
    }
    let artifact = ClusterTransportRecoveryArtifact {
        schema_version: TRANSPORT_RECOVERY_ARTIFACT_SCHEMA_VERSION,
        evidence_scope: EVIDENCE_SCOPE.to_owned(),
        build_sha,
        platform: "linux".to_owned(),
        started_at_unix_ms: voter.started_at_unix_ms.min(learner.started_at_unix_ms),
        finished_at_unix_ms: voter.finished_at_unix_ms.max(learner.finished_at_unix_ms),
        snapshot_policy: voter.snapshot_policy,
        transport: voter.transport,
        resource_thread_envelope_allowance: voter.resource_thread_envelope_allowance,
        resource_socket_envelope_allowance: voter.resource_socket_envelope_allowance,
        resource_owned_async_task_envelope_allowance: voter
            .resource_owned_async_task_envelope_allowance,
        resource_cleanup_horizon_millis: voter.resource_cleanup_horizon_millis,
        resource_stable_samples: voter.resource_stable_samples,
        learner: learner.campaign,
        voter: voter.campaign,
    };
    validate_transport_recovery_artifact(&artifact)?;
    let mut bytes = serde_json::to_vec_pretty(&artifact)?;
    bytes.push(b'\n');
    publish_artifact_atomically(&plan.output, &bytes)?;
    println!(
        "cluster-check: transport-recovery artifact {}",
        lexical_absolute(&plan.output)?.display()
    );
    Ok(())
}

#[derive(Clone, Copy)]
struct RoleValidationContext {
    cycles_required: u32,
    started_at_unix_ms: i64,
    finished_at_unix_ms: i64,
    allowances: ResourceEnvelopeAllowances,
    resource_envelope_asserted: bool,
}

fn validate_role_campaign(
    campaign: &RecoveryRoleCampaign,
    role: RecoveryRole,
    minimum: u64,
    context: RoleValidationContext,
) -> Result<()> {
    if campaign.role != role
        || campaign.cycles_required != context.cycles_required
        || campaign.minimum_sqlite_bytes != minimum
        || campaign.cycles.len() != context.cycles_required as usize
        || campaign.worst_durations != worst_durations(&campaign.cycles)
    {
        bail!("{} recovery campaign shape drifted", role.label());
    }
    validate_campaign_resource_envelopes(campaign, role, context)?;
    let mut snapshot_ids = BTreeSet::new();
    let mut established_resource_baselines = None;
    for (position, cycle) in campaign.cycles.iter().enumerate() {
        let expected_cycle = u32::try_from(position)?.saturating_add(1);
        let expected_marker = format!("transport-recovery-{}-image-v1", role.label());
        validate_image(&cycle.image, minimum, &expected_marker)?;
        validate_cycle_transport(cycle)?;
        validate_cycle_resources(cycle)?;
        let cycle_baselines = cycle
            .node_resources
            .iter()
            .map(|sample| (sample.node_id, sample.baseline.clone()))
            .collect::<Vec<_>>();
        if established_resource_baselines
            .as_ref()
            .is_some_and(|established| established != &cycle_baselines)
        {
            bail!(
                "{} recovery campaign rebased its resource baseline",
                role.label()
            );
        }
        established_resource_baselines.get_or_insert(cycle_baselines);
        validate_acknowledged_writes(
            &cycle.acknowledged_writes,
            cycle.recovery_started_at_unix_ms,
            cycle.recovery_finished_at_unix_ms,
        )?;
        let recorded_wall_recovery = elapsed_event_millis(
            cycle.recovery_started_at_unix_ms,
            cycle.recovery_finished_at_unix_ms,
            "recorded whole recovery",
        )?;
        let duration_drift = cycle.recovery_millis.abs_diff(recorded_wall_recovery);
        if cycle.role != role
            || cycle.cycle != expected_cycle
            || cycle.recovery_started_at_unix_ms >= cycle.recovery_finished_at_unix_ms
            || cycle.recovery_started_at_unix_ms < context.started_at_unix_ms
            || cycle.recovery_finished_at_unix_ms > context.finished_at_unix_ms
            || cycle.purged_index < cycle.snapshot_index
            || cycle.applied_index < cycle.snapshot_index
            || cycle.source_snapshot != cycle.installed_snapshot
            || cycle.source_snapshot.bytes < minimum
            || !is_sha256(&cycle.source_snapshot.sha256)
            || cycle.recovery_millis == 0
            || cycle.recovery_millis > duration_millis(RECOVERY_DEADLINE)
            || recorded_wall_recovery > duration_millis(RECOVERY_DEADLINE)
            || duration_drift > 1_000
            || cycle.acknowledged_writes.len()
                < MIN_ACKNOWLEDGED_WRITES_DURING_RECOVERY.saturating_add(1)
            || cycle.acknowledged_writes.first().is_none_or(|write| {
                write.acknowledged_at_unix_ms >= cycle.recovery_started_at_unix_ms
            })
            || cycle.acknowledged_write_digest
                != acknowledged_write_digest(&cycle.acknowledged_writes)
            || cycle.target_write_digest != cycle.acknowledged_write_digest
            || cycle.acknowledged_writes.iter().any(|write| {
                write.key.is_empty()
                    || !is_sha256(&write.value_sha256)
                    || write.acknowledged_at_unix_ms < context.started_at_unix_ms
                    || write.acknowledged_at_unix_ms > context.finished_at_unix_ms
            })
        {
            bail!(
                "{} recovery cycle {expected_cycle} failed evidence validation",
                role.label()
            );
        }
        if !snapshot_ids.insert(cycle.source_snapshot.snapshot_id.clone()) {
            bail!("{} recovery campaign reused a snapshot image", role.label());
        }
    }
    Ok(())
}

fn validate_cycle_transport(cycle: &RecoveryCycleEvidence) -> Result<()> {
    let source = &cycle.source_outbound;
    let target = &cycle.target_inbound;
    let source_elapsed = elapsed_event_millis(
        source.attempt_started_at_unix_ms,
        source.completed_at_unix_ms,
        "recorded source outbound transfer",
    )?;
    let target_transfer = elapsed_event_millis(
        target.receive_started_at_unix_ms,
        target.install_started_at_unix_ms,
        "recorded target inbound receive",
    )?;
    let target_install = elapsed_event_millis(
        target.install_started_at_unix_ms,
        target.install_completed_at_unix_ms,
        "recorded target snapshot install",
    )?;
    if !(1..TARGET_NODE).contains(&source.observing_node_id)
        || source.target_node_id != TARGET_NODE
        || source.attempt_id == 0
        || source.attempt_started_at_unix_ms < cycle.recovery_started_at_unix_ms
        || source.completed_at_unix_ms > cycle.recovery_finished_at_unix_ms
        || source.attempted_bytes != cycle.source_snapshot.bytes
        || source.acknowledged_bytes != cycle.source_snapshot.bytes
        || source.total_bytes != cycle.source_snapshot.bytes
        || source.attempts == 0
        || source.attempts < source.retry_count.saturating_add(1)
        || source.elapsed_millis != source_elapsed
        || target.observing_node_id != TARGET_NODE
        || target.source_node_id != source.observing_node_id
        || target.attempt_id == 0
        || timestamp_precedes_beyond_tolerance(
            target.receive_started_at_unix_ms,
            source.attempt_started_at_unix_ms,
        )
        || target.last_local_receive_at_unix_ms < target.install_completed_at_unix_ms
        || timestamp_follows_beyond_tolerance(
            target.last_local_receive_at_unix_ms,
            source.completed_at_unix_ms,
        )
        || timestamp_follows_beyond_tolerance(
            target.install_completed_at_unix_ms,
            source.completed_at_unix_ms,
        )
        || target.received_bytes != cycle.source_snapshot.bytes
        || target.total_bytes != cycle.source_snapshot.bytes
        || target.transfer_millis != target_transfer
        || target.install_millis != target_install
    {
        bail!("recovery cycle transport evidence is incomplete or inconsistent");
    }
    Ok(())
}

fn timestamp_precedes_beyond_tolerance(value: i64, floor: i64) -> bool {
    value < floor.saturating_sub(CROSS_PROCESS_TIMESTAMP_TOLERANCE_MILLIS)
}

fn timestamp_follows_beyond_tolerance(value: i64, ceiling: i64) -> bool {
    value > ceiling.saturating_add(CROSS_PROCESS_TIMESTAMP_TOLERANCE_MILLIS)
}

/// The campaign's leak question, asked again offline from the recorded cycles.
///
/// The recorded envelopes must be exactly what the cycles produce — an
/// envelope written by hand proves nothing — the closing window must be the
/// full ten samples the assertion is designed for, and then no closing band
/// may sit above its opening band by more than the allowance the artifact
/// recorded, which the identity check has already pinned to the constants.
/// This is what makes the artifact self-proving without the harness: anyone
/// holding the file can recompute both halves. `resource_envelopes` refuses
/// a campaign whose cycle records are incomplete, reordered or renumbered on
/// its own, so this does not depend on the per-cycle checks running first.
fn validate_campaign_resource_envelopes(
    campaign: &RecoveryRoleCampaign,
    role: RecoveryRole,
    context: RoleValidationContext,
) -> Result<()> {
    let recomputed = resource_envelopes(&campaign.cycles)
        .with_context(|| format!("{} recovery campaign resource records", role.label()))?;
    if campaign.resource_envelopes != recomputed
        || recomputed.nodes.len() != TARGET_NODE as usize
        || recomputed.opening_window_last_cycle
            != opening_window_last_cycle(context.cycles_required as usize + 1)
        || envelope_is_asserted(&recomputed) != context.resource_envelope_asserted
    {
        bail!(
            "{} recovery campaign resource envelopes do not match its cycles",
            role.label()
        );
    }
    if context.resource_envelope_asserted {
        let rose = resource_envelopes_over_allowance(&recomputed, context.allowances);
        if !rose.is_empty() {
            bail!(
                "{} recovery campaign leaked resources: {}",
                role.label(),
                rose.join("; ")
            );
        }
    }
    Ok(())
}

/// Each cycle's resource record has to be complete and in order. Whether a
/// count is high is not a per-cycle question any more; see
/// [`validate_campaign_resource_envelopes`].
fn validate_cycle_resources(cycle: &RecoveryCycleEvidence) -> Result<()> {
    if cycle.node_resources.len() != TARGET_NODE as usize {
        bail!("recovery cycle must sample all four cluster processes");
    }
    for (position, sample) in cycle.node_resources.iter().enumerate() {
        let expected_node = u64::try_from(position)?.saturating_add(1);
        let expected_kind = if expected_node == TARGET_NODE {
            RecoveryResourceNodeKind::RestartedTarget
        } else {
            RecoveryResourceNodeKind::PersistentSourceVoter
        };
        if sample.node_id != expected_node || sample.kind != expected_kind {
            bail!("node {expected_node} resource evidence is out of order or mislabelled");
        }
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_git_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(ordinal: u32, at: i64) -> AcknowledgedRecoveryWrite {
        AcknowledgedRecoveryWrite {
            key: format!("cluster.transport-recovery.learner.01.{ordinal:06}"),
            value_sha256: "b".repeat(64),
            acknowledged_at_unix_ms: at,
        }
    }

    fn role_campaign_with_cycles(
        role: RecoveryRole,
        minimum: u64,
        cycles_required: u32,
    ) -> RecoveryRoleCampaign {
        let marker = format!("transport-recovery-{}-image-v1", role.label());
        let cycles = (1..=cycles_required)
            .map(|cycle| {
                let writes = vec![
                    write(cycle.saturating_mul(3), 1_500),
                    write(cycle.saturating_mul(3).saturating_add(1), 2_500),
                    write(cycle.saturating_mul(3).saturating_add(2), 3_500),
                ];
                let digest = acknowledged_write_digest(&writes);
                let snapshot = SnapshotFileEvidence {
                    snapshot_id: format!("00000000-0000-7000-8000-{cycle:012}"),
                    bytes: minimum,
                    sha256: "a".repeat(64),
                };
                RecoveryCycleEvidence {
                    role,
                    cycle,
                    recovery_started_at_unix_ms: 2_000,
                    recovery_finished_at_unix_ms: 7_000,
                    snapshot_index: u64::from(cycle) * 300,
                    purged_index: u64::from(cycle) * 300,
                    applied_index: u64::from(cycle) * 300 + 1,
                    source_snapshot: snapshot.clone(),
                    installed_snapshot: snapshot,
                    image: RecoveryImageEvidence {
                        marker: marker.clone(),
                        minimum_sqlite_bytes: minimum,
                        rows: 24,
                        payload_bytes: minimum,
                        content_sha256: "c".repeat(64),
                    },
                    source_outbound: SourceOutboundTransportEvidence {
                        observing_node_id: 1,
                        target_node_id: TARGET_NODE,
                        attempt_id: u64::from(cycle),
                        attempt_started_at_unix_ms: 2_100,
                        completed_at_unix_ms: 6_000,
                        attempted_bytes: minimum,
                        acknowledged_bytes: minimum,
                        total_bytes: minimum,
                        attempts: 1,
                        reconnect_count: 0,
                        retry_count: 0,
                        elapsed_millis: 3_900,
                    },
                    target_inbound: TargetInboundTransportEvidence {
                        observing_node_id: TARGET_NODE,
                        source_node_id: 1,
                        attempt_id: u64::from(cycle),
                        receive_started_at_unix_ms: 2_200,
                        last_local_receive_at_unix_ms: 5_510,
                        install_started_at_unix_ms: 5_000,
                        install_completed_at_unix_ms: 5_500,
                        received_bytes: minimum,
                        total_bytes: minimum,
                        transfer_millis: 2_800,
                        install_millis: 500,
                    },
                    recovery_millis: 5_000,
                    acknowledged_writes: writes,
                    acknowledged_write_digest: digest.clone(),
                    target_write_digest: digest,
                    node_resources: (1..=TARGET_NODE)
                        .map(|node_id| RecoveryNodeResourceEvidence {
                            node_id,
                            kind: if node_id == TARGET_NODE {
                                RecoveryResourceNodeKind::RestartedTarget
                            } else {
                                RecoveryResourceNodeKind::PersistentSourceVoter
                            },
                            baseline: ProcessResourceCount {
                                threads: 12,
                                sockets: 8,
                                owned_async_tasks: 10,
                            },
                            post_quiescence: ProcessResourceCount {
                                threads: 12,
                                sockets: 8,
                                owned_async_tasks: 10,
                            },
                        })
                        .collect(),
                }
            })
            .collect::<Vec<_>>();
        RecoveryRoleCampaign {
            role,
            cycles_required,
            minimum_sqlite_bytes: minimum,
            worst_durations: worst_durations(&cycles),
            resource_envelopes: resource_envelopes(&cycles).expect("fixture envelopes"),
            cycles,
        }
    }

    fn role_campaign(role: RecoveryRole, minimum: u64) -> RecoveryRoleCampaign {
        role_campaign_with_cycles(role, minimum, TRANSPORT_RECOVERY_CYCLES_PER_ROLE)
    }

    /// Rewrite one node's post-quiescence series across a campaign and
    /// re-derive the floors from it, the way the harness would have recorded
    /// them. `series(cycle)` is the count for `cycle` in `1..=20`.
    fn with_node_series(
        campaign: &mut RecoveryRoleCampaign,
        node_index: usize,
        series: impl Fn(u32) -> ProcessResourceCount,
    ) {
        for cycle in &mut campaign.cycles {
            cycle.node_resources[node_index].post_quiescence = series(cycle.cycle);
        }
        campaign.resource_envelopes =
            resource_envelopes(&campaign.cycles).expect("rewritten envelopes");
    }

    fn counts(threads: u64, sockets: u64, owned_async_tasks: u64) -> ProcessResourceCount {
        ProcessResourceCount {
            threads,
            sockets,
            owned_async_tasks,
        }
    }

    fn artifact() -> ClusterTransportRecoveryArtifact {
        ClusterTransportRecoveryArtifact {
            schema_version: TRANSPORT_RECOVERY_ARTIFACT_SCHEMA_VERSION,
            evidence_scope: EVIDENCE_SCOPE.to_owned(),
            build_sha: "d".repeat(40),
            platform: "linux".to_owned(),
            started_at_unix_ms: 1_000,
            finished_at_unix_ms: 10_000,
            snapshot_policy: RecoverySnapshotPolicy {
                validation_only: true,
                logs_since_last: TRANSPORT_RECOVERY_SNAPSHOT_LOGS_SINCE_LAST,
                production_default_unchanged: true,
            },
            transport: RecoveryTransportContract {
                raft_transport: "hiqlite_production_tls_chunked_snapshot".to_owned(),
                api_writer_transport: "hiqlite_production_tls_websocket".to_owned(),
                tls_enabled: true,
                tls_certificate_verification_disabled_for_self_signed_harness: true,
                separate_node_processes: true,
                separate_writer_process: true,
            },
            resource_thread_envelope_allowance: THREAD_ENVELOPE_ALLOWANCE,
            resource_socket_envelope_allowance: SOCKET_ENVELOPE_ALLOWANCE,
            resource_owned_async_task_envelope_allowance: OWNED_ASYNC_TASK_ENVELOPE_ALLOWANCE,
            resource_cleanup_horizon_millis: duration_millis(RESOURCE_CLEANUP_HORIZON),
            resource_stable_samples: RESOURCE_STABLE_SAMPLES,
            learner: role_campaign(RecoveryRole::Learner, TRANSPORT_RECOVERY_SMALL_IMAGE_BYTES),
            voter: role_campaign(RecoveryRole::Voter, TRANSPORT_RECOVERY_LARGE_IMAGE_BYTES),
        }
    }

    fn role_report(role: RecoveryRole, cycles_required: u32) -> TransportRecoveryRoleReport {
        let campaign =
            role_campaign_with_cycles(role, role.minimum_sqlite_bytes(), cycles_required);
        TransportRecoveryRoleReport {
            schema_version: TRANSPORT_RECOVERY_ROLE_REPORT_SCHEMA_VERSION,
            kind: ROLE_REPORT_KIND.to_owned(),
            execution_id: "test-execution-1".to_owned(),
            build_sha: "d".repeat(40),
            platform: "linux".to_owned(),
            build_profile: build_profile_descriptor(),
            started_at_unix_ms: 1_000,
            finished_at_unix_ms: 10_000,
            scope: role_report_scope(cycles_required),
            resource_envelope_asserted: envelope_is_asserted(&campaign.resource_envelopes),
            snapshot_policy: fixed_snapshot_policy(),
            transport: fixed_transport_contract(),
            resource_thread_envelope_allowance: THREAD_ENVELOPE_ALLOWANCE,
            resource_socket_envelope_allowance: SOCKET_ENVELOPE_ALLOWANCE,
            resource_owned_async_task_envelope_allowance: OWNED_ASYNC_TASK_ENVELOPE_ALLOWANCE,
            resource_cleanup_horizon_millis: duration_millis(RESOURCE_CLEANUP_HORIZON),
            resource_stable_samples: RESOURCE_STABLE_SAMPLES,
            campaign,
        }
    }

    fn role_plan(role: RecoveryRole, root: &Path) -> RecoveryRolePlan {
        RecoveryRolePlan {
            role,
            cycles_required: 3,
            execution_id: "test-execution-1".to_owned(),
            output: root.join(format!("{}-role.json", role.label())),
            diagnostics_dir: root.join(format!("{}-diagnostics", role.label())),
            max_runtime_seconds: None,
        }
    }

    #[test]
    fn complete_twenty_plus_twenty_artifact_is_accepted() {
        validate_transport_recovery_artifact(&artifact()).expect("valid recovery artifact");
    }

    #[test]
    fn role_cli_requires_every_explicit_input_and_rejects_ambiguous_paths() {
        let valid = vec![
            "--role".to_owned(),
            "learner".to_owned(),
            "--cycles".to_owned(),
            "3".to_owned(),
            "--execution-id".to_owned(),
            "run_1.ok".to_owned(),
            "--output".to_owned(),
            "target/role.json".to_owned(),
            "--diagnostics-dir".to_owned(),
            "target/diagnostics".to_owned(),
            "--max-runtime-seconds".to_owned(),
            "30".to_owned(),
        ];
        let plan = parse_recovery_role_plan(&valid).expect("valid role plan");
        assert_eq!(plan.role, RecoveryRole::Learner);
        assert_eq!(plan.cycles_required, 3);
        assert_eq!(plan.max_runtime_seconds, Some(30));

        for invalid in [
            vec![],
            vec!["--unknown".to_owned(), "x".to_owned()],
            vec!["--role".to_owned()],
            [valid.clone(), vec!["--role".to_owned(), "voter".to_owned()]].concat(),
        ] {
            assert!(parse_recovery_role_plan(&invalid).is_err(), "{invalid:?}");
        }
        for (position, replacement) in [(1, "unsupported"), (3, "0"), (5, "bad/path"), (11, "0")] {
            let mut invalid = valid.clone();
            invalid[position] = replacement.to_owned();
            assert!(parse_recovery_role_plan(&invalid).is_err(), "{invalid:?}");
        }
        let mut colliding = valid;
        colliding[9] = "target/role.json/diagnostics".to_owned();
        assert!(parse_recovery_role_plan(&colliding).is_err());
    }

    #[test]
    fn assembly_cli_is_strict_and_refuses_path_collisions() {
        let valid = vec![
            "--execution-id".to_owned(),
            "paired-1".to_owned(),
            "--voter".to_owned(),
            "voter.json".to_owned(),
            "--learner".to_owned(),
            "learner.json".to_owned(),
            "--output".to_owned(),
            "combined.json".to_owned(),
        ];
        assert!(parse_recovery_assemble_plan(&valid).is_ok());
        for invalid in [
            vec![],
            vec!["--unexpected".to_owned(), "x".to_owned()],
            [
                valid.clone(),
                vec!["--voter".to_owned(), "another.json".to_owned()],
            ]
            .concat(),
        ] {
            assert!(parse_recovery_assemble_plan(&invalid).is_err());
        }
        let mut colliding = valid;
        colliding[7] = "voter.json".to_owned();
        assert!(parse_recovery_assemble_plan(&colliding).is_err());
    }

    #[test]
    fn bounded_role_reports_derive_scope_and_resource_assertion() {
        for (cycles, scope, asserted) in [
            (1, RecoveryRoleReportScope::Smoke, false),
            (18, RecoveryRoleReportScope::Smoke, false),
            (19, RecoveryRoleReportScope::Smoke, true),
            (20, RecoveryRoleReportScope::QualificationRole, true),
        ] {
            let report = role_report(RecoveryRole::Voter, cycles);
            assert_eq!(report.scope, scope);
            assert_eq!(report.resource_envelope_asserted, asserted);
            validate_transport_recovery_role_report(&report)
                .unwrap_or_else(|error| panic!("{cycles}-cycle report failed: {error:#}"));
        }
    }

    #[test]
    fn purge_evidence_pins_the_boundary_that_was_awaited() {
        let evidence = snapshot_purge_evidence(1_278, 1_278).expect("awaited purge boundary");
        assert_eq!(evidence.snapshot_index, 1_278);
        assert_eq!(evidence.purged_index, 1_278);
        assert!(snapshot_purge_evidence(1_279, 1_278).is_err());
    }

    #[test]
    fn cross_process_timestamp_tolerance_is_bounded() {
        assert!(!timestamp_precedes_beyond_tolerance(998, 1_000));
        assert!(timestamp_precedes_beyond_tolerance(997, 1_000));
        assert!(!timestamp_follows_beyond_tolerance(1_002, 1_000));
        assert!(timestamp_follows_beyond_tolerance(1_003, 1_000));
    }

    #[test]
    fn role_report_refuses_identity_contract_and_cycle_drift() {
        let valid = role_report(RecoveryRole::Voter, 3);
        for mutate in [
            |value: &mut TransportRecoveryRoleReport| value.execution_id = "bad/id".to_owned(),
            |value: &mut TransportRecoveryRoleReport| value.build_sha = "x".repeat(40),
            |value: &mut TransportRecoveryRoleReport| value.build_profile.clear(),
            |value: &mut TransportRecoveryRoleReport| {
                value.scope = RecoveryRoleReportScope::QualificationRole
            },
            |value: &mut TransportRecoveryRoleReport| value.resource_envelope_asserted = true,
            |value: &mut TransportRecoveryRoleReport| {
                value.transport.separate_writer_process = false
            },
            |value: &mut TransportRecoveryRoleReport| value.campaign.cycles[0].cycle = 2,
        ] {
            let mut value = valid.clone();
            mutate(&mut value);
            assert!(validate_transport_recovery_role_report(&value).is_err());
        }
    }

    #[test]
    fn role_report_bytes_reject_unknown_top_level_and_campaign_fields() {
        for path in ["unexpected", "campaign.unexpected"] {
            let mut value = serde_json::to_value(role_report(RecoveryRole::Learner, 3))
                .expect("serialize role report");
            if path == "unexpected" {
                value["unexpected"] = serde_json::json!(true);
            } else {
                value["campaign"]["unexpected"] = serde_json::json!(true);
            }
            let bytes = serde_json::to_vec(&value).expect("encode mutated role report");
            assert!(validate_transport_recovery_role_bytes(&bytes).is_err());
        }
    }

    #[test]
    fn assembler_requires_two_qualified_reports_from_one_execution() {
        let root = tempfile::tempdir().expect("assembly root");
        let voter_path = root.path().join("voter.json");
        let learner_path = root.path().join("learner.json");
        let output = root.path().join("combined.json");
        let build_sha = resolve_build_sha().expect("test checkout SHA");
        let mut voter = role_report(RecoveryRole::Voter, 20);
        voter.build_sha.clone_from(&build_sha);
        let mut learner = role_report(RecoveryRole::Learner, 20);
        learner.build_sha.clone_from(&build_sha);
        std::fs::write(
            &voter_path,
            serde_json::to_vec(&voter).expect("encode voter"),
        )
        .expect("write voter");
        std::fs::write(
            &learner_path,
            serde_json::to_vec(&learner).expect("encode learner"),
        )
        .expect("write learner");
        assemble_transport_recovery_reports(RecoveryAssemblePlan {
            execution_id: "test-execution-1".to_owned(),
            voter: voter_path.clone(),
            learner: learner_path.clone(),
            output: output.clone(),
        })
        .expect("assemble qualified reports");
        validate_transport_recovery_bytes(&std::fs::read(&output).expect("read artifact"))
            .expect("validate assembled canonical artifact");

        voter = role_report(RecoveryRole::Voter, 3);
        voter.build_sha = build_sha;
        std::fs::write(
            &voter_path,
            serde_json::to_vec(&voter).expect("encode smoke voter"),
        )
        .expect("write smoke voter");
        std::fs::write(&output, b"stale success").expect("seed stale output");
        assert!(assemble_transport_recovery_reports(RecoveryAssemblePlan {
            execution_id: "test-execution-1".to_owned(),
            voter: voter_path,
            learner: learner_path,
            output: output.clone(),
        })
        .is_err());
        assert!(!output.exists(), "failed assembly retained stale success");
    }

    #[test]
    fn assembler_refuses_valid_reports_from_another_role_run_or_source() {
        let root = tempfile::tempdir().expect("assembly mismatch root");
        let voter_path = root.path().join("voter.json");
        let learner_path = root.path().join("learner.json");
        let output = root.path().join("combined.json");
        let build_sha = resolve_build_sha().expect("test checkout SHA");
        let mut voter = role_report(RecoveryRole::Voter, 20);
        voter.build_sha.clone_from(&build_sha);
        let mut learner = role_report(RecoveryRole::Learner, 20);
        learner.build_sha.clone_from(&build_sha);

        let mut wrong_execution = voter.clone();
        wrong_execution.execution_id = "other-execution".to_owned();
        let mut wrong_sha = voter.clone();
        wrong_sha.build_sha = "e".repeat(40);
        let mut wrong_role = role_report(RecoveryRole::Learner, 20);
        wrong_role.build_sha.clone_from(&build_sha);
        for (case, candidate) in [
            ("wrong execution", wrong_execution),
            ("wrong source", wrong_sha),
            ("wrong role", wrong_role),
        ] {
            std::fs::write(
                &voter_path,
                serde_json::to_vec(&candidate).expect("encode invalid pair member"),
            )
            .expect("write invalid pair member");
            std::fs::write(
                &learner_path,
                serde_json::to_vec(&learner).expect("encode learner"),
            )
            .expect("write learner");
            std::fs::write(&output, b"stale success").expect("seed stale output");
            let error = assemble_transport_recovery_reports(RecoveryAssemblePlan {
                execution_id: "test-execution-1".to_owned(),
                voter: voter_path.clone(),
                learner: learner_path.clone(),
                output: output.clone(),
            })
            .expect_err("mismatched role reports must not assemble");
            assert!(!output.exists(), "{case} retained stale output: {error:#}");
        }
    }

    #[test]
    fn independent_role_errors_remain_attributed() {
        let voter = role_report(RecoveryRole::Voter, 3);
        let learner = role_report(RecoveryRole::Learner, 3);
        for (results, expected) in [
            (
                (Err(anyhow::anyhow!("voter cause")), Ok(learner.clone())),
                "voter transport-recovery role failed",
            ),
            (
                (Ok(voter.clone()), Err(anyhow::anyhow!("learner cause"))),
                "learner transport-recovery role failed",
            ),
        ] {
            let error = require_independent_roles(results).expect_err("one role failed");
            assert!(format!("{error:#}").contains(expected));
        }
    }

    #[tokio::test]
    async fn voter_failure_does_not_skip_the_independent_learner_attempt() {
        let root = tempfile::tempdir().expect("sequence root");
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = calls.clone();
        let results = run_role_sequence(
            role_plan(RecoveryRole::Voter, root.path()),
            role_plan(RecoveryRole::Learner, root.path()),
            move |plan| {
                let observed = observed.clone();
                async move {
                    observed.lock().expect("record role call").push(plan.role);
                    bail!("{} injected failure", plan.role.label())
                }
            },
        )
        .await;
        assert_eq!(
            *calls.lock().expect("read role calls"),
            vec![RecoveryRole::Voter, RecoveryRole::Learner]
        );
        let error = require_independent_roles(results).expect_err("both roles failed");
        let message = format!("{error:#}");
        assert!(message.contains("voter") && message.contains("learner"));
    }

    #[tokio::test]
    async fn external_cancellation_prevents_the_learner_attempt() {
        let root = tempfile::tempdir().expect("sequence root");
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = calls.clone();
        let results = run_role_sequence(
            role_plan(RecoveryRole::Voter, root.path()),
            role_plan(RecoveryRole::Learner, root.path()),
            move |plan| {
                let observed = observed.clone();
                async move {
                    observed.lock().expect("record role call").push(plan.role);
                    Err(anyhow::Error::new(RecoveryCancelled {
                        signal: "injected SIGTERM".to_owned(),
                    }))
                }
            },
        )
        .await;
        assert_eq!(
            *calls.lock().expect("read role calls"),
            vec![RecoveryRole::Voter]
        );
        assert!(results
            .0
            .expect_err("voter cancelled")
            .is::<RecoveryCancelled>());
        assert!(results
            .1
            .expect_err("learner suppressed")
            .is::<RecoveryCancelled>());
    }

    #[test]
    fn recovery_failure_after_checkpoint_retains_completed_cycle() {
        use crate::transport_recovery_diagnostics::{
            read_complete_jsonl, CleanupSummary, RecoveryCycleCheckpoint, RecoveryDiagnosticSummary,
        };

        let root = tempfile::tempdir().expect("diagnostic root");
        let plan = role_plan(RecoveryRole::Voter, root.path());
        std::fs::create_dir(&plan.diagnostics_dir).expect("create diagnostics directory");
        let diagnostics = RecoveryDiagnostics::create(
            &plan,
            &"d".repeat(40),
            &build_profile_descriptor(),
            diagnostic_contracts(RecoveryRole::Voter),
            1,
        )
        .expect("create diagnostics");
        let evidence = role_report(RecoveryRole::Voter, 3)
            .campaign
            .cycles
            .remove(0);
        diagnostics
            .checkpoint(1, &evidence)
            .expect("durable checkpoint");
        let token = diagnostics
            .phase_start("target_install_observed", 2, None)
            .expect("recovery phase start");
        diagnostics
            .phase_end(token, &Err(anyhow::anyhow!("injected recovery failure")))
            .expect("recovery phase failure");
        diagnostics
            .record_terminal_observation(
                DiagnosticOutcome::Failed,
                Some(FailureClass::Recovery),
                Some("injected recovery failure"),
            )
            .expect("terminal observation");
        diagnostics
            .write_summary(
                DiagnosticOutcome::Failed,
                false,
                Some(FailureClass::Recovery),
                Some("injected recovery failure"),
                CleanupSummary {
                    attempted: false,
                    budget_millis: duration_millis(CLEANUP_BUDGET),
                    registered_processes: 0,
                    reaped_processes: 0,
                    surviving_processes: Vec::new(),
                },
            )
            .expect("failed summary");
        let checkpoints = read_complete_jsonl::<RecoveryCycleCheckpoint>(
            &plan.diagnostics_dir.join("cycles.jsonl"),
        )
        .expect("read checkpoints");
        assert_eq!(checkpoints.records.len(), 1);
        assert_eq!(checkpoints.records[0].cycle, 1);
        let summary: RecoveryDiagnosticSummary = serde_json::from_slice(
            &std::fs::read(plan.diagnostics_dir.join("summary.json"))
                .expect("read diagnostic summary"),
        )
        .expect("decode diagnostic summary");
        assert_eq!(summary.completed_cycles, 1);
        assert_eq!(summary.failing_cycle, Some(2));
        assert!(!plan.output.exists());
    }

    #[test]
    fn smoke_aliases_are_bounded_and_cannot_claim_full_qualification() {
        let default = smoke_cycles(&[], RecoveryRole::Voter).expect("default voter smoke");
        assert_eq!(default, TRANSPORT_RECOVERY_DEFAULT_VOTER_SMOKE_CYCLES);
        assert_ne!(
            default, TRANSPORT_RECOVERY_CYCLES_PER_ROLE,
            "smoke success must remain distinct from full qualification"
        );
        assert_eq!(
            smoke_cycles(&[], RecoveryRole::Learner).expect("default learner smoke"),
            TRANSPORT_RECOVERY_DEFAULT_LEARNER_SMOKE_CYCLES
        );

        let requested =
            smoke_cycles(&["3".to_owned()], RecoveryRole::Voter).expect("requested smoke count");
        assert_eq!(requested, 3);
        for arguments in [
            vec!["0".to_owned()],
            vec![(TRANSPORT_RECOVERY_CYCLES_PER_ROLE + 1).to_string()],
            vec!["not-a-count".to_owned()],
            vec!["3".to_owned(), "extra".to_owned()],
        ] {
            assert!(smoke_cycles(&arguments, RecoveryRole::Voter).is_err());
        }
    }

    #[test]
    fn recovery_writer_sql_executes_against_the_production_settings_shape() {
        let connection = rusqlite::Connection::open_in_memory().expect("open settings database");
        connection
            .execute_batch(
                "CREATE TABLE settings (\
                    key TEXT PRIMARY KEY,\
                    value TEXT NOT NULL,\
                    updated_at INTEGER NOT NULL\
                ) STRICT;",
            )
            .expect("create production settings shape");
        connection
            .execute(
                RECOVERY_WRITE_SQL,
                rusqlite::params!["recovery.key", "recovery.value", 1_i64],
            )
            .expect("execute recovery writer statement");
        let stored: (String, i64) = connection
            .query_row(
                "SELECT value, updated_at FROM settings WHERE key = 'recovery.key'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read recovery writer row");
        assert_eq!(stored, ("recovery.value".to_owned(), 1));
    }

    #[test]
    fn retained_bytes_receive_the_same_closed_schema_and_semantic_validation() {
        let bytes = serde_json::to_vec(&artifact()).expect("serialize complete evidence");
        validate_transport_recovery_bytes(&bytes).expect("validate complete retained bytes");

        let partial = serde_json::to_vec(&serde_json::json!({
            "build_sha": "d".repeat(40),
            "cycles": []
        }))
        .expect("serialize partial evidence");
        assert!(validate_transport_recovery_bytes(&partial).is_err());
    }

    #[test]
    fn worst_transfer_duration_covers_the_complete_source_series() {
        let mut campaign =
            role_campaign(RecoveryRole::Learner, TRANSPORT_RECOVERY_SMALL_IMAGE_BYTES);
        campaign.cycles[0].source_outbound.attempts = 3;
        campaign.cycles[0].source_outbound.retry_count = 1;
        campaign.cycles[0]
            .source_outbound
            .attempt_started_at_unix_ms = 2_001;
        campaign.cycles[0].source_outbound.elapsed_millis = 3_999;
        campaign.worst_durations = worst_durations(&campaign.cycles);

        assert_eq!(campaign.worst_durations.transfer_millis, 3_999);
        assert!(
            campaign.worst_durations.transfer_millis
                > campaign.cycles[0].target_inbound.transfer_millis
        );
    }

    #[test]
    fn artifact_publication_replaces_stale_bytes_without_exposing_a_partial_result() {
        let root = tempfile::tempdir().expect("artifact publication root");
        let output = root.path().join("nested/recovery.json");
        std::fs::create_dir_all(output.parent().expect("artifact parent"))
            .expect("create stale artifact parent");
        std::fs::write(&output, b"stale evidence").expect("write stale artifact");

        prepare_artifact_output(&output).expect("clear stale artifact");
        assert!(!output.exists());
        publish_artifact_atomically(&output, b"{\"complete\":true}\n")
            .expect("publish complete artifact");

        assert_eq!(
            std::fs::read(&output).expect("read published artifact"),
            b"{\"complete\":true}\n"
        );
        assert_eq!(
            std::fs::read_dir(output.parent().expect("artifact parent"))
                .expect("list artifact directory")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn installed_snapshot_evidence_survives_rotation_after_threshold_writes() {
        let root = tempfile::tempdir().expect("snapshot rotation fixture root");
        let source_snapshots = root.path().join("node-1").join("state_machine/snapshots");
        let target_snapshots = root
            .path()
            .join(format!("node-{TARGET_NODE}"))
            .join("state_machine/snapshots");
        std::fs::create_dir_all(&source_snapshots).expect("create source snapshot fixture");
        std::fs::create_dir_all(&target_snapshots).expect("create target snapshot fixture");
        let installed_id = "00000000-0000-7000-8000-000000000001";
        let rotated_id = "00000000-0000-7000-8000-000000000002";
        std::fs::write(
            source_snapshots.join(installed_id),
            b"installed recovery snapshot",
        )
        .expect("write installed snapshot");
        std::fs::write(source_snapshots.join("current"), installed_id)
            .expect("point at installed snapshot");

        let source_snapshot = snapshot_file(root.path(), 1).expect("source evidence");
        let installed_snapshot = wait_for_installed_snapshot_file(
            root.path(),
            TARGET_NODE,
            &source_snapshot,
            Instant::now() + Duration::from_secs(2),
        );
        let rotate_snapshot = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            std::fs::write(target_snapshots.join(installed_id), b"partial snapshot")
                .expect("write partial target snapshot");
            std::fs::write(target_snapshots.join("current"), installed_id)
                .expect("publish partial target snapshot");
            tokio::time::sleep(Duration::from_millis(30)).await;
            std::fs::write(
                target_snapshots.join(installed_id),
                b"installed recovery snapshot",
            )
            .expect("install target snapshot");
            tokio::time::sleep(Duration::from_millis(20)).await;

            let continued_writes = root.path().join("continued-writes");
            std::fs::create_dir(&continued_writes).expect("create continued-write fixture");
            for ordinal in 1..=TRANSPORT_RECOVERY_SNAPSHOT_LOGS_SINCE_LAST + 1 {
                std::fs::write(
                    continued_writes.join(format!("{ordinal:04}")),
                    ordinal.to_string(),
                )
                .expect("record continued write");
            }
            std::fs::write(
                target_snapshots.join(rotated_id),
                b"later snapshot after continued writes",
            )
            .expect("write rotated snapshot");
            std::fs::write(target_snapshots.join("current"), rotated_id)
                .expect("rotate current snapshot pointer");
            std::fs::remove_file(target_snapshots.join(installed_id))
                .expect("clean up superseded installed snapshot");
        };
        let (installed_snapshot, ()) = tokio::join!(installed_snapshot, rotate_snapshot);
        let installed_snapshot =
            installed_snapshot.expect("capture installed snapshot at installation");

        let current_snapshot = snapshot_file(root.path(), TARGET_NODE).expect("current evidence");
        assert_ne!(current_snapshot, source_snapshot);
        assert_eq!(installed_snapshot, source_snapshot);
        assert!(!target_snapshots.join(installed_id).exists());
    }

    #[test]
    fn missing_cycle_is_rejected() {
        let mut value = artifact();
        value.learner.cycles.pop();
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    #[test]
    fn write_not_acknowledged_during_recovery_is_rejected() {
        let mut value = artifact();
        for write in &mut value.learner.cycles[0].acknowledged_writes {
            write.acknowledged_at_unix_ms = 1_500;
        }
        value.learner.cycles[0].acknowledged_write_digest =
            acknowledged_write_digest(&value.learner.cycles[0].acknowledged_writes);
        value.learner.cycles[0].target_write_digest =
            value.learner.cycles[0].acknowledged_write_digest.clone();
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    #[test]
    fn readiness_write_without_continuing_recovery_load_is_rejected() {
        let mut value = artifact();
        value.learner.cycles[0].acknowledged_writes.truncate(1);
        value.learner.cycles[0].acknowledged_write_digest =
            acknowledged_write_digest(&value.learner.cycles[0].acknowledged_writes);
        value.learner.cycles[0].target_write_digest =
            value.learner.cycles[0].acknowledged_write_digest.clone();
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    #[test]
    fn mismatched_installed_snapshot_is_rejected() {
        let mut value = artifact();
        value.voter.cycles[0].installed_snapshot.sha256 = "e".repeat(64);
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    /// The fixture's baseline is 12 threads, 8 sockets, 10 owned tasks on
    /// every node; the series below are written against it.
    #[test]
    fn a_socket_leaked_every_cycle_is_rejected() {
        let mut value = artifact();
        with_node_series(&mut value.voter, 0, |cycle| {
            counts(12, 8 + u64::from(cycle), 10)
        });
        let error = validate_transport_recovery_artifact(&value).expect_err("a socket leak");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("node 1 sockets floor rose 9 -> 19"),
            "{rendered}"
        );
        assert!(
            rendered.contains("node 1 sockets ceiling rose 18 -> 25"),
            "{rendered}"
        );
    }

    #[test]
    fn a_thread_leaked_every_cycle_is_rejected() {
        let mut value = artifact();
        with_node_series(&mut value.learner, 3, |cycle| {
            counts(12 + u64::from(cycle), 8, 10)
        });
        let error = validate_transport_recovery_artifact(&value).expect_err("a thread leak");
        assert!(
            format!("{error:#}").contains("node 4 threads floor rose 13 -> 23"),
            "{error:#}"
        );
    }

    /// A leak that starts after the opening window never lifts the closing
    /// floor — the closing window's first cycles are still at the old floor
    /// — so it is the ceiling that has to catch it: most of the closing
    /// window sits above anything the opening window showed. The onset here
    /// is cycle 13, past the edge, which puts eight samples above the
    /// opening ceiling and makes the fourth-highest 13; a single leaked
    /// socket at cycle 15 that is never released is the smallest possible
    /// late leak and puts six.
    #[test]
    fn a_leak_that_begins_late_in_the_campaign_is_rejected() {
        let mut value = artifact();
        with_node_series(&mut value.voter, 1, |cycle| {
            counts(12, 8 + u64::from(cycle.saturating_sub(12)), 10)
        });
        let error = validate_transport_recovery_artifact(&value).expect_err("a late leak");
        let rendered = format!("{error:#}");
        assert!(!rendered.contains("floor rose"), "{rendered}");
        assert!(
            rendered.contains("node 2 sockets ceiling rose 8 -> 13"),
            "{rendered}"
        );

        let mut value = artifact();
        with_node_series(&mut value.voter, 1, |cycle| {
            counts(12, if cycle >= 15 { 9 } else { 8 }, 10)
        });
        assert!(validate_transport_recovery_artifact(&value).is_err());

        // On the signal the lane actually samples — a follower alternating
        // between 8 and 9 sockets one recovery apart — only the high-side
        // samples after the leak sit above the opening ceiling, so the
        // boundary is earlier: one connection leaked by cycle 12 is caught
        // and one leaked at cycle 15 is the documented miss; a per-cycle
        // leak from cycle 15 is caught and one from cycle 18 is not.
        let drain = |cycle: u32| if cycle % 2 == 1 { 9 } else { 8 };
        let mut value = artifact();
        with_node_series(&mut value.voter, 1, |cycle| {
            counts(12, drain(cycle) + u64::from(cycle >= 12), 10)
        });
        assert!(validate_transport_recovery_artifact(&value).is_err());
        let mut value = artifact();
        with_node_series(&mut value.voter, 1, |cycle| {
            counts(12, drain(cycle) + u64::from(cycle >= 15), 10)
        });
        validate_transport_recovery_artifact(&value).expect("the documented miss");
        let mut value = artifact();
        with_node_series(&mut value.voter, 1, |cycle| {
            counts(12, drain(cycle) + u64::from(cycle.saturating_sub(14)), 10)
        });
        assert!(validate_transport_recovery_artifact(&value).is_err());
        let mut value = artifact();
        with_node_series(&mut value.voter, 1, |cycle| {
            counts(12, drain(cycle) + u64::from(cycle.saturating_sub(17)), 10)
        });
        validate_transport_recovery_artifact(&value).expect("the documented miss");
    }

    /// Exactly the measured shape: one connection and one owned task per
    /// peer, alternating one recovery apart, on a source with three peers.
    /// Nothing accumulates, so nothing is a leak — and the same series with
    /// the warmup on the high side, or landing high only every third cycle,
    /// is the same answer.
    #[test]
    fn the_per_peer_transport_drain_is_not_growth() {
        let mut value = artifact();
        with_node_series(&mut value.voter, 0, |cycle| {
            if cycle % 2 == 1 {
                counts(12, 8 + 3, 10 + 3)
            } else {
                counts(12, 8, 10)
            }
        });
        validate_transport_recovery_artifact(&value).expect("a drain is not a leak");
        let node = &value.voter.resource_envelopes.nodes[0];
        assert_eq!(node.opening.floor, counts(12, 8, 10));
        assert_eq!(node.opening.ceiling, counts(12, 11, 13));
        assert_eq!(node.closing.floor, counts(12, 8, 10));
        assert_eq!(node.closing.ceiling, counts(12, 11, 13));

        let mut value = artifact();
        value.voter.cycles.iter_mut().for_each(|cycle| {
            cycle.node_resources[0].baseline = counts(12, 11, 13);
        });
        with_node_series(&mut value.voter, 0, |cycle| {
            if cycle % 3 == 0 {
                counts(12, 11, 13)
            } else {
                counts(12, 8, 10)
            }
        });
        validate_transport_recovery_artifact(&value).expect("a high warmup is not a leak");
    }

    /// Node 1 of the voter campaign on run 1034, sample for sample: a cold
    /// baseline of fourteen threads, sixteen once recoveries start, seventeen
    /// from cycle ten onward, and then eleven consecutive cycles that do not
    /// move. Sockets and owned tasks drain a peer's worth every third cycle
    /// or so throughout.
    ///
    /// A pool that grows twice during warmup and then stops is not a leak: a
    /// leak has no last cycle. Admitting the baseline to the opening floor
    /// made this campaign fail with `threads floor rose 14 -> 17`, comparing
    /// steady state against a cold start rather than the campaign against
    /// itself.
    #[test]
    fn a_warmup_step_that_plateaus_is_not_growth() {
        const THREADS: [u64; 20] = [
            16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17,
        ];
        let high = [1_u64, 12, 13, 16, 20];
        let mut value = artifact();
        value.voter.cycles.iter_mut().for_each(|cycle| {
            cycle.node_resources[0].baseline = counts(14, 16, 31);
        });
        with_node_series(&mut value.voter, 0, |cycle| {
            let threads = THREADS[cycle as usize - 1];
            if high.contains(&u64::from(cycle)) {
                counts(threads, 19, 34)
            } else {
                counts(threads, 16, 31)
            }
        });
        validate_transport_recovery_artifact(&value)
            .expect("a warmup step that plateaus is not a leak");

        let node = &value.voter.resource_envelopes.nodes[0];
        assert_eq!(
            node.opening.floor.threads, 16,
            "the cold baseline is not the floor"
        );
        assert_eq!(node.closing.floor.threads, 17);

        // The same series with the plateau replaced by continued growth is a
        // leak, and is still caught.
        let mut value = artifact();
        value.voter.cycles.iter_mut().for_each(|cycle| {
            cycle.node_resources[0].baseline = counts(14, 16, 31);
        });
        with_node_series(&mut value.voter, 0, |cycle| {
            counts(16 + u64::from(cycle) / 3, 16, 31)
        });
        let error = validate_transport_recovery_artifact(&value)
            .expect_err("growth that never stops is a leak");
        assert!(
            format!("{error:#}").contains("node 1 threads floor rose"),
            "{error:#}"
        );
    }

    /// The other measured shape: thread counts wander a couple either side
    /// of their floor with no recovery in flight, and the floor is not seen
    /// on every cycle.
    #[test]
    fn thread_jitter_inside_the_allowance_is_not_growth() {
        let mut value = artifact();
        with_node_series(&mut value.learner, 3, |cycle| {
            counts(12 + [1_u64, 0, 2, 1, 1][cycle as usize % 5], 8, 10)
        });
        validate_transport_recovery_artifact(&value).expect("thread jitter is not a leak");

        // Even a closing window that never shows the floor again, and shows
        // a count the opening window never did, stays inside the allowance.
        let mut value = artifact();
        with_node_series(&mut value.learner, 3, |cycle| {
            if cycle > 10 {
                counts(12 + THREAD_ENVELOPE_ALLOWANCE, 8, 10)
            } else {
                counts(12, 8, 10)
            }
        });
        validate_transport_recovery_artifact(&value).expect("inside the thread allowance");
    }

    #[test]
    fn a_thread_envelope_past_the_allowance_is_rejected() {
        let mut value = artifact();
        with_node_series(&mut value.learner, 3, |cycle| {
            if cycle > 10 {
                counts(12 + THREAD_ENVELOPE_ALLOWANCE + 1, 8, 10)
            } else {
                counts(12, 8, 10)
            }
        });
        assert!(validate_transport_recovery_artifact(&value).is_err());

        // One thread every third recovery lifts the closing floor by three
        // against an allowance of two; every fourth lifts it by two and sits
        // inside the allowance at both edges, and that is the fastest thread
        // leak the allowance hides.
        let mut value = artifact();
        with_node_series(&mut value.learner, 3, |cycle| {
            counts(12 + u64::from(cycle / 3), 8, 10)
        });
        let error = validate_transport_recovery_artifact(&value).expect_err("one per three");
        assert!(
            format!("{error:#}").contains("node 4 threads floor rose 12 -> 15"),
            "{error:#}"
        );
        let mut value = artifact();
        with_node_series(&mut value.learner, 3, |cycle| {
            counts(12 + u64::from(cycle / 4), 8, 10)
        });
        validate_transport_recovery_artifact(&value).expect("hidden inside the allowance");
    }

    /// One high sample, in either window, is recorded and is not a verdict,
    /// whether or not the other window ever showed that height. This is the
    /// sample the old per-cycle ceiling failed the whole lane on.
    #[test]
    fn a_single_high_sample_in_either_window_is_recorded_not_rejected() {
        let mut value = artifact();
        value.voter.cycles[0].node_resources[0].post_quiescence = counts(14, 11, 13);
        value.voter.cycles[15].node_resources[2].post_quiescence = counts(15, 12, 14);
        value.voter.resource_envelopes =
            resource_envelopes(&value.voter.cycles).expect("rewritten envelopes");
        validate_transport_recovery_artifact(&value).expect("a high sample is evidence");
    }

    /// The closing ceiling is the fourth-highest sample: three cycles that
    /// reached a count the opening window never did are spikes — a
    /// replacement connection, a slower drain, a blocking thread that had
    /// not idled out — and four are a trend. The opening ceiling is the
    /// highest of its cycles, so a spike there raises the bar for the
    /// closing window rather than lowering it, and the warmup baseline,
    /// which nothing recomputes, bounds the floor only.
    #[test]
    fn three_high_closing_samples_are_spikes_and_four_are_a_trend() {
        let mut value = artifact();
        with_node_series(&mut value.voter, 0, |cycle| {
            counts(12, if [12, 15, 19].contains(&cycle) { 9 } else { 8 }, 10)
        });
        validate_transport_recovery_artifact(&value).expect("three spikes");
        assert_eq!(
            value.voter.resource_envelopes.nodes[0]
                .closing
                .ceiling
                .sockets,
            8
        );

        let mut value = artifact();
        with_node_series(&mut value.voter, 0, |cycle| {
            counts(
                12,
                if [12, 15, 17, 19].contains(&cycle) {
                    9
                } else {
                    8
                },
                10,
            )
        });
        let error = validate_transport_recovery_artifact(&value).expect_err("four samples");
        assert!(
            format!("{error:#}").contains("node 1 sockets ceiling rose 8 -> 9"),
            "{error:#}"
        );

        let mut value = artifact();
        value.voter.cycles.iter_mut().for_each(|cycle| {
            cycle.node_resources[0].baseline = counts(12, 99, 10);
        });
        with_node_series(&mut value.voter, 0, |cycle| {
            counts(12, if cycle >= 13 { 8 + u64::from(cycle) } else { 8 }, 10)
        });
        let envelopes = &value.voter.resource_envelopes;
        assert_eq!(envelopes.nodes[0].opening.floor.sockets, 8);
        assert_eq!(envelopes.nodes[0].opening.ceiling.sockets, 8);
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    #[test]
    fn recorded_resource_envelopes_must_match_the_cycles() {
        let mut value = artifact();
        value.learner.resource_envelopes.nodes[0]
            .closing
            .floor
            .sockets -= 1;
        assert!(validate_transport_recovery_artifact(&value).is_err());

        let mut value = artifact();
        value.learner.resource_envelopes.nodes[0]
            .opening
            .ceiling
            .sockets += 1;
        assert!(validate_transport_recovery_artifact(&value).is_err());

        let mut value = artifact();
        value.learner.resource_envelopes.opening_window_last_cycle = 15;
        assert!(validate_transport_recovery_artifact(&value).is_err());

        let mut value = artifact();
        value.learner.resource_envelopes.closing_window_samples = 11;
        assert!(validate_transport_recovery_artifact(&value).is_err());

        let mut value = artifact();
        value
            .learner
            .resource_envelopes
            .closing_ceiling_sustained_samples = 1;
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    /// The envelope is computed over every cycle or not at all: a campaign
    /// whose records are reordered, mislabelled or renumbered is refused by
    /// the envelope itself, before any per-cycle check looks at it.
    #[test]
    fn an_envelope_over_incomplete_or_reordered_records_is_refused() {
        let complete = role_campaign(RecoveryRole::Voter, TRANSPORT_RECOVERY_LARGE_IMAGE_BYTES);
        assert!(resource_envelopes(&[]).is_err());

        let mut reordered = complete.clone();
        reordered.cycles[5].node_resources.swap(0, 1);
        assert!(resource_envelopes(&reordered.cycles).is_err());

        let mut short = complete.clone();
        short.cycles[5].node_resources.pop();
        assert!(resource_envelopes(&short.cycles).is_err());

        let mut renumbered = complete.clone();
        renumbered.cycles[19].cycle = 1;
        assert!(resource_envelopes(&renumbered.cycles).is_err());

        let mut mislabelled = complete;
        mislabelled.cycles[5].node_resources[3].kind =
            RecoveryResourceNodeKind::PersistentSourceVoter;
        assert!(resource_envelopes(&mislabelled.cycles).is_err());
    }

    /// The opening window is the baseline plus the first half of the cycles,
    /// rounded up, for any campaign length the smoke can ask for — and the
    /// envelope is a verdict only once the closing window holds the ten
    /// samples the twenty-cycle campaign gives it.
    #[test]
    fn the_opening_window_is_the_first_half_of_the_series_rounded_up() {
        assert_eq!(opening_window_last_cycle(21), 10);
        assert_eq!(opening_window_last_cycle(4), 1);
        assert_eq!(opening_window_last_cycle(2), 0);
        assert_eq!(opening_window_last_cycle(1), 0);

        let mut campaign = role_campaign(RecoveryRole::Voter, TRANSPORT_RECOVERY_LARGE_IMAGE_BYTES);
        assert_eq!(campaign.resource_envelopes.opening_window_last_cycle, 10);
        assert_eq!(campaign.resource_envelopes.closing_window_samples, 10);
        assert!(envelope_is_asserted(&campaign.resource_envelopes));

        campaign.cycles.truncate(3);
        with_node_series(&mut campaign, 0, |cycle| {
            counts(12, 8 + u64::from(cycle), 10)
        });
        let envelopes = &campaign.resource_envelopes;
        assert_eq!(envelopes.opening_window_last_cycle, 1);
        assert_eq!(envelopes.closing_window_samples, 2);
        assert!(!envelope_is_asserted(envelopes));
        // Cycle 1 is the whole opening window, and the baseline of eight
        // sockets is not a member of it: the band is that cycle alone.
        assert_eq!(envelopes.nodes[0].opening.floor, counts(12, 9, 10));
        assert_eq!(envelopes.nodes[0].opening.ceiling, counts(12, 9, 10));
        assert_eq!(envelopes.nodes[0].closing.floor, counts(12, 10, 10));
        // Two closing samples: the sustained ceiling clamps to the lower.
        assert_eq!(envelopes.nodes[0].closing.ceiling, counts(12, 10, 10));

        for cycles in [1_usize, 2, 18] {
            let mut campaign =
                role_campaign(RecoveryRole::Voter, TRANSPORT_RECOVERY_LARGE_IMAGE_BYTES);
            campaign.cycles.truncate(cycles);
            let envelopes = resource_envelopes(&campaign.cycles).expect("short campaign");
            assert!(!envelope_is_asserted(&envelopes), "{cycles} cycles");
        }
        let mut campaign = role_campaign(RecoveryRole::Voter, TRANSPORT_RECOVERY_LARGE_IMAGE_BYTES);
        campaign.cycles.truncate(19);
        let envelopes = resource_envelopes(&campaign.cycles).expect("19-cycle campaign");
        assert!(envelope_is_asserted(&envelopes));
    }

    #[test]
    fn every_persistent_source_voter_requires_resource_evidence() {
        let mut value = artifact();
        value.learner.cycles[0].node_resources.remove(1);
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    #[test]
    fn persistent_source_resource_baselines_cannot_be_rebased_between_cycles() {
        let mut value = artifact();
        value.learner.cycles[1].node_resources[0].baseline.threads += 1;
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    #[test]
    fn outbound_attempt_metrics_must_come_from_a_timed_source_observation() {
        let mut value = artifact();
        value.voter.cycles[0]
            .source_outbound
            .attempt_started_at_unix_ms = 6_000;
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    #[test]
    fn large_snapshot_receive_and_install_timings_cannot_be_zero() {
        let mut value = artifact();
        value.voter.cycles[0].target_inbound.install_millis = 0;
        value.voter.cycles[0]
            .target_inbound
            .install_started_at_unix_ms = 5_500;
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    #[test]
    fn recovery_acknowledgements_must_include_a_cadence_separated_pair() {
        let mut value = artifact();
        value.learner.cycles[0].acknowledged_writes[1].acknowledged_at_unix_ms = 2_500;
        value.learner.cycles[0].acknowledged_writes[2].acknowledged_at_unix_ms = 2_999;
        value.learner.cycles[0].acknowledged_write_digest =
            acknowledged_write_digest(&value.learner.cycles[0].acknowledged_writes);
        value.learner.cycles[0].target_write_digest =
            value.learner.cycles[0].acknowledged_write_digest.clone();
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    #[test]
    fn two_early_acknowledgements_cannot_hide_a_long_recovery_gap() {
        let mut value = artifact();
        let cycle = &mut value.learner.cycles[0];
        cycle.recovery_finished_at_unix_ms = 102_000;
        cycle.recovery_millis = 100_000;
        cycle.acknowledged_writes[1].acknowledged_at_unix_ms = 2_500;
        cycle.acknowledged_writes[2].acknowledged_at_unix_ms = 40_000;
        cycle.acknowledged_write_digest = acknowledged_write_digest(&cycle.acknowledged_writes);
        cycle.target_write_digest = cycle.acknowledged_write_digest.clone();
        value.finished_at_unix_ms = 103_000;
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    #[test]
    fn writer_must_cover_the_end_of_recovery() {
        let mut value = artifact();
        let cycle = &mut value.learner.cycles[0];
        cycle.recovery_finished_at_unix_ms = 50_000;
        cycle.recovery_millis = 48_000;
        cycle.acknowledged_write_digest = acknowledged_write_digest(&cycle.acknowledged_writes);
        cycle.target_write_digest = cycle.acknowledged_write_digest.clone();
        value.finished_at_unix_ms = 51_000;
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    #[test]
    fn whole_recovery_duration_cannot_exceed_the_absolute_deadline() {
        let mut value = artifact();
        value.learner.cycles[0].recovery_millis =
            duration_millis(RECOVERY_DEADLINE).saturating_add(1);
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    fn resource_counts(
        node: u64,
        threads: u64,
        sockets: u64,
        tasks: u64,
    ) -> (u64, ProcessResourceCount) {
        (
            node,
            ProcessResourceCount {
                threads,
                sockets,
                owned_async_tasks: tasks,
            },
        )
    }

    /// A recovery that never finished has to be named, because a longer
    /// horizon is the wrong answer to it and the right answer to the case
    /// below.
    #[test]
    fn an_expired_resource_wait_names_every_node_still_owning_work() {
        let current = vec![
            resource_counts(1, 10, 10, 10),
            resource_counts(3, 10, 10, 10),
        ];
        let report = describe_resource_wait(&[1, 3], &current, None, 0);
        assert!(
            report.contains("node(s) 1, 3 still owning transport work"),
            "{report}"
        );
        assert!(!report.contains("every node idle"), "{report}");

        let error = annotate_resource_wait(
            remaining_before(Instant::now(), "stable idle resource sampling")
                .expect_err("an elapsed deadline"),
            Some(&report),
        );
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("node(s) 1, 3 still owning transport work"),
            "{rendered}"
        );
    }

    /// Whether a longer horizon would help is decided by what moved, so the
    /// report has to carry the deltas and has to say when there were none.
    #[test]
    fn an_unstable_resource_wait_names_what_moved_between_the_two_samples() {
        let previous = vec![
            resource_counts(1, 10, 9, 10),
            resource_counts(2, 10, 10, 10),
        ];
        let current = vec![
            resource_counts(1, 10, 10, 10),
            resource_counts(2, 10, 10, 8),
        ];
        let report = describe_resource_wait(&[], &current, Some(&previous), 1);
        assert!(report.contains("node 1 sockets 9 -> 10"), "{report}");
        assert!(
            report.contains("node 2 owned async tasks 10 -> 8"),
            "{report}"
        );
        assert!(!report.contains("node 1 threads"), "{report}");

        let settled = describe_resource_wait(&[], &current, Some(&current), 1);
        assert!(
            settled.contains("the last two samples were identical"),
            "{settled}"
        );
    }

    /// The case a longer horizon does fix: nothing is wrong, the machine is
    /// just too loaded for two samples in a row to match.
    #[test]
    fn an_expired_resource_wait_that_is_only_unstable_says_exactly_that() {
        let current = vec![resource_counts(1, 10, 10, 10)];
        let report = describe_resource_wait(&[], &current, None, 1);
        assert!(report.contains("every node idle"), "{report}");
        assert!(
            report.contains(&format!(
                "1 of {RESOURCE_STABLE_SAMPLES} consecutive repeated samples"
            )),
            "{report}"
        );

        let error = annotate_resource_wait(
            remaining_before(Instant::now(), "stable idle resource sampling")
                .expect_err("an elapsed deadline"),
            None,
        );
        assert!(
            format!("{error:#}").contains("no resource sample completed inside the horizon"),
            "{error:#}"
        );
    }

    /// The sampler is a sample, not a verdict: a count above the warmup is
    /// recorded as it stands, and the campaign asks the leak question once,
    /// over the floors. An owned task that is never released moves the
    /// closing floor by one for every cycle it survives, and the allowance
    /// for that resource is zero, so one per-peer transport that stops being
    /// released is exactly the size of leak this catches.
    #[test]
    fn owned_async_task_growth_has_no_resource_slack() {
        assert_eq!(RESOURCE_BASELINE_WARMUP_CYCLES, 1);
        assert_eq!(OWNED_ASYNC_TASK_ENVELOPE_ALLOWANCE, 0);
        assert_eq!(SOCKET_ENVELOPE_ALLOWANCE, 0);

        let baseline = vec![resource_counts(1, 10, 10, 10)];
        let mut high = baseline.clone();
        high[0].1.owned_async_tasks += 1;
        let recorded = collect_node_resource_evidence(&baseline, &high)
            .expect("a high sample is recorded, not refused");
        assert_eq!(recorded[0].post_quiescence.owned_async_tasks, 11);
        assert_eq!(recorded[0].baseline.owned_async_tasks, 10);

        let mut value = artifact();
        with_node_series(&mut value.voter, 0, |cycle| {
            counts(12, 8, 10 + u64::from(cycle))
        });
        let error = validate_transport_recovery_artifact(&value).expect_err("an owned task leak");
        assert!(
            format!("{error:#}").contains("node 1 owned async tasks floor rose 11 -> 21"),
            "{error:#}"
        );

        // The smallest possible rise — the closing window never again shows
        // the opening floor — is still a leak at zero allowance.
        let mut value = artifact();
        with_node_series(&mut value.voter, 0, |cycle| {
            counts(12, 8, if cycle > 10 { 11 } else { 10 })
        });
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    #[tokio::test]
    async fn writer_exit_before_readiness_fails_promptly() {
        let root = tempfile::tempdir().expect("writer readiness root");
        let child = Command::new(std::env::current_exe().expect("current test executable"))
            .args(["--exact", "transport_recovery_no_such_test"])
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn short-lived writer stand-in");
        let mut writer = RecoveryWriter {
            child,
            stdout: None,
            identity: None,
        };

        let error = wait_for_writer_ready(
            &mut writer,
            &root.path().join("never-ready"),
            Duration::from_secs(2),
        )
        .await
        .expect_err("writer exit must fail readiness");
        assert!(error
            .to_string()
            .contains("exited before publishing readiness"));
    }

    #[tokio::test]
    async fn writer_exit_after_readiness_fails_the_recovery_promptly() {
        use tokio::io::AsyncReadExt;

        let root = tempfile::tempdir().expect("writer supervisor root");
        let mut child = Command::new(std::env::current_exe().expect("current test executable"))
            .args(["--exact", "transport_recovery_no_such_test"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn short-lived writer stand-in");
        let mut stdout = child.stdout.take().expect("stand-in stdout");
        let stdout = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).await?;
            Ok(bytes)
        });
        let mut writer = RecoveryWriter {
            child,
            stdout: Some(stdout),
            identity: None,
        };
        let config = RecoveryWriterConfig {
            addresses: vec!["one".into(), "two".into(), "three".into()],
            prefix: "writer-supervisor.".into(),
            ready_path: root.path().join("ready"),
            progress_path: root.path().join("progress"),
            start_path: root.path().join("start"),
            stop_path: root.path().join("stop"),
            interval_millis: WRITER_INTERVAL_MILLIS,
            max_operations: WRITER_MAX_OPERATIONS,
        };
        let error = supervise_recovery_writer(
            &mut writer,
            &config,
            1,
            Instant::now() + Duration::from_secs(2),
            std::future::pending::<Result<()>>(),
        )
        .await
        .expect_err("writer exit must fail recovery");
        assert!(error
            .to_string()
            .contains("exited before recovery completed"));
    }

    #[test]
    fn recovery_acknowledgement_identifiers_must_be_unique() {
        let mut value = artifact();
        value.learner.cycles[0].acknowledged_writes[2].key =
            value.learner.cycles[0].acknowledged_writes[1].key.clone();
        value.learner.cycles[0].acknowledged_write_digest =
            acknowledged_write_digest(&value.learner.cycles[0].acknowledged_writes);
        value.learner.cycles[0].target_write_digest =
            value.learner.cycles[0].acknowledged_write_digest.clone();
        assert!(validate_transport_recovery_artifact(&value).is_err());
    }

    #[test]
    fn ambiguous_write_resolution_accepts_only_the_exact_committed_value() {
        let exact = RecoveryWriteRow {
            key: "recovery.unique.000001".to_owned(),
            value: "expected".to_owned(),
        };
        assert_eq!(
            classify_recovery_write("recovery.unique.000001", "expected", &[exact])
                .expect("classify exact committed value"),
            RecoveryWriteResolution::Exact
        );
        assert_eq!(
            classify_recovery_write("recovery.unique.000001", "expected", &[])
                .expect("classify unresolved value"),
            RecoveryWriteResolution::Missing
        );
        let mismatch = RecoveryWriteRow {
            key: "recovery.unique.000001".to_owned(),
            value: "different".to_owned(),
        };
        assert!(
            classify_recovery_write("recovery.unique.000001", "expected", &[mismatch]).is_err()
        );
    }

    #[test]
    fn schema_is_closed_and_valid_draft_2020_12() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../benchmarks/cluster-transport-recovery.schema.json"
        ))
        .expect("parse recovery schema");
        jsonschema::draft202012::meta::validate(&schema).expect("valid recovery meta-schema");
        let validator = jsonschema::draft202012::new(&schema).expect("compile recovery schema");
        let mut value = serde_json::to_value(artifact()).expect("serialize recovery fixture");
        value
            .as_object_mut()
            .expect("artifact object")
            .insert("invented".to_owned(), serde_json::json!(true));
        assert!(!validator.is_valid(&value));

        let role_schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../benchmarks/cluster-transport-recovery-role.schema.json"
        ))
        .expect("parse recovery role schema");
        jsonschema::draft202012::meta::validate(&role_schema)
            .expect("valid recovery role meta-schema");
        let role_validator =
            jsonschema::draft202012::new(&role_schema).expect("compile recovery role schema");
        let mut role_value = serde_json::to_value(role_report(RecoveryRole::Voter, 3))
            .expect("serialize recovery role fixture");
        role_value
            .as_object_mut()
            .expect("role report object")
            .insert("invented".to_owned(), serde_json::json!(true));
        assert!(!role_validator.is_valid(&role_value));
    }
}
