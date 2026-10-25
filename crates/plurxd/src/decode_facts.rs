//! Descriptor-bound decoder fact collection and its bounded node-local cache.
//!
//! The source handle is opened and authorized by the preparation owner. This
//! module never reopens its pathname: FFprobe receives a duplicate of that
//! exact handle, and metadata is checked again after probing before the facts
//! can be returned or cached.

use std::collections::{BTreeMap, VecDeque};
#[cfg(unix)]
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::ffi::{CString, OsStr};
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use plurx_core::domain::ScanType;
use plurx_core::transcode::{
    DecodeCatalogMetadata, DecodeFacts, DecodeSourceIdentity, InterlaceVerdict,
};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

#[cfg(target_os = "macos")]
mod macos_wasm;
#[cfg(target_os = "linux")]
mod pi_namespace;

#[cfg(any(target_os = "linux", test))]
pub(crate) mod mastering;

/// Internal synchronous launcher dispatch; normal daemon startup stays unchanged.
pub(crate) fn dispatch_probe_bootstrap() -> Option<std::io::Result<()>> {
    #[cfg(target_os = "linux")]
    {
        pi_namespace::dispatch()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(unix)]
fn probe_launch_path(
    snapshot: &ExecutableSnapshot,
    mode: ProbeLaunchMode,
) -> Result<PathBuf, DecodeFactError> {
    #[cfg(target_os = "linux")]
    {
        pi_namespace::launch_path(snapshot, mode)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = mode;
        Ok(snapshot_execution_path(snapshot))
    }
}

const MAX_PROBE_STDOUT_BYTES: usize = 256 * 1024;
const MAX_PROBE_STDERR_BYTES: usize = 16 * 1024;
const MAX_PROBE_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_VERSION_BYTES: usize = 64 * 1024;
/// Bump when the selective FFprobe projection changes the meaning of cached facts.
///
/// v3 is the projection S-08 published on main, which added `field_order`.
/// S-07 drafted its luminance side-data projection as v2 against the older
/// main; merged, the projection is a superset of v3, so it takes the next
/// number rather than either branch's.
// v5 adds independently collected HEVC progressive/frame-only PTL evidence.
const PROBE_SCHEMA_VERSION: u32 = 5;
const IDENTITY_DEADLINE: Duration = Duration::from_secs(10);
/// How long a bound probe identity is trusted on its `fstat` tuples alone
/// (DECODE-FACTS-GATE-AND-FALLBACK §3.3, M3).
///
/// Within one interval a validation compares the seven-field tuple of the
/// held descriptor, of the path and of the snapshot (and, on Linux, the
/// snapshot's seals); any difference forces the full content hash before the
/// verdict. Past it, the next validation hashes all three regardless. Facts
/// are unaffected — they come from the sealed image and the key names its
/// digest. What this trades is refusal latency: a replacement the tuple
/// cannot show (a root-level actor who also rewinds `ctime`, outside this
/// trust model) is refused within one interval rather than at the next
/// lookup. A constant, not a setting.
const PROBE_REVALIDATION_INTERVAL: Duration = Duration::from_secs(30);
const VERSION_DEADLINE: Duration = Duration::from_secs(5);
const PROBE_DEADLINE: Duration = Duration::from_secs(10);
const IDET_MEDIA_SECONDS: u8 = 10;
const MAX_CACHE_ENTRIES: usize = 256;
const DECODE_FACT_PHASE_BUCKETS: &[(u64, &str)] = &[
    (1_000_000, "0.001"),
    (5_000_000, "0.005"),
    (10_000_000, "0.01"),
    (25_000_000, "0.025"),
    (50_000_000, "0.05"),
    (100_000_000, "0.1"),
    (250_000_000, "0.25"),
    (500_000_000, "0.5"),
    (1_000_000_000, "1"),
    (2_000_000_000, "2"),
    (5_000_000_000, "5"),
    (10_000_000_000, "10"),
];
#[cfg(unix)]
const HELD_PROBE_FD: std::os::fd::RawFd = 4;

fn identity_gate() -> Arc<tokio::sync::Semaphore> {
    static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1))))
}

fn version_gate() -> Arc<tokio::sync::Semaphore> {
    static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1))))
}

static INTERLACE_FLAG_CONFIRMED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static INTERLACE_FLAG_OVERRULED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static INTERLACE_IDET_UNAVAILABLE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn record_interlace_verdict(verdict: InterlaceVerdict) {
    use std::sync::atomic::Ordering;

    let counter = match verdict {
        InterlaceVerdict::FlagConfirmed => &INTERLACE_FLAG_CONFIRMED,
        InterlaceVerdict::FlagOverruled => &INTERLACE_FLAG_OVERRULED,
        InterlaceVerdict::IdetUnavailable => &INTERLACE_IDET_UNAVAILABLE,
        InterlaceVerdict::NotChecked => return,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn interlace_prometheus() -> String {
    use std::sync::atomic::Ordering;

    format!(
        "plurx_interlace_verdicts_total{{verdict=\"flag_confirmed\"}} {}\n\
         plurx_interlace_verdicts_total{{verdict=\"flag_overruled\"}} {}\n\
         plurx_interlace_verdicts_total{{verdict=\"idet_unavailable\"}} {}\n",
        INTERLACE_FLAG_CONFIRMED.load(Ordering::Relaxed),
        INTERLACE_FLAG_OVERRULED.load(Ordering::Relaxed),
        INTERLACE_IDET_UNAVAILABLE.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
fn test_discovery_gate() -> Arc<tokio::sync::Semaphore> {
    static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(GATE.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1))))
}

#[derive(Clone, Copy)]
enum DecodeFactPhase {
    GateWait,
    IdentityValidation,
    SourceObservation,
    Collection,
    FinalValidation,
}

impl DecodeFactPhase {
    const ALL: [Self; 5] = [
        Self::GateWait,
        Self::IdentityValidation,
        Self::SourceObservation,
        Self::Collection,
        Self::FinalValidation,
    ];

    const fn index(self) -> usize {
        self as usize
    }

    const fn label(self) -> &'static str {
        match self {
            Self::GateWait => "gate_wait",
            Self::IdentityValidation => "identity_validation",
            Self::SourceObservation => "source_observation",
            Self::Collection => "collection",
            Self::FinalValidation => "final_validation",
        }
    }
}

#[derive(Clone, Copy)]
enum DecodeFactOutcome {
    Ok,
    Deadline,
    Cancelled,
    Changed,
    Error,
}

impl DecodeFactOutcome {
    const ALL: [Self; 5] = [
        Self::Ok,
        Self::Deadline,
        Self::Cancelled,
        Self::Changed,
        Self::Error,
    ];

    const fn index(self) -> usize {
        self as usize
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Deadline => "deadline",
            Self::Cancelled => "cancelled",
            Self::Changed => "changed",
            Self::Error => "error",
        }
    }

    fn of(result: &Result<(), DecodeFactError>) -> Self {
        match result {
            Ok(()) => Self::Ok,
            Err(DecodeFactError::Deadline) => Self::Deadline,
            Err(DecodeFactError::Cancelled) => Self::Cancelled,
            Err(DecodeFactError::ProbeChanged | DecodeFactError::SourceChanged) => Self::Changed,
            Err(_) => Self::Error,
        }
    }
}

#[derive(Default)]
struct DecodeFactPhaseCell {
    count: AtomicU64,
    elapsed_nanos: AtomicU64,
    buckets: [AtomicU64; DECODE_FACT_PHASE_BUCKETS.len()],
}

pub(crate) struct DecodeFactMetrics {
    phases: [DecodeFactPhaseCell; 25],
    hit_phases: [DecodeFactPhaseCell; 5],
    lookups: [AtomicU64; 3],
    fallbacks: [AtomicU64; 7],
}

