//! Physical ownership, not a caller's assertion that a PID was reaped.
//!
//! The ledger retains this bounded custody independently of an async waiter.
//! Roots enter only through the real launcher; writers are real owned tasks.
//! Generic root waits/writer joins leave UnknownHeld. A private live-close path
//! combines original Windows Job extinction with audited local owners only;
//! restart-journal reconciliation and production composition remain required.

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
    RetainedRole,
};
use crate::process::observation::{Metric, ProcessIdentity, ProcessObserver, UnknownReason};
use crate::process::{spawn_job_custodied, ChildJob, ChildWork, OwnedSpawnFailure};

mod budgets;
#[cfg(target_os = "linux")]
pub(super) mod copy_vod;
pub(super) mod finite_pipes;
#[cfg(unix)]
pub(super) mod finite_vod;
#[cfg(target_os = "linux")]
pub(in crate::adaptive_resources) mod fixed_engine;
#[cfg(target_os = "linux")]
pub(in crate::adaptive_resources) mod fixed_probe;
mod live_close;
mod live_jobs;
mod source_files;
pub use source_files::GraphSourceFile;
mod pipe_drains;
pub(super) use pipe_drains::GraphPipeDrain;

const MAX_ROOTS: usize = 32;
const MAX_WRITERS: usize = 32;
static PROCESS_INSTANCE: LazyLock<Uuid> = LazyLock::new(Uuid::new_v4);
static BOOT_IDENTITY: LazyLock<Metric<Uuid>> = LazyLock::new(read_boot_identity);

/// The journal and every actual graph share this process instance and native
/// boot observation. Neither identity is an old-owner fence or close proof.
pub(super) fn daemon_identity() -> (Uuid, Metric<Uuid>) {
    (*PROCESS_INSTANCE, *BOOT_IDENTITY)
}

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
    control: Option<crate::process::priority::RegisteredChildControl>,
}

impl GraphRoot {
    pub(in crate::adaptive_resources) fn registered_control(
        self,
    ) -> Option<crate::process::priority::RegisteredChildControl> {
        self.control
    }
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
    job: Option<ChildJob>,
    process: Metric<ProcessIdentity>,
    waited: bool,
    exit_status: Option<std::process::ExitStatus>,
    pipes_claimed: bool,
}

struct Members {
    acquired: bool,
    // An incomplete launch may lack the actual child/job identity. Successful
    // observations of other roots must never conceal that acquisition window.
    uncertain_launch: bool,
    // Set BEFORE any arbitrary factory can receive pipes (including a panic
    // before task registration), or copied source bytes escape. Never cleared.
    unaudited_handoff: bool,
    // One actual graph can mint at most one nonclone physical receipt.
    receipt_issued: bool,
    finite_process_closed: bool,
    #[cfg(target_os = "linux")]
    copy_process_closed: bool,
    #[cfg(target_os = "linux")]
    copy_hold: Option<copy_vod::CopyTaskHoldReason>,
    roots: Vec<Root>,
    writers: Vec<Writer>,
    joined_writers: usize,
    abandoned_writers: usize,
    source_files: Vec<source_files::SourceFile>,
}

struct Writer {
    task: JoinHandle<()>,
    // Caller-supplied generic futures never receive an audited adapter record.
    drain: Option<pipe_drains::DrainRecord>,
    #[cfg(unix)]
    finite: Option<finite_vod::FiniteRecord>,
    #[cfg(target_os = "linux")]
    copy: Option<copy_vod::CopyRecord>,
    joined: bool,
}

impl Writer {
    fn is_concrete(&self) -> bool {
        if self.drain.is_some() {
            return true;
        }
        #[cfg(unix)]
        if self.finite.is_some() {
            return true;
        }
        #[cfg(target_os = "linux")]
        if self.copy.is_some() {
            return true;
        }
        false
    }
}

pub(super) struct GraphCustody {
    binding: ReservationBinding,
    identity: GraphIdentity,
    root_limit: usize,
    source_limit: usize,
    source_read_limit: usize,
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
    SourceCloseFailed,
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
    pub source_files: usize,
    pub closed_source_files: usize,
}

