use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::transport_recovery::{
    ProcessResourceCount, RecoveryCycleEvidence, RecoveryRole, RecoveryRolePlan,
};

pub const DIAGNOSTIC_SCHEMA_VERSION: u32 = 1;
pub const CLEANUP_BUDGET: Duration = Duration::from_secs(30);
const EVENT_KIND: &str = "transport_recovery_event";
const CYCLE_KIND: &str = "transport_recovery_cycle_checkpoint";
const SUMMARY_KIND: &str = "transport_recovery_summary";
const MAX_SAFE_MESSAGE_BYTES: usize = 2_048;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticContracts {
    pub minimum_sqlite_bytes: u64,
    pub snapshot_logs_since_last: u64,
    pub recovery_deadline_millis: u64,
    pub resource_cleanup_horizon_millis: u64,
    pub resource_sample_interval_millis: u64,
    pub resource_stable_samples: usize,
    pub thread_envelope_allowance: u64,
    pub socket_envelope_allowance: u64,
    pub owned_async_task_envelope_allowance: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticOutcome {
    Passed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    Admission,
    Cleanup,
    EvidenceIo,
    Integrity,
    OrchestrationTimeout,
    ProcessLifecycle,
    Recovery,
    Resource,
    Validation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_ticks: u64,
    pub process_kind: String,
    pub node_id: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceDiagnosticSample {
    pub node_id: u64,
    pub counts: ProcessResourceCount,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RecoveryDiagnosticEvent {
    ExecutionStart {
        build_sha: String,
        build_profile: String,
        requested_cycles: u32,
        platform: String,
        runner_label: Option<String>,
        max_runtime_seconds: Option<u64>,
        contracts: DiagnosticContracts,
        role_report_path: PathBuf,
        diagnostics_dir: PathBuf,
    },
    PhaseStart {
        phase: String,
        cycle: u32,
        parent_phase: Option<String>,
    },
    PhaseEnd {
        phase: String,
        cycle: u32,
        parent_phase: Option<String>,
        outcome: DiagnosticOutcome,
        duration_millis: u64,
        message: Option<String>,
    },
    ProcessLifecycle {
        action: String,
        identity: ProcessIdentity,
        outcome: DiagnosticOutcome,
        message: Option<String>,
    },
    ResourceSample {
        cycle: u32,
        busy_nodes: Vec<u64>,
        counts: Vec<ResourceDiagnosticSample>,
        stable_samples: usize,
        required_stable_samples: usize,
    },
    TerminalObserved {
        outcome: DiagnosticOutcome,
        failure_class: Option<FailureClass>,
        phase: Option<String>,
        cycle: Option<u32>,
        message: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryDiagnosticRecord {
    pub schema_version: u32,
    pub kind: String,
    pub execution_id: String,
    pub role: RecoveryRole,
    pub wall_time_unix_ms: i64,
    pub monotonic_elapsed_millis: u64,
    pub detail: RecoveryDiagnosticEvent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryCycleCheckpoint {
    pub schema_version: u32,
    pub kind: String,
    pub execution_id: String,
    pub role: RecoveryRole,
    pub wall_time_unix_ms: i64,
    pub monotonic_elapsed_millis: u64,
    pub cycle: u32,
    pub source_leader: u64,
    pub phase_durations_millis: BTreeMap<String, u64>,
    pub resource_sample: Vec<ResourceDiagnosticSample>,
    pub evidence: RecoveryCycleEvidence,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupSummary {
    pub attempted: bool,
    pub budget_millis: u64,
    pub registered_processes: usize,
    pub reaped_processes: usize,
    pub surviving_processes: Vec<ProcessIdentity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TimingStatistic {
    pub sample_count: usize,
    pub median_millis: u64,
    pub maximum_millis: u64,
    pub total_millis: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryDiagnosticSummary {
    pub schema_version: u32,
    pub kind: String,
    pub execution_id: String,
    pub role: RecoveryRole,
    pub status: DiagnosticOutcome,
    pub requested_cycles: u32,
    pub completed_cycles: u32,
    pub resource_envelope_asserted: bool,
    pub failing_phase: Option<String>,
    pub failing_cycle: Option<u32>,
    pub failure_class: Option<FailureClass>,
    pub message: Option<String>,
    pub role_report_path: PathBuf,
    pub events_path: PathBuf,
    pub cycles_path: PathBuf,
    pub cleanup: CleanupSummary,
    pub phase_timings: BTreeMap<String, TimingStatistic>,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub total_wall_millis: u64,
}

#[derive(Clone, Debug)]
pub struct PhaseToken {
    phase: String,
    cycle: u32,
    parent_phase: Option<String>,
    started: Instant,
}

#[derive(Clone)]
pub struct RecoveryDiagnostics {
    inner: Arc<Mutex<DiagnosticState>>,
}

struct DiagnosticState {
    execution_id: String,
    role: RecoveryRole,
    requested_cycles: u32,
    role_report_path: PathBuf,
    diagnostics_dir: PathBuf,
    events_path: PathBuf,
    cycles_path: PathBuf,
    started_at_unix_ms: i64,
    started: Instant,
    events: File,
    cycles: File,
    active_phases: Vec<(String, u32)>,
    failed_phase: Option<String>,
    failed_cycle: Option<u32>,
    completed_cycles: u32,
    phase_durations: BTreeMap<u32, BTreeMap<String, u64>>,
    processes: BTreeMap<(u32, u64), ProcessIdentity>,
}

impl RecoveryDiagnostics {
    pub fn create(
        plan: &RecoveryRolePlan,
        build_sha: &str,
        build_profile: &str,
        contracts: DiagnosticContracts,
        started_at_unix_ms: i64,
    ) -> Result<Self> {
        let events_path = plan.diagnostics_dir.join("events.jsonl");
        let cycles_path = plan.diagnostics_dir.join("cycles.jsonl");
        let events = create_new(&events_path)?;
        let cycles = create_new(&cycles_path)?;
        let diagnostics = Self {
            inner: Arc::new(Mutex::new(DiagnosticState {
                execution_id: plan.execution_id.clone(),
                role: plan.role,
                requested_cycles: plan.cycles_required,
                role_report_path: plan.output.clone(),
                diagnostics_dir: plan.diagnostics_dir.clone(),
                events_path,
                cycles_path,
                started_at_unix_ms,
                started: Instant::now(),
                events,
                cycles,
                active_phases: Vec::new(),
                failed_phase: None,
                failed_cycle: None,
                completed_cycles: 0,
                phase_durations: BTreeMap::new(),
                processes: BTreeMap::new(),
            })),
        };
        diagnostics.record_event(RecoveryDiagnosticEvent::ExecutionStart {
            build_sha: build_sha.to_owned(),
            build_profile: build_profile.to_owned(),
            requested_cycles: plan.cycles_required,
            platform: std::env::consts::OS.to_owned(),
            runner_label: std::env::var("PLURX_RUNNER_LABEL").ok(),
            max_runtime_seconds: plan.max_runtime_seconds,
            contracts,
            role_report_path: plan.output.clone(),
            diagnostics_dir: plan.diagnostics_dir.clone(),
        })?;
        Ok(diagnostics)
    }

    pub fn phase_start(
        &self,
        phase: impl Into<String>,
        cycle: u32,
        parent_phase: Option<&str>,
    ) -> Result<PhaseToken> {
        let phase = phase.into();
        self.record_event(RecoveryDiagnosticEvent::PhaseStart {
            phase: phase.clone(),
            cycle,
            parent_phase: parent_phase.map(str::to_owned),
        })?;
        let mut state = self.lock()?;
        state.active_phases.push((phase.clone(), cycle));
        Ok(PhaseToken {
            phase,
            cycle,
            parent_phase: parent_phase.map(str::to_owned),
            started: Instant::now(),
        })
    }

    pub fn phase_end(&self, token: PhaseToken, result: &Result<()>) -> Result<()> {
        let duration_millis = duration_millis(token.started.elapsed());
        let (outcome, message) = match result {
            Ok(()) => (DiagnosticOutcome::Passed, None),
            Err(error) => (
                DiagnosticOutcome::Failed,
                Some(sanitize_message(&format!("{error:#}"))),
            ),
        };
        {
            let mut state = self.lock()?;
            let phase_durations = state.phase_durations.entry(token.cycle).or_default();
            let duration = phase_durations.entry(token.phase.clone()).or_default();
            *duration = duration.saturating_add(duration_millis);
            if let Some(position) = state
                .active_phases
                .iter()
                .rposition(|active| active == &(token.phase.clone(), token.cycle))
            {
                state.active_phases.remove(position);
            }
            if result.is_err() {
                state.failed_phase = Some(token.phase.clone());
                state.failed_cycle = Some(token.cycle);
            }
        }
        self.record_event(RecoveryDiagnosticEvent::PhaseEnd {
            phase: token.phase,
            cycle: token.cycle,
            parent_phase: token.parent_phase,
            outcome,
            duration_millis,
            message,
        })
    }

    pub fn register_process(
        &self,
        pid: u32,
        process_kind: impl Into<String>,
        node_id: Option<u64>,
    ) -> Result<ProcessIdentity> {
        let identity = ProcessIdentity {
            pid,
            start_ticks: process_start_ticks(pid)?,
            process_kind: process_kind.into(),
            node_id,
        };
        {
            let mut state = self.lock()?;
            state
                .processes
                .insert((identity.pid, identity.start_ticks), identity.clone());
        }
        self.record_process("start", identity.clone(), DiagnosticOutcome::Passed, None)?;
        Ok(identity)
    }

    pub fn process_action(
        &self,
        action: &str,
        identity: &ProcessIdentity,
        result: &Result<()>,
    ) -> Result<()> {
        let (outcome, message) = match result {
            Ok(()) => (DiagnosticOutcome::Passed, None),
            Err(error) => (
                DiagnosticOutcome::Failed,
                Some(sanitize_message(&format!("{error:#}"))),
            ),
        };
        if action == "reap" && result.is_ok() {
            self.lock()?
                .processes
                .remove(&(identity.pid, identity.start_ticks));
        }
        self.record_process(action, identity.clone(), outcome, message)
    }

    pub fn registered_for_node(&self, node_id: u64) -> Result<Option<ProcessIdentity>> {
        Ok(self
            .lock()?
            .processes
            .values()
            .rev()
            .find(|identity| identity.node_id == Some(node_id))
            .cloned())
    }

    pub fn registered_processes(&self) -> Result<Vec<ProcessIdentity>> {
        Ok(self.lock()?.processes.values().cloned().collect())
    }

    pub fn record_resource_sample(
        &self,
        cycle: u32,
        busy_nodes: &[u64],
        counts: &[(u64, ProcessResourceCount)],
        stable_samples: usize,
        required_stable_samples: usize,
    ) -> Result<()> {
        self.record_event(RecoveryDiagnosticEvent::ResourceSample {
            cycle,
            busy_nodes: busy_nodes.to_vec(),
            counts: counts
                .iter()
                .map(|(node_id, counts)| ResourceDiagnosticSample {
                    node_id: *node_id,
                    counts: counts.clone(),
                })
                .collect(),
            stable_samples,
            required_stable_samples,
        })
    }

    pub fn record_terminal_observation(
        &self,
        outcome: DiagnosticOutcome,
        failure_class: Option<FailureClass>,
        message: Option<&str>,
    ) -> Result<()> {
        let (phase, cycle) = self.active_phase()?;
        self.record_event(RecoveryDiagnosticEvent::TerminalObserved {
            outcome,
            failure_class,
            phase,
            cycle,
            message: message.map(sanitize_message),
        })
    }

    pub fn checkpoint(&self, source_leader: u64, evidence: &RecoveryCycleEvidence) -> Result<()> {
        let mut state = self.lock()?;
        let checkpoint = RecoveryCycleCheckpoint {
            schema_version: DIAGNOSTIC_SCHEMA_VERSION,
            kind: CYCLE_KIND.to_owned(),
            execution_id: state.execution_id.clone(),
            role: state.role,
            wall_time_unix_ms: unix_ms()?,
            monotonic_elapsed_millis: duration_millis(state.started.elapsed()),
            cycle: evidence.cycle,
            source_leader,
            phase_durations_millis: state
                .phase_durations
                .get(&evidence.cycle)
                .cloned()
                .unwrap_or_default(),
            resource_sample: evidence
                .node_resources
                .iter()
                .map(|sample| ResourceDiagnosticSample {
                    node_id: sample.node_id,
                    counts: sample.post_quiescence.clone(),
                })
                .collect(),
            evidence: evidence.clone(),
        };
        append_json_line(&mut state.cycles, &checkpoint, true)?;
        state.completed_cycles = state.completed_cycles.max(evidence.cycle);
        Ok(())
    }

    pub fn active_phase(&self) -> Result<(Option<String>, Option<u32>)> {
        let state = self.lock()?;
        Ok(state
            .active_phases
            .last()
            .cloned()
            .map_or((None, None), |(phase, cycle)| (Some(phase), Some(cycle))))
    }

    pub async fn cleanup_registered(&self, budget: Duration) -> Result<CleanupSummary> {
        let processes = self.registered_processes()?;
        if processes.is_empty() {
            return Ok(CleanupSummary {
                attempted: false,
                budget_millis: duration_millis(budget),
                registered_processes: 0,
                reaped_processes: 0,
                surviving_processes: Vec::new(),
            });
        }
        for identity in &processes {
            let result = kill_exact_process(identity);
            self.process_action("kill", identity, &result)?;
        }
        let deadline = Instant::now() + budget;
        let mut survivors = processes.clone();
        loop {
            survivors.retain(|identity| process_identity_is_live(identity).unwrap_or(true));
            if survivors.is_empty() || Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let survivor_keys = survivors
            .iter()
            .map(|identity| (identity.pid, identity.start_ticks))
            .collect::<std::collections::BTreeSet<_>>();
        for identity in &processes {
            if !survivor_keys.contains(&(identity.pid, identity.start_ticks)) {
                let result = Ok(());
                self.process_action("reap", identity, &result)?;
            }
        }
        Ok(CleanupSummary {
            attempted: true,
            budget_millis: duration_millis(budget),
            registered_processes: processes.len(),
            reaped_processes: processes.len().saturating_sub(survivors.len()),
            surviving_processes: survivors,
        })
    }

    pub fn write_summary(
        &self,
        status: DiagnosticOutcome,
        resource_envelope_asserted: bool,
        failure_class: Option<FailureClass>,
        message: Option<&str>,
        cleanup: CleanupSummary,
    ) -> Result<RecoveryDiagnosticSummary> {
        let state = self.lock()?;
        let finished_at_unix_ms = unix_ms()?;
        let phase_timings = phase_timing_statistics(&state.phase_durations);
        let active_phase = state.active_phases.last().cloned();
        let summary = RecoveryDiagnosticSummary {
            schema_version: DIAGNOSTIC_SCHEMA_VERSION,
            kind: SUMMARY_KIND.to_owned(),
            execution_id: state.execution_id.clone(),
            role: state.role,
            status,
            requested_cycles: state.requested_cycles,
            completed_cycles: state.completed_cycles,
            resource_envelope_asserted,
            failing_phase: state
                .failed_phase
                .clone()
                .or_else(|| active_phase.as_ref().map(|(phase, _)| phase.clone())),
            failing_cycle: state
                .failed_cycle
                .or_else(|| active_phase.as_ref().map(|(_, cycle)| *cycle)),
            failure_class,
            message: message.map(sanitize_message),
            role_report_path: state.role_report_path.clone(),
            events_path: state.events_path.clone(),
            cycles_path: state.cycles_path.clone(),
            cleanup,
            phase_timings,
            started_at_unix_ms: state.started_at_unix_ms,
            finished_at_unix_ms,
            total_wall_millis: duration_millis(state.started.elapsed()),
        };
        let path = state.diagnostics_dir.join("summary.json");
        let events_path = state.events_path.clone();
        let cycles_path = state.cycles_path.clone();
        drop(state);
        let events = read_complete_jsonl::<RecoveryDiagnosticRecord>(&events_path)?;
        if events.truncated_final_line || events.records.is_empty() {
            bail!("transport-recovery event journal is truncated or empty");
        }
        let cycles = read_complete_jsonl::<RecoveryCycleCheckpoint>(&cycles_path)?;
        if cycles.truncated_final_line {
            bail!("transport-recovery cycle journal has a truncated final record");
        }
        publish_json_atomically(&path, &summary)?;
        Ok(summary)
    }

    pub fn cycle_phase_durations(&self, cycle: u32) -> Result<BTreeMap<String, u64>> {
        Ok(self
            .lock()?
            .phase_durations
            .get(&cycle)
            .cloned()
            .unwrap_or_default())
    }

    fn record_process(
        &self,
        action: &str,
        identity: ProcessIdentity,
        outcome: DiagnosticOutcome,
        message: Option<String>,
    ) -> Result<()> {
        self.record_event(RecoveryDiagnosticEvent::ProcessLifecycle {
            action: action.to_owned(),
            identity,
            outcome,
            message,
        })
    }

    fn record_event(&self, detail: RecoveryDiagnosticEvent) -> Result<()> {
        let mut state = self.lock()?;
        let record = RecoveryDiagnosticRecord {
            schema_version: DIAGNOSTIC_SCHEMA_VERSION,
            kind: EVENT_KIND.to_owned(),
            execution_id: state.execution_id.clone(),
            role: state.role,
            wall_time_unix_ms: unix_ms()?,
            monotonic_elapsed_millis: duration_millis(state.started.elapsed()),
            detail,
        };
        append_json_line(&mut state.events, &record, false)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, DiagnosticState>> {
        self.inner
            .lock()
            .map_err(|_| anyhow::anyhow!("transport-recovery diagnostic state was poisoned"))
    }
}

fn phase_timing_statistics(
    cycles: &BTreeMap<u32, BTreeMap<String, u64>>,
) -> BTreeMap<String, TimingStatistic> {
    let mut samples = BTreeMap::<String, Vec<u64>>::new();
    for durations in cycles.values() {
        for (phase, duration) in durations {
            samples.entry(phase.clone()).or_default().push(*duration);
        }
    }
    samples
        .into_iter()
        .map(|(phase, mut values)| {
            values.sort_unstable();
            let sample_count = values.len();
            let median_millis = if sample_count % 2 == 0 {
                values[sample_count / 2 - 1].saturating_add(values[sample_count / 2]) / 2
            } else {
                values[sample_count / 2]
            };
            let maximum_millis = *values.last().unwrap_or(&0);
            let total_millis = values
                .iter()
                .fold(0_u64, |total, value| total.saturating_add(*value));
            (
                phase,
                TimingStatistic {
                    sample_count,
                    median_millis,
                    maximum_millis,
                    total_millis,
                },
            )
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonlRead<T> {
    pub records: Vec<T>,
    pub truncated_final_line: bool,
}

pub fn read_complete_jsonl<T: DeserializeOwned>(path: &Path) -> Result<JsonlRead<T>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read transport-recovery journal {path:?}"))?;
    read_complete_jsonl_bytes(&bytes)
}

pub fn read_complete_jsonl_bytes<T: DeserializeOwned>(bytes: &[u8]) -> Result<JsonlRead<T>> {
    let truncated_final_line = !bytes.is_empty() && !bytes.ends_with(b"\n");
    let complete = if truncated_final_line {
        bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(&[][..], |position| &bytes[..=position])
    } else {
        bytes
    };
    let mut records = Vec::new();
    for line in BufReader::new(complete).lines() {
        let line = line.context("read complete transport-recovery journal line")?;
        if !line.is_empty() {
            records.push(
                serde_json::from_str(&line)
                    .context("decode complete transport-recovery journal line")?,
            );
        }
    }
    Ok(JsonlRead {
        records,
        truncated_final_line,
    })
}

pub fn classify_failure(message: &str) -> FailureClass {
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("diagnostic") || normalized.contains("evidence i/o") {
        FailureClass::EvidenceIo
    } else if normalized.contains("admission") || normalized.contains("join") {
        FailureClass::Admission
    } else if normalized.contains("resource") || normalized.contains("leaked") {
        FailureClass::Resource
    } else if normalized.contains("timeout") || normalized.contains("exceeded") {
        FailureClass::OrchestrationTimeout
    } else if normalized.contains("shutdown")
        || normalized.contains("cleanup")
        || normalized.contains("reap")
    {
        FailureClass::Cleanup
    } else if normalized.contains("process") || normalized.contains("writer exited") {
        FailureClass::ProcessLifecycle
    } else if normalized.contains("schema") || normalized.contains("validation") {
        FailureClass::Validation
    } else if normalized.contains("digest")
        || normalized.contains("snapshot")
        || normalized.contains("acknowledg")
    {
        FailureClass::Integrity
    } else {
        FailureClass::Recovery
    }
}

fn create_new(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .with_context(|| format!("create new transport-recovery diagnostic {path:?}"))
}

fn append_json_line<T: Serialize>(file: &mut File, value: &T, synchronize: bool) -> Result<()> {
    serde_json::to_writer(&mut *file, value)
        .context("serialize transport-recovery diagnostic record")?;
    file.write_all(b"\n")
        .context("terminate transport-recovery diagnostic record")?;
    file.flush()
        .context("flush transport-recovery diagnostic record")?;
    if synchronize {
        file.sync_data()
            .context("synchronize transport-recovery diagnostic checkpoint")?;
    }
    Ok(())
}

fn publish_json_atomically<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("diagnostic summary has no parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .context("create temporary transport-recovery diagnostic summary")?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), value)
        .context("write transport-recovery diagnostic summary")?;
    temporary
        .as_file_mut()
        .write_all(b"\n")
        .context("terminate transport-recovery diagnostic summary")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync transport-recovery diagnostic summary")?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("publish transport-recovery diagnostic summary")?;
    File::open(parent)
        .context("open transport-recovery diagnostic directory")?
        .sync_all()
        .context("sync transport-recovery diagnostic directory")
}

fn sanitize_message(message: &str) -> String {
    let mut sanitized = message
        .chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\t') {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if sanitized.len() > MAX_SAFE_MESSAGE_BYTES {
        sanitized.truncate(MAX_SAFE_MESSAGE_BYTES);
        sanitized.push('…');
    }
    sanitized
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn unix_ms() -> Result<i64> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock precedes Unix epoch")?
        .as_millis();
    i64::try_from(millis).context("Unix timestamp exceeds i64")
}

#[cfg(target_os = "linux")]
fn process_start_ticks(pid: u32) -> Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .with_context(|| format!("read process identity for PID {pid}"))?;
    let command_end = stat
        .rfind(')')
        .context("process stat command has no closing parenthesis")?;
    stat.get(command_end.saturating_add(2)..)
        .context("process stat fields are missing")?
        .split_whitespace()
        .nth(19)
        .context("process stat start time is missing")?
        .parse::<u64>()
        .context("parse process stat start time")
}

#[cfg(not(target_os = "linux"))]
fn process_start_ticks(_pid: u32) -> Result<u64> {
    bail!("transport-recovery process identity requires Linux /proc")
}

#[cfg(target_os = "linux")]
fn process_identity_is_live(identity: &ProcessIdentity) -> Result<bool> {
    match process_start_ticks(identity.pid) {
        Ok(start_ticks) => Ok(start_ticks == identity.start_ticks),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(not(target_os = "linux"))]
fn process_identity_is_live(_identity: &ProcessIdentity) -> Result<bool> {
    Ok(false)
}

#[cfg(unix)]
fn kill_exact_process(identity: &ProcessIdentity) -> Result<()> {
    if !process_identity_is_live(identity)? {
        return Ok(());
    }
    let pid = libc::pid_t::try_from(identity.pid).context("process id overflowed pid_t")?;
    // SAFETY: the PID and Linux start time were captured from a child spawned
    // by this invocation and revalidated immediately above, and SIGKILL is the
    // fixed cleanup signal used only for that exact identity.
    if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error).context("kill owned transport-recovery process");
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn kill_exact_process(_identity: &ProcessIdentity) -> Result<()> {
    bail!("transport-recovery owned-process cleanup requires Unix signals")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(root: &Path) -> RecoveryRolePlan {
        let diagnostics_dir = root.join("diagnostics");
        std::fs::create_dir(&diagnostics_dir).expect("create diagnostics directory");
        RecoveryRolePlan {
            role: RecoveryRole::Voter,
            cycles_required: 3,
            execution_id: "diagnostic-test".to_owned(),
            output: root.join("role.json"),
            diagnostics_dir,
            max_runtime_seconds: None,
        }
    }

    fn contracts() -> DiagnosticContracts {
        DiagnosticContracts {
            minimum_sqlite_bytes: 1,
            snapshot_logs_since_last: 1,
            recovery_deadline_millis: 1,
            resource_cleanup_horizon_millis: 1,
            resource_sample_interval_millis: 1,
            resource_stable_samples: 2,
            thread_envelope_allowance: 2,
            socket_envelope_allowance: 0,
            owned_async_task_envelope_allowance: 0,
        }
    }

    #[test]
    fn truncated_final_jsonl_keeps_every_complete_record() {
        let complete = RecoveryDiagnosticRecord {
            schema_version: DIAGNOSTIC_SCHEMA_VERSION,
            kind: EVENT_KIND.to_owned(),
            execution_id: "run-1".to_owned(),
            role: RecoveryRole::Voter,
            wall_time_unix_ms: 1,
            monotonic_elapsed_millis: 1,
            detail: RecoveryDiagnosticEvent::PhaseStart {
                phase: "cluster_start".to_owned(),
                cycle: 0,
                parent_phase: None,
            },
        };
        let mut bytes = serde_json::to_vec(&complete).expect("serialize record");
        bytes.extend_from_slice(b"\n{\"schema_version\":1");
        let read = read_complete_jsonl_bytes::<RecoveryDiagnosticRecord>(&bytes)
            .expect("read complete records");
        assert_eq!(read.records, vec![complete]);
        assert!(read.truncated_final_line);
    }

    #[test]
    fn failure_classification_keeps_evidence_and_resource_failures_distinct() {
        assert_eq!(
            classify_failure("diagnostic evidence I/O failed"),
            FailureClass::EvidenceIo
        );
        assert_eq!(
            classify_failure("voter campaign leaked resources"),
            FailureClass::Resource
        );
        assert_eq!(
            classify_failure("learner admission token exchange failed"),
            FailureClass::Admission
        );
    }

    #[test]
    fn diagnostics_directory_cannot_be_reused_or_mixed() {
        let root = tempfile::tempdir().expect("diagnostic root");
        let plan = plan(root.path());
        RecoveryDiagnostics::create(&plan, &"a".repeat(40), "debug", contracts(), 1)
            .expect("create first diagnostics");
        let error = RecoveryDiagnostics::create(&plan, &"a".repeat(40), "debug", contracts(), 1)
            .err()
            .expect("reuse must fail");
        assert!(format!("{error:#}").contains("create new"));
    }

    #[test]
    fn phase_failure_and_resource_progress_are_durable() {
        let root = tempfile::tempdir().expect("diagnostic root");
        let plan = plan(root.path());
        let diagnostics =
            RecoveryDiagnostics::create(&plan, &"a".repeat(40), "debug", contracts(), 1)
                .expect("create diagnostics");
        let token = diagnostics
            .phase_start("admission_token_redeem", 0, Some("learner_admission"))
            .expect("phase start");
        diagnostics
            .phase_end(token, &Err(anyhow::anyhow!("injected admission failure")))
            .expect("phase failure");
        diagnostics
            .record_resource_sample(
                0,
                &[2],
                &[(
                    2,
                    ProcessResourceCount {
                        threads: 4,
                        sockets: 3,
                        owned_async_tasks: 1,
                    },
                )],
                0,
                2,
            )
            .expect("resource sample");
        diagnostics
            .record_terminal_observation(
                DiagnosticOutcome::Failed,
                Some(FailureClass::Admission),
                Some("injected admission failure"),
            )
            .expect("terminal observation");
        let records = read_complete_jsonl::<RecoveryDiagnosticRecord>(
            &plan.diagnostics_dir.join("events.jsonl"),
        )
        .expect("read durable events");
        assert!(!records.truncated_final_line);
        assert!(records.records.len() >= 5);
        assert!(!plan.output.exists());
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn sigterm_cleanup_reaps_a_real_owned_child() {
        let root = tempfile::tempdir().expect("diagnostic root");
        let plan = plan(root.path());
        let diagnostics =
            RecoveryDiagnostics::create(&plan, &"a".repeat(40), "debug", contracts(), 1)
                .expect("create diagnostics");
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .kill_on_drop(false)
            .spawn()
            .expect("spawn real child");
        let pid = child.id().expect("child pid");
        diagnostics
            .register_process(pid, "writer", None)
            .expect("register child identity");
        let cancellation = crate::transport_recovery::wait_for_recovery_cancellation();
        tokio::pin!(cancellation);
        let sender = tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            // SAFETY: this test installs Tokio's SIGTERM listener before the
            // delayed signal and sends the signal only to its own process.
            assert_eq!(
                unsafe { libc::kill(std::process::id() as libc::pid_t, libc::SIGTERM) },
                0
            );
        });
        assert_eq!(cancellation.await.expect("observe cancellation"), "SIGTERM");
        sender.await.expect("join signal sender");
        let wait = tokio::spawn(async move { child.wait().await.expect("wait for killed child") });
        let cleanup = diagnostics
            .cleanup_registered(Duration::from_secs(3))
            .await
            .expect("bounded cleanup");
        let status = wait.await.expect("join child wait");
        assert!(!status.success());
        assert!(cleanup.surviving_processes.is_empty());
        assert_eq!(cleanup.reaped_processes, 1);
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn evidence_corruption_is_reported_without_erasing_complete_records() {
        let root = tempfile::tempdir().expect("diagnostic root");
        let plan = plan(root.path());
        let diagnostics =
            RecoveryDiagnostics::create(&plan, &"a".repeat(40), "debug", contracts(), 1)
                .expect("create diagnostics");
        diagnostics
            .record_terminal_observation(
                DiagnosticOutcome::Failed,
                Some(FailureClass::EvidenceIo),
                Some("injected evidence write failure"),
            )
            .expect("terminal observation");
        let events_path = plan.diagnostics_dir.join("events.jsonl");
        OpenOptions::new()
            .append(true)
            .open(&events_path)
            .expect("open journal for injected truncation")
            .write_all(b"{\"truncated\":")
            .expect("inject truncated record");
        let read = read_complete_jsonl::<RecoveryDiagnosticRecord>(&events_path)
            .expect("retain complete records");
        assert!(read.truncated_final_line);
        assert_eq!(read.records.len(), 2);
        let summary_error = diagnostics
            .write_summary(
                DiagnosticOutcome::Failed,
                false,
                Some(FailureClass::EvidenceIo),
                Some("injected evidence write failure"),
                CleanupSummary {
                    attempted: false,
                    budget_millis: 1,
                    registered_processes: 0,
                    reaped_processes: 0,
                    surviving_processes: Vec::new(),
                },
            )
            .expect_err("corrupt journal must prevent terminal summary");
        assert!(format!("{summary_error:#}").contains("truncated"));
        assert!(!plan.output.exists());
    }
}