impl Default for DecodeFactMetrics {
    fn default() -> Self {
        Self {
            phases: std::array::from_fn(|_| DecodeFactPhaseCell::default()),
            hit_phases: std::array::from_fn(|_| DecodeFactPhaseCell::default()),
            lookups: std::array::from_fn(|_| AtomicU64::new(0)),
            fallbacks: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

#[derive(Clone, Copy)]
enum DecodeFactLookupResult {
    Hit,
    MissCollected,
    Refused,
}

impl DecodeFactLookupResult {
    const ALL: [Self; 3] = [Self::Hit, Self::MissCollected, Self::Refused];

    const fn index(self) -> usize {
        self as usize
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::MissCollected => "miss_collected",
            Self::Refused => "refused",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecodePlanFallbackReason {
    Deadline,
    Cancelled,
    ProbeChanged,
    ProbeFailed,
    IdentityIo,
    Invariant,
    RefusedSourceChanged,
}

impl DecodePlanFallbackReason {
    const ALL: [Self; 7] = [
        Self::Deadline,
        Self::Cancelled,
        Self::ProbeChanged,
        Self::ProbeFailed,
        Self::IdentityIo,
        Self::Invariant,
        Self::RefusedSourceChanged,
    ];

    const fn index(self) -> usize {
        self as usize
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Deadline => "deadline",
            Self::Cancelled => "cancelled",
            Self::ProbeChanged => "probe_changed",
            Self::ProbeFailed => "probe_failed",
            Self::IdentityIo => "identity_io",
            Self::Invariant => "invariant",
            Self::RefusedSourceChanged => "refused_source_changed",
        }
    }
}

impl DecodeFactMetrics {
    fn saturating_add(target: &AtomicU64, amount: u64) {
        let _ = target.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            (current != u64::MAX).then(|| current.saturating_add(amount))
        });
    }

    fn phase_cell(
        &self,
        phase: DecodeFactPhase,
        outcome: DecodeFactOutcome,
    ) -> &DecodeFactPhaseCell {
        &self.phases[phase.index() * DecodeFactOutcome::ALL.len() + outcome.index()]
    }

    fn record_phase(
        &self,
        phase: DecodeFactPhase,
        result: &Result<(), DecodeFactError>,
        elapsed: Duration,
    ) {
        let cell = self.phase_cell(phase, DecodeFactOutcome::of(result));
        let elapsed_nanos = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
        Self::saturating_add(&cell.count, 1);
        Self::saturating_add(&cell.elapsed_nanos, elapsed_nanos);
        if let Some(index) = DECODE_FACT_PHASE_BUCKETS
            .iter()
            .position(|(upper, _)| elapsed_nanos <= *upper)
        {
            Self::saturating_add(&cell.buckets[index], 1);
        }
    }

    fn record_lookup(&self, result: DecodeFactLookupResult) {
        Self::saturating_add(&self.lookups[result.index()], 1);
    }

    fn record_hit_phase(&self, phase: DecodeFactPhase, elapsed: Duration) {
        let cell = &self.hit_phases[phase.index()];
        let elapsed_nanos = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
        Self::saturating_add(&cell.count, 1);
        Self::saturating_add(&cell.elapsed_nanos, elapsed_nanos);
        if let Some(index) = DECODE_FACT_PHASE_BUCKETS
            .iter()
            .position(|(upper, _)| elapsed_nanos <= *upper)
        {
            Self::saturating_add(&cell.buckets[index], 1);
        }
    }

    pub(crate) fn record_fallback(&self, reason: DecodePlanFallbackReason) {
        Self::saturating_add(&self.fallbacks[reason.index()], 1);
    }

    pub(crate) fn prometheus(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::from(
            "# HELP plurx_decode_facts_phase_seconds Decode-fact lookup latency by bounded phase and outcome.\n\
             # TYPE plurx_decode_facts_phase_seconds histogram\n",
        );
        for phase in DecodeFactPhase::ALL {
            for outcome in DecodeFactOutcome::ALL {
                let cell = self.phase_cell(phase, outcome);
                let mut cumulative = 0_u64;
                for (index, (_, upper)) in DECODE_FACT_PHASE_BUCKETS.iter().enumerate() {
                    cumulative =
                        cumulative.saturating_add(cell.buckets[index].load(Ordering::Relaxed));
                    let _ = writeln!(
                        out,
                        "plurx_decode_facts_phase_seconds_bucket{{phase=\"{}\",outcome=\"{}\",le=\"{}\"}} {}",
                        phase.label(),
                        outcome.label(),
                        upper,
                        cumulative
                    );
                }
                let count = cell.count.load(Ordering::Relaxed);
                let _ = writeln!(
                    out,
                    "plurx_decode_facts_phase_seconds_bucket{{phase=\"{}\",outcome=\"{}\",le=\"+Inf\"}} {count}",
                    phase.label(),
                    outcome.label()
                );
                let seconds = cell.elapsed_nanos.load(Ordering::Relaxed) as f64 / 1_000_000_000.0;
                let _ = writeln!(
                    out,
                    "plurx_decode_facts_phase_seconds_sum{{phase=\"{}\",outcome=\"{}\"}} {seconds:.9}",
                    phase.label(),
                    outcome.label()
                );
                let _ = writeln!(
                    out,
                    "plurx_decode_facts_phase_seconds_count{{phase=\"{}\",outcome=\"{}\"}} {count}",
                    phase.label(),
                    outcome.label()
                );
            }
        }
        out.push_str(
            "# HELP plurx_decode_facts_hit_phase_seconds Decode-fact latency for cache-hit lookups only.\n\
             # TYPE plurx_decode_facts_hit_phase_seconds histogram\n",
        );
        for phase in DecodeFactPhase::ALL {
            let cell = &self.hit_phases[phase.index()];
            let mut cumulative = 0_u64;
            for (index, (_, upper)) in DECODE_FACT_PHASE_BUCKETS.iter().enumerate() {
                cumulative = cumulative.saturating_add(cell.buckets[index].load(Ordering::Relaxed));
                let _ = writeln!(
                    out,
                    "plurx_decode_facts_hit_phase_seconds_bucket{{phase=\"{}\",le=\"{}\"}} {}",
                    phase.label(),
                    upper,
                    cumulative
                );
            }
            let count = cell.count.load(Ordering::Relaxed);
            let _ = writeln!(
                out,
                "plurx_decode_facts_hit_phase_seconds_bucket{{phase=\"{}\",le=\"+Inf\"}} {count}",
                phase.label()
            );
            let seconds = cell.elapsed_nanos.load(Ordering::Relaxed) as f64 / 1_000_000_000.0;
            let _ = writeln!(
                out,
                "plurx_decode_facts_hit_phase_seconds_sum{{phase=\"{}\"}} {seconds:.9}",
                phase.label()
            );
            let _ = writeln!(
                out,
                "plurx_decode_facts_hit_phase_seconds_count{{phase=\"{}\"}} {count}",
                phase.label()
            );
        }
        out.push_str(
            "# HELP plurx_decode_facts_lookups_total Decode-fact cache lookups by bounded result.\n\
             # TYPE plurx_decode_facts_lookups_total counter\n",
        );
        for result in DecodeFactLookupResult::ALL {
            let _ = writeln!(
                out,
                "plurx_decode_facts_lookups_total{{result=\"{}\"}} {}",
                result.label(),
                self.lookups[result.index()].load(Ordering::Relaxed)
            );
        }
        out.push_str(
            "# HELP plurx_decode_plan_fallbacks_total Bound decode-plan fallbacks and identity refusals by bounded reason.\n\
             # TYPE plurx_decode_plan_fallbacks_total counter\n",
        );
        for reason in DecodePlanFallbackReason::ALL {
            let _ = writeln!(
                out,
                "plurx_decode_plan_fallbacks_total{{reason=\"{}\"}} {}",
                reason.label(),
                self.fallbacks[reason.index()].load(Ordering::Relaxed)
            );
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CacheKey {
    probe_schema_version: u32,
    source: DecodeSourceIdentity,
    ffprobe_build_digest: String,
    catalog_digest: Option<String>,
    selected_stream: ProbeStreamSelection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)] // FirstPlayable is consumed by M2's selected-stream adapter.
pub(crate) enum ProbeStreamSelection {
    FirstPlayable,
    LegacyVideoOrdinal(u32),
    Absolute(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeLaunchMode {
    Production,
    #[cfg(all(test, target_os = "linux"))]
    ProductionNamespace,
    #[cfg(test)]
    Fixture,
    #[cfg(all(test, target_os = "linux"))]
    ProductionPidfdOpenFailure,
    #[cfg(all(test, target_os = "linux"))]
    ProductionPidfdReadFailure,
    #[cfg(all(test, target_os = "linux"))]
    ProductionSupervisorDelay,
    #[cfg(all(test, target_os = "linux"))]
    ProductionSupervisorReceiveFailure,
    #[cfg(all(test, target_os = "linux"))]
    ProductionBootstrapInterrupted,
    #[cfg(all(test, target_os = "linux"))]
    ProductionBootstrapInterruptedUntilDeadline(LinuxBootstrapPhase),
    #[cfg(all(test, target_os = "linux"))]
    ProductionSteadyResponseInterruptedUntilDeadline,
    #[cfg(all(test, target_os = "linux"))]
    ProductionSteadyResponseInterruptedUntilStop,
    #[cfg(all(test, target_os = "linux"))]
    ProductionFdExport(std::os::fd::RawFd),
}

/// The version-check and probe deadlines of the test-only `Fixture` launch
/// mode. Fixture scripts run as the production background class (nice 15), so
/// a loaded runner can starve them past the 5 s version and 10 s probe
/// deadlines, and the tests using them then failed on `Deadline` rather than
/// on what they assert. No test measures a deadline through this mode: the
/// deadline proofs launch in the `Production*` modes with their own budgets,
/// and those keep the production constants.
#[cfg(test)]
const FIXTURE_HANG_GUARD: Duration = Duration::from_secs(60);

impl ProbeLaunchMode {
    /// How long discovery's `-version` check may take.
    fn version_deadline(self) -> Duration {
        #[cfg(test)]
        if self == Self::Fixture {
            return FIXTURE_HANG_GUARD;
        }
        VERSION_DEADLINE
    }

    /// The ceiling on any caller's probe budget.
    fn probe_deadline(self) -> Duration {
        #[cfg(test)]
        if self == Self::Fixture {
            return FIXTURE_HANG_GUARD;
        }
        PROBE_DEADLINE
    }

    fn is_production(self) -> bool {
        match self {
            Self::Production => true,
            #[cfg(test)]
            Self::Fixture => false,
            #[cfg(all(test, target_os = "linux"))]
            Self::ProductionNamespace
            | Self::ProductionPidfdOpenFailure
            | Self::ProductionPidfdReadFailure
            | Self::ProductionSupervisorDelay
            | Self::ProductionSupervisorReceiveFailure
            | Self::ProductionBootstrapInterrupted
            | Self::ProductionBootstrapInterruptedUntilDeadline(_)
            | Self::ProductionSteadyResponseInterruptedUntilDeadline
            | Self::ProductionSteadyResponseInterruptedUntilStop
            | Self::ProductionFdExport(_) => true,
        }
    }

    #[cfg(target_os = "linux")]
    fn inject_pidfd_open_failure(self) -> bool {
        #[cfg(test)]
        if self == Self::ProductionPidfdOpenFailure {
            return true;
        }
        false
    }

    #[cfg(all(test, target_os = "linux"))]
    fn inject_pidfd_read_failure(self) -> bool {
        if self == Self::ProductionPidfdReadFailure {
            return true;
        }
        false
    }

    #[cfg(all(test, target_os = "linux"))]
    fn inject_slow_reap(self) -> bool {
        matches!(
            self,
            Self::ProductionPidfdOpenFailure | Self::ProductionPidfdReadFailure
        )
    }

    /// The steady-response deadline test holds the detached reap until it
    /// has looked at what the reap still owns, instead of racing a fixed
    /// sleep: see `STEADY_RESPONSE_REAP_RELEASE`.
    #[cfg(all(test, target_os = "linux"))]
    fn inject_held_reap(self) -> bool {
        self == Self::ProductionSteadyResponseInterruptedUntilDeadline
    }

    #[cfg(all(test, target_os = "linux"))]
    fn inject_supervisor_delay(self) -> bool {
        self == Self::ProductionSupervisorDelay
    }

    #[cfg(all(test, target_os = "linux"))]
    fn inject_supervisor_receive_failure(self) -> bool {
        self == Self::ProductionSupervisorReceiveFailure
    }

    #[cfg(all(test, target_os = "linux"))]
    fn export_socket_fd(self) -> Option<std::os::fd::RawFd> {
        match self {
            Self::ProductionFdExport(fd) => Some(fd),
            _ => None,
        }
    }

    #[cfg(target_os = "linux")]
    fn bootstrap_interrupts(self) -> LinuxBootstrapInterrupts {
        #[cfg(test)]
        match self {
            Self::ProductionBootstrapInterrupted => {
                return LinuxBootstrapInterrupts {
                    ready_send: 1,
                    acknowledgement_send: 1,
                    first_notification_receive: 1,
                    first_notification_response: 1,
                    ..LinuxBootstrapInterrupts::default()
                };
            }
            Self::ProductionBootstrapInterruptedUntilDeadline(phase) => {
                let mut interrupts = LinuxBootstrapInterrupts::default();
                *match phase {
                    LinuxBootstrapPhase::ReadySend => &mut interrupts.ready_send,
                    LinuxBootstrapPhase::AcknowledgementSend => {
                        &mut interrupts.acknowledgement_send
                    }
                    LinuxBootstrapPhase::FirstNotificationReceive => {
                        &mut interrupts.first_notification_receive
                    }
                    LinuxBootstrapPhase::FirstNotificationInvalidated => {
                        interrupts.invalidate_first_notification_on_interrupt = true;
                        &mut interrupts.first_notification_receive
                    }
                    LinuxBootstrapPhase::FirstNotificationResponse => {
                        &mut interrupts.first_notification_response
                    }
                } = if phase == LinuxBootstrapPhase::FirstNotificationInvalidated {
                    1
                } else {
                    u8::MAX
                };
                return interrupts;
            }
            Self::ProductionSteadyResponseInterruptedUntilDeadline => {
                return LinuxBootstrapInterrupts {
                    steady_notification_response: u8::MAX,
                    steady_notification_response_interrupts: Some(
                        &STEADY_RESPONSE_DEADLINE_INTERRUPT_HITS,
                    ),
                    steady_notification_response_deadline_exits: Some(
                        &STEADY_RESPONSE_DEADLINE_EXITS,
                    ),
                    ..LinuxBootstrapInterrupts::default()
                };
            }
            Self::ProductionSteadyResponseInterruptedUntilStop => {
                return LinuxBootstrapInterrupts {
                    steady_notification_response: u8::MAX,
                    steady_notification_response_interrupts: Some(
                        &STEADY_RESPONSE_STOP_INTERRUPT_HITS,
                    ),
                    steady_notification_response_deadline_exits: Some(
                        &STEADY_RESPONSE_STOP_DEADLINE_EXITS,
                    ),
                    steady_notification_response_stop_exits: Some(&STEADY_RESPONSE_STOP_EXITS),
                    stop_after_steady_notification_response_interrupt: true,
                    ..LinuxBootstrapInterrupts::default()
                };
            }
            _ => {}
        }
        LinuxBootstrapInterrupts::default()
    }

    #[cfg(all(test, target_os = "linux"))]
    fn supervisor_test_counter(self) -> Option<&'static std::sync::atomic::AtomicUsize> {
        match self {
            Self::ProductionPidfdOpenFailure => Some(&PIDFD_OPEN_SUPERVISOR_OWNERS),
            Self::ProductionPidfdReadFailure => Some(&PIDFD_READ_SUPERVISOR_OWNERS),
            Self::ProductionSupervisorDelay => Some(&DELAYED_SUPERVISOR_OWNERS),
            Self::ProductionSupervisorReceiveFailure => Some(&FAILED_SUPERVISOR_OWNERS),
            Self::ProductionBootstrapInterruptedUntilDeadline(_) => {
                Some(&INTERRUPTED_SUPERVISOR_OWNERS)
            }
            Self::ProductionSteadyResponseInterruptedUntilDeadline => {
                Some(&STEADY_RESPONSE_SUPERVISOR_OWNERS)
            }
            Self::ProductionSteadyResponseInterruptedUntilStop => {
                Some(&STEADY_RESPONSE_STOP_SUPERVISOR_OWNERS)
            }
            _ => None,
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxBootstrapPhase {
    ReadySend,
    AcknowledgementSend,
    FirstNotificationReceive,
    FirstNotificationInvalidated,
    FirstNotificationResponse,
}

/// The points of a decode-fact probe's source-identity observation that a
/// test can delay (TRANSCODE-DECOMPOSITION-PLAN §3.9, M8).
///
/// The source holds one of these in every build, so its layout and the
/// observation's calls are the same in the test and release binaries: the
/// delay is read from the hook and handed to the blocking identity syscall
/// owner, which sleeps only when it is non-zero. Production installs
/// [`NoopDecodeFactSourceHooks`]; tests install [`IdentityDelays`] through
/// `with_identity_delay` and `with_final_identity_delay`. The delay's length
/// is still a test artefact: what this makes identical is the struct and the
/// path the observation takes, not its timing.
pub(crate) trait DecodeFactSourceHooks: Send + Sync {
    /// Held inside the first source-identity observation, before its read.
    fn initial_identity_delay(&self) -> Duration;

    /// Held inside the final source-identity observation, before its read.
    fn final_identity_delay(&self) -> Duration;
}

/// What production installs: no observation is delayed.
pub(crate) struct NoopDecodeFactSourceHooks;

impl DecodeFactSourceHooks for NoopDecodeFactSourceHooks {
    fn initial_identity_delay(&self) -> Duration {
        Duration::ZERO
    }

    fn final_identity_delay(&self) -> Duration {
        Duration::ZERO
    }
}

/// The delays the source-identity race tests install.
#[cfg(test)]
struct IdentityDelays {
    initial: Duration,
    last: Duration,
}

#[cfg(test)]
impl DecodeFactSourceHooks for IdentityDelays {
    fn initial_identity_delay(&self) -> Duration {
        self.initial
    }

    fn final_identity_delay(&self) -> Duration {
        self.last
    }
}

/// A held source descriptor and the exclusive ownership lane for every child
/// that can seek its shared open-file description.
#[derive(Clone)]
pub(crate) struct DecodeFactSource {
    handle: Arc<std::fs::File>,
    offset_gate: Arc<tokio::sync::Semaphore>,
    /// The class and purpose of every probe run for this source: the
    /// caller's, since only it knows whether a viewer is waiting.
    work: crate::process_control::ChildWork,
    projection: ProbeProjection,
    seed: Option<FreshFactSeed>,
    expected_frame_key: Option<CacheKey>,
    /// The observation delays; see [`DecodeFactSourceHooks`]. Shared by
    /// clones, since a clone is the same source.
    hooks: Arc<dyn DecodeFactSourceHooks>,
}

impl DecodeFactSource {
    pub(crate) fn new(
        handle: Arc<std::fs::File>,
        offset_gate: Arc<tokio::sync::Semaphore>,
        work: crate::process_control::ChildWork,
    ) -> Self {
        Self {
            handle,
            offset_gate,
            work,
            projection: ProbeProjection::Decoder,
            seed: None,
            expected_frame_key: None,
            hooks: Arc::new(NoopDecodeFactSourceHooks),
        }
    }

    #[cfg(test)]
    fn isolated(handle: Arc<std::fs::File>) -> Self {
        Self::new(
            handle,
            Arc::new(tokio::sync::Semaphore::new(1)),
            TEST_FACT_WORK,
        )
    }

    #[cfg(test)]
    fn with_identity_delay(mut self, delay: Duration) -> Self {
        self.hooks = Arc::new(IdentityDelays {
            initial: delay,
            last: self.hooks.final_identity_delay(),
        });
        self
    }

    #[cfg(test)]
    pub(crate) fn with_final_identity_delay(mut self, delay: Duration) -> Self {
        self.hooks = Arc::new(IdentityDelays {
            initial: self.hooks.initial_identity_delay(),
            last: delay,
        });
        self
    }

    fn initial_identity_delay(&self) -> Duration {
        self.hooks.initial_identity_delay()
    }

    fn final_identity_delay(&self) -> Duration {
        self.hooks.final_identity_delay()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProbeFileIdentity {
    bytes: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    device: u64,
    inode: u64,
    content_digest: String,
}

impl ProbeFileIdentity {
    /// The seven `fstat` fields, without the content digest. An in-place write
    /// moves `ctime` (an unprivileged writer cannot set it back) and a
    /// replacement moves `inode`, so equal tuples are what lets a validation
    /// skip the hash within one [`PROBE_REVALIDATION_INTERVAL`].
    fn same_tuple(&self, other: &Self) -> bool {
        (
            self.bytes,
            self.modified_seconds,
            self.modified_nanoseconds,
            self.changed_seconds,
            self.changed_nanoseconds,
            self.device,
            self.inode,
        ) == (
            other.bytes,
            other.modified_seconds,
            other.modified_nanoseconds,
            other.changed_seconds,
            other.changed_nanoseconds,
            other.device,
            other.inode,
        )
    }
}

/// Startup-bound parser identity. Linux executes an operator-trusted,
/// self-contained ELF from a sealed anonymous descriptor and prevents later
/// exec. macOS interprets no native parser: a package companion is compiled
/// from immutable copied WebAssembly bytes with only held-source/result
/// capabilities; its generated image cannot create a process or load code. This binds the primary image; it is not a sandbox for malicious
/// parser behavior such as interpreting or mapping readable bytes as code.
/// Path revalidation before and after collection detects replacement of the
/// configured artifact without allowing a transient swap to choose another
/// primary executable object.
#[derive(Debug, Clone)]
pub(crate) struct DecodeProbeIdentity {
    executable: PathBuf,
    executable_file: Arc<std::fs::File>,
    executable_snapshot: Arc<ExecutableSnapshot>,
    build_digest: String,
    reporter: String,
    file: ProbeFileIdentity,
    snapshot_file: ProbeFileIdentity,
    launch_mode: ProbeLaunchMode,
    /// When the held descriptor, the path and the snapshot were last proved
    /// by content; `None` makes the next validation hash. Shared by clones:
    /// it is a fact about the bound objects, not about one holder.
    content_verified_at: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    /// Content verifications `validate_current` has run (discovery's own
    /// hashes are not counted).
    content_verifications: Arc<AtomicU64>,
}

#[derive(Debug)]
struct ExecutableSnapshot {
    file: std::fs::File,
    #[cfg(target_os = "macos")]
    parser: Option<Arc<macos_wasm::Parser>>,
    #[cfg(target_os = "macos")]
    parser_image_digest: Option<String>,
    // Non-Linux fixtures execute an owned temporary path. macOS production
    // instead uses the compiled immutable parser above, never this pathname.
    #[cfg(not(target_os = "linux"))]
    _path: tempfile::TempPath,
}

impl ExecutableSnapshot {
    fn as_file(&self) -> &std::fs::File {
        &self.file
    }

    #[cfg(not(target_os = "linux"))]
    fn path(&self) -> &Path {
        self._path.as_ref()
    }
}

#[cfg(target_os = "macos")]
impl Drop for ExecutableSnapshot {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;

        unsafe {
            libc::fchflags(self.file.as_raw_fd(), 0);
        }
    }
}

impl DecodeProbeIdentity {
    pub(crate) async fn discover(bin: &str) -> Result<Self, DecodeFactError> {
        Self::discover_with_mode(bin, ProbeLaunchMode::Production).await
    }

    #[cfg(test)]
    pub(crate) async fn discover_fixture(bin: &str) -> Result<Self, DecodeFactError> {
        Self::discover_with_mode(bin, ProbeLaunchMode::Fixture).await
    }

    async fn discover_with_mode(
        bin: &str,
        launch_mode: ProbeLaunchMode,
    ) -> Result<Self, DecodeFactError> {
        let discovery_started = std::time::Instant::now();
        // Unit fixtures are executable scripts and exercise many independent
        // startup identities in parallel. Serialize their full discovery so
        // host scheduler load cannot consume a production-scale version
        // deadline before an individual fixture runs.
        #[cfg(test)]
        let _test_discovery = test_discovery_gate()
            .acquire_owned()
            .await
            .map_err(|_| DecodeFactError::CacheInvariant)?;
        let executable = resolve_executable(bin)?;
        #[cfg(target_os = "macos")]
        let executable = if launch_mode.is_production() {
            let companion = executable
                .parent()
                .ok_or_else(|| {
                    DecodeFactError::ProbeIdentity(
                        "configured FFprobe has no package directory".into(),
                    )
                })?
                .join("plurx-source-parser.wasm");
            std::fs::canonicalize(companion).map_err(|error| {
                DecodeFactError::ProbeIdentity(format!(
                    "packaged capability-bound source parser is unavailable: {error}"
                ))
            })?
        } else {
            executable
        };
        let executable_file = Arc::new(
            std::fs::File::open(&executable)
                .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?,
        );
        let before =
            held_probe_file_identity_within(Arc::clone(&executable_file), IDENTITY_DEADLINE, None)
                .await?;
        let snapshot_source = Arc::clone(&executable_file);
        let executable_snapshot = Arc::new(
            run_bounded_identity_task(IDENTITY_DEADLINE, None, move || {
                snapshot_executable(&snapshot_source)
            })
            .await?,
        );
        let snapshot_file = held_probe_file_identity_within(
            Arc::new(
                executable_snapshot
                    .as_file()
                    .try_clone()
                    .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?,
            ),
            IDENTITY_DEADLINE,
            None,
        )
        .await?;
        #[cfg(target_os = "macos")]
        if launch_mode.is_production()
            && executable_snapshot.parser_image_digest.as_deref()
                != Some(before.content_digest.as_str())
        {
            return Err(DecodeFactError::ProbeChanged);
        }
        if snapshot_file.content_digest != before.content_digest {
            return Err(DecodeFactError::ProbeChanged);
        }
        if launch_mode.is_production() {
            require_direct_probe_executable(executable_snapshot.as_file())?;
        }
        let version = probe_version(&executable_snapshot, &executable, launch_mode).await?;
        let after_path = executable.clone();
        let after = path_probe_file_identity_within(after_path, IDENTITY_DEADLINE, None).await?;
        if before != after {
            return Err(DecodeFactError::ProbeChanged);
        }
        #[allow(unused_mut)]
        let mut digest_input = serde_json::json!({
            "canonical_path": &executable,
            "content_digest": &before.content_digest,
            "version": &version,
        });
        #[cfg(target_os = "macos")]
        if launch_mode.is_production() {
            digest_input["parser_abi"] = serde_json::Value::String(macos_wasm::ABI_ID.into());
        }
        Ok(Self {
            executable,
            executable_file,
            executable_snapshot,
            build_digest: hex::encode(Sha256::digest(digest_input.to_string().as_bytes())),
            reporter: String::from_utf8_lossy(&version)
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or_default()
                .chars()
                .take(160)
                .collect(),
            file: before,
            snapshot_file,
            launch_mode,
            content_verified_at: Arc::new(std::sync::Mutex::new(Some(discovery_started))),
            content_verifications: Arc::new(AtomicU64::new(0)),
        })
    }

    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }

    pub(crate) fn content_digest(&self) -> &str {
        &self.file.content_digest
    }

    pub(crate) fn build_digest(&self) -> &str {
        &self.build_digest
    }

    /// Prove the bound probe objects are still the ones discovered.
    ///
    /// Within one [`PROBE_REVALIDATION_INTERVAL`] of the last content proof
    /// this is three `fstat`s (and the snapshot's seals): every VOD start
    /// validates two or three times under the process-wide `probe_gate`, and
    /// three full-content SHA-256s of a static FFprobe each time are what made
    /// concurrent starts queue behind one another. Any tuple difference, or
    /// an elapsed interval, takes the full content path before the verdict.
    async fn validate_current(
        &self,
        budget: Duration,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<(), DecodeFactError> {
        let started = std::time::Instant::now();
        if !self.content_verification_due() {
            let held = Arc::clone(&self.executable_file);
            let snapshot = self
                .executable_snapshot
                .as_file()
                .try_clone()
                .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
            let path = self.executable.clone();
            let (held, snapshot, current) =
                run_bounded_identity_task(budget, cancelled, move || {
                    Ok((
                        probe_file_tuple_from_file(&held)?,
                        snapshot_tuple_from_file(&snapshot)?,
                        probe_path_tuple(&path)?,
                    ))
                })
                .await?;
            if held.same_tuple(&self.file)
                && current.same_tuple(&self.file)
                && snapshot.is_some_and(|snapshot| snapshot.same_tuple(&self.snapshot_file))
            {
                return Ok(());
            }
        }
        let remaining = budget.saturating_sub(started.elapsed());
        self.validate_content(remaining, cancelled).await
    }

    fn content_verification_due(&self) -> bool {
        self.content_verified_at
            .lock()
            .map(|verified| verified.is_none_or(|at| at.elapsed() >= PROBE_REVALIDATION_INTERVAL))
            .unwrap_or(true)
    }

    /// The full check: all three objects hashed and compared with discovery.
    async fn validate_content(
        &self,
        budget: Duration,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<(), DecodeFactError> {
        let started = std::time::Instant::now();
        self.content_verifications.fetch_add(1, Ordering::Relaxed);
        let held =
            held_probe_file_identity_within(Arc::clone(&self.executable_file), budget, cancelled)
                .await?;
        let remaining = budget.saturating_sub(started.elapsed());
        let snapshot = held_probe_file_identity_within(
            Arc::new(
                self.executable_snapshot
                    .as_file()
                    .try_clone()
                    .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?,
            ),
            remaining,
            cancelled,
        )
        .await?;
        let remaining = budget.saturating_sub(started.elapsed());
        let current =
            path_probe_file_identity_within(self.executable.clone(), remaining, cancelled).await?;
        if held == self.file && current == self.file && snapshot == self.snapshot_file {
            if let Ok(mut verified) = self.content_verified_at.lock() {
                *verified = Some(started);
            }
            Ok(())
        } else {
            Err(DecodeFactError::ProbeChanged)
        }
    }
}

#[cfg(test)]
impl DecodeProbeIdentity {
    fn content_verifications(&self) -> u64 {
        self.content_verifications.load(Ordering::Relaxed)
    }

    /// Stand in for [`PROBE_REVALIDATION_INTERVAL`] elapsing.
    fn expire_content_verification(&self) {
        *self.content_verified_at.lock().expect("verification clock") = None;
    }
}

/// Qualified fact collection executes one self-contained primary parser image.
/// Scripts and dynamic ELF images have an external launch/dependency closure,
/// so production refuses them. Structural ELF inspection does not establish
/// that arbitrary parser code is trustworthy or unable to interpret readable
/// input as code; safe enablement requires an operator-trusted artifact.
#[cfg(unix)]
fn require_direct_probe_executable(file: &std::fs::File) -> Result<(), DecodeFactError> {
    #[cfg(target_os = "linux")]
    {
        require_self_contained_linux_elf(file)
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::FileExt;
        let mut magic = [0; 8];
        file.read_exact_at(&mut magic, 0)
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
        if magic == *b"\0asm\x01\0\0\0" {
            Ok(())
        } else {
            Err(DecodeFactError::ProbeIdentity(
                "source parser must be a capability-bound WebAssembly image".into(),
            ))
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = file;
        Err(DecodeFactError::UnsupportedPlatform)
    }
}

#[cfg(windows)]
fn require_direct_probe_executable(file: &std::fs::File) -> Result<(), DecodeFactError> {
    use std::os::windows::fs::FileExt;

    let metadata = file
        .metadata()
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    if !metadata.is_file() || metadata.len() < 64 || metadata.len() > MAX_PROBE_EXECUTABLE_BYTES {
        return Err(DecodeFactError::ProbeIdentity(
            "configured FFprobe is not a bounded PE executable".to_owned(),
        ));
    }
    let mut dos = [0_u8; 64];
    file.seek_read(&mut dos, 0)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    if dos[..2] != *b"MZ" {
        return Err(DecodeFactError::ProbeIdentity(
            "configured FFprobe does not have a PE DOS header".to_owned(),
        ));
    }
    let pe_offset = u32::from_le_bytes(dos[0x3c..0x40].try_into().expect("four bytes")) as u64;
    if pe_offset.saturating_add(6) > metadata.len() {
        return Err(DecodeFactError::ProbeIdentity(
            "configured FFprobe has a truncated PE header".to_owned(),
        ));
    }
    let mut pe = [0_u8; 6];
    file.seek_read(&mut pe, pe_offset)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    if pe[..4] != *b"PE\0\0" || u16::from_le_bytes([pe[4], pe[5]]) != 0x8664 {
        return Err(DecodeFactError::ProbeIdentity(
            "configured FFprobe must be an x64 PE executable".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn require_self_contained_linux_elf(file: &std::fs::File) -> Result<(), DecodeFactError> {
    use std::os::unix::fs::FileExt;

    fn value(bytes: &[u8], little_endian: bool) -> u64 {
        let mut padded = [0_u8; 8];
        padded[..bytes.len()].copy_from_slice(bytes);
        if little_endian {
            u64::from_le_bytes(padded)
        } else {
            padded[..bytes.len()].reverse();
            u64::from_le_bytes(padded)
        }
    }

    let mut header = [0_u8; 64];
    file.read_exact_at(&mut header, 0).map_err(|error| {
        DecodeFactError::ProbeIdentity(format!("FFprobe is not a complete ELF executable: {error}"))
    })?;
    if header[..4] != *b"\x7fELF" {
        return Err(DecodeFactError::ProbeIdentity(
            "configured FFprobe must be a self-contained Linux ELF; wrappers are not build-bound"
                .to_owned(),
        ));
    }
    let little_endian = match header[5] {
        1 => true,
        2 => false,
        _ => {
            return Err(DecodeFactError::ProbeIdentity(
                "configured FFprobe has an unsupported ELF byte order".to_owned(),
            ));
        }
    };
    let (program_offset, entry_size, entry_count) = match header[4] {
        1 => (
            value(&header[28..32], little_endian),
            value(&header[42..44], little_endian),
            value(&header[44..46], little_endian),
        ),
        2 => (
            value(&header[32..40], little_endian),
            value(&header[54..56], little_endian),
            value(&header[56..58], little_endian),
        ),
        _ => {
            return Err(DecodeFactError::ProbeIdentity(
                "configured FFprobe has an unsupported ELF class".to_owned(),
            ));
        }
    };
    if entry_size < 4 || entry_count == 0 || entry_count > 1_024 {
        return Err(DecodeFactError::ProbeIdentity(
            "configured FFprobe has an invalid ELF program table".to_owned(),
        ));
    }
    let table_bytes = entry_size
        .checked_mul(entry_count)
        .and_then(|bytes| program_offset.checked_add(bytes))
        .ok_or_else(|| {
            DecodeFactError::ProbeIdentity("FFprobe ELF program table overflowed".to_owned())
        })?;
    if table_bytes
        > file
            .metadata()
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?
            .len()
    {
        return Err(DecodeFactError::ProbeIdentity(
            "configured FFprobe has a truncated ELF program table".to_owned(),
        ));
    }
    let mut program_type = [0_u8; 4];
    for entry in 0..entry_count {
        file.read_exact_at(&mut program_type, program_offset + entry * entry_size)
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
        let program_type = value(&program_type, little_endian);
        if program_type == 2 || program_type == 3 {
            return Err(DecodeFactError::ProbeIdentity(
                "configured FFprobe must be statically linked with no interpreter or dynamic dependency closure"
                    .to_owned(),
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn resolve_executable(bin: &str) -> Result<PathBuf, DecodeFactError> {
    use std::os::unix::fs::PermissionsExt;

    let candidate = Path::new(bin);
    let resolved = if candidate.components().count() > 1 {
        candidate.to_owned()
    } else {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
            .map(|directory| directory.join(candidate))
            .find(|path| {
                path.metadata().is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
            .ok_or_else(|| DecodeFactError::ProbeIdentity("executable is not on PATH".to_owned()))?
    };
    std::fs::canonicalize(resolved)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))
}

#[cfg(windows)]
fn resolve_executable(bin: &str) -> Result<PathBuf, DecodeFactError> {
    let candidate = Path::new(bin);
    let mut candidates = Vec::new();
    if candidate.components().count() > 1 {
        candidates.push(candidate.to_owned());
    } else {
        for directory in std::env::var_os("PATH")
            .into_iter()
            .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        {
            candidates.push(directory.join(candidate));
            if candidate.extension().is_none() {
                candidates.push(directory.join(format!("{bin}.exe")));
            }
        }
    }
    let resolved = candidates
        .into_iter()
        .find(|path| path.metadata().is_ok_and(|metadata| metadata.is_file()))
        .ok_or_else(|| DecodeFactError::ProbeIdentity("executable is not on PATH".to_owned()))?;
    std::fs::canonicalize(resolved)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))
}

#[cfg(unix)]
fn duplicate_child_fd(source: std::os::fd::RawFd) -> std::io::Result<std::os::fd::RawFd> {
    let duplicate = unsafe { libc::fcntl(source, libc::F_DUPFD_CLOEXEC, 10) };
    if duplicate == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(duplicate)
    }
}

#[cfg(unix)]
fn assign_child_fd(
    duplicate: std::os::fd::RawFd,
    target: std::os::fd::RawFd,
) -> std::io::Result<()> {
    if unsafe { libc::dup2(duplicate, target) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let flags = unsafe { libc::fcntl(target, libc::F_GETFD) };
    if flags == -1 || unsafe { libc::fcntl(target, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } == -1
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn install_child_fd(source: std::os::fd::RawFd, target: std::os::fd::RawFd) -> std::io::Result<()> {
    let duplicate = duplicate_child_fd(source)?;
    let result = assign_child_fd(duplicate, target);
    unsafe { libc::close(duplicate) };
    result
}

#[cfg(unix)]
fn install_probe_child_fds(
    source: std::os::fd::RawFd,
    executable: std::os::fd::RawFd,
) -> std::io::Result<()> {
    let source_duplicate = duplicate_child_fd(source)?;
    let executable_duplicate = match duplicate_child_fd(executable) {
        Ok(duplicate) => duplicate,
        Err(error) => {
            unsafe { libc::close(source_duplicate) };
            return Err(error);
        }
    };
    let result = assign_child_fd(source_duplicate, 3)
        .and_then(|()| assign_child_fd(executable_duplicate, HELD_PROBE_FD));
    unsafe {
        libc::close(source_duplicate);
        libc::close(executable_duplicate);
    }
    result
}

#[cfg(target_os = "linux")]
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
#[cfg(target_os = "linux")]
const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1;

#[cfg(target_os = "linux")]
#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
}

/// Deny execution from every path-backed filesystem. The first parser exec is
/// performed directly from the sealed anonymous descriptor, which Landlock
/// deliberately does not mediate. A separate one-shot seccomp supervisor
/// admits that exact exec and denies every later exec attempt.
#[cfg(target_os = "linux")]
fn install_linux_filesystem_execute_denial() -> std::io::Result<()> {
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if abi < 1 {
        return Err(std::io::Error::last_os_error());
    }
    let ruleset_attr = LandlockRulesetAttr {
        handled_access_fs: LANDLOCK_ACCESS_FS_EXECUTE,
    };
    let ruleset_fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &raw const ruleset_attr,
            std::mem::size_of::<LandlockRulesetAttr>(),
            0,
        )
    };
    if ruleset_fd == -1 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == -1 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(ruleset_fd as libc::c_int) };
        return Err(error);
    }
    let restrict_result = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset_fd, 0) };
    unsafe { libc::close(ruleset_fd as libc::c_int) };
    if restrict_result == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;
#[cfg(target_os = "linux")]
const SECCOMP_DATA_ARGS_OFFSET: u32 = 16;
#[cfg(target_os = "linux")]
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
#[cfg(target_os = "linux")]
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
#[cfg(target_os = "linux")]
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
#[cfg(target_os = "linux")]
const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc0_0000;
#[cfg(target_os = "linux")]
const SECCOMP_FILTER_FLAG_NEW_LISTENER: u32 = 1 << 3;
#[cfg(target_os = "linux")]
const SECCOMP_USER_NOTIF_FLAG_CONTINUE: u32 = 1;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const LINUX_AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const LINUX_AUDIT_ARCH: u32 = 0xc000_00b7;
#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
const LINUX_AUDIT_ARCH: u32 = 0;

#[cfg(target_os = "linux")]
fn seccomp_statement(code: u16, value: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k: value,
    }
}

#[cfg(target_os = "linux")]
fn seccomp_jump(code: u16, value: u32, yes: u8, no: u8) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: yes,
        jf: no,
        k: value,
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
struct LinuxProbeSeccomp {
    filter: Vec<libc::sock_filter>,
    filter_len: u16,
}

#[cfg(target_os = "linux")]
impl LinuxProbeSeccomp {
    fn install(&self) -> std::io::Result<()> {
        let program = libc::sock_fprog {
            len: self.filter_len,
            filter: self.filter.as_ptr().cast_mut(),
        };
        if unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                0,
                &raw const program,
            )
        } == -1
        {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn install_listener(&self) -> std::io::Result<std::os::fd::OwnedFd> {
        use std::os::fd::FromRawFd;

        let program = libc::sock_fprog {
            len: self.filter_len,
            filter: self.filter.as_ptr().cast_mut(),
        };
        let listener = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                SECCOMP_FILTER_FLAG_NEW_LISTENER,
                &raw const program,
            )
        };
        if listener == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            let listener = libc::c_int::try_from(listener)
                .map_err(|_| std::io::Error::from_raw_os_error(libc::EOVERFLOW))?;
            Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(listener) })
        }
    }
}

/// Keep FD 4 permanently bound to the sealed parser, deny process-group escape,
/// and route all descriptor execution to a one-shot supervisor.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn build_linux_probe_seccomp() -> std::io::Result<LinuxProbeSeccomp> {
    const LOAD_WORD_ABSOLUTE: u16 = 0x20;
    const JUMP_EQUAL: u16 = 0x15;
    const JUMP_GREATER_THAN: u16 = 0x25;
    const JUMP_GREATER_OR_EQUAL: u16 = 0x35;
    const RETURN_CONSTANT: u16 = 0x06;
    const DENIED: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;

    let mut filter = vec![
        seccomp_statement(LOAD_WORD_ABSOLUTE, SECCOMP_DATA_ARCH_OFFSET),
        seccomp_jump(JUMP_EQUAL, LINUX_AUDIT_ARCH, 1, 0),
        seccomp_statement(RETURN_CONSTANT, SECCOMP_RET_KILL_PROCESS),
        seccomp_statement(LOAD_WORD_ABSOLUTE, 0),
    ];
    #[cfg(target_arch = "x86_64")]
    filter.extend([
        // x32 shares AUDIT_ARCH_X86_64 but offsets its syscall table. Refuse
        // that alternate ABI so native-number checks cannot be bypassed.
        seccomp_jump(0x45, 0x4000_0000, 0, 1),
        seccomp_statement(RETURN_CONSTANT, DENIED),
    ]);
    for syscall in [
        libc::SYS_execve,
        libc::SYS_recvmsg,
        libc::SYS_recvmmsg,
        libc::SYS_sendmmsg,
        libc::SYS_pidfd_getfd,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_setsid,
        libc::SYS_setpgid,
    ] {
        filter.push(seccomp_jump(JUMP_EQUAL, syscall as u32, 0, 1));
        filter.push(seccomp_statement(RETURN_CONSTANT, DENIED));
    }
    let descriptor_mutations = [
        (libc::SYS_close, 0_u32),
        (libc::SYS_fcntl, 0_u32),
        (libc::SYS_dup3, 1_u32),
        #[cfg(target_arch = "x86_64")]
        (libc::SYS_dup2, 1_u32),
    ];
    for (syscall, argument) in descriptor_mutations {
        filter.push(seccomp_jump(JUMP_EQUAL, syscall as u32, 0, 3));
        filter.push(seccomp_statement(
            LOAD_WORD_ABSOLUTE,
            SECCOMP_DATA_ARGS_OFFSET + argument * 8,
        ));
        filter.push(seccomp_jump(JUMP_EQUAL, HELD_PROBE_FD as u32, 0, 1));
        filter.push(seccomp_statement(RETURN_CONSTANT, DENIED));
        filter.push(seccomp_statement(LOAD_WORD_ABSOLUTE, 0));
    }
    filter.extend([
        seccomp_jump(JUMP_EQUAL, libc::SYS_close_range as u32, 0, 5),
        seccomp_statement(LOAD_WORD_ABSOLUTE, SECCOMP_DATA_ARGS_OFFSET),
        seccomp_jump(JUMP_GREATER_THAN, HELD_PROBE_FD as u32, 3, 0),
        seccomp_statement(LOAD_WORD_ABSOLUTE, SECCOMP_DATA_ARGS_OFFSET + 8),
        seccomp_jump(JUMP_GREATER_OR_EQUAL, HELD_PROBE_FD as u32, 0, 1),
        seccomp_statement(RETURN_CONSTANT, DENIED),
        seccomp_statement(LOAD_WORD_ABSOLUTE, 0),
        // A classic BPF filter cannot inspect the pathname or remember that
        // the first exec already happened. Every execveat therefore goes to a
        // one-shot supervisor: it continues only the trusted pre-exec call and
        // returns EPERM for every request made by the installed image or a
        // descendant.
        seccomp_jump(JUMP_EQUAL, libc::SYS_execveat as u32, 0, 1),
        seccomp_statement(RETURN_CONSTANT, SECCOMP_RET_USER_NOTIF),
        seccomp_statement(RETURN_CONSTANT, SECCOMP_RET_ALLOW),
    ]);
    let filter_len = u16::try_from(filter.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "probe seccomp program exceeded the kernel length field",
        )
    })?;
    Ok(LinuxProbeSeccomp { filter, filter_len })
}

/// The bootstrap filter must allow one `sendmsg` so the child can transfer
/// its listener. Stack this filter immediately afterwards; the installed
/// parser and every descendant then fail closed if they try to export a held
/// descriptor with SCM_RIGHTS.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn build_linux_probe_post_transfer_seccomp() -> std::io::Result<LinuxProbeSeccomp> {
    const LOAD_WORD_ABSOLUTE: u16 = 0x20;
    const JUMP_EQUAL: u16 = 0x15;
    const RETURN_CONSTANT: u16 = 0x06;
    const DENIED: u32 = SECCOMP_RET_ERRNO | libc::EPERM as u32;

    let mut filter = vec![
        seccomp_statement(LOAD_WORD_ABSOLUTE, SECCOMP_DATA_ARCH_OFFSET),
        seccomp_jump(JUMP_EQUAL, LINUX_AUDIT_ARCH, 1, 0),
        seccomp_statement(RETURN_CONSTANT, SECCOMP_RET_KILL_PROCESS),
        seccomp_statement(LOAD_WORD_ABSOLUTE, 0),
    ];
    #[cfg(target_arch = "x86_64")]
    filter.extend([
        seccomp_jump(0x45, 0x4000_0000, 0, 1),
        seccomp_statement(RETURN_CONSTANT, DENIED),
    ]);
    for syscall in [libc::SYS_sendmsg, libc::SYS_sendmmsg] {
        filter.push(seccomp_jump(JUMP_EQUAL, syscall as u32, 0, 1));
        filter.push(seccomp_statement(RETURN_CONSTANT, DENIED));
    }
    filter.push(seccomp_statement(RETURN_CONSTANT, SECCOMP_RET_ALLOW));
    let filter_len = u16::try_from(filter.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "post-transfer seccomp program exceeded the kernel length field",
        )
    })?;
    Ok(LinuxProbeSeccomp { filter, filter_len })
}

#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
fn build_linux_probe_post_transfer_seccomp() -> std::io::Result<LinuxProbeSeccomp> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "qualified FFprobe post-transfer seccomp is unsupported on this architecture",
    ))
}