impl ResourceReservation {
    /// Move exclusive reservation custody into the real acquisition owner.
    /// There is no externally launched child/receipt/cleanup callback argument.
    /// Unsupported containment refuses speculation BEFORE any launch; normal
    /// acquisition remains conservative and can accumulate UnknownHeld.
    pub fn into_physical_graph(mut self) -> Result<OwnedPhysicalGraph, AdmissionRefusal> {
        // Boot lookup may perform bounded OS I/O. Never hold the ledger's
        // short accounting lock while doing that or initializing randomness.
        let (process_instance, os_boot) = daemon_identity();
        let identity = GraphIdentity {
            generation: Uuid::new_v4(),
            process_instance,
            os_boot,
        };
        let mut state = self
            .ledger
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if state.invalid || state.native_overage_fenced {
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
        let source_limit = usize::try_from(entry.resources.handles)
            .unwrap_or(usize::MAX)
            .min(source_files::MAX_SOURCE_FILES);
        let custody = Arc::new(GraphCustody {
            binding: self.binding,
            identity,
            root_limit: usize::try_from(entry.resources.processes)
                .unwrap_or(usize::MAX)
                .min(MAX_ROOTS),
            source_limit,
            // Every source may retain one completed/pending read after waiter
            // cancellation. Divide the buffer budget across the entire bounded
            // inventory, not just the caller currently holding the async lock.
            source_read_limit: usize::try_from(entry.resources.peak_memory_bytes)
                .unwrap_or(usize::MAX)
                / source_limit.max(1),
            sealed: AtomicBool::new(false),
            members: Mutex::new(Members {
                acquired: false,
                uncertain_launch: false,
                unaudited_handoff: false,
                receipt_issued: false,
                finite_process_closed: false,
                #[cfg(target_os = "linux")]
                copy_process_closed: false,
                #[cfg(target_os = "linux")]
                copy_hold: None,
                roots: Vec::new(),
                writers: Vec::new(),
                joined_writers: 0,
                abandoned_writers: 0,
                source_files: Vec::new(),
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

    pub fn retained_role(&self) -> RetainedRole<'_> {
        retained_role(self.custody())
    }

    /// Spawn directly under custody. Mark uncertainty BEFORE the shared launcher
    /// can partially succeed, and retain the actual Child and ChildJob together.
    pub async fn spawn_root(
        &mut self,
        command: &mut Command,
        work: ChildWork,
    ) -> io::Result<GraphRoot> {
        self.spawn_root_inner(command, work, None, None).await
    }

    pub(super) async fn spawn_durable_root(
        &mut self,
        command: &mut Command,
        work: ChildWork,
        arm: super::runtime::DurableArm,
    ) -> io::Result<GraphRoot> {
        self.spawn_root_inner(command, work, Some(arm), None).await
    }

    #[cfg(unix)]
    pub(super) async fn spawn_selected_durable_root(
        &mut self,
        command: &mut Command,
        work: ChildWork,
        arm: super::runtime::DurableArm,
        entered: &mut bool,
    ) -> io::Result<GraphRoot> {
        self.spawn_root_inner(command, work, Some(arm), Some(entered))
            .await
    }

    async fn spawn_root_inner(
        &mut self,
        command: &mut Command,
        work: ChildWork,
        arm: Option<super::runtime::DurableArm>,
        mut entered: Option<&mut bool>,
    ) -> io::Result<GraphRoot> {
        let custody = self.custody();
        let mut members = custody.members.lock().await;
        if custody.sealed.load(Ordering::Acquire) || members.roots.len() >= custody.root_limit {
            return Err(io::Error::other(
                "physical graph sealed or root bound reached",
            ));
        }
        budgets::check(
            &self.ledger,
            custody,
            &members,
            budgets::BudgetDelta {
                roots: 1,
                ..Default::default()
            },
        )?;
        if entered.as_deref().is_some_and(|entered| *entered) {
            return Err(io::Error::other("selected root launch was already entered"));
        }
        begin_acquisition_with_arm(&self.ledger, custody, &mut members, arm.as_ref())?;
        // Set in the same uninterrupted graph-lock scope as native entry. A
        // cancelled graph-lock waiter cannot falsely consume this launch slot.
        if let Some(entered) = entered.as_mut() {
            **entered = true;
        }
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
        members
            .roots
            .get(root.index)
            .filter(|member| {
                member.process == root.process && !member.pipes_claimed && !member.waited
            })
            .ok_or_else(|| io::Error::other("physical graph root unavailable for writer"))?;
        // A durable graph may create tasks only through its journal-backed
        // concrete task adapter, never this legacy public entry point.
        if self
            .ledger
            .inner
            .lock()
            .map_err(|_| io::Error::other("resource ledger unavailable"))?
            .entries
            .get(&custody.binding.reservation)
            .is_none_or(|entry| entry.durable.is_some())
        {
            return Err(io::Error::other("durable writer requires armed adapter"));
        }
        members.unaudited_handoff = true;
        let member = &mut members.roots[root.index];
        member.pipes_claimed = true;
        let pipes = RootPipes {
            stdin: member.child.stdin.take(),
            stdout: member.child.stdout.take(),
            stderr: member.child.stderr.take(),
        };
        // No await separates task creation from retaining its JoinHandle. If
        // factory construction panics, the root/reservation stay in custody.
        members.writers.push(Writer {
            task: runtime.spawn(writer(pipes)),
            drain: None,
            #[cfg(unix)]
            finite: None,
            #[cfg(target_os = "linux")]
            copy: None,
            joined: false,
        });
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
        control: job.as_ref().and_then(ChildJob::registered_control),
    };
    members.roots.push(Root {
        child,
        job,
        process,
        waited: false,
        exit_status: None,
        pipes_claimed: false,
    });
    root
}

fn retain_launch_failure(
    custody: &GraphCustody,
    members: &mut Members,
    failure: OwnedSpawnFailure,
) -> io::Error {
    members.uncertain_launch = true;
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
    begin_acquisition_with_arm(ledger, custody, members, None)
}

fn begin_acquisition_with_arm(
    ledger: &ResourceLedger,
    custody: &GraphCustody,
    members: &mut Members,
    arm: Option<&super::runtime::DurableArm>,
) -> io::Result<()> {
    let mut state = ledger
        .inner
        .lock()
        .map_err(|_| io::Error::other("resource ledger unavailable"))?;
    if state.invalid || state.native_overage_fenced {
        return Err(io::Error::other("resource ledger invalid"));
    }
    let epoch = state.epoch;
    let draining = state.draining;
    let entry = state
        .entries
        .get_mut(&custody.binding.reservation)
        .filter(|entry| entry.binding == custody.binding)
        .ok_or_else(|| io::Error::other("physical reservation unavailable"))?;
    if entry.durable.is_some()
        && (draining
            || epoch != entry.binding.allocation_epoch
            || !arm.is_some_and(|arm| arm.matches(custody.binding, custody.identity.generation)))
    {
        return Err(io::Error::other(
            "durable acquisition is not armed or current",
        ));
    }
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
    if entry.state == ReservationState::Reserved && entry.durable.is_none() {
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
    /// Another selection of the SAME retained custody, no descriptor duplication
    /// or second receipt. Used to release the runtime bundle lock before joins.
    pub(in crate::adaptive_resources) fn select_same(&self) -> Self {
        Self {
            ledger: self.ledger.clone(),
            custody: Arc::clone(&self.custody),
        }
    }

    pub fn retained_role(&self) -> RetainedRole<'_> {
        retained_role(&self.custody)
    }

    /// An exited leader does NOT close its job, writers or reservation.
    pub async fn try_wait_root(
        &self,
        root: GraphRoot,
    ) -> io::Result<Option<std::process::ExitStatus>> {
        let mut members = self.custody.members.lock().await;
        let member = root_member(&self.custody, &mut members, root)?;
        if let Some(status) = member.exit_status {
            return Ok(Some(status));
        }
        let status = member.child.try_wait()?;
        if status.is_some() {
            member.waited = true;
            member.exit_status = status;
        }
        Ok(status)
    }

    /// Signal under the actual un-reaped child handle's lock, not a supplied
    /// PID. Completion invalidates signalling even if the OS recycles its PID.
    pub async fn signal_root(
        &self,
        root: GraphRoot,
        signal: crate::process::ProcessSignal,
    ) -> io::Result<bool> {
        let mut members = self.custody.members.lock().await;
        let member = root_member(&self.custody, &mut members, root)?;
        let pid = member
            .child
            .id()
            .filter(|_| !member.waited)
            .ok_or_else(|| io::Error::other("physical root already waited"))?;
        crate::process::signal(pid, signal)
    }

    pub async fn start_kill_root(&self, root: GraphRoot) -> io::Result<()> {
        let mut members = self.custody.members.lock().await;
        let member = root_member(&self.custody, &mut members, root)?;
        if member.waited {
            return Ok(());
        }
        member.child.start_kill()
    }

    /// Narrow producer-root wait. Custody retains child/job and all charges.
    pub async fn wait_root(&self, root: GraphRoot) -> io::Result<std::process::ExitStatus> {
        let mut members = self.custody.members.lock().await;
        let member = root_member(&self.custody, &mut members, root)?;
        if let Some(status) = member.exit_status {
            return Ok(status);
        }
        let status = member.child.wait().await?;
        member.waited = true;
        member.exit_status = Some(status);
        Ok(status)
    }

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
            source_files: members.source_files.len(),
            closed_source_files: members
                .source_files
                .iter()
                .filter(|source| source.closed())
                .count(),
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
        // Terminate ORIGINAL retained Jobs, including jobs whose leader already
        // exited. The request is not extinction proof. Unsupported Unix jobs
        // retain the existing root-only cleanup and UnknownHeld semantics.
        live_jobs::request_termination(&members);
        // Concrete drains own only pipes, no output flush or external work.
        // Joining their aborted futures closes those handles even if an
        // uncontained descendant keeps its other pipe end open.
        for writer in &members.writers {
            if writer.is_concrete() && !writer.joined {
                writer.task.abort();
            }
        }
        // Request all roots' termination before awaiting any of them. Their
        // real retained handles, not recycled numeric PIDs, select the targets.
        for root in &mut members.roots {
            if !root.waited {
                let _ = root.child.start_kill();
            }
        }
        for root in &mut members.roots {
            if !root.waited {
                let status = match root.child.wait().await {
                    Ok(status) => status,
                    Err(_) => {
                        return GraphCloseOutcome::UnknownHeld(GraphHoldReason::RootWaitFailed)
                    }
                };
                root.waited = true;
                root.exit_status = Some(status);
            }
        }
        for index in 0..members.writers.len() {
            let writer = &mut members.writers[index];
            if writer.joined {
                continue;
            }
            let abandoned = (&mut writer.task).await.is_err();
            writer.joined = true;
            #[cfg(unix)]
            if let Some(finite) = writer.finite.as_mut() {
                finite.note_join(abandoned);
            }
            #[cfg(target_os = "linux")]
            if let Some(copy) = writer.copy.as_mut() {
                copy.joined_abandoned = abandoned;
            }
            members.joined_writers += 1;
            members.abandoned_writers += usize::from(abandoned);
        }
        #[cfg(unix)]
        for writer in &members.writers {
            if let Some(finite) = &writer.finite {
                if finite.close_joined_pipes().is_err() {
                    return GraphCloseOutcome::UnknownHeld(GraphHoldReason::SourceCloseFailed);
                }
            }
            #[cfg(target_os = "linux")]
            if let Some(copy) = &writer.copy {
                if copy
                    .pipes
                    .iter()
                    .any(|pipe| pipe.close_after_reader_drop().is_err())
                {
                    return GraphCloseOutcome::UnknownHeld(GraphHoldReason::SourceCloseFailed);
                }
            }
        }
        for source in &mut members.source_files {
            if source.close().await.is_err() {
                return GraphCloseOutcome::UnknownHeld(GraphHoldReason::SourceCloseFailed);
            }
        }
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    }
}

fn retained_role(custody: &GraphCustody) -> RetainedRole<'_> {
    RetainedRole {
        binding: custody.binding,
        owner: std::marker::PhantomData,
    }
}

fn root_member<'a>(
    custody: &GraphCustody,
    members: &'a mut Members,
    root: GraphRoot,
) -> io::Result<&'a mut Root> {
    if root.generation != custody.identity.generation {
        return Err(io::Error::other("root belongs to another physical graph"));
    }
    members
        .roots
        .get_mut(root.index)
        .filter(|member| member.process == root.process)
        .ok_or_else(|| io::Error::other("physical graph root unavailable"))
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

// Heap-only M graphs have no native members. Refuse other graphs rather than
// presenting an incomplete byte count for nested live job/pipe allocations.
#[cfg(target_os = "linux")]
fn empty_graph_heap_bytes(custody: &GraphCustody) -> Option<usize> {
    let members = custody.members.try_lock().ok()?;
    if !members.roots.is_empty() || !members.writers.is_empty() || !members.source_files.is_empty()
    {
        return None;
    }
    std::mem::size_of::<GraphCustody>()
        .checked_add(2 * std::mem::size_of::<usize>())?
        .checked_add(
            members
                .roots
                .capacity()
                .checked_mul(std::mem::size_of::<Root>())?,
        )?
        .checked_add(
            members
                .writers
                .capacity()
                .checked_mul(std::mem::size_of::<Writer>())?,
        )?
        .checked_add(
            members
                .source_files
                .capacity()
                .checked_mul(std::mem::size_of::<source_files::SourceFile>())?,
        )
}
/// Requested capacity of one graph representation with no member and no
/// member-vector capacity: the shared custody plus its Arc header. A retained
/// empty graph must not exceed this (`retained_empty_heap_bytes`).
#[cfg(target_os = "linux")]
pub(in crate::adaptive_resources) fn empty_graph_structural_bytes() -> Option<usize> {
    std::mem::size_of::<GraphCustody>().checked_add(2 * std::mem::size_of::<usize>())
}
#[cfg(target_os = "linux")]
impl OwnedPhysicalGraph {
    pub(in crate::adaptive_resources) fn retained_empty_heap_bytes(&self) -> Option<usize> {
        empty_graph_heap_bytes(self.custody.as_deref()?)
    }
}
#[cfg(target_os = "linux")]
impl SealedPhysicalGraph {
    pub(in crate::adaptive_resources) fn retained_empty_heap_bytes(&self) -> Option<usize> {
        empty_graph_heap_bytes(&self.custody)
    }
}
