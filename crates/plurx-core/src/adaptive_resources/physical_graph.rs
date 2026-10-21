//! Physical ownership, not a caller's assertion that a PID was reaped.
//!
//! The ledger retains this bounded custody independently of an async waiter.
//! Roots enter only through the real launcher; writers are real owned tasks.
//! This initial backend cannot certify descendant extinction. Successful root
//! waits and writer joins therefore leave UnknownHeld, never a close receipt.
//! Positive kernel containment, audited nonprocess adapters and restart-journal
//! reconciliation remain required before this can enable production expansion.

use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::{
    AdmissionRefusal, ReservationBinding, ReservationState, ResourceLedger, ResourceReservation,
};
use crate::process::observation::{Metric, ProcessIdentity, ProcessObserver, UnknownReason};
use crate::process::{spawn_job_custodied, ChildJob, ChildWork, OwnedSpawnFailure};

const MAX_ROOTS: usize = 32;
const MAX_WRITERS: usize = 32;
static PROCESS_INSTANCE: LazyLock<Uuid> = LazyLock::new(Uuid::new_v4);
static BOOT_IDENTITY: LazyLock<Metric<Uuid>> = LazyLock::new(read_boot_identity);

/// Diagnostic identities, never a receipt or admission capability. Unknown boot
/// identity cannot be replaced with a process-random UUID for reboot recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphIdentity {
    pub generation: Uuid,
    pub process_instance: Uuid,
    pub os_boot: Metric<Uuid>,
}

/// Minted by this graph for the actual child it launched. Numeric PIDs supplied
/// by another owner cannot be attached retroactively or used to select a root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphRoot {
    generation: Uuid,
    index: usize,
    pub process: Metric<ProcessIdentity>,
}

/// Pipes move directly from the owned root into its registered task. This is
/// not a generic resource-close adapter: task code must not export a counted
/// handle or detach work, and requires a concrete adapter audit before any
/// positive physical-close backend may rely on it.
pub struct RootPipes {
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
}

struct Root {
    child: Child,
    // Retain the actual job even after leader wait. Dropping it is not proof.
    _job: Option<ChildJob>,
    process: Metric<ProcessIdentity>,
    waited: bool,
    pipes_claimed: bool,
}

struct Members {
    acquired: bool,
    roots: Vec<Root>,
    writers: Vec<JoinHandle<()>>,
    joined_writers: usize,
    abandoned_writers: usize,
}

pub(super) struct GraphCustody {
    binding: ReservationBinding,
    identity: GraphIdentity,
    root_limit: usize,
    sealed: AtomicBool,
    members: Mutex<Members>,
}

impl GraphCustody {
    pub(super) fn seal_entry(&self) {
        self.sealed.store(true, Ordering::Release);
    }
}

/// Exclusive acquisition owner. Dropping it seals entry, but neither drops
/// possible physical owners nor clears their accounting from the ledger.
#[must_use]
pub struct OwnedPhysicalGraph {
    ledger: ResourceLedger,
    custody: Option<Arc<GraphCustody>>,
}