#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
fn build_linux_probe_seccomp() -> std::io::Result<LinuxProbeSeccomp> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "qualified FFprobe seccomp is unsupported on this architecture",
    ))
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Default)]
struct LinuxSeccompData {
    nr: libc::c_int,
    arch: u32,
    instruction_pointer: u64,
    args: [u64; 6],
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Default)]
struct LinuxSeccompNotification {
    id: u64,
    pid: u32,
    flags: u32,
    data: LinuxSeccompData,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Debug, Default)]
struct LinuxSeccompResponse {
    id: u64,
    val: i64,
    error: i32,
    flags: u32,
}

/// Per-launch EINTR injection stays on the stack so parallel tests cannot
/// interfere with one another. Production always constructs the zero value.
#[cfg(target_os = "linux")]
#[derive(Debug, Default)]
struct LinuxBootstrapInterrupts {
    ready_send: u8,
    acknowledgement_send: u8,
    first_notification_receive: u8,
    first_notification_response: u8,
    steady_notification_response: u8,
    /// Test-only launch modes attach a mode-local observation counter so the
    /// regression cannot pass merely because some earlier operation stalled.
    steady_notification_response_interrupts: Option<&'static std::sync::atomic::AtomicUsize>,
    /// Distinct exit observations make deadline and stop mutations fail their
    /// corresponding regressions even if outer task cleanup eventually stops
    /// the same supervisor thread.
    steady_notification_response_deadline_exits: Option<&'static std::sync::atomic::AtomicUsize>,
    steady_notification_response_stop_exits: Option<&'static std::sync::atomic::AtomicUsize>,
    /// Test-only stop-path injection. Production constructs `false` through
    /// `Default`, so only the explicit regression can request this signal.
    stop_after_steady_notification_response_interrupt: bool,
    invalidate_first_notification_on_interrupt: bool,
    first_notification_invalidated: bool,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, Default)]
struct LinuxNotificationInterruptionHooks {
    interrupt_observer: Option<&'static std::sync::atomic::AtomicUsize>,
    deadline_exit_observer: Option<&'static std::sync::atomic::AtomicUsize>,
    stop_exit_observer: Option<&'static std::sync::atomic::AtomicUsize>,
    stop_after_interrupt: bool,
}

#[cfg(target_os = "linux")]
fn consume_linux_bootstrap_interrupt(remaining: &mut u8) -> bool {
    if *remaining == 0 {
        false
    } else {
        // `u8::MAX` is the test-only persistent mode. It exercises expiry of
        // the shared absolute launch deadline without process-global signals
        // or errno mutation.
        if *remaining != u8::MAX {
            *remaining -= 1;
        }
        true
    }
}

#[cfg(target_os = "linux")]
fn ensure_linux_probe_deadline(deadline: Option<std::time::Instant>) -> std::io::Result<()> {
    if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
        Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT))
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
const fn linux_ioctl_read_write<T>(number: u8) -> libc::c_ulong {
    const IOC_WRITE: libc::c_ulong = 1;
    const IOC_READ: libc::c_ulong = 2;
    const IOC_DIRECTION_SHIFT: libc::c_ulong = 30;
    const IOC_SIZE_SHIFT: libc::c_ulong = 16;
    const IOC_TYPE_SHIFT: libc::c_ulong = 8;
    ((IOC_READ | IOC_WRITE) << IOC_DIRECTION_SHIFT)
        | ((std::mem::size_of::<T>() as libc::c_ulong) << IOC_SIZE_SHIFT)
        | ((b'!' as libc::c_ulong) << IOC_TYPE_SHIFT)
        | number as libc::c_ulong
}

#[cfg(target_os = "linux")]
fn receive_linux_seccomp_notification(
    listener: std::os::fd::RawFd,
    injected_interrupts: &mut u8,
) -> std::io::Result<LinuxSeccompNotification> {
    let mut notification = LinuxSeccompNotification::default();
    let injected = consume_linux_bootstrap_interrupt(injected_interrupts);
    let received = if injected {
        -1
    } else {
        unsafe {
            libc::ioctl(
                listener,
                linux_ioctl_read_write::<LinuxSeccompNotification>(0),
                &raw mut notification,
            )
        }
    };
    if received == 0 {
        Ok(notification)
    } else if injected {
        // A just-consumed test injection represents EINTR without mutating
        // process-global errno.
        Err(std::io::Error::from_raw_os_error(libc::EINTR))
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn answer_linux_seccomp_notification_until(
    listener: std::os::fd::RawFd,
    response: &mut LinuxSeccompResponse,
    deadline: Option<std::time::Instant>,
    stop: Option<&AtomicBool>,
    injected_interrupts: &mut u8,
    test_hooks: LinuxNotificationInterruptionHooks,
) -> std::io::Result<()> {
    loop {
        if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            if let Some(observer) = test_hooks.stop_exit_observer {
                observer.fetch_add(1, Ordering::Release);
            }
            return Ok(());
        }
        if let Err(error) = ensure_linux_probe_deadline(deadline) {
            if let Some(observer) = test_hooks.deadline_exit_observer {
                observer.fetch_add(1, Ordering::Release);
            }
            return Err(error);
        }
        let injected = consume_linux_bootstrap_interrupt(injected_interrupts);
        if injected {
            if let Some(observer) = test_hooks.interrupt_observer {
                observer.fetch_add(1, Ordering::Release);
            }
            if test_hooks.stop_after_interrupt {
                if let Some(stop) = stop {
                    stop.store(true, Ordering::Release);
                }
            }
        }
        let answered = if injected {
            -1
        } else {
            unsafe {
                libc::ioctl(
                    listener,
                    linux_ioctl_read_write::<LinuxSeccompResponse>(1),
                    response as *mut LinuxSeccompResponse,
                )
            }
        };
        if answered == 0 {
            return Ok(());
        }
        let error = if injected {
            std::io::Error::from_raw_os_error(libc::EINTR)
        } else {
            std::io::Error::last_os_error()
        };
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(target_os = "linux")]
fn send_linux_seccomp_listener(
    socket: std::os::fd::RawFd,
    listener: std::os::fd::RawFd,
) -> std::io::Result<()> {
    let mut payload = [0_u8; 1];
    let mut io = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: payload.len(),
    };
    // CMSG_* may dereference a cmsghdr at this address, so the backing storage
    // must have native alignment rather than the byte alignment of `[u8; N]`.
    let mut control = [0 as libc::c_ulong; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut io;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    let header = unsafe { libc::CMSG_FIRSTHDR(&raw const message) };
    if header.is_null() {
        return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
    }
    unsafe {
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) as usize;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<libc::c_int>(), listener);
        message.msg_controllen = (*header).cmsg_len;
    }
    if unsafe {
        libc::sendmsg(
            socket,
            &raw const message,
            libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
        )
    } == 1
    {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn receive_linux_seccomp_listener(
    socket: std::os::fd::RawFd,
    deadline: std::time::Instant,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;

    let mut payload = [0_u8; 1];
    let mut io = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: payload.len(),
    };
    // Match the native cmsghdr alignment required by CMSG_FIRSTHDR/CMSG_DATA.
    let mut control = [0 as libc::c_ulong; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut io;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    wait_for_linux_fd(socket, libc::POLLIN, deadline)?;
    let received = loop {
        let received = unsafe { libc::recvmsg(socket, &raw mut message, libc::MSG_CMSG_CLOEXEC) };
        if received != -1 {
            break received;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
        wait_for_linux_fd(socket, libc::POLLIN, deadline)?;
    };
    if received != 1 {
        return Err(if received == -1 {
            std::io::Error::last_os_error()
        } else {
            std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "probe exited before transferring its seccomp listener",
            )
        });
    }
    if message.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "probe seccomp listener control message was truncated",
        ));
    }
    let header = unsafe { libc::CMSG_FIRSTHDR(&raw const message) };
    if header.is_null()
        || unsafe { (*header).cmsg_level } != libc::SOL_SOCKET
        || unsafe { (*header).cmsg_type } != libc::SCM_RIGHTS
        || unsafe { (*header).cmsg_len }
            < unsafe { libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) as usize }
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "probe transferred an invalid seccomp listener",
        ));
    }
    let listener =
        unsafe { std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<libc::c_int>()) };
    if listener < 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "probe transferred a negative seccomp listener",
        ));
    }
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(listener) })
}

#[cfg(target_os = "linux")]
fn wait_for_linux_fd(
    fd: std::os::fd::RawFd,
    events: libc::c_short,
    deadline: std::time::Instant,
) -> std::io::Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT));
        }
        let timeout_ms = i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX);
        let mut descriptor = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&raw mut descriptor, 1, timeout_ms) };
        if ready > 0 && descriptor.revents & events != 0 {
            return Ok(());
        }
        if ready == 0 {
            return Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT));
        }
        if ready == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        return Err(std::io::Error::from_raw_os_error(libc::EPIPE));
    }
}

#[cfg(target_os = "linux")]
fn receive_linux_seccomp_notification_before(
    listener: std::os::fd::RawFd,
    deadline: std::time::Instant,
    interrupts: &mut LinuxBootstrapInterrupts,
) -> std::io::Result<LinuxSeccompNotification> {
    loop {
        wait_for_linux_fd(listener, libc::POLLIN, deadline)?;
        let received = if interrupts.first_notification_invalidated {
            Err(std::io::Error::from_raw_os_error(libc::ENOENT))
        } else {
            receive_linux_seccomp_notification(listener, &mut interrupts.first_notification_receive)
        };
        match received {
            Ok(notification) => return Ok(notification),
            Err(error)
                if error.kind() == std::io::ErrorKind::Interrupted
                    && interrupts.invalidate_first_notification_on_interrupt =>
            {
                // Model the kernel race where target interruption invalidates
                // the ready notification. Every retry must return to poll and
                // the absolute deadline rather than blocking in RECV.
                interrupts.first_notification_invalidated = true;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::Interrupted
                    || matches!(
                        error.raw_os_error(),
                        Some(libc::ENOENT) | Some(libc::EAGAIN)
                    ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct LinuxProbeReceiver {
    descriptor: std::sync::Mutex<Option<std::os::fd::OwnedFd>>,
}

#[cfg(target_os = "linux")]
impl LinuxProbeReceiver {
    fn raw_fd(&self) -> std::io::Result<std::os::fd::RawFd> {
        self.descriptor
            .lock()
            .map_err(|_| std::io::Error::other("probe receiver owner was poisoned"))?
            .as_ref()
            .map(std::os::fd::AsRawFd::as_raw_fd)
            .ok_or_else(|| std::io::Error::other("probe receiver was already closed"))
    }

    fn shutdown(&self) {
        if let Ok(descriptor) = self.descriptor.lock() {
            if let Some(descriptor) = descriptor.as_ref() {
                use std::os::fd::AsRawFd;
                unsafe {
                    libc::shutdown(descriptor.as_raw_fd(), libc::SHUT_RDWR);
                }
            }
        }
    }

    fn close(&self) {
        self.shutdown();
        if let Ok(mut descriptor) = self.descriptor.lock() {
            descriptor.take();
        }
    }
}

#[cfg(target_os = "linux")]
fn receive_linux_probe_fork_ready(
    socket: std::os::fd::RawFd,
    deadline: std::time::Instant,
) -> std::io::Result<()> {
    wait_for_linux_fd(socket, libc::POLLIN, deadline)?;
    let mut byte = 0_u8;
    loop {
        let received = unsafe { libc::recv(socket, (&raw mut byte).cast(), 1, 0) };
        if received == 1 && byte == b'F' {
            return Ok(());
        }
        if received == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                wait_for_linux_fd(socket, libc::POLLIN, deadline)?;
                continue;
            }
            return Err(error);
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "probe child did not confirm its post-fork descriptor table",
        ));
    }
}

#[cfg(target_os = "linux")]
fn acknowledge_linux_probe_fork(
    socket: std::os::fd::RawFd,
    deadline: std::time::Instant,
    injected_interrupts: &mut u8,
) -> std::io::Result<()> {
    let byte = b'A';
    loop {
        ensure_linux_probe_deadline(Some(deadline))?;
        let injected = consume_linux_bootstrap_interrupt(injected_interrupts);
        if !injected
            && unsafe {
                libc::send(
                    socket,
                    (&raw const byte).cast(),
                    1,
                    libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
                )
            } == 1
        {
            return Ok(());
        }
        let error = if injected {
            std::io::Error::from_raw_os_error(libc::EINTR)
        } else {
            std::io::Error::last_os_error()
        };
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(target_os = "linux")]
fn confirm_linux_probe_fork(
    socket: std::os::fd::RawFd,
    deadline: std::time::Instant,
    injected_interrupts: &mut u8,
) -> std::io::Result<()> {
    let byte = b'F';
    loop {
        ensure_linux_probe_deadline(Some(deadline))?;
        let injected = consume_linux_bootstrap_interrupt(injected_interrupts);
        if !injected
            && unsafe {
                libc::send(
                    socket,
                    (&raw const byte).cast(),
                    1,
                    libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
                )
            } == 1
        {
            break;
        }
        let error = if injected {
            std::io::Error::from_raw_os_error(libc::EINTR)
        } else {
            std::io::Error::last_os_error()
        };
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    wait_for_linux_fd(socket, libc::POLLIN, deadline)?;
    let mut acknowledgement = 0_u8;
    loop {
        let received = unsafe { libc::recv(socket, (&raw mut acknowledgement).cast(), 1, 0) };
        if received == 1 && acknowledgement == b'A' {
            return Ok(());
        }
        if received == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                wait_for_linux_fd(socket, libc::POLLIN, deadline)?;
                continue;
            }
            return Err(error);
        }
        return Err(std::io::Error::from_raw_os_error(libc::EPIPE));
    }
}

#[cfg(target_os = "linux")]
fn supervise_linux_probe_execs(
    receiver: Arc<LinuxProbeReceiver>,
    stop: Arc<AtomicBool>,
    launch_mode: ProbeLaunchMode,
    launch_deadline: std::time::Instant,
) -> std::io::Result<()> {
    let mut interrupts = launch_mode.bootstrap_interrupts();

    struct ReceiverCleanup {
        receiver: Arc<LinuxProbeReceiver>,
        fork_confirmed: bool,
    }
    impl Drop for ReceiverCleanup {
        fn drop(&mut self) {
            if self.fork_confirmed {
                self.receiver.close();
            } else {
                // Until the child confirms fork, keep the descriptor number
                // occupied while shutdown unblocks any eventual handshake.
                self.receiver.shutdown();
            }
        }
    }
    let socket = receiver.raw_fd()?;
    let mut receiver_cleanup = ReceiverCleanup {
        receiver: Arc::clone(&receiver),
        fork_confirmed: false,
    };
    receive_linux_probe_fork_ready(socket, launch_deadline)?;
    receiver_cleanup.fork_confirmed = true;
    #[cfg(test)]
    if launch_mode.inject_supervisor_delay() {
        std::thread::sleep(Duration::from_millis(250));
    }
    #[cfg(test)]
    if launch_mode.inject_supervisor_receive_failure() {
        return Err(std::io::Error::other(
            "injected seccomp listener receive failure",
        ));
    }
    if std::time::Instant::now() >= launch_deadline {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "probe supervisor launch handshake timed out",
        ));
    }
    acknowledge_linux_probe_fork(
        socket,
        launch_deadline,
        &mut interrupts.acknowledgement_send,
    )?;
    let listener = receive_linux_seccomp_listener(socket, launch_deadline)?;
    receiver.close();
    use std::os::fd::AsRawFd;
    let first = receive_linux_seccomp_notification_before(
        listener.as_raw_fd(),
        launch_deadline,
        &mut interrupts,
    )?;
    if first.flags != 0
        || first.data.arch != LINUX_AUDIT_ARCH
        || first.data.nr != libc::SYS_execveat as libc::c_int
        || first.data.args[0] != HELD_PROBE_FD as u64
        || first.data.args[4] != libc::AT_EMPTY_PATH as u64
    {
        let mut denied = LinuxSeccompResponse {
            id: first.id,
            error: -libc::EPERM,
            ..LinuxSeccompResponse::default()
        };
        let _ = answer_linux_seccomp_notification_until(
            listener.as_raw_fd(),
            &mut denied,
            Some(launch_deadline),
            None,
            &mut interrupts.first_notification_response,
            LinuxNotificationInterruptionHooks::default(),
        );
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "first supervised probe exec did not match the bound descriptor call",
        ));
    }
    // The first notification can only originate in the single-threaded
    // post-fork pre-exec closure: parser code has not run yet and cannot race
    // its arguments. Continuing it is therefore the one safe use of the
    // notification API; every request after the image starts is denied.
    let mut allowed = LinuxSeccompResponse {
        id: first.id,
        flags: SECCOMP_USER_NOTIF_FLAG_CONTINUE,
        ..LinuxSeccompResponse::default()
    };
    answer_linux_seccomp_notification_until(
        listener.as_raw_fd(),
        &mut allowed,
        Some(launch_deadline),
        None,
        &mut interrupts.first_notification_response,
        LinuxNotificationInterruptionHooks::default(),
    )?;

    while !stop.load(Ordering::Acquire) {
        let mut descriptor = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&raw mut descriptor, 1, 25) };
        if ready == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            continue;
        }
        if descriptor.revents & libc::POLLIN == 0 {
            // HUP after the last filtered task exits is the normal terminal
            // event. Closing this listener also leaves every racing notified
            // exec failed closed by the kernel.
            break;
        }
        let mut no_interrupts = 0;
        let request =
            match receive_linux_seccomp_notification(listener.as_raw_fd(), &mut no_interrupts) {
                Ok(request) => request,
                Err(error)
                    if error.kind() == std::io::ErrorKind::Interrupted
                        || matches!(
                            error.raw_os_error(),
                            Some(libc::ENOENT) | Some(libc::EAGAIN)
                        ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
        let mut denied = LinuxSeccompResponse {
            id: request.id,
            error: -libc::EPERM,
            ..LinuxSeccompResponse::default()
        };
        match answer_linux_seccomp_notification_until(
            listener.as_raw_fd(),
            &mut denied,
            Some(launch_deadline),
            Some(stop.as_ref()),
            &mut interrupts.steady_notification_response,
            LinuxNotificationInterruptionHooks {
                interrupt_observer: interrupts.steady_notification_response_interrupts,
                deadline_exit_observer: interrupts.steady_notification_response_deadline_exits,
                stop_exit_observer: interrupts.steady_notification_response_stop_exits,
                stop_after_interrupt: interrupts.stop_after_steady_notification_response_interrupt,
            },
        ) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct LinuxProbeExecSupervisor {
    child_sender: Option<std::os::fd::OwnedFd>,
    receiver: Option<Arc<LinuxProbeReceiver>>,
    child_sender_fd: std::os::fd::RawFd,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<std::io::Result<()>>>,
    #[cfg(test)]
    test_counter: Option<&'static std::sync::atomic::AtomicUsize>,
}

#[cfg(target_os = "linux")]
impl LinuxProbeExecSupervisor {
    fn start(
        launch_mode: ProbeLaunchMode,
        launch_deadline: std::time::Instant,
    ) -> std::io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};

        let mut sockets = [-1; 2];
        if unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                sockets.as_mut_ptr(),
            )
        } == -1
        {
            return Err(std::io::Error::last_os_error());
        }
        let receiver_fd = unsafe { libc::fcntl(sockets[0], libc::F_DUPFD_CLOEXEC, 10) };
        let sender_fd = unsafe { libc::fcntl(sockets[1], libc::F_DUPFD_CLOEXEC, 10) };
        unsafe {
            libc::close(sockets[0]);
            libc::close(sockets[1]);
        }
        if receiver_fd == -1 || sender_fd == -1 {
            if receiver_fd != -1 {
                unsafe { libc::close(receiver_fd) };
            }
            if sender_fd != -1 {
                unsafe { libc::close(sender_fd) };
            }
            return Err(std::io::Error::last_os_error());
        }
        let receiver = Arc::new(LinuxProbeReceiver {
            descriptor: std::sync::Mutex::new(Some(unsafe {
                std::os::fd::OwnedFd::from_raw_fd(receiver_fd)
            })),
        });
        let child_sender = unsafe { std::os::fd::OwnedFd::from_raw_fd(sender_fd) };
        let child_sender_fd = child_sender.as_raw_fd();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_receiver = Arc::clone(&receiver);
        let worker = std::thread::Builder::new()
            .name("plurx-probe-exec".to_owned())
            .spawn(move || {
                supervise_linux_probe_execs(
                    worker_receiver,
                    worker_stop,
                    launch_mode,
                    launch_deadline,
                )
            })?;
        #[cfg(test)]
        let test_counter = launch_mode.supervisor_test_counter();
        #[cfg(test)]
        if let Some(counter) = test_counter {
            counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        Ok(Self {
            child_sender: Some(child_sender),
            receiver: Some(receiver),
            child_sender_fd,
            stop,
            worker: Some(worker),
            #[cfg(test)]
            test_counter,
        })
    }

    fn child_sender_fd(&self) -> std::os::fd::RawFd {
        self.child_sender_fd
    }

    fn child_receiver_fd(&self) -> std::os::fd::RawFd {
        self.receiver
            .as_ref()
            .expect("receiver exists through spawn")
            .raw_fd()
            .expect("receiver descriptor exists through fork")
    }

    fn parent_after_spawn(&mut self) {
        self.child_sender.take();
        self.receiver.take();
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.parent_after_spawn();
        self.stop.store(true, Ordering::Release);
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        worker
            .join()
            .map_err(|_| std::io::Error::other("probe seccomp supervisor panicked"))?
    }
}

#[cfg(target_os = "linux")]
impl Drop for LinuxProbeExecSupervisor {
    fn drop(&mut self) {
        self.parent_after_spawn();
        self.stop.store(true, Ordering::Release);
        #[cfg(test)]
        if let Some(counter) = self.test_counter.take() {
            counter.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
static PIDFD_OPEN_SUPERVISOR_OWNERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(all(test, target_os = "linux"))]
static PIDFD_READ_SUPERVISOR_OWNERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(all(test, target_os = "linux"))]
static DELAYED_SUPERVISOR_OWNERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(all(test, target_os = "linux"))]
static FAILED_SUPERVISOR_OWNERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(all(test, target_os = "linux"))]
static INTERRUPTED_SUPERVISOR_OWNERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(all(test, target_os = "linux"))]
static STEADY_RESPONSE_SUPERVISOR_OWNERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(all(test, target_os = "linux"))]
static STEADY_RESPONSE_STOP_SUPERVISOR_OWNERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(all(test, target_os = "linux"))]
static STEADY_RESPONSE_DEADLINE_INTERRUPT_HITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(all(test, target_os = "linux"))]
static STEADY_RESPONSE_STOP_INTERRUPT_HITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(all(test, target_os = "linux"))]
static STEADY_RESPONSE_DEADLINE_EXITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Deadline exits of the stop-path proof's response loop, which must stay
/// zero: the stop signal, not the launch deadline, is what releases it.
#[cfg(all(test, target_os = "linux"))]
static STEADY_RESPONSE_STOP_DEADLINE_EXITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The detached reap of the steady-response deadline proof waits for one
/// permit here, so the test can observe it still holding version ownership
/// for as long as it likes — the slow reap it stands for is unbounded — and
/// then let it finish. A fixed sleep made that observation a race.
#[cfg(all(test, target_os = "linux"))]
static STEADY_RESPONSE_REAP_RELEASE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(0);

#[cfg(all(test, target_os = "linux"))]
static STEADY_RESPONSE_STOP_EXITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(unix)]
struct ProbeExecutionSupervisor {
    #[cfg(target_os = "linux")]
    linux: Option<LinuxProbeExecSupervisor>,
}

#[cfg(unix)]
impl ProbeExecutionSupervisor {
    fn none() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            linux: None,
        }
    }

    #[cfg(target_os = "linux")]
    fn linux(supervisor: LinuxProbeExecSupervisor) -> Self {
        Self {
            linux: Some(supervisor),
        }
    }

    fn parent_after_spawn(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(supervisor) = self.linux.as_mut() {
            supervisor.parent_after_spawn();
        }
    }

    fn finish(&mut self) -> Result<(), DecodeFactError> {
        #[cfg(target_os = "linux")]
        if let Some(supervisor) = self.linux.as_mut() {
            supervisor
                .finish()
                .map_err(|error| DecodeFactError::Read(error.to_string()))?;
        }
        Ok(())
    }
}

/// The probe binary's `-version` check at discovery: nobody is waiting on it.
/// A source's probes take their class from [`DecodeFactSource`] instead.
const DECODE_FACT_VERSION_PROBE: crate::process_control::ChildWork =
    crate::process_control::ChildWork::background("decode-fact probe version check");

#[cfg(test)]
const TEST_FACT_WORK: crate::process_control::ChildWork =
    crate::process_control::ChildWork::background("decode-fact test probe");

#[cfg(unix)]
async fn spawn_configured_probe(
    mut command: tokio::process::Command,
    mut supervisor: ProbeExecutionSupervisor,
    work: crate::process_control::ChildWork,
) -> Result<
    (
        tokio::process::Child,
        crate::process_control::ChildJob,
        ProbeExecutionSupervisor,
    ),
    DecodeFactError,
> {
    tokio::task::spawn_blocking(move || {
        // `configure_probe_execution` already registered the priority ahead
        // of its own exec-from-`pre_exec`; the launcher's second registration
        // lands after that closure and never runs.
        let spawned = crate::process_control::spawn_job_owned(&mut command, work);
        supervisor.parent_after_spawn();
        match spawned {
            Ok((child, job)) => Ok((child, job, supervisor)),
            Err(error) => {
                let _ = supervisor.finish();
                Err(DecodeFactError::Spawn(error.to_string()))
            }
        }
    })
    .await
    .map_err(|error| DecodeFactError::Spawn(error.to_string()))?
}