/// Reaping/reconciliation capability. It cannot launch or acquire members.
/// Concurrent reconcilers serialize on the SAME retained owner set.
#[must_use]
pub struct SealedPhysicalGraph {
    ledger: ResourceLedger,
    custody: Arc<GraphCustody>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphHoldReason {
    ContainmentUnavailable,
    RootWaitFailed,
    LedgerUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphCloseOutcome {
    /// No launch, writer or resource acquisition could have happened.
    NoAcquisition,
    /// Resources and the trial latch stay counted. No success receipt exists.
    UnknownHeld(GraphHoldReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphCustodySnapshot {
    pub identity: GraphIdentity,
    pub sealed: bool,
    pub acquired: bool,
    pub roots: usize,
    pub waited_roots: usize,
    pub writers: usize,
    pub joined_writers: usize,
    pub abandoned_writers: usize,
}

impl ResourceReservation {
    /// Move exclusive reservation custody into the real acquisition owner.
    /// There is no externally launched child/receipt/cleanup callback argument.
    /// Unsupported containment refuses speculation BEFORE any launch; normal
    /// acquisition remains conservative and can accumulate UnknownHeld.
    pub fn into_physical_graph(mut self) -> Result<OwnedPhysicalGraph, AdmissionRefusal> {
        // Boot lookup may perform bounded OS I/O. Never hold the ledger's
        // short accounting lock while doing that or initializing randomness.
        let identity = GraphIdentity {
            generation: Uuid::new_v4(),
            process_instance: *PROCESS_INSTANCE,
            os_boot: *BOOT_IDENTITY,
        };
        let mut state = self
            .ledger
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if state.invalid {
            return Err(AdmissionRefusal::Unavailable);
        }
        if state
            .trial
            .as_ref()
            .is_some_and(|members| members.contains(&self.binding.reservation))
        {
            return Err(AdmissionRefusal::InsufficientEvidence);
        }
        let entry = state
            .entries
            .get_mut(&self.binding.reservation)
            .ok_or(AdmissionRefusal::UnknownReservation)?;
        if entry.binding != self.binding
            || entry.state != ReservationState::Reserved
            || entry.custody.is_some()
        {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        let custody = Arc::new(GraphCustody {
            binding: self.binding,
            identity,
            root_limit: usize::try_from(entry.resources.processes)
                .unwrap_or(usize::MAX)
                .min(MAX_ROOTS),
            sealed: AtomicBool::new(false),
            members: Mutex::new(Members {
                acquired: false,
                roots: Vec::new(),
                writers: Vec::new(),
                joined_writers: 0,
                abandoned_writers: 0,
            }),
        });
        entry.custody = Some(Arc::clone(&custody));
        self.transferred = true;
        Ok(OwnedPhysicalGraph {
            ledger: self.ledger.clone(),
            custody: Some(custody),
        })
    }
}

impl ResourceLedger {
    /// Existing supervised cleanup owners can retry unfinished closure without
    /// taking an active graph's acquisition capability or inventing a new epoch.
    /// This is bounded by the reservation table, not a watchdog or TTL release.
    pub fn retained_physical_graphs(&self) -> Result<Vec<SealedPhysicalGraph>, AdmissionRefusal> {
        let state = self
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        Ok(state
            .entries
            .values()
            .filter_map(|entry| {
                let custody = entry.custody.as_ref()?;
                custody
                    .sealed
                    .load(Ordering::Acquire)
                    .then(|| SealedPhysicalGraph {
                        ledger: self.clone(),
                        custody: Arc::clone(custody),
                    })
            })
            .collect())
    }
}

impl OwnedPhysicalGraph {
    fn custody(&self) -> &Arc<GraphCustody> {
        self.custody.as_ref().expect("unsealed graph owns custody")
    }

    pub fn identity(&self) -> GraphIdentity {
        self.custody().identity
    }

    /// Spawn directly under custody. Mark uncertainty BEFORE the shared launcher
    /// can partially succeed, and retain the actual Child and ChildJob together.
    pub async fn spawn_root(
        &mut self,
        command: &mut Command,
        work: ChildWork,
    ) -> io::Result<GraphRoot> {
        let custody = self.custody();
        let mut members = custody.members.lock().await;
        if custody.sealed.load(Ordering::Acquire) || members.roots.len() >= custody.root_limit {
            return Err(io::Error::other(
                "physical graph sealed or root bound reached",
            ));
        }
        begin_acquisition(&self.ledger, custody, &mut members)?;
        // A returned spawn error is NOT no-spawn proof, including attach/resume
        // failures inside the shared launcher. Ledger custody remains held.
        let (child, job) = match spawn_job_custodied(command, work) {
            Ok(owned) => owned,
            Err(failure) => {
                let error = retain_launch_failure(custody, &mut members, *failure);
                // A partial/ambiguous launch consumes the rest of this graph's
                // acquisition authority; repeated errors cannot start hidden
                // children outside the reservation's process bound.
                retain_or_release_unacquired(&self.ledger, custody);
                return Err(error);
            }
        };
        Ok(retain_root(custody, &mut members, child, Some(job)))
    }

    /// Core starts and retains the actual task, not a task-owned completion
    /// sender or a caller-supplied future claiming that cleanup succeeded.
    /// Join completion proves only this task ended, not descendant extinction or
    /// closure of arbitrary resources its code may have delegated elsewhere.
    pub async fn spawn_root_writer<F, Fut>(&mut self, root: GraphRoot, writer: F) -> io::Result<()>
    where
        F: FnOnce(RootPipes) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        let custody = self.custody();
        let mut members = custody.members.lock().await;
        if custody.sealed.load(Ordering::Acquire) || members.writers.len() >= MAX_WRITERS {
            return Err(io::Error::other(
                "physical graph sealed or writer bound reached",
            ));
        }
        if root.generation != custody.identity.generation {
            return Err(io::Error::other(
                "writer root belongs to another physical graph",
            ));
        }
        let member = members
            .roots
            .get_mut(root.index)
            .filter(|member| {
                member.process == root.process && !member.pipes_claimed && !member.waited
            })
            .ok_or_else(|| io::Error::other("physical graph root unavailable for writer"))?;
        member.pipes_claimed = true;
        let pipes = RootPipes {
            stdin: member.child.stdin.take(),
            stdout: member.child.stdout.take(),
            stderr: member.child.stderr.take(),
        };
        // No await separates task creation from retaining its JoinHandle. If
        // factory construction panics, the root/reservation stay in custody.
        members.writers.push(runtime.spawn(writer(pipes)));
        Ok(())
    }

    pub fn seal(mut self) -> SealedPhysicalGraph {
        let custody = self.custody.take().expect("unsealed graph owns custody");
        custody.sealed.store(true, Ordering::Release);
        SealedPhysicalGraph {
            ledger: self.ledger.clone(),
            custody,
        }
    }
}

fn retain_root(
    custody: &GraphCustody,
    members: &mut Members,
    child: Child,
    job: Option<ChildJob>,
) -> GraphRoot {
    let process = child
        .id()
        .map(
            |pid| match ProcessObserver::new(pid).snapshot(Instant::now()).counters {
                Metric::Known(counters) => Metric::Known(counters.identity),
                Metric::Unknown(reason) => Metric::Unknown(reason),
            },
        )
        .unwrap_or(Metric::Unknown(UnknownReason::UnboundIdentity));
    let root = GraphRoot {
        generation: custody.identity.generation,
        index: members.roots.len(),
        process,
    };
    members.roots.push(Root {
        child,
        _job: job,
        process,
        waited: false,
        pipes_claimed: false,
    });
    root
}

fn retain_launch_failure(
    custody: &GraphCustody,
    members: &mut Members,
    failure: OwnedSpawnFailure,
) -> io::Error {
    if let Some(child) = failure.child {
        retain_root(custody, members, child, failure.job);
    }
    failure.error
}

fn begin_acquisition(
    ledger: &ResourceLedger,
    custody: &GraphCustody,
    members: &mut Members,
) -> io::Result<()> {
    let mut state = ledger
        .inner
        .lock()
        .map_err(|_| io::Error::other("resource ledger unavailable"))?;
    if state.invalid {
        return Err(io::Error::other("resource ledger invalid"));
    }
    let entry = state
        .entries
        .get_mut(&custody.binding.reservation)
        .filter(|entry| entry.binding == custody.binding)
        .ok_or_else(|| io::Error::other("physical reservation unavailable"))?;
    if custody.sealed.load(Ordering::Acquire)
        || !matches!(
            entry.state,
            ReservationState::Reserved | ReservationState::SpawnPossible
        )
    {
        return Err(io::Error::other(
            "physical reservation no longer accepts acquisition",
        ));
    }
    if !members.acquired {
        if entry.state != ReservationState::Reserved {
            return Err(io::Error::other("physical reservation is not reserved"));
        }
        entry.state = ReservationState::SpawnPossible;
        members.acquired = true;
    }
    Ok(())
}

/// Called only by actual acquisition-owner Drop or sealed closure. No state in
/// a remote request or arbitrary success callback can enter this operation.
fn retain_or_release_unacquired(ledger: &ResourceLedger, custody: &GraphCustody) {
    custody.sealed.store(true, Ordering::Release);
    let Ok(mut state) = ledger.inner.lock() else {
        return;
    };
    let Some(entry) = state.entries.get_mut(&custody.binding.reservation) else {
        return;
    };
    if entry.binding != custody.binding {
        return;
    }
    if entry.state == ReservationState::Reserved {
        // The exclusive acquisition owner never reached begin_acquisition.
        let _ = state.release(custody.binding.reservation);
    } else {
        entry.state = ReservationState::UnknownHeld;
    }
}

impl Drop for OwnedPhysicalGraph {
    fn drop(&mut self) {
        if let Some(custody) = self.custody.take() {
            retain_or_release_unacquired(&self.ledger, &custody);
        }
    }
}

impl Drop for SealedPhysicalGraph {
    fn drop(&mut self) {
        retain_or_release_unacquired(&self.ledger, &self.custody);
    }
}

impl SealedPhysicalGraph {
    pub async fn snapshot(&self) -> GraphCustodySnapshot {
        let members = self.custody.members.lock().await;
        GraphCustodySnapshot {
            identity: self.custody.identity,
            sealed: self.custody.sealed.load(Ordering::Acquire),
            acquired: members.acquired,
            roots: members.roots.len(),
            waited_roots: members.roots.iter().filter(|root| root.waited).count(),
            writers: members.writers.len(),
            joined_writers: members.joined_writers,
            abandoned_writers: members.abandoned_writers,
        }
    }

    /// Cancellation drops only this waiter. The ledger retains every Child,
    /// Job and JoinHandle, including an unfinished wait/join, for reconciliation.
    /// No root wait, sender drop, successful terminate request or task panic is
    /// promoted to a physical-close receipt on an uncontained backend.
    pub async fn close(&self) -> GraphCloseOutcome {
        let mut members = self.custody.members.lock().await;
        if !members.acquired {
            retain_or_release_unacquired(&self.ledger, &self.custody);
            return match self.ledger.inner.lock() {
                Ok(state)
                    if !state
                        .entries
                        .contains_key(&self.custody.binding.reservation) =>
                {
                    GraphCloseOutcome::NoAcquisition
                }
                _ => GraphCloseOutcome::UnknownHeld(GraphHoldReason::LedgerUnavailable),
            };
        }
        retain_or_release_unacquired(&self.ledger, &self.custody);
        // Request all roots' termination before awaiting any of them. Their
        // real retained handles, not recycled numeric PIDs, select the targets.
        for root in &mut members.roots {
            if !root.waited {
                let _ = root.child.start_kill();
            }
        }
        for root in &mut members.roots {
            if !root.waited {
                if root.child.wait().await.is_err() {
                    return GraphCloseOutcome::UnknownHeld(GraphHoldReason::RootWaitFailed);
                }
                root.waited = true;
            }
        }
        while members.joined_writers < members.writers.len() {
            let index = members.joined_writers;
            let abandoned = (&mut members.writers[index]).await.is_err();
            members.joined_writers += 1;
            members.abandoned_writers += usize::from(abandoned);
        }
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    }
}

#[cfg(target_os = "linux")]
fn read_boot_identity() -> Metric<Uuid> {
    use std::io::Read;
    let result = std::fs::File::open("/proc/sys/kernel/random/boot_id").and_then(|file| {
        let mut value = String::new();
        file.take(65).read_to_string(&mut value)?;
        Ok(value)
    });
    parse_boot_identity(result.as_deref().map(str::as_bytes).ok())
}

#[cfg(target_os = "macos")]
fn read_boot_identity() -> Metric<Uuid> {
    let mut bytes = [0u8; 64];
    let mut length = bytes.len();
    // SAFETY: a fixed read-only sysctl, valid output buffer and length pointer;
    // no new value is supplied. Failure is Unknown, never a synthetic boot ID.
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    parse_boot_identity((result == 0 && length <= bytes.len()).then(|| &bytes[..length]))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_boot_identity() -> Metric<Uuid> {
    Metric::Unknown(UnknownReason::Unsupported)
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn parse_boot_identity(bytes: Option<&[u8]>) -> Metric<Uuid> {
    let value = bytes
        .filter(|value| value.len() <= 64)
        .and_then(|value| std::str::from_utf8(value).ok())
        .map(|value| value.trim_end_matches('\0').trim());
    match value
        .and_then(|value| Uuid::parse_str(value).ok())
        .filter(|value| !value.is_nil())
    {
        Some(value) => Metric::Known(value),
        None => Metric::Unknown(UnknownReason::Inaccessible),
    }
}

#[cfg(test)]
mod tests;