#[cfg(target_os = "linux")]
fn mark_unrelated_fds_close_on_exec() -> std::io::Result<()> {
    let result = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            HELD_PROBE_FD + 1,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if result == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
struct LinuxExecveArguments {
    _argv: Vec<CString>,
    argv_pointers: Vec<usize>,
    _environment: Vec<CString>,
    environment_pointers: Vec<usize>,
}

#[cfg(target_os = "linux")]
impl LinuxExecveArguments {
    fn new(arg0: &OsStr, arguments: &[OsString]) -> std::io::Result<Self> {
        use std::os::unix::ffi::OsStrExt;

        let mut argv = Vec::with_capacity(arguments.len() + 1);
        argv.push(CString::new(arg0.as_bytes())?);
        for argument in arguments {
            argv.push(CString::new(argument.as_os_str().as_bytes())?);
        }
        let environment = std::env::vars_os()
            .map(|(key, value)| {
                let mut entry = key.as_os_str().as_bytes().to_vec();
                entry.push(b'=');
                entry.extend_from_slice(value.as_os_str().as_bytes());
                CString::new(entry)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut argv_pointers = argv
            .iter()
            .map(|argument| argument.as_ptr() as usize)
            .collect::<Vec<_>>();
        argv_pointers.push(0);
        let mut environment_pointers = environment
            .iter()
            .map(|entry| entry.as_ptr() as usize)
            .collect::<Vec<_>>();
        environment_pointers.push(0);
        Ok(Self {
            _argv: argv,
            argv_pointers,
            _environment: environment,
            environment_pointers,
        })
    }

    fn execute_held_probe(&self) -> std::io::Result<()> {
        let result = unsafe {
            libc::syscall(
                libc::SYS_execveat,
                HELD_PROBE_FD,
                c"".as_ptr(),
                self.argv_pointers.as_ptr().cast::<*const libc::c_char>(),
                self.environment_pointers
                    .as_ptr()
                    .cast::<*const libc::c_char>(),
                libc::AT_EMPTY_PATH,
            )
        };
        let _ = result;
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn configure_probe_execution(
    command: &mut tokio::process::Command,
    launch_mode: ProbeLaunchMode,
    launch_deadline: std::time::Instant,
    executable_fd: std::os::fd::RawFd,
    source_fd: Option<std::os::fd::RawFd>,
    arg0: &Path,
    arguments: &[OsString],
    class: crate::process_control::ChildClass,
) -> Result<ProbeExecutionSupervisor, DecodeFactError> {
    #[cfg(target_os = "linux")]
    if command.as_std().get_program() == OsStr::new("/usr/bin/bwrap") {
        return pi_namespace::configure(
            command,
            launch_mode,
            executable_fd,
            source_fd,
            arg0,
            arguments,
            class,
            launch_deadline,
        );
    }
    command.args(arguments);
    #[cfg(not(target_os = "linux"))]
    let _ = launch_deadline;
    #[cfg(target_os = "linux")]
    let production_arguments = if launch_mode.is_production() {
        Some(
            LinuxExecveArguments::new(arg0.as_os_str(), arguments)
                .map_err(|error| DecodeFactError::Spawn(error.to_string()))?,
        )
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    let production_seccomp = if launch_mode.is_production() {
        Some(
            build_linux_probe_seccomp()
                .map_err(|error| DecodeFactError::Spawn(error.to_string()))?,
        )
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    let production_post_transfer_seccomp = if launch_mode.is_production() {
        Some(
            build_linux_probe_post_transfer_seccomp()
                .map_err(|error| DecodeFactError::Spawn(error.to_string()))?,
        )
    } else {
        None
    };
    #[cfg(not(target_os = "linux"))]
    let _ = (arg0, arguments);
    #[cfg(target_os = "linux")]
    let production_supervisor = if launch_mode.is_production() {
        Some(
            LinuxProbeExecSupervisor::start(launch_mode, launch_deadline)
                .map_err(|error| DecodeFactError::Spawn(error.to_string()))?,
        )
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    let supervisor_sender_fd = production_supervisor
        .as_ref()
        .map(LinuxProbeExecSupervisor::child_sender_fd);
    #[cfg(target_os = "linux")]
    let supervisor_receiver_fd = production_supervisor
        .as_ref()
        .map(LinuxProbeExecSupervisor::child_receiver_fd);
    // Before the closure below: it execs the probe from inside itself, so a
    // `pre_exec` registered after it (the launcher's) never runs.
    crate::process_control::priority::apply(command, class);
    unsafe {
        command.pre_exec(move || {
            start_probe_session()?;
            match source_fd {
                Some(source_fd) => install_probe_child_fds(source_fd, executable_fd)?,
                None => install_child_fd(executable_fd, HELD_PROBE_FD)?,
            }
            if !launch_mode.is_production() {
                return Ok(());
            }
            #[cfg(target_os = "linux")]
            {
                let mut interrupts = launch_mode.bootstrap_interrupts();
                let Some(receiver_fd) = supervisor_receiver_fd else {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                };
                let Some(sender_fd) = supervisor_sender_fd else {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                };
                confirm_linux_probe_fork(sender_fd, launch_deadline, &mut interrupts.ready_send)?;
                libc::close(receiver_fd);
                mark_unrelated_fds_close_on_exec()?;
                #[cfg(test)]
                if let Some(export_socket) = launch_mode.export_socket_fd() {
                    install_child_fd(export_socket, 5)?;
                }
                install_linux_filesystem_execute_denial()?;
                let Some(seccomp) = production_seccomp.as_ref() else {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                };
                let Some(arguments) = production_arguments.as_ref() else {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                };
                let listener = seccomp.install_listener()?;
                use std::os::fd::AsRawFd;
                send_linux_seccomp_listener(sender_fd, listener.as_raw_fd())?;
                let Some(post_transfer_seccomp) = production_post_transfer_seccomp.as_ref() else {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                };
                post_transfer_seccomp.install()?;
                drop(listener);
                arguments.execute_held_probe()
            }
            #[cfg(not(target_os = "linux"))]
            {
                Err(std::io::Error::from_raw_os_error(libc::ENOTSUP))
            }
        });
    }
    #[cfg(target_os = "linux")]
    if let Some(supervisor) = production_supervisor {
        return Ok(ProbeExecutionSupervisor::linux(supervisor));
    }
    Ok(ProbeExecutionSupervisor::none())
}

#[cfg(unix)]
fn start_probe_session() -> std::io::Result<()> {
    if unsafe { libc::setsid() } == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
async fn terminate_probe_session(
    child: &mut tokio::process::Child,
    session: &ProbeSessionGuard,
    supervisor: &mut ProbeExecutionSupervisor,
    _launch_mode: ProbeLaunchMode,
) {
    // This potentially unbounded reap runs only inside an owned task. The
    // caller-facing deadline detaches that task while it retains every source
    // and admission guard needed for safe cleanup.
    session.kill_once();
    let _ = wait_for_probe_reap(child, _launch_mode).await;
    let _ = supervisor.finish();
}

#[cfg(unix)]
async fn wait_for_probe_reap(
    child: &mut tokio::process::Child,
    _launch_mode: ProbeLaunchMode,
) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(all(test, target_os = "linux"))]
    if _launch_mode.inject_held_reap() {
        if let Ok(permit) = STEADY_RESPONSE_REAP_RELEASE.acquire().await {
            permit.forget();
        }
    } else if _launch_mode.inject_slow_reap() {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    child.wait().await
}

#[cfg(unix)]
fn kill_probe_session(process_group: Option<libc::pid_t>) {
    if let Some(process_group) = process_group {
        unsafe {
            libc::kill(-process_group, libc::SIGKILL);
        }
    }
}

#[cfg(target_os = "linux")]
struct ProbeExitAnchor {
    pidfd: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
    #[cfg(test)]
    inject_read_failure: bool,
}

#[cfg(target_os = "linux")]
impl ProbeExitAnchor {
    fn open(
        process_group: Option<libc::pid_t>,
        launch_mode: ProbeLaunchMode,
    ) -> std::io::Result<Self> {
        use std::os::fd::FromRawFd;

        if launch_mode.inject_pidfd_open_failure() {
            return Err(std::io::Error::other("injected FFprobe pidfd open failure"));
        }
        let pid = process_group.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "spawned FFprobe has no process identifier",
            )
        })?;
        let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if raw_fd == -1 {
            return Err(std::io::Error::last_os_error());
        }
        let raw_fd = libc::c_int::try_from(raw_fd).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "FFprobe pidfd exceeded the descriptor range",
            )
        })?;
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw_fd) };
        Ok(Self {
            pidfd: tokio::io::unix::AsyncFd::new(owned)?,
            #[cfg(test)]
            inject_read_failure: launch_mode.inject_pidfd_read_failure(),
        })
    }

    async fn wait_until_exit(&self) -> std::io::Result<()> {
        #[cfg(test)]
        if self.inject_read_failure {
            tokio::time::sleep(Duration::from_millis(50)).await;
            return Err(std::io::Error::other(
                "injected FFprobe pidfd readiness failure",
            ));
        }
        let _readiness = self.pidfd.readable().await?;
        Ok(())
    }
}

#[cfg(unix)]
#[derive(Clone)]
struct ProbeSessionTerminator {
    process_group: Arc<std::sync::Mutex<Option<libc::pid_t>>>,
}

#[cfg(unix)]
impl ProbeSessionTerminator {
    fn take_process_group(&self) -> Option<libc::pid_t> {
        match self.process_group.lock() {
            Ok(mut process_group) => process_group.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }

    fn kill_once(&self) {
        kill_probe_session(self.take_process_group());
    }
}

#[cfg(unix)]
struct ProbeSessionGuard {
    terminator: ProbeSessionTerminator,
}

#[cfg(unix)]
impl ProbeSessionGuard {
    fn new(process_group: Option<libc::pid_t>) -> Self {
        Self {
            terminator: ProbeSessionTerminator {
                process_group: Arc::new(std::sync::Mutex::new(process_group)),
            },
        }
    }

    #[cfg(test)]
    fn terminator(&self) -> ProbeSessionTerminator {
        self.terminator.clone()
    }

    fn kill_once(&self) {
        self.terminator.kill_once();
    }
}

#[cfg(unix)]
impl Drop for ProbeSessionGuard {
    fn drop(&mut self) {
        self.kill_once();
    }
}

#[cfg(unix)]
fn copy_executable(
    source: &std::fs::File,
    snapshot: &mut std::fs::File,
) -> Result<(), DecodeFactError> {
    use std::os::unix::fs::FileExt;

    let mut buffer = [0_u8; 64 * 1024];
    let mut offset = 0_u64;
    loop {
        let read = source
            .read_at(&mut buffer, offset)
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
        if read == 0 {
            break;
        }
        offset = offset
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or_else(|| DecodeFactError::ProbeIdentity("executable size overflow".to_owned()))?;
        if offset > MAX_PROBE_EXECUTABLE_BYTES {
            return Err(DecodeFactError::ProbeIdentity(
                "executable exceeded identity limit".to_owned(),
            ));
        }
        snapshot
            .write_all(&buffer[..read])
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    }
    snapshot
        .flush()
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    Ok(())
}

#[cfg(windows)]
fn copy_executable(
    source: &std::fs::File,
    snapshot: &mut std::fs::File,
) -> Result<(), DecodeFactError> {
    use std::os::windows::fs::FileExt;

    let mut buffer = [0_u8; 64 * 1024];
    let mut offset = 0_u64;
    loop {
        let read = source
            .seek_read(&mut buffer, offset)
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
        if read == 0 {
            break;
        }
        offset = offset
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or_else(|| DecodeFactError::ProbeIdentity("executable size overflow".to_owned()))?;
        if offset > MAX_PROBE_EXECUTABLE_BYTES {
            return Err(DecodeFactError::ProbeIdentity(
                "executable exceeded identity limit".to_owned(),
            ));
        }
        snapshot
            .write_all(&buffer[..read])
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    }
    snapshot
        .sync_all()
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))
}

/// The seals that make the Linux snapshot immutable for the process lifetime.
#[cfg(target_os = "linux")]
const LINUX_SNAPSHOT_SEALS: libc::c_int =
    libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;

#[cfg(target_os = "linux")]
fn snapshot_executable(source: &std::fs::File) -> Result<ExecutableSnapshot, DecodeFactError> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::PermissionsExt;

    let raw_fd = unsafe {
        libc::memfd_create(
            c"plurx-ffprobe".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw_fd == -1 {
        return Err(DecodeFactError::ProbeIdentity(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(raw_fd) };
    copy_executable(source, &mut file)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o500))
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, LINUX_SNAPSHOT_SEALS) } == -1 {
        return Err(DecodeFactError::ProbeIdentity(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    Ok(ExecutableSnapshot { file })
}

#[cfg(all(unix, not(target_os = "linux")))]
fn snapshot_executable(source: &std::fs::File) -> Result<ExecutableSnapshot, DecodeFactError> {
    snapshot_executable_in(source, None)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn snapshot_executable_in(
    source: &std::fs::File,
    directory: Option<&Path>,
) -> Result<ExecutableSnapshot, DecodeFactError> {
    use std::os::unix::fs::PermissionsExt;

    let mut builder = tempfile::Builder::new();
    builder.prefix("plurx-ffprobe-");
    let mut temporary = match directory {
        Some(directory) => builder.tempfile_in(directory),
        None => builder.tempfile(),
    }
    .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    copy_executable(source, temporary.as_file_mut())?;
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o700))
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    // Install the cleanup owner before setting immutability on the exact
    // copied object. Reopening readonly and initializing the parser can both
    // fail; neither may leave an immutable file outside its cleanup owner.
    let (file, path) = temporary.into_parts();
    let mut snapshot = ExecutableSnapshot {
        file,
        _path: path,
        #[cfg(target_os = "macos")]
        parser: None,
        #[cfg(target_os = "macos")]
        parser_image_digest: None,
    };
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;

        if unsafe { libc::fchflags(snapshot.file.as_raw_fd(), libc::UF_IMMUTABLE) } == -1 {
            return Err(DecodeFactError::ProbeIdentity(
                std::io::Error::last_os_error().to_string(),
            ));
        }
    }
    snapshot.file = std::fs::File::open(snapshot.path())
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::FileExt;

        let size = snapshot
            .file
            .metadata()
            .map_err(|e| DecodeFactError::ProbeIdentity(e.to_string()))?
            .len();
        let mut magic = [0; 8];
        if snapshot.file.read_exact_at(&mut magic, 0).is_ok() && magic == *b"\0asm\x01\0\0\0" {
            if size > 32 * 1024 * 1024 {
                return Err(DecodeFactError::ProbeIdentity(
                    "source parser exceeds image bound".into(),
                ));
            }
            let mut bytes = vec![0; size as usize];
            snapshot
                .file
                .read_exact_at(&mut bytes, 0)
                .map_err(|e| DecodeFactError::ProbeIdentity(e.to_string()))?;
            snapshot.parser_image_digest = Some(hex::encode(Sha256::digest(&bytes)));
            snapshot.parser = Some(Arc::new(
                macos_wasm::Parser::new(&bytes).map_err(DecodeFactError::ProbeIdentity)?,
            ));
        }
    }
    Ok(snapshot)
}

#[cfg(windows)]
fn snapshot_executable(source: &std::fs::File) -> Result<ExecutableSnapshot, DecodeFactError> {
    let mut snapshot = tempfile::Builder::new()
        .prefix("plurx-ffprobe-")
        .suffix(".exe")
        .tempfile()
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    copy_executable(source, snapshot.as_file_mut())?;
    let path = snapshot.into_temp_path();
    let file = std::fs::File::open(&path)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    Ok(ExecutableSnapshot { file, _path: path })
}

#[cfg(target_os = "linux")]
fn snapshot_execution_path(_snapshot: &ExecutableSnapshot) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{HELD_PROBE_FD}"))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn snapshot_execution_path(snapshot: &ExecutableSnapshot) -> PathBuf {
    snapshot.path().to_owned()
}

#[cfg(windows)]
fn snapshot_execution_path(snapshot: &ExecutableSnapshot) -> PathBuf {
    snapshot.path().to_owned()
}

/// The `fstat` tuple of an open probe object, with no content digest.
#[cfg(unix)]
fn probe_file_tuple_from_file(file: &std::fs::File) -> Result<ProbeFileIdentity, DecodeFactError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file
        .metadata()
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    Ok(ProbeFileIdentity {
        bytes: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        device: metadata.dev(),
        inode: metadata.ino(),
        content_digest: String::new(),
    })
}

#[cfg(unix)]
fn probe_path_tuple(path: &Path) -> Result<ProbeFileIdentity, DecodeFactError> {
    let file = std::fs::File::open(path)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    probe_file_tuple_from_file(&file)
}

/// The snapshot's tuple, or `None` when its seals are not the ones it was
/// created with (which sends the caller to the content check).
#[cfg(target_os = "linux")]
fn snapshot_tuple_from_file(
    file: &std::fs::File,
) -> Result<Option<ProbeFileIdentity>, DecodeFactError> {
    use std::os::fd::AsRawFd;

    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    if seals == -1 {
        return Err(DecodeFactError::ProbeIdentity(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    if seals & LINUX_SNAPSHOT_SEALS != LINUX_SNAPSHOT_SEALS {
        return Ok(None);
    }
    probe_file_tuple_from_file(file).map(Some)
}

/// Elsewhere the snapshot is an owned copy, checked like the other objects.
#[cfg(not(target_os = "linux"))]
fn snapshot_tuple_from_file(
    file: &std::fs::File,
) -> Result<Option<ProbeFileIdentity>, DecodeFactError> {
    probe_file_tuple_from_file(file).map(Some)
}

#[cfg(unix)]
fn probe_file_identity(path: &Path) -> Result<ProbeFileIdentity, DecodeFactError> {
    let file = std::fs::File::open(path)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    probe_file_identity_from_file(&file)
}

#[cfg(unix)]
fn probe_file_identity_from_file(
    file: &std::fs::File,
) -> Result<ProbeFileIdentity, DecodeFactError> {
    use std::os::unix::fs::FileExt;
    use std::os::unix::fs::MetadataExt;

    let metadata = file
        .metadata()
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_PROBE_EXECUTABLE_BYTES {
        return Err(DecodeFactError::ProbeIdentity(
            "executable size is outside the bounded identity envelope".to_owned(),
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut offset = 0_u64;
    loop {
        let read = file
            .read_at(&mut buffer, offset)
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
        if read == 0 {
            break;
        }
        offset = offset
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or_else(|| DecodeFactError::ProbeIdentity("executable size overflow".to_owned()))?;
        if offset > MAX_PROBE_EXECUTABLE_BYTES {
            return Err(DecodeFactError::ProbeIdentity(
                "executable exceeded identity limit".to_owned(),
            ));
        }
        hasher.update(&buffer[..read]);
    }
    Ok(ProbeFileIdentity {
        bytes: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        device: metadata.dev(),
        inode: metadata.ino(),
        content_digest: hex::encode(hasher.finalize()),
    })
}

async fn run_bounded_identity_task<T, F>(
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
    work: F,
) -> Result<T, DecodeFactError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, DecodeFactError> + Send + 'static,
{
    run_bounded_identity_task_on(identity_gate(), budget, cancelled, work).await
}

async fn run_bounded_identity_task_on<T, F>(
    gate: Arc<tokio::sync::Semaphore>,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
    work: F,
) -> Result<T, DecodeFactError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, DecodeFactError> + Send + 'static,
{
    if budget.is_zero() {
        return Err(DecodeFactError::Deadline);
    }
    let started = std::time::Instant::now();
    let permit = tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => return Err(DecodeFactError::Cancelled),
        permit = tokio::time::timeout(budget, gate.acquire_owned()) => {
            permit
                .map_err(|_| DecodeFactError::Deadline)?
                .map_err(|_| DecodeFactError::CacheInvariant)?
        }
    };
    let remaining = budget.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(DecodeFactError::Deadline);
    }
    // The owned permit lives in the worker. If this waiter is cancelled or
    // times out, the blocking hash finishes in the sole lane before another
    // hash can begin; detached work therefore cannot fan out.
    let mut task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    });
    tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => Err(DecodeFactError::Cancelled),
        result = tokio::time::timeout(remaining, &mut task) => {
            result
                .map_err(|_| DecodeFactError::Deadline)?
                .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?
        }
    }
}

async fn path_probe_file_identity_within(
    path: PathBuf,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<ProbeFileIdentity, DecodeFactError> {
    run_bounded_identity_task(budget, cancelled, move || probe_file_identity(&path)).await
}

async fn held_probe_file_identity_within(
    file: Arc<std::fs::File>,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<ProbeFileIdentity, DecodeFactError> {
    run_bounded_identity_task(budget, cancelled, move || {
        probe_file_identity_from_file(&file)
    })
    .await
}

#[cfg(windows)]
fn probe_file_tuple_from_file(file: &std::fs::File) -> Result<ProbeFileIdentity, DecodeFactError> {
    let metadata = file
        .metadata()
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    let held = plurx_core::fs_secure::std_file_identity(file)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    let (modified_seconds, modified_nanoseconds) = system_time_parts(
        metadata
            .modified()
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?,
    );
    Ok(ProbeFileIdentity {
        bytes: held.size,
        modified_seconds,
        modified_nanoseconds,
        changed_seconds: held.changed_seconds,
        changed_nanoseconds: held.changed_nanoseconds,
        device: held.device,
        inode: held.inode ^ held.inode_high.rotate_left(1),
        content_digest: String::new(),
    })
}

#[cfg(windows)]
fn probe_path_tuple(path: &Path) -> Result<ProbeFileIdentity, DecodeFactError> {
    let file = plurx_core::fs_secure::open_read_nofollow_blocking(path)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    probe_file_tuple_from_file(&file)
}

#[cfg(windows)]
fn probe_file_identity(path: &Path) -> Result<ProbeFileIdentity, DecodeFactError> {
    let file = plurx_core::fs_secure::open_read_nofollow_blocking(path)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    probe_file_identity_from_file(&file)
}

#[cfg(windows)]
fn probe_file_identity_from_file(
    file: &std::fs::File,
) -> Result<ProbeFileIdentity, DecodeFactError> {
    use std::os::windows::fs::FileExt;

    let metadata = file
        .metadata()
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    let held = plurx_core::fs_secure::std_file_identity(file)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    if !metadata.is_file() || held.size == 0 || held.size > MAX_PROBE_EXECUTABLE_BYTES {
        return Err(DecodeFactError::ProbeIdentity(
            "executable size is outside the bounded identity envelope".to_owned(),
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut offset = 0_u64;
    while offset < held.size {
        let read = file
            .seek_read(&mut buffer, offset)
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        offset = offset
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or_else(|| DecodeFactError::ProbeIdentity("executable size overflow".to_owned()))?;
    }
    let after = plurx_core::fs_secure::std_file_identity(file)
        .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?;
    if offset != held.size || after != held {
        return Err(DecodeFactError::ProbeChanged);
    }
    let (modified_seconds, modified_nanoseconds) = system_time_parts(
        metadata
            .modified()
            .map_err(|error| DecodeFactError::ProbeIdentity(error.to_string()))?,
    );
    Ok(ProbeFileIdentity {
        bytes: held.size,
        modified_seconds,
        modified_nanoseconds,
        changed_seconds: held.changed_seconds,
        changed_nanoseconds: held.changed_nanoseconds,
        device: held.device,
        inode: held.inode ^ held.inode_high.rotate_left(1),
        content_digest: hex::encode(hasher.finalize()),
    })
}

#[cfg(windows)]
fn system_time_parts(value: std::time::SystemTime) -> (i64, i64) {
    match value.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => (
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
            i64::from(duration.subsec_nanos()),
        ),
        Err(error) => {
            let duration = error.duration();
            (
                -i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
                -i64::from(duration.subsec_nanos()),
            )
        }
    }
}

#[cfg(unix)]
async fn probe_version(
    executable: &Arc<ExecutableSnapshot>,
    configured_path: &Path,
    launch_mode: ProbeLaunchMode,
) -> Result<Vec<u8>, DecodeFactError> {
    #[cfg(not(test))]
    let _ = configured_path;
    probe_version_with_deadline(
        executable,
        configured_path,
        launch_mode,
        launch_mode.version_deadline(),
    )
    .await
}

#[cfg(unix)]
async fn probe_version_with_deadline(
    executable: &Arc<ExecutableSnapshot>,
    configured_path: &Path,
    launch_mode: ProbeLaunchMode,
    deadline: Duration,
) -> Result<Vec<u8>, DecodeFactError> {
    probe_version_with_deadline_on(
        executable,
        configured_path,
        launch_mode,
        deadline,
        version_gate(),
    )
    .await
}

#[cfg(unix)]
async fn probe_version_with_deadline_on(
    executable: &Arc<ExecutableSnapshot>,
    configured_path: &Path,
    launch_mode: ProbeLaunchMode,
    deadline: Duration,
    gate: Arc<tokio::sync::Semaphore>,
) -> Result<Vec<u8>, DecodeFactError> {
    let started = std::time::Instant::now();
    let launch_deadline = started + deadline;
    let version_permit = tokio::time::timeout(deadline, gate.acquire_owned())
        .await
        .map_err(|_| DecodeFactError::Deadline)?
        .map_err(|_| DecodeFactError::CacheInvariant)?;
    let remaining = deadline.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(DecodeFactError::Deadline);
    }
    let executable = Arc::clone(executable);
    let configured_path = configured_path.to_owned();
    let mut task = tokio::spawn(async move {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;

        let _version_permit = version_permit;
        #[cfg(target_os = "macos")]
        if launch_mode.is_production() {
            let output = run_macos_parser(
                &executable,
                None,
                vec!["-version".into()],
                MAX_VERSION_BYTES,
                launch_deadline,
                None,
            )
            .await?;
            if output.status != 0 || output.stdout.is_empty() {
                return Err(DecodeFactError::ProbeIdentity(
                    "bounded parser version check failed".into(),
                ));
            }
            return Ok(output.stdout);
        }
        let executable_fd = executable.as_file().as_raw_fd();
        let arguments = vec![OsString::from("-version")];
        let mut command =
            tokio::process::Command::new(probe_launch_path(&executable, launch_mode)?);
        command.as_std_mut().arg0(&configured_path);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let supervisor = configure_probe_execution(
            &mut command,
            launch_mode,
            launch_deadline,
            executable_fd,
            None,
            &configured_path,
            &arguments,
            DECODE_FACT_VERSION_PROBE.class,
        )?;
        let (mut child, _child_job, mut supervisor) =
            spawn_configured_probe(command, supervisor, DECODE_FACT_VERSION_PROBE).await?;
        let process_group = child.id().and_then(|id| libc::pid_t::try_from(id).ok());
        let session = ProbeSessionGuard::new(process_group);
        #[cfg(target_os = "linux")]
        let exit_anchor = match ProbeExitAnchor::open(process_group, launch_mode) {
            Ok(anchor) => anchor,
            Err(error) => {
                terminate_probe_session(&mut child, &session, &mut supervisor, launch_mode).await;
                return Err(DecodeFactError::Spawn(format!(
                    "opening FFprobe exit anchor: {error}"
                )));
            }
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                terminate_probe_session(&mut child, &session, &mut supervisor, launch_mode).await;
                return Err(DecodeFactError::MissingPipe);
            }
        };
        let remaining = launch_deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            terminate_probe_session(&mut child, &session, &mut supervisor, launch_mode).await;
            return Err(DecodeFactError::Deadline);
        }
        let outcome = tokio::time::timeout(remaining, async {
            #[cfg(target_os = "linux")]
            let (stdout, exited) = tokio::join!(read_bounded(stdout, MAX_VERSION_BYTES), async {
                let exited = exit_anchor.wait_until_exit().await;
                // Kill descendants as soon as the leader exits so inherited
                // pipe writers cannot hold the drain open.
                session.kill_once();
                exited
            });
            #[cfg(target_os = "linux")]
            let (status, exit_error) = {
                // pidfd readiness proves exit without reaping. The zombie anchors
                // the PID/PGID until descendants are killed and only then is the
                // leader reaped.
                let exit_error = exited
                    .err()
                    .map(|error| DecodeFactError::Read(error.to_string()));
                (
                    wait_for_probe_reap(&mut child, launch_mode).await,
                    exit_error,
                )
            };
            #[cfg(not(target_os = "linux"))]
            let (stdout, status) =
                tokio::join!(read_bounded(stdout, MAX_VERSION_BYTES), child.wait());
            #[cfg(not(target_os = "linux"))]
            session.kill_once();
            #[cfg(not(target_os = "linux"))]
            let exit_error: Option<DecodeFactError> = None;
            (stdout, status, exit_error)
        })
        .await;
        let (stdout, status, exit_error) = match outcome {
            Ok(outcome) => outcome,
            Err(_) => {
                terminate_probe_session(&mut child, &session, &mut supervisor, launch_mode).await;
                return Err(DecodeFactError::Deadline);
            }
        };
        let supervisor_result = supervisor.finish();
        if let Some(error) = exit_error {
            return Err(error);
        }
        supervisor_result?;
        let stdout = stdout?;
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                return Err(DecodeFactError::Read(error.to_string()));
            }
        };
        if stdout.1 || stdout.0.is_empty() || !status.success() {
            return Err(DecodeFactError::ProbeIdentity(
                "bounded version probe failed".to_owned(),
            ));
        }
        Ok(stdout.0)
    });
    tokio::select! {
        biased;
        result = &mut task => {
            result.map_err(|error| DecodeFactError::Read(error.to_string()))?
        }
        _ = tokio::time::sleep(remaining) => {
            Err(DecodeFactError::Deadline)
        }
    }
}

#[cfg(windows)]
async fn probe_version(
    executable: &Arc<ExecutableSnapshot>,
    configured_path: &Path,
    _launch_mode: ProbeLaunchMode,
) -> Result<Vec<u8>, DecodeFactError> {
    #[cfg(not(test))]
    let _ = configured_path;
    let permit = tokio::time::timeout(VERSION_DEADLINE, version_gate().acquire_owned())
        .await
        .map_err(|_| DecodeFactError::Deadline)?
        .map_err(|_| DecodeFactError::CacheInvariant)?;
    let before = probe_file_identity_from_file(executable.as_file())?;
    let mut command = tokio::process::Command::new(snapshot_execution_path(executable));
    #[cfg(test)]
    command.env("PLURX_TEST_PROBE_PATH", configured_path);
    command
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let (mut child, _child_job) =
        crate::process_control::spawn_job_owned(&mut command, DECODE_FACT_VERSION_PROBE)
            .map_err(|error| DecodeFactError::Spawn(error.to_string()))?;
    let stdout = child.stdout.take().ok_or(DecodeFactError::MissingPipe)?;
    let outcome = tokio::time::timeout(VERSION_DEADLINE, async {
        tokio::join!(read_bounded(stdout, MAX_VERSION_BYTES), child.wait())
    })
    .await;
    let (stdout, status) = match outcome {
        Ok(outcome) => outcome,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(DecodeFactError::Deadline);
        }
    };
    drop(permit);
    let stdout = stdout?;
    let status = status.map_err(|error| DecodeFactError::Read(error.to_string()))?;
    let after = probe_file_identity_from_file(executable.as_file())?;
    if before != after {
        return Err(DecodeFactError::ProbeChanged);
    }
    if stdout.1 || stdout.0.is_empty() || !status.success() {
        return Err(DecodeFactError::ProbeIdentity(
            "bounded version probe failed".to_owned(),
        ));
    }
    Ok(stdout.0)
}

/// One freshly collected document and the typed facts derived from the same
/// executable and source. The document is transient; only facts enter cache.
pub(crate) struct FreshSourceProbe {
    pub(crate) document: String,
    facts: Result<DecodeFacts, DecodeFactError>,
    key: CacheKey,
    #[cfg(target_os = "linux")]
    selected_absolute_stream: Option<u32>,
}

#[cfg(target_os = "linux")]
impl FreshSourceProbe {
    pub(crate) fn absolute_video_stream(&self) -> Option<u32> {
        self.selected_absolute_stream
    }
}

#[derive(Clone, Debug)]
struct FreshFactSeed {
    facts: Result<DecodeFacts, DecodeFactError>,
    key: CacheKey,
}

struct CollectedProbe {
    facts: Result<DecodeFacts, DecodeFactError>,
    document: Option<String>,
    key: Option<CacheKey>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeProjection {
    Decoder,
    SourceDocument,
    #[cfg(any(target_os = "linux", test))]
    SourceFrames(u32),
}

impl ProbeProjection {
    fn arguments(self) -> Vec<String> {
        let mut args = vec!["-v", "error", "-print_format", "json"];
        match self {
            Self::Decoder => args.extend(["-show_entries",
                "stream=index,codec_type,codec_name,profile,pix_fmt,width,height,sample_aspect_ratio,bits_per_raw_sample,avg_frame_rate,r_frame_rate,field_order,color_range,color_space,color_transfer,color_primaries:stream_disposition=attached_pic:stream_tags=rotate:stream_side_data=side_data_type,rotation,displaymatrix,max_content,max_average,max_luminance",
                "-show_streams"]),
            Self::SourceDocument => args.extend(["-show_format", "-show_streams", "-show_chapters"]),
            #[cfg(any(target_os = "linux", test))]
            Self::SourceFrames(_) => args.extend([
                "-threads", "1", "-read_intervals", "%+#3", "-show_frames", "-show_entries",
                "frame=stream_index:frame_side_data=side_data_type,red_x,red_y,green_x,green_y,blue_x,blue_y,white_point_x,white_point_y,min_luminance,max_luminance",
            ]),
        }
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        #[cfg(any(target_os = "linux", test))]
        if let Self::SourceFrames(index) = self {
            let mut args = args;
            args.extend(["-select_streams".to_owned(), index.to_string()]);
            return args;
        }
        args
    }

    fn output_limit(self) -> usize {
        match self {
            Self::Decoder => MAX_PROBE_STDOUT_BYTES,
            // Same bound as the held-source comparison this replaces.
            Self::SourceDocument => 16 * 1024 * 1024,
            #[cfg(any(target_os = "linux", test))]
            Self::SourceFrames(_) => 256 * 1024,
        }
    }

    fn finish(
        self,
        facts: Result<DecodeFacts, DecodeFactError>,
        stdout: &[u8],
        reporter: &str,
    ) -> Result<CollectedProbe, DecodeFactError> {
        let document = if self != Self::Decoder {
            let mut document: serde_json::Value = serde_json::from_slice(stdout)
                .map_err(|error| DecodeFactError::InvalidJson(error.to_string()))?;
            if self == Self::SourceDocument {
                plurx_core::scan::probe::stamp_reporter(&mut document, reporter);
            }
            Some(
                serde_json::to_string(&document)
                    .map_err(|error| DecodeFactError::InvalidJson(error.to_string()))?,
            )
        } else {
            None
        };
        Ok(CollectedProbe {
            facts,
            document,
            key: None,
        })
    }
}

/// A lazy frame-side-data observation of one already verified source/stream.
/// Its private cache is distinct from decoder/raw-document caches: the key is
/// the complete source/probe/catalog identity, selected absolute stream, and
/// this projection revision. No frame JSON is grafted into the raw document.
#[cfg(target_os = "linux")]
pub(crate) struct SourceFrameProbe {
    cache: Arc<DecodeFactCache>,
    probe: DecodeProbeIdentity,
    source: DecodeFactSource,
    catalog: Option<DecodeCatalogMetadata>,
    expected: CacheKey,
    absolute_stream: u32,
    projection_revision: u32,
    document: tokio::sync::OnceCell<serde_json::Value>,
}

#[cfg(target_os = "linux")]
impl SourceFrameProbe {
    pub(crate) fn absolute_stream(&self) -> u32 {
        self.absolute_stream
    }

    pub(crate) async fn document(
        &self,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<&serde_json::Value, DecodeFactError> {
        self.document
            .get_or_try_init(|| async {
                if self.projection_revision != 1 {
                    return Err(DecodeFactError::CacheInvariant);
                }
                let mut source = self.source.clone();
                source.projection = ProbeProjection::SourceFrames(self.absolute_stream);
                source.expected_frame_key = Some(self.expected.clone());
                let (collected, _) = self
                    .cache
                    .get_or_probe_inner(
                        &self.probe,
                        source,
                        self.catalog.as_ref(),
                        self.expected.selected_stream,
                        Duration::from_secs(5),
                        cancelled,
                    )
                    .await?;
                if collected.key.as_ref() != Some(&self.expected) {
                    return Err(DecodeFactError::SourceChanged);
                }
                serde_json::from_str(&collected.document.ok_or(DecodeFactError::CacheInvariant)?)
                    .map_err(|error| DecodeFactError::InvalidJson(error.to_string()))
            })
            .await
    }
}

/// Bounded FIFO cache. The preparation path supplies the exact FFprobe build
/// digest, so replacing the binary cannot reuse facts from an older parser.
pub(crate) struct DecodeFactCache {
    entries: tokio::sync::Mutex<BTreeMap<CacheKey, DecodeFacts>>,
    order: tokio::sync::Mutex<VecDeque<CacheKey>>,
    /// One bounded global lane serializes executable revalidation and cache
    /// misses. Fact preparation cannot fan out unbounded hashing or child
    /// processes when several sessions start together.
    probe_gate: Arc<tokio::sync::Semaphore>,
    metrics: Arc<DecodeFactMetrics>,
    capacity: usize,
}

impl DecodeFactCache {
    #[cfg(target_os = "linux")]
    pub(crate) fn source_frame_probe(
        self: &Arc<Self>,
        probe: &DecodeProbeIdentity,
        source: DecodeFactSource,
        catalog: Option<DecodeCatalogMetadata>,
        prepared: &FreshSourceProbe,
        absolute_stream: u32,
    ) -> Result<Arc<SourceFrameProbe>, DecodeFactError> {
        if prepared.selected_absolute_stream != Some(absolute_stream) {
            return Err(DecodeFactError::InvalidFacts(
                "source frame projection selected stream differs from held probe".into(),
            ));
        }
        Ok(Arc::new(SourceFrameProbe {
            cache: Arc::clone(self),
            probe: probe.clone(),
            source,
            catalog,
            expected: prepared.key.clone(),
            absolute_stream,
            projection_revision: 1,
            document: tokio::sync::OnceCell::new(),
        }))
    }

    pub(crate) fn new() -> Self {
        Self::with_capacity(MAX_CACHE_ENTRIES)
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: tokio::sync::Mutex::new(BTreeMap::new()),
            order: tokio::sync::Mutex::new(VecDeque::new()),
            probe_gate: Arc::new(tokio::sync::Semaphore::new(1)),
            metrics: Arc::new(DecodeFactMetrics::default()),
            capacity: capacity.clamp(1, MAX_CACHE_ENTRIES),
        }
    }

    pub(crate) fn metrics(&self) -> &DecodeFactMetrics {
        &self.metrics
    }

    pub(crate) fn metrics_handle(&self) -> Arc<DecodeFactMetrics> {
        Arc::clone(&self.metrics)
    }

    pub(crate) async fn get_or_probe(
        &self,
        probe: &DecodeProbeIdentity,
        source: DecodeFactSource,
        catalog: Option<&DecodeCatalogMetadata>,
        selected_stream: ProbeStreamSelection,
        budget: Duration,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<DecodeFacts, DecodeFactError> {
        let result = self
            .get_or_probe_inner(probe, source, catalog, selected_stream, budget, cancelled)
            .await;
        self.metrics.record_lookup(match &result {
            Ok((_, result)) => *result,
            Err(_) => DecodeFactLookupResult::Refused,
        });
        result.and_then(|(collected, _)| collected.facts)
    }

    /// Fresh verification cannot be satisfied by a cache hit: comparison of
    /// the scan and the held object is a current observation, even when the
    /// decode facts for that object are already cached.
    pub(crate) async fn probe_source_document(
        &self,
        probe: &DecodeProbeIdentity,
        mut source: DecodeFactSource,
        catalog: Option<&DecodeCatalogMetadata>,
        selected_stream: ProbeStreamSelection,
        budget: Duration,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<FreshSourceProbe, DecodeFactError> {
        source.projection = ProbeProjection::SourceDocument;
        let result = self
            .get_or_probe_inner(probe, source, catalog, selected_stream, budget, cancelled)
            .await;
        self.metrics.record_lookup(match &result {
            Ok((_, outcome)) => *outcome,
            Err(_) => DecodeFactLookupResult::Refused,
        });
        let (collected, _) = result?;
        let document = collected.document.ok_or(DecodeFactError::CacheInvariant)?;
        #[cfg(target_os = "linux")]
        let value: serde_json::Value = serde_json::from_str(&document)
            .map_err(|error| DecodeFactError::InvalidJson(error.to_string()))?;
        #[cfg(target_os = "linux")]
        let selected_absolute_stream = match selected_stream {
            ProbeStreamSelection::FirstPlayable => first_playable_video_index(&value),
            ProbeStreamSelection::LegacyVideoOrdinal(ordinal) => {
                absolute_video_ordinal(&value, ordinal)
            }
            ProbeStreamSelection::Absolute(index) => value
                .get("streams")
                .and_then(serde_json::Value::as_array)
                .and_then(|streams| {
                    streams.iter().find(|stream| {
                        stream.get("index").and_then(serde_json::Value::as_u64)
                            == Some(u64::from(index))
                            && stream.get("codec_type").and_then(serde_json::Value::as_str)
                                == Some("video")
                    })
                })
                .map(|_| index),
        };
        Ok(FreshSourceProbe {
            facts: collected.facts,
            document,
            key: collected.key.ok_or(DecodeFactError::CacheInvariant)?,
            #[cfg(target_os = "linux")]
            selected_absolute_stream,
        })
    }

    /// Complete optional decoder refinement using a source document already
    /// validated by this owner. Its complete cache identity must still match;
    /// a changed object or executable cannot turn into a catalog fallback.
    pub(crate) async fn refine_source_document(
        &self,
        probe: &DecodeProbeIdentity,
        mut source: DecodeFactSource,
        catalog: Option<&DecodeCatalogMetadata>,
        prepared: FreshSourceProbe,
        budget: Duration,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<DecodeFacts, DecodeFactError> {
        let selected_stream = prepared.key.selected_stream;
        source.seed = Some(FreshFactSeed {
            facts: prepared.facts,
            key: prepared.key,
        });
        self.get_or_probe(probe, source, catalog, selected_stream, budget, cancelled)
            .await
    }

    async fn get_or_probe_inner(
        &self,
        probe: &DecodeProbeIdentity,
        source: DecodeFactSource,
        catalog: Option<&DecodeCatalogMetadata>,
        selected_stream: ProbeStreamSelection,
        budget: Duration,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<(CollectedProbe, DecodeFactLookupResult), DecodeFactError> {
        let started = std::time::Instant::now();
        let remaining = budget
            .min(probe.launch_mode.probe_deadline())
            .saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(DecodeFactError::Deadline);
        }
        let phase_started = std::time::Instant::now();
        let gate_result = tokio::select! {
            biased;
            _ = wait_for_cancellation(cancelled) => Err(DecodeFactError::Cancelled),
            gate = tokio::time::timeout(remaining, Arc::clone(&self.probe_gate).acquire_owned()) => {
                match gate {
                    Ok(Ok(gate)) => Ok(gate),
                    Ok(Err(_)) => Err(DecodeFactError::CacheInvariant),
                    Err(_) => Err(DecodeFactError::Deadline),
                }
            }
        };
        let gate_elapsed = phase_started.elapsed();
        self.metrics.record_phase(
            DecodeFactPhase::GateWait,
            &gate_result.as_ref().map(|_| ()).map_err(Clone::clone),
            gate_elapsed,
        );
        let gate = gate_result?;
        let remaining = budget
            .min(probe.launch_mode.probe_deadline())
            .saturating_sub(started.elapsed());
        let phase_started = std::time::Instant::now();
        let validation = probe.validate_current(remaining, cancelled).await;
        let identity_elapsed = phase_started.elapsed();
        self.metrics.record_phase(
            DecodeFactPhase::IdentityValidation,
            &validation,
            identity_elapsed,
        );
        validation?;
        let remaining = budget
            .min(probe.launch_mode.probe_deadline())
            .saturating_sub(started.elapsed());
        let phase_started = std::time::Instant::now();
        let observation = source_observation_with_probe_gate(
            Arc::clone(&source.handle),
            gate,
            remaining,
            cancelled,
            source.initial_identity_delay(),
        )
        .await;
        let observation_elapsed = phase_started.elapsed();
        self.metrics.record_phase(
            DecodeFactPhase::SourceObservation,
            &observation.as_ref().map(|_| ()).map_err(Clone::clone),
            observation_elapsed,
        );
        let (bound_source, gate) = observation?;
        let bound_identity = bound_source.identity.clone();
        let key = CacheKey {
            probe_schema_version: PROBE_SCHEMA_VERSION,
            source: bound_identity.clone(),
            ffprobe_build_digest: probe.build_digest().to_owned(),
            catalog_digest: catalog.map(|metadata| metadata.digest().to_owned()),
            selected_stream,
        };
        if let Some(expected) = &source.expected_frame_key {
            if expected.source != key.source {
                return Err(DecodeFactError::SourceChanged);
            }
            if expected.ffprobe_build_digest != key.ffprobe_build_digest {
                return Err(DecodeFactError::ProbeChanged);
            }
            if expected != &key {
                return Err(DecodeFactError::CacheInvariant);
            }
        }
        if let Some(seed) = &source.seed {
            if seed.key.source != key.source {
                return Err(DecodeFactError::SourceChanged);
            }
            if seed.key.ffprobe_build_digest != key.ffprobe_build_digest {
                return Err(DecodeFactError::ProbeChanged);
            }
            if seed.key != key {
                return Err(DecodeFactError::CacheInvariant);
            }
            if let Err(error) = &seed.facts {
                return Err(error.clone());
            }
        }
        let cached = if source.projection == ProbeProjection::Decoder {
            self.entries.lock().await.get(&key).cloned()
        } else {
            None
        };
        if let Some(facts) = cached {
            self.metrics
                .record_hit_phase(DecodeFactPhase::GateWait, gate_elapsed);
            self.metrics
                .record_hit_phase(DecodeFactPhase::IdentityValidation, identity_elapsed);
            self.metrics
                .record_hit_phase(DecodeFactPhase::SourceObservation, observation_elapsed);
            return Ok((
                CollectedProbe {
                    facts: Ok(facts),
                    document: None,
                    key: Some(key.clone()),
                },
                DecodeFactLookupResult::Hit,
            ));
        }
        let remaining = budget
            .min(probe.launch_mode.probe_deadline())
            .saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(DecodeFactError::Deadline);
        }
        let source_offset_permit = tokio::select! {
            biased;
            _ = wait_for_cancellation(cancelled) => return Err(DecodeFactError::Cancelled),
            permit = tokio::time::timeout(
                remaining,
                Arc::clone(&source.offset_gate).acquire_owned(),
            ) => {
                permit
                    .map_err(|_| DecodeFactError::Deadline)?
                    .map_err(|_| DecodeFactError::CacheInvariant)?
            }
        };
        let remaining = budget
            .min(probe.launch_mode.probe_deadline())
            .saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(DecodeFactError::Deadline);
        }
        let owned_probe = probe.clone();
        let owned_catalog = catalog.cloned();
        let owned_cancelled = cancelled.cloned();
        let owned_metrics = Arc::clone(&self.metrics);
        // The task owns both the source descriptor and the singleflight
        // permit. Dropping or aborting this waiter cannot abandon a live child
        // or restore the shared source offset before kill/reap completes.
        let collection = tokio::spawn(async move {
            let phase_started = std::time::Instant::now();
            let result = collect(
                &owned_probe,
                DecodeFactCollectionSource {
                    handle: Arc::clone(&source.handle),
                    observation: bound_source,
                    offset_permit: source_offset_permit,
                    work: source.work,
                    projection: source.projection,
                    seed: source.seed.clone(),
                },
                owned_catalog.as_ref(),
                selected_stream,
                remaining,
                owned_cancelled.as_ref(),
            )
            .await;
            owned_metrics.record_phase(
                DecodeFactPhase::Collection,
                &result.as_ref().map(|_| ()).map_err(Clone::clone),
                phase_started.elapsed(),
            );
            (result, gate, source)
        });
        let collection_result = await_owned_collection(collection, remaining, cancelled).await;
        let (collected, gate, source) = collection_result?;
        let mut collected = collected?;
        let remaining = budget
            .min(probe.launch_mode.probe_deadline())
            .saturating_sub(started.elapsed());
        let phase_started = std::time::Instant::now();
        let validation = probe.validate_current(remaining, cancelled).await;
        self.metrics.record_phase(
            DecodeFactPhase::FinalValidation,
            &validation,
            phase_started.elapsed(),
        );
        validation?;
        let remaining = budget
            .min(probe.launch_mode.probe_deadline())
            .saturating_sub(started.elapsed());
        let phase_started = std::time::Instant::now();
        let observation = source_observation_with_probe_gate(
            Arc::clone(&source.handle),
            gate,
            remaining,
            cancelled,
            source.final_identity_delay(),
        )
        .await;
        let (after_source, gate) = match observation {
            Ok(observation) => observation,
            Err(error) => {
                self.metrics.record_phase(
                    DecodeFactPhase::SourceObservation,
                    &Err(error.clone()),
                    phase_started.elapsed(),
                );
                return Err(error);
            }
        };
        let source_outcome = if after_source.identity == key.source {
            Ok(())
        } else {
            Err(DecodeFactError::SourceChanged)
        };
        self.metrics.record_phase(
            DecodeFactPhase::SourceObservation,
            &source_outcome,
            phase_started.elapsed(),
        );
        source_outcome?;
        collected.key = Some(key.clone());
        #[cfg(any(target_os = "linux", test))]
        if matches!(source.projection, ProbeProjection::SourceFrames(_)) {
            // The lazy sidecar owns its projection-specific cache. Even a
            // surprising extra stream list must never replace decoder facts.
            return Ok((collected, DecodeFactLookupResult::MissCollected));
        }
        let mut entries = self.entries.lock().await;
        let Ok(facts) = &collected.facts else {
            // Source verification succeeded; preserve the decoder's typed
            // parse refusal for its existing, separately bounded fallback.
            return Ok((collected, DecodeFactLookupResult::MissCollected));
        };
        // A fresh catalogue document carries no binary scan proof. Do not
        // replace the refined decoder cache with its still-unknown HEVC facts.
        if source.projection == ProbeProjection::SourceDocument && needs_hevc_scan_refinement(facts)
        {
            return Ok((collected, DecodeFactLookupResult::MissCollected));
        }
        let pending_refinement = source.projection == ProbeProjection::SourceDocument
            && matches!(facts.scan_type(), ScanType::Interlaced(_));
        if pending_refinement {
            if let Some(existing) = entries.get(&key) {
                collected.facts = Ok(facts
                    .clone()
                    .with_interlace_verdict(existing.interlace_verdict()));
            }
            return Ok((collected, DecodeFactLookupResult::MissCollected));
        }
        if let Some(existing) = entries.get_mut(&key) {
            *existing = facts.clone();
            return Ok((collected, DecodeFactLookupResult::MissCollected));
        }
        let mut order = self.order.lock().await;
        while entries.len() >= self.capacity {
            let Some(oldest) = order.pop_front() else {
                return Err(DecodeFactError::CacheInvariant);
            };
            entries.remove(&oldest);
        }
        entries.insert(key.clone(), facts.clone());
        order.push_back(key);
        drop(gate);
        Ok((collected, DecodeFactLookupResult::MissCollected))
    }
}

/// Wait for caller-visible completion without abandoning cleanup ownership.
/// Dropping a timed-out join handle detaches the task; the task continues to
/// own the child, source descriptor, offset restoration, and both permits.
async fn await_owned_collection<T>(
    mut collection: tokio::task::JoinHandle<T>,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<T, DecodeFactError>
where
    T: Send + 'static,
{
    if budget.is_zero() {
        return Err(DecodeFactError::Deadline);
    }
    tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => Err(DecodeFactError::Cancelled),
        _ = tokio::time::sleep(budget) => Err(DecodeFactError::Deadline),
        result = &mut collection => {
            result.map_err(|error| DecodeFactError::Read(error.to_string()))
        }
    }
}

async fn wait_for_cancellation(cancelled: Option<&tokio_util::sync::CancellationToken>) {
    match cancelled {
        Some(cancelled) => cancelled.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DecodeFactError {
    ProbeIdentity(String),
    ProbeChanged,
    SourceMetadata(String),
    SourceChanged,
    Spawn(String),
    MissingPipe,
    Deadline,
    Cancelled,
    Read(String),
    OversizedOutput,
    Failed(Option<i32>, String),
    InvalidJson(String),
    InvalidFacts(String),
    CacheInvariant,
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    UnsupportedPlatform,
}

impl DecodeFactError {
    pub(crate) const fn fallback_reason(&self) -> DecodePlanFallbackReason {
        match self {
            Self::Deadline => DecodePlanFallbackReason::Deadline,
            Self::Cancelled => DecodePlanFallbackReason::Cancelled,
            Self::ProbeChanged => DecodePlanFallbackReason::ProbeChanged,
            Self::SourceChanged => DecodePlanFallbackReason::RefusedSourceChanged,
            Self::Spawn(_)
            | Self::MissingPipe
            | Self::Read(_)
            | Self::OversizedOutput
            | Self::Failed(_, _)
            | Self::InvalidJson(_)
            | Self::InvalidFacts(_) => DecodePlanFallbackReason::ProbeFailed,
            Self::ProbeIdentity(_) | Self::SourceMetadata(_) => {
                DecodePlanFallbackReason::IdentityIo
            }
            Self::CacheInvariant => DecodePlanFallbackReason::Invariant,
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            Self::UnsupportedPlatform => DecodePlanFallbackReason::Invariant,
        }
    }
}

impl std::fmt::Display for DecodeFactError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProbeIdentity(error) => write!(formatter, "identifying FFprobe: {error}"),
            Self::ProbeChanged => formatter.write_str("FFprobe changed after startup identity"),
            Self::SourceMetadata(error) => {
                write!(formatter, "reading bound source metadata: {error}")
            }
            Self::SourceChanged => {
                formatter.write_str("bound source changed during decoder probing")
            }
            Self::Spawn(error) => write!(formatter, "starting bound FFprobe: {error}"),
            Self::MissingPipe => formatter.write_str("bound FFprobe pipe was unavailable"),
            Self::Deadline => formatter.write_str("bound FFprobe exceeded its deadline"),
            Self::Cancelled => formatter.write_str("bound FFprobe was cancelled"),
            Self::Read(error) => write!(formatter, "reading bound FFprobe output: {error}"),
            Self::OversizedOutput => formatter.write_str("bound FFprobe output exceeded its limit"),
            Self::Failed(code, reason) => {
                write!(formatter, "bound FFprobe failed ({code:?}): {reason}")
            }
            Self::InvalidJson(error) => write!(formatter, "parsing bound FFprobe JSON: {error}"),
            Self::InvalidFacts(error) => {
                write!(formatter, "validating bound decode facts: {error}")
            }
            Self::CacheInvariant => {
                formatter.write_str("decoder fact cache ordering is inconsistent")
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            Self::UnsupportedPlatform => formatter
                .write_str("descriptor-bound decoder probing is unsupported on this platform"),
        }
    }
}

impl std::error::Error for DecodeFactError {}

#[derive(Debug)]
struct DecodeSourceObservation {
    identity: DecodeSourceIdentity,
    #[cfg(unix)]
    executable: bool,
}

#[derive(Debug)]
struct DecodeFactCollectionSource {
    handle: Arc<std::fs::File>,
    observation: DecodeSourceObservation,
    offset_permit: tokio::sync::OwnedSemaphorePermit,
    work: crate::process_control::ChildWork,
    projection: ProbeProjection,
    seed: Option<FreshFactSeed>,
}

#[cfg(unix)]
fn source_observation(source: &std::fs::File) -> Result<DecodeSourceObservation, DecodeFactError> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    let metadata = source
        .metadata()
        .map_err(|error| DecodeFactError::SourceMetadata(error.to_string()))?;
    if !metadata.is_file() {
        return Err(DecodeFactError::SourceMetadata(
            "source handle is not a regular file".to_owned(),
        ));
    }
    let body = serde_json::json!({
        "bytes": metadata.len(),
        "modified_seconds": metadata.mtime(),
        "modified_nanoseconds": metadata.mtime_nsec(),
        "changed_seconds": metadata.ctime(),
        "changed_nanoseconds": metadata.ctime_nsec(),
        "device": metadata.dev(),
        "inode": metadata.ino(),
    });
    let identity =
        DecodeSourceIdentity::from_sha256(hex::encode(Sha256::digest(body.to_string().as_bytes())))
            .map_err(|error| DecodeFactError::SourceMetadata(error.to_string()))?;
    Ok(DecodeSourceObservation {
        identity,
        executable: metadata.permissions().mode() & 0o111 != 0,
    })
}

#[cfg(unix)]
async fn source_observation_with_probe_gate(
    source: Arc<std::fs::File>,
    probe_gate: tokio::sync::OwnedSemaphorePermit,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
    delay: Duration,
) -> Result<(DecodeSourceObservation, tokio::sync::OwnedSemaphorePermit), DecodeFactError> {
    if budget.is_zero() {
        return Err(DecodeFactError::Deadline);
    }
    let started = std::time::Instant::now();
    let identity_permit = tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => return Err(DecodeFactError::Cancelled),
        permit = tokio::time::timeout(budget, identity_gate().acquire_owned()) => {
            permit
                .map_err(|_| DecodeFactError::Deadline)?
                .map_err(|_| DecodeFactError::CacheInvariant)?
        }
    };
    let remaining = budget.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(DecodeFactError::Deadline);
    }
    // Both admissions move into the blocking owner. A timed-out caller can
    // detach, but another probe or metadata syscall cannot enter until this
    // one actually returns.
    let mut task = tokio::task::spawn_blocking(move || {
        let _identity_permit = identity_permit;
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        (source_observation(&source), probe_gate)
    });
    let (observation, probe_gate) = tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => return Err(DecodeFactError::Cancelled),
        result = tokio::time::timeout(remaining, &mut task) => {
            result
                .map_err(|_| DecodeFactError::Deadline)?
                .map_err(|error| DecodeFactError::SourceMetadata(error.to_string()))?
        }
    };
    Ok((observation?, probe_gate))
}

#[cfg(windows)]
fn source_observation_windows(
    source: &std::fs::File,
) -> Result<DecodeSourceObservation, DecodeFactError> {
    let identity = plurx_core::fs_secure::std_file_identity(source)
        .map_err(|error| DecodeFactError::SourceMetadata(error.to_string()))?;
    let body = serde_json::json!({
        "bytes": identity.size,
        "changed_seconds": identity.changed_seconds,
        "changed_nanoseconds": identity.changed_nanoseconds,
        "device": identity.device,
        "inode": identity.inode,
        "inode_high": identity.inode_high,
    });
    let identity =
        DecodeSourceIdentity::from_sha256(hex::encode(Sha256::digest(body.to_string().as_bytes())))
            .map_err(|error| DecodeFactError::SourceMetadata(error.to_string()))?;
    Ok(DecodeSourceObservation { identity })
}

#[cfg(windows)]
async fn source_observation_with_probe_gate(
    source: Arc<std::fs::File>,
    probe_gate: tokio::sync::OwnedSemaphorePermit,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
    delay: Duration,
) -> Result<(DecodeSourceObservation, tokio::sync::OwnedSemaphorePermit), DecodeFactError> {
    if budget.is_zero() {
        return Err(DecodeFactError::Deadline);
    }
    let started = std::time::Instant::now();
    let identity_permit = tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => return Err(DecodeFactError::Cancelled),
        permit = tokio::time::timeout(budget, identity_gate().acquire_owned()) => {
            permit
                .map_err(|_| DecodeFactError::Deadline)?
                .map_err(|_| DecodeFactError::CacheInvariant)?
        }
    };
    let remaining = budget.saturating_sub(started.elapsed());
    let mut task = tokio::task::spawn_blocking(move || {
        let _identity_permit = identity_permit;
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        Ok::<_, DecodeFactError>((source_observation_windows(&source)?, probe_gate))
    });
    tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => Err(DecodeFactError::Cancelled),
        result = tokio::time::timeout(remaining, &mut task) => {
            result
                .map_err(|_| DecodeFactError::Deadline)?
                .map_err(|error| DecodeFactError::SourceMetadata(error.to_string()))?
        }
    }
}

async fn read_bounded<R>(mut reader: R, limit: usize) -> Result<(Vec<u8>, bool), DecodeFactError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut retained = Vec::with_capacity(limit.min(8 * 1024));
    let mut scratch = [0_u8; 8 * 1024];
    let mut overflow = false;
    loop {
        let count = reader
            .read(&mut scratch)
            .await
            .map_err(|error| DecodeFactError::Read(error.to_string()))?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        retained.extend_from_slice(&scratch[..count.min(remaining)]);
        overflow |= count > remaining;
    }
    Ok((retained, overflow))
}

fn idet_count(summary: &str, label: &str) -> Option<u64> {
    let tail = summary.split_once(label)?.1.trim_start();
    let digits = tail
        .strip_prefix(':')?
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn parse_idet_verdict(stderr: &[u8]) -> Option<InterlaceVerdict> {
    let summary = String::from_utf8_lossy(stderr)
        .lines()
        .rev()
        .find(|line| line.contains("Multi frame detection:"))?
        .to_owned();
    let tff = idet_count(&summary, "TFF")?;
    let bff = idet_count(&summary, "BFF")?;
    let progressive = idet_count(&summary, "Progressive")?;
    let undetermined = idet_count(&summary, "Undetermined")?;
    let total = tff
        .checked_add(bff)?
        .checked_add(progressive)?
        .checked_add(undetermined)?;
    if total == 0 {
        return None;
    }
    if progressive.saturating_mul(100) > total.saturating_mul(90) {
        Some(InterlaceVerdict::FlagOverruled)
    } else {
        Some(InterlaceVerdict::FlagConfirmed)
    }
}

#[cfg(target_os = "macos")]
fn duplicate_macos_source(fd: std::os::fd::RawFd) -> Result<Arc<std::fs::File>, DecodeFactError> {
    use std::os::fd::FromRawFd;
    let copied = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if copied < 0 {
        return Err(DecodeFactError::Read(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    Ok(Arc::new(unsafe { std::fs::File::from_raw_fd(copied) }))
}

#[cfg(target_os = "macos")]
async fn run_macos_parser(
    snapshot: &Arc<ExecutableSnapshot>,
    source: Option<Arc<std::fs::File>>,
    args: Vec<String>,
    stdout_limit: usize,
    deadline: std::time::Instant,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<macos_wasm::Output, DecodeFactError> {
    let parser = snapshot.parser.as_ref().cloned().ok_or_else(|| {
        DecodeFactError::ProbeIdentity("packaged source parser was not compiled".into())
    })?;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let mut worker = tokio::task::spawn_blocking(move || {
        parser.run(
            source,
            args,
            (stdout_limit, MAX_PROBE_STDERR_BYTES),
            deadline,
            worker_stop,
        )
    });
    let result = tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => Err(DecodeFactError::Cancelled),
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => Err(DecodeFactError::Deadline),
        result = &mut worker => return result.map_err(|e| DecodeFactError::Read(e.to_string()))?
            .map_err(|reason| if reason == "parser deadline" { DecodeFactError::Deadline }
                else if reason == "parser cancelled" { DecodeFactError::Cancelled }
                else if reason == "parser output exceeds bound" { DecodeFactError::OversizedOutput }
                else { DecodeFactError::Read(reason) }),
    };
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    // A filesystem syscall can settle after the caller's deadline. The owned
    // collection task still holds source/admission guards until this join.
    let _ = worker.await;
    result
}

#[cfg(unix)]
async fn collect_idet_verdict(
    probe: &DecodeProbeIdentity,
    source_fd: std::os::fd::RawFd,
    input_video_stream: u32,
    launch_deadline: std::time::Instant,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
    work: crate::process_control::ChildWork,
) -> Result<InterlaceVerdict, DecodeFactError> {
    let filter =
        format!("movie=/dev/fd/3:si={input_video_stream},trim=duration={IDET_MEDIA_SECONDS},idet");
    let arguments = vec![
        OsString::from("-hide_banner"),
        OsString::from("-v"),
        OsString::from("info"),
        OsString::from("-f"),
        OsString::from("lavfi"),
        OsString::from("-i"),
        OsString::from(filter),
        OsString::from("-show_entries"),
        OsString::from("frame=media_type"),
        OsString::from("-of"),
        OsString::from("compact=p=0:nk=1"),
    ];
    Ok(collect_refinement_output(
        probe,
        source_fd,
        arguments,
        launch_deadline,
        cancelled,
        work,
    )
    .await?
    .and_then(|(_, stderr)| parse_idet_verdict(&stderr))
    .unwrap_or(InterlaceVerdict::IdetUnavailable))
}

fn needs_hevc_scan_refinement(facts: &DecodeFacts) -> bool {
    facts.codec() == Some("hevc")
        && facts.scan_type() == ScanType::Unknown
        && facts.field_order().is_none_or(|order| order == "unknown")
}

#[cfg(unix)]
async fn collect_refinement_output(
    probe: &DecodeProbeIdentity,
    source_fd: std::os::fd::RawFd,
    arguments: Vec<OsString>,
    launch_deadline: std::time::Instant,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
    work: crate::process_control::ChildWork,
) -> Result<Option<(Vec<u8>, Vec<u8>)>, DecodeFactError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    #[cfg(target_os = "macos")]
    if probe.launch_mode.is_production() {
        let source = duplicate_macos_source(source_fd)?;
        let output = run_macos_parser(
            &probe.executable_snapshot,
            Some(source),
            arguments
                .iter()
                .map(|v| v.to_string_lossy().into_owned())
                .collect(),
            MAX_PROBE_STDOUT_BYTES,
            launch_deadline,
            cancelled,
        )
        .await?;
        if output.status != 0 {
            return Ok(None);
        }
        return Ok(Some((output.stdout, output.stderr)));
    }
    let executable_fd = probe.executable_snapshot.as_file().as_raw_fd();
    let mut command = tokio::process::Command::new(probe_launch_path(
        &probe.executable_snapshot,
        probe.launch_mode,
    )?);
    command.as_std_mut().arg0(probe.executable());
    #[cfg(test)]
    command.env("PLURX_TEST_PROBE_PATH", probe.executable());
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let supervisor = configure_probe_execution(
        &mut command,
        probe.launch_mode,
        launch_deadline,
        executable_fd,
        Some(source_fd),
        probe.executable(),
        &arguments,
        work.class,
    )?;
    let (mut child, _child_job, mut supervisor) =
        spawn_configured_probe(command, supervisor, work).await?;
    let process_group = child.id().and_then(|id| libc::pid_t::try_from(id).ok());
    let session = ProbeSessionGuard::new(process_group);
    #[cfg(target_os = "linux")]
    let exit_anchor = match ProbeExitAnchor::open(process_group, probe.launch_mode) {
        Ok(anchor) => anchor,
        Err(error) => {
            terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
            return Err(DecodeFactError::Spawn(format!(
                "opening refinement FFprobe exit anchor: {error}"
            )));
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
            return Err(DecodeFactError::MissingPipe);
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
            return Err(DecodeFactError::MissingPipe);
        }
    };
    let remaining = launch_deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
        return Err(DecodeFactError::Deadline);
    }
    let outcome = tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => {
            terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
            return Err(DecodeFactError::Cancelled);
        }
        outcome = tokio::time::timeout(remaining, async {
            #[cfg(target_os = "linux")]
            let (stdout, stderr, exited) = tokio::join!(
                read_bounded(stdout, MAX_PROBE_STDOUT_BYTES),
                read_bounded(stderr, MAX_PROBE_STDERR_BYTES),
                async {
                    let exited = exit_anchor.wait_until_exit().await;
                    session.kill_once();
                    exited
                },
            );
            #[cfg(target_os = "linux")]
            let (status, exit_error) = {
                let exit_error = exited.err().map(|error| DecodeFactError::Read(error.to_string()));
                (wait_for_probe_reap(&mut child, probe.launch_mode).await, exit_error)
            };
            #[cfg(not(target_os = "linux"))]
            let (stdout, stderr, status) = tokio::join!(
                read_bounded(stdout, MAX_PROBE_STDOUT_BYTES),
                read_bounded(stderr, MAX_PROBE_STDERR_BYTES),
                child.wait(),
            );
            #[cfg(not(target_os = "linux"))]
            session.kill_once();
            #[cfg(not(target_os = "linux"))]
            let exit_error: Option<DecodeFactError> = None;
            Ok::<_, DecodeFactError>((stdout, stderr, status, exit_error))
        }) => outcome,
    };
    let (stdout, stderr, status, exit_error) = match outcome {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => return Err(error),
        Err(_) => {
            terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
            return Err(DecodeFactError::Deadline);
        }
    };
    let supervisor_result = supervisor.finish();
    if let Some(error) = exit_error {
        return Err(error);
    }
    supervisor_result?;
    let stdout = stdout?;
    let stderr = stderr?;
    let status = status.map_err(|error| DecodeFactError::Read(error.to_string()))?;
    if stdout.1 || stderr.1 || !status.success() {
        return Ok(None);
    }
    Ok(Some((stdout.0, stderr.0)))
}

#[cfg(unix)]
async fn refine_scan_type(
    probe: &DecodeProbeIdentity,
    source_fd: std::os::fd::RawFd,
    mut facts: DecodeFacts,
    deadline: std::time::Instant,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
    work: crate::process_control::ChildWork,
) -> Result<DecodeFacts, DecodeFactError> {
    if needs_hevc_scan_refinement(&facts) {
        // Select only this video stream: a full show_data document also dumps
        // attached fonts and pictures and needlessly consumes the probe bound.
        let arguments = [
            "-v",
            "error",
            "-select_streams",
            &facts.input_video_stream().to_string(),
            "-show_entries",
            "stream=index,codec_type,codec_name,extradata",
            "-show_data",
            "-of",
            "json",
            "/dev/fd/3",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        match collect_refinement_output(probe, source_fd, arguments, deadline, cancelled, work)
            .await
        {
            Ok(Some((stdout, _))) => {
                if let Ok(document) = serde_json::from_slice(&stdout) {
                    facts = facts.with_hevc_scan_metadata(&document);
                }
            }
            Err(DecodeFactError::Cancelled) => return Err(DecodeFactError::Cancelled),
            _ => {} // Missing evidence preserves Unknown and the CPU route.
        }
    }
    if matches!(facts.scan_type(), ScanType::Interlaced(_)) {
        let verdict = match collect_idet_verdict(
            probe,
            source_fd,
            facts.input_video_stream(),
            deadline,
            cancelled,
            work,
        )
        .await
        {
            Ok(verdict) => verdict,
            Err(DecodeFactError::Cancelled) => return Err(DecodeFactError::Cancelled),
            Err(_) => InterlaceVerdict::IdetUnavailable,
        };
        record_interlace_verdict(verdict);
        facts = facts.with_interlace_verdict(verdict);
    }
    Ok(facts)
}

#[cfg(unix)]
async fn collect(
    probe: &DecodeProbeIdentity,
    source: DecodeFactCollectionSource,
    catalog: Option<&DecodeCatalogMetadata>,
    selected_stream: ProbeStreamSelection,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<CollectedProbe, DecodeFactError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let DecodeFactCollectionSource {
        handle,
        observation,
        offset_permit,
        work,
        projection,
        seed,
    } = source;
    let started = std::time::Instant::now();
    let launch_deadline = started + budget.min(probe.launch_mode.probe_deadline());
    let source_fd = handle.as_raw_fd();
    if probe.launch_mode.is_production() && observation.executable {
        return Err(DecodeFactError::SourceMetadata(
            "bound media source must not have executable mode bits".to_owned(),
        ));
    }
    let source_offset = unsafe { libc::lseek(source_fd, 0, libc::SEEK_CUR) };
    if source_offset == -1 {
        return Err(DecodeFactError::SourceMetadata(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    // A duplicated descriptor shares its open-file offset. FFprobe is allowed
    // to seek while inspecting the held inode, but the later FFmpeg spawn must
    // inherit the same offset the preparation owner held before probing.
    struct RestoreOffset {
        fd: std::os::fd::RawFd,
        offset: libc::off_t,
    }
    impl Drop for RestoreOffset {
        fn drop(&mut self) {
            unsafe {
                libc::lseek(self.fd, self.offset, libc::SEEK_SET);
            }
        }
    }
    let restore_offset = RestoreOffset {
        fd: source_fd,
        offset: source_offset,
    };
    if let Some(seed) = seed {
        let facts = refine_scan_type(
            probe,
            source_fd,
            seed.facts?,
            launch_deadline,
            cancelled,
            work,
        )
        .await?;
        drop(restore_offset);
        drop(offset_permit);
        return projection.finish(Ok(facts), &[], &probe.reporter);
    }
    #[cfg(target_os = "macos")]
    if probe.launch_mode.is_production() {
        let mut args = projection.arguments();
        args.push("/dev/fd/3".into());
        let output = run_macos_parser(
            &probe.executable_snapshot,
            Some(duplicate_macos_source(source_fd)?),
            args,
            projection.output_limit(),
            launch_deadline,
            cancelled,
        )
        .await?;
        if output.status != 0 {
            return Err(DecodeFactError::Failed(
                Some(output.status),
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(512)
                    .collect(),
            ));
        }
        let facts = parse_collected_facts(
            &output.stdout,
            observation.identity,
            catalog,
            selected_stream,
        );
        let facts = if projection == ProbeProjection::Decoder {
            Ok(
                refine_scan_type(probe, source_fd, facts?, launch_deadline, cancelled, work)
                    .await?,
            )
        } else {
            facts
        };
        drop(restore_offset);
        drop(offset_permit);
        return projection.finish(facts, &output.stdout, &probe.reporter);
    }
    let executable_fd = probe.executable_snapshot.as_file().as_raw_fd();
    let mut arguments = projection
        .arguments()
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
    arguments.push(OsString::from("/dev/fd/3"));
    let mut command = tokio::process::Command::new(probe_launch_path(
        &probe.executable_snapshot,
        probe.launch_mode,
    )?);
    command.as_std_mut().arg0(probe.executable());
    #[cfg(test)]
    command.env("PLURX_TEST_PROBE_PATH", probe.executable());
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let supervisor = configure_probe_execution(
        &mut command,
        probe.launch_mode,
        launch_deadline,
        executable_fd,
        Some(source_fd),
        probe.executable(),
        &arguments,
        work.class,
    )?;
    let (mut child, _child_job, mut supervisor) =
        spawn_configured_probe(command, supervisor, work).await?;
    let process_group = child.id().and_then(|id| libc::pid_t::try_from(id).ok());
    let session = ProbeSessionGuard::new(process_group);
    #[cfg(target_os = "linux")]
    let exit_anchor = match ProbeExitAnchor::open(process_group, probe.launch_mode) {
        Ok(anchor) => anchor,
        Err(error) => {
            terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
            return Err(DecodeFactError::Spawn(format!(
                "opening FFprobe exit anchor: {error}"
            )));
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
            return Err(DecodeFactError::MissingPipe);
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
            return Err(DecodeFactError::MissingPipe);
        }
    };
    let remaining = launch_deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
        return Err(DecodeFactError::Deadline);
    }
    let outcome = tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => {
            terminate_probe_session(
                &mut child,
                &session,
                &mut supervisor,
                probe.launch_mode,
            ).await;
            return Err(DecodeFactError::Cancelled);
        }
        outcome = tokio::time::timeout(remaining, async {
            #[cfg(target_os = "linux")]
            let (stdout, stderr, exited) = tokio::join!(
                read_bounded(stdout, projection.output_limit()),
                read_bounded(stderr, MAX_PROBE_STDERR_BYTES),
                async {
                    let exited = exit_anchor.wait_until_exit().await;
                    session.kill_once();
                    exited
                },
            );
            #[cfg(target_os = "linux")]
            let (status, exit_error) = {
                let exit_error = exited
                    .err()
                    .map(|error| DecodeFactError::Read(error.to_string()));
                (
                    wait_for_probe_reap(&mut child, probe.launch_mode).await,
                    exit_error,
                )
            };
            #[cfg(not(target_os = "linux"))]
            let (stdout, stderr, status) = tokio::join!(
                read_bounded(stdout, projection.output_limit()),
                read_bounded(stderr, MAX_PROBE_STDERR_BYTES),
                child.wait(),
            );
            #[cfg(not(target_os = "linux"))]
            session.kill_once();
            #[cfg(not(target_os = "linux"))]
            let exit_error: Option<DecodeFactError> = None;
            Ok::<_, DecodeFactError>((stdout, stderr, status, exit_error))
        }) => outcome,
    };
    let (stdout, stderr, status, exit_error) = match outcome {
        Ok(Ok((stdout, stderr, status, exit_error))) => (stdout, stderr, status, exit_error),
        Ok(Err(error)) => return Err(error),
        Err(_) => {
            terminate_probe_session(&mut child, &session, &mut supervisor, probe.launch_mode).await;
            return Err(DecodeFactError::Deadline);
        }
    };
    let supervisor_result = supervisor.finish();
    if let Some(error) = exit_error {
        return Err(error);
    }
    supervisor_result?;
    let stdout = stdout?;
    let stderr = stderr?;
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            return Err(DecodeFactError::Read(error.to_string()));
        }
    };
    if stdout.1 || stderr.1 {
        return Err(DecodeFactError::OversizedOutput);
    }
    if !status.success() {
        let reason = String::from_utf8_lossy(&stderr.0)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("FFprobe gave no reason")
            .chars()
            .take(512)
            .collect();
        return Err(DecodeFactError::Failed(status.code(), reason));
    }
    let facts = parse_collected_facts(&stdout.0, observation.identity, catalog, selected_stream);
    if projection != ProbeProjection::Decoder {
        drop(restore_offset);
        drop(offset_permit);
        return projection.finish(facts, &stdout.0, &probe.reporter);
    }
    let mut facts = facts?;
    if projection == ProbeProjection::Decoder {
        facts = refine_scan_type(probe, source_fd, facts, launch_deadline, cancelled, work).await?;
    }
    // Every descriptor-reading child has now been reaped. Restore the shared
    // open-file offset and release the producer lane before the authoritative
    // final metadata observation.
    drop(restore_offset);
    drop(offset_permit);
    projection.finish(Ok(facts), &stdout.0, &probe.reporter)
}

fn parse_collected_facts(
    stdout: &[u8],
    source_identity: DecodeSourceIdentity,
    catalog: Option<&DecodeCatalogMetadata>,
    selected_stream: ProbeStreamSelection,
) -> Result<DecodeFacts, DecodeFactError> {
    let json: serde_json::Value = serde_json::from_slice(stdout)
        .map_err(|error| DecodeFactError::InvalidJson(error.to_string()))?;
    match selected_stream {
        ProbeStreamSelection::FirstPlayable => match catalog {
            Some(catalog) => {
                DecodeFacts::from_ffprobe_json_with_catalog(&json, source_identity, catalog)
            }
            None => DecodeFacts::from_ffprobe_json(&json, source_identity),
        },
        ProbeStreamSelection::LegacyVideoOrdinal(ordinal) => {
            let index = absolute_video_ordinal(&json, ordinal).ok_or_else(|| {
                DecodeFactError::InvalidFacts("legacy video stream is missing".to_owned())
            })?;
            legacy_ordinal_facts(&json, source_identity, index, catalog)
        }
        ProbeStreamSelection::Absolute(index) => {
            match catalog.filter(|_| first_playable_video_index(&json) == Some(index)) {
                Some(catalog) => DecodeFacts::from_ffprobe_json_at_with_catalog(
                    &json,
                    source_identity,
                    index,
                    catalog,
                ),
                None => DecodeFacts::from_ffprobe_json_at(&json, source_identity, index),
            }
        }
    }
    // Read through the descriptor held across the whole measurement, so these
    // facts are evidence about the bytes on disk and not merely about a row.
    .map(DecodeFacts::descriptor_bound)
    .map_err(|error| DecodeFactError::InvalidFacts(error.to_string()))
}

/// The absolute index FFmpeg's `0:v:<ordinal>` resolves to.
///
/// Attached pictures are counted, because FFmpeg counts them: `-map 0:v:0` on a
/// file with cover art selects the cover art. That is what the shipping command
/// does, so it is what the plan has to describe. Skipping them here would be a
/// silent routing change dressed up as a fix — and it would still leave the two
/// planning routes disagreeing, because the disagreement is about which rule to
/// use, not about how the rule counts. Both routes use this one.
/// Facts for the stream `0:v:<ordinal>` resolves to, catalog applied only when
/// that stream is the film.
///
/// Both planning routes call this. They used to select differently — the
/// descriptor-bound route by FFmpeg ordinal, the stored-probe route by "first
/// playable" — which on a file carrying cover art is two different streams,
/// two different plans and two artifact names for one title, only one of which
/// matched the command. One rule, in one place, so they cannot drift again.
pub(crate) fn legacy_ordinal_facts(
    json: &serde_json::Value,
    source_identity: plurx_core::transcode::DecodeSourceIdentity,
    index: u32,
    catalog: Option<&DecodeCatalogMetadata>,
) -> Result<DecodeFacts, plurx_core::transcode::PlanError> {
    // The catalog row describes the film. Applied to an attached picture it
    // would claim that a JPEG is Dolby Vision.
    match catalog.filter(|_| first_playable_video_index(json) == Some(index)) {
        Some(catalog) => {
            DecodeFacts::from_ffprobe_json_at_with_catalog(json, source_identity, index, catalog)
        }
        None => DecodeFacts::from_ffprobe_json_at(json, source_identity, index),
    }
}

pub(crate) fn absolute_video_ordinal(json: &serde_json::Value, ordinal: u32) -> Option<u32> {
    json.get("streams")?
        .as_array()?
        .iter()
        .filter(|stream| {
            stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
        })
        .nth(usize::try_from(ordinal).ok()?)?
        .get("index")?
        .as_u64()
        .and_then(|index| u32::try_from(index).ok())
}

pub(crate) fn first_playable_video_index(json: &serde_json::Value) -> Option<u32> {
    json.get("streams")?
        .as_array()?
        .iter()
        .find(|stream| {
            stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
                && !stream
                    .get("disposition")
                    .and_then(|value| value.get("attached_pic"))
                    .and_then(serde_json::Value::as_i64)
                    .is_some_and(|value| value != 0)
        })?
        .get("index")?
        .as_u64()
        .and_then(|index| u32::try_from(index).ok())
}

#[cfg(windows)]
async fn collect(
    probe: &DecodeProbeIdentity,
    source: DecodeFactCollectionSource,
    catalog: Option<&DecodeCatalogMetadata>,
    selected_stream: ProbeStreamSelection,
    budget: Duration,
    cancelled: Option<&tokio_util::sync::CancellationToken>,
) -> Result<CollectedProbe, DecodeFactError> {
    let DecodeFactCollectionSource {
        handle,
        observation,
        offset_permit,
        work,
        projection,
        seed,
    } = source;
    let launch_deadline =
        std::time::Instant::now() + budget.min(probe.launch_mode.probe_deadline());
    let source_path = plurx_core::fs_secure::std_file_path(&handle)
        .map_err(|error| DecodeFactError::SourceMetadata(error.to_string()))?;
    if source_observation_windows(&handle)?.identity != observation.identity {
        return Err(DecodeFactError::ProbeChanged);
    }
    if let Some(seed) = seed {
        let facts = seed.facts?;
        let facts = if matches!(facts.scan_type(), ScanType::Interlaced(_)) {
            facts.with_interlace_verdict(InterlaceVerdict::IdetUnavailable)
        } else {
            facts
        };
        drop(offset_permit);
        return projection.finish(Ok(facts), &[], &probe.reporter);
    }
    let arguments = projection.arguments();
    let mut command =
        tokio::process::Command::new(snapshot_execution_path(&probe.executable_snapshot));
    command
        .args(arguments)
        .arg(&source_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let (mut child, _child_job) = crate::process_control::spawn_job_owned(&mut command, work)
        .map_err(|error| DecodeFactError::Spawn(error.to_string()))?;
    let stdout = child.stdout.take().ok_or(DecodeFactError::MissingPipe)?;
    let stderr = child.stderr.take().ok_or(DecodeFactError::MissingPipe)?;
    let remaining = launch_deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        terminate_windows_probe(&mut child).await;
        return Err(DecodeFactError::Deadline);
    }
    let outcome = tokio::select! {
        biased;
        _ = wait_for_cancellation(cancelled) => {
            terminate_windows_probe(&mut child).await;
            return Err(DecodeFactError::Cancelled);
        }
        result = tokio::time::timeout(remaining, async {
            tokio::join!(
                read_bounded(stdout, projection.output_limit()),
                read_bounded(stderr, MAX_PROBE_STDERR_BYTES),
                child.wait(),
            )
        }) => result,
    };
    let (stdout, stderr, status) = match outcome {
        Ok(result) => result,
        Err(_) => {
            terminate_windows_probe(&mut child).await;
            return Err(DecodeFactError::Deadline);
        }
    };
    drop(offset_permit);
    let stdout = stdout?;
    let stderr = stderr?;
    let status = status.map_err(|error| DecodeFactError::Read(error.to_string()))?;
    if stdout.1 || stderr.1 {
        return Err(DecodeFactError::OversizedOutput);
    }
    if !status.success() {
        let reason = String::from_utf8_lossy(&stderr.0)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("FFprobe gave no reason")
            .chars()
            .take(512)
            .collect();
        return Err(DecodeFactError::Failed(status.code(), reason));
    }
    if source_observation_windows(&handle)?.identity != observation.identity {
        return Err(DecodeFactError::ProbeChanged);
    }
    let facts = parse_collected_facts(&stdout.0, observation.identity, catalog, selected_stream);
    if projection != ProbeProjection::Decoder {
        return projection.finish(facts, &stdout.0, &probe.reporter);
    }
    let mut facts = facts?;
    if projection == ProbeProjection::Decoder
        && matches!(facts.scan_type(), ScanType::Interlaced(_))
    {
        record_interlace_verdict(InterlaceVerdict::IdetUnavailable);
        facts = facts.with_interlace_verdict(InterlaceVerdict::IdetUnavailable);
    }
    projection.finish(Ok(facts), &stdout.0, &probe.reporter)
}

#[cfg(windows)]
async fn terminate_windows_probe(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        let _ =
            crate::process_control::signal(pid, crate::process_control::ProcessSignal::Terminate);
    }
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests;
