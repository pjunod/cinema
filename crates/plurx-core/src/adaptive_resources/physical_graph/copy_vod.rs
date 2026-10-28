//! Fixed one-input rolling copy root and parser. The graph keeps the original
//! child pipes and parser JoinHandle; parser outcomes alone cannot close a
//! root, source, scratch member or ledger reservation.
use super::*;
use crate::adaptive_resources::runtime::copy_vod::CopySink;
use crate::copy_segmenter::{self, Limits, Outcome, PreparedCopyParser};
use crate::domain::MediaFile;
use crate::source_identity::SourceIdentity;
use crate::transcode::CopyVideoOptions;
use std::sync::Mutex as StdMutex;
use tokio::io::AsyncReadExt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::adaptive_resources) struct GraphCopyTask {
    generation: Uuid,
    member: Uuid,
    index: usize,
    root: usize,
}

/// Nonserde fixed-image fact from the actual verified executable SourceV1.
/// This is negative binding until the original root/task are joined.
#[must_use]
pub(in crate::adaptive_resources) struct CopyFixedImage {
    root_member: Uuid,
    task_member: Uuid,
    acquisition_id: Uuid,
    close_operation_id: Uuid,
    identity: SourceIdentity,
    sha256: [u8; 32],
}
impl CopyFixedImage {
    pub(in crate::adaptive_resources) fn from_verified_source(
        root_member: Uuid,
        task_member: Uuid,
        acquisition_id: Uuid,
        identity: SourceIdentity,
        sha256: [u8; 32],
        verified_elf: bool,
    ) -> io::Result<Self> {
        if !cfg!(target_os = "linux")
            || !verified_elf
            || root_member.is_nil()
            || task_member.is_nil()
            || acquisition_id.is_nil()
            || root_member == task_member
            || identity.size == 0
            || identity.size > 512 * 1024 * 1024
        {
            return Err(io::Error::other("unverified fixed rolling copy image"));
        }
        Ok(Self {
            root_member,
            task_member,
            acquisition_id,
            close_operation_id: Uuid::new_v4(),
            identity,
            sha256,
        })
    }
}

/// Original Linux copy root and parser closure. This remains a process-only
/// fact until the same source cells and ScratchAssetsV3 member close.
#[must_use]
pub(in crate::adaptive_resources) struct CopyProcessClosure {
    binding: ReservationBinding,
    generation: Uuid,
    root: GraphRoot,
    root_member: Uuid,
    acquisition_id: Uuid,
    close_operation_id: Uuid,
    task: GraphCopyTask,
    executable_identity: SourceIdentity,
    executable_sha256: [u8; 32],
    terminal: CopyTaskTerminal,
    start_disposition: CopyStartDisposition,
}
impl CopyProcessClosure {
    pub(in crate::adaptive_resources) fn matches(&self, graph: &SealedPhysicalGraph) -> bool {
        self.binding == graph.custody.binding
            && self.generation == graph.custody.identity.generation
            && graph.custody.sealed.load(Ordering::Acquire)
    }
    pub(in crate::adaptive_resources) fn root(&self) -> GraphRoot {
        self.root
    }
    pub(in crate::adaptive_resources) fn generation(&self) -> Uuid {
        self.generation
    }
    pub(in crate::adaptive_resources) fn root_member(&self) -> Uuid {
        self.root_member
    }
    pub(in crate::adaptive_resources) fn acquisition_id(&self) -> Uuid {
        self.acquisition_id
    }
    pub(in crate::adaptive_resources) fn close_operation_id(&self) -> Uuid {
        self.close_operation_id
    }
    pub(in crate::adaptive_resources) fn task(&self) -> GraphCopyTask {
        self.task
    }
    pub(in crate::adaptive_resources) fn executable_identity(&self) -> SourceIdentity {
        self.executable_identity
    }
    pub(in crate::adaptive_resources) fn executable_sha256(&self) -> [u8; 32] {
        self.executable_sha256
    }
    pub(in crate::adaptive_resources) fn terminal(&self) -> CopyTaskTerminal {
        self.terminal
    }
    pub(in crate::adaptive_resources) fn start_disposition(&self) -> CopyStartDisposition {
        self.start_disposition
    }
}

/// Native-only closure for the original retained root before parser
/// registration. No task/reader/buffer acquisition is asserted by this value.
#[must_use]
pub(in crate::adaptive_resources) struct CopyRootOnlyClosure {
    binding: ReservationBinding,
    generation: Uuid,
    root: GraphRoot,
    root_member: Uuid,
    task_member: Uuid,
    acquisition_id: Uuid,
    close_operation_id: Uuid,
    executable_identity: SourceIdentity,
    executable_sha256: [u8; 32],
}
impl CopyRootOnlyClosure {
    pub(in crate::adaptive_resources) fn matches(&self, graph: &SealedPhysicalGraph) -> bool {
        self.binding == graph.custody.binding
            && self.generation == graph.custody.identity.generation
            && graph.custody.sealed.load(Ordering::Acquire)
    }
    pub(in crate::adaptive_resources) fn root(&self) -> GraphRoot {
        self.root
    }
    pub(in crate::adaptive_resources) fn generation(&self) -> Uuid {
        self.generation
    }
    pub(in crate::adaptive_resources) fn root_member(&self) -> Uuid {
        self.root_member
    }
    pub(in crate::adaptive_resources) fn task_member(&self) -> Uuid {
        self.task_member
    }
    pub(in crate::adaptive_resources) fn acquisition_id(&self) -> Uuid {
        self.acquisition_id
    }
    pub(in crate::adaptive_resources) fn close_operation_id(&self) -> Uuid {
        self.close_operation_id
    }
    pub(in crate::adaptive_resources) fn executable_identity(&self) -> SourceIdentity {
        self.executable_identity
    }
    pub(in crate::adaptive_resources) fn executable_sha256(&self) -> [u8; 32] {
        self.executable_sha256
    }
}

/// Internal entry distinction; neither alternative is a physical close proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::adaptive_resources) enum CopyTaskTerminal {
    NeverEntered,
    Entered(CopyTaskSummary),
}
/// Decided exactly once under sealed graph membership before natural join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::adaptive_resources) enum CopyStartDisposition {
    DroppedUnsent,
    TakenBeforeSeal,
}
/// Native uncertainty. Failed kill requests are retryable advice while the
/// original handles remain; every other reason is sticky. None is close proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::adaptive_resources) enum CopyTaskHoldReason {
    TaskAbandoned,
    TerminalPoisoned,
    TerminalMissing,
    StartDispositionMismatch,
    PipeCloseFailed,
    FailureKillUnavailable,
    FailureKillFailed,
}
impl CopyTaskHoldReason {
    fn permanent(self) -> bool {
        !matches!(self, Self::FailureKillUnavailable | Self::FailureKillFailed)
    }
}
#[derive(Default)]
struct CopyTaskState {
    terminal: Option<CopyTaskTerminal>,
    hold: Option<CopyTaskHoldReason>,
}

pub(in crate::adaptive_resources) struct CopyWork {
    pub sink: CopySink,
    pub source: MediaFile,
    pub video: CopyVideoOptions,
    pub limits: Limits,
    pub diagnostic_id: String,
    pub parser: PreparedCopyParser,
}
impl CopyWork {
    pub(in crate::adaptive_resources) fn known_memory_bytes(&self) -> io::Result<u64> {
        let bytes = std::mem::size_of::<Self>()
            .checked_add(self.sink.reserved_memory_bytes())
            .and_then(|n| n.checked_add(self.source.requested_heap_bytes()?))
            .and_then(|n| n.checked_add(self.diagnostic_id.capacity()))
            .and_then(|n| n.checked_add(self.parser.charged_bytes()))
            .and_then(|n| n.checked_add(8192)) // fixed stderr read buffer
            .ok_or_else(|| io::Error::other("copy task known memory overflow"))?;
        u64::try_from(bytes).map_err(io::Error::other)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::adaptive_resources) struct CopyTaskSummary {
    pub completed: bool,
    pub segments: u64,
}

pub(in crate::adaptive_resources) struct CopyResultWaiter {
    finished: Arc<StdMutex<CopyTaskState>>,
    completion: Arc<tokio::sync::Notify>,
}
impl CopyResultWaiter {
    pub(in crate::adaptive_resources) async fn wait(
        self,
        deadline: std::time::Instant,
    ) -> io::Result<CopyTaskTerminal> {
        loop {
            let notified = self.completion.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self
                    .finished
                    .lock()
                    .map_err(|_| io::Error::other("copy result slot poisoned"))?;
                if state.hold.is_some() {
                    return Err(io::Error::other("copy task native uncertainty held"));
                }
                if let Some(terminal) = state.terminal {
                    return Ok(terminal);
                }
            }
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), notified)
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "original copy parser has not returned",
                    )
                })?;
        }
    }
}

pub(super) struct CopyRecord {
    selection: GraphCopyTask,
    pub(super) pipes: [super::finite_pipes::PipeRecord; 2],
    finished: Arc<StdMutex<CopyTaskState>>,
    completion: Arc<tokio::sync::Notify>,
    image: CopyFixedImage,
    minimum_memory: u64,
    start: Option<tokio::sync::oneshot::Sender<()>>,
    start_disposition: Option<CopyStartDisposition>,
    pub(super) joined_abandoned: bool,
}
impl CopyRecord {
    pub(super) fn minimum_memory_bytes(&self) -> u64 {
        self.minimum_memory
    }
}

/// Fixed original parser future, pipe records and result-cell allocation.
/// Variable source/parser/sink payloads are added by CopyWork separately.
pub(in crate::adaptive_resources) fn copy_task_structural_bytes() -> Option<usize> {
    std::mem::size_of_val(&original_copy_task(None, None, None, None, None, None))
        .checked_add(super::finite_pipes::resident_memory_bytes().ok()?)?
        .checked_add(std::mem::size_of::<CopyRecord>())?
        .checked_add(std::mem::size_of::<StdMutex<CopyTaskState>>())?
        .checked_add(std::mem::size_of::<tokio::sync::Notify>())?
        .checked_add(4 * std::mem::size_of::<usize>())
}

async fn drain_diagnostics(mut reader: super::finite_pipes::FinitePipeReader) -> io::Result<()> {
    let mut buffer = [0u8; 8192];
    loop {
        if reader.read(&mut buffer).await? == 0 {
            return Ok(());
        }
    }
}

fn publish_copy_terminal(
    finished: &StdMutex<CopyTaskState>,
    completion: &tokio::sync::Notify,
    terminal: CopyTaskTerminal,
) {
    if let Ok(mut state) = finished.lock() {
        state.terminal = Some(terminal);
    }
    // A poisoned slot remains Unknown, but every observer wakes to see it.
    completion.notify_waiters();
}
fn copy_task_hold(finished: &StdMutex<CopyTaskState>) -> Option<CopyTaskHoldReason> {
    match finished.lock() {
        Ok(state) => state.hold,
        Err(_) => Some(CopyTaskHoldReason::TerminalPoisoned),
    }
}
fn request_failed_copy_stop(
    control: Option<crate::process::priority::RegisteredChildControl>,
    finished: &StdMutex<CopyTaskState>,
    completion: &tokio::sync::Notify,
) {
    // SAME retained registration key, never a PID reconstructed from data.
    // Signal success is only a request; normal join/wait/checked closes remain
    // necessary. Failure wakes a waiter so it can release the members lock.
    let held = match control {
        Some(control) => control
            .start_kill()
            .err()
            .map(|_| CopyTaskHoldReason::FailureKillFailed),
        None => Some(CopyTaskHoldReason::FailureKillUnavailable),
    };
    if let Some(held) = held {
        if let Ok(mut state) = finished.lock() {
            state.hold.get_or_insert(held);
        }
        completion.notify_waiters();
    }
}

async fn original_copy_task(
    pipes: Option<super::finite_pipes::FiniteRootPipes>,
    work: Option<CopyWork>,
    start: Option<tokio::sync::oneshot::Receiver<()>>,
    finished: Option<Arc<StdMutex<CopyTaskState>>>,
    completion: Option<Arc<tokio::sync::Notify>>,
    control: Option<crate::process::priority::RegisteredChildControl>,
) {
    let (Some(pipes), Some(mut work), Some(start), Some(finished), Some(completion)) =
        (pipes, work, start, finished, completion)
    else {
        return;
    };
    // First await: only destructuring precedes the one original start latch.
    if start.await.is_err() {
        drop(pipes);
        drop(work);
        publish_copy_terminal(&finished, &completion, CopyTaskTerminal::NeverEntered);
        return;
    }
    // The task and sink MUST NOT acquire graph.members: join retains that
    // lock while this SAME task finishes. The sink uses the bundle lock;
    // the runtime driver must release it before joining (lock-order contract).
    let parser = async {
        let outcome = copy_segmenter::run_copy_pipe_prepared(
            pipes.stdout,
            &mut work.sink,
            &work.diagnostic_id,
            work.limits,
            &work.source,
            work.video,
            work.parser,
        )
        .await;
        if !matches!(&outcome, Outcome::Completed(_)) {
            // Before awaiting diagnostics: a live root may ignore EPIPE and
            // keep stderr open forever. Post-task-join kill cannot solve it.
            request_failed_copy_stop(control, &finished, &completion);
        }
        outcome
    };
    let diagnostics = async {
        let result = drain_diagnostics(pipes.stderr).await;
        if result.is_err() {
            request_failed_copy_stop(control, &finished, &completion);
        }
        result
    };
    let (result, diagnostics) = tokio::join!(parser, diagnostics);
    let summary = match result {
        Outcome::Completed(counts) if diagnostics.is_ok() => CopyTaskSummary {
            completed: true,
            segments: counts.segments,
        },
        _ => CopyTaskSummary {
            completed: false,
            segments: 0,
        },
    };
    publish_copy_terminal(&finished, &completion, CopyTaskTerminal::Entered(summary));
}

impl OwnedPhysicalGraph {
    /// Observe the original parser's terminal slot without joining it or
    /// claiming root, source or scratch closure. A panic leaves no result and
    /// therefore no successful completion signal; the registered task still
    /// belongs to the charged graph for its exact close path.
    pub(in crate::adaptive_resources) async fn copy_result_waiter(
        &self,
        task: GraphCopyTask,
    ) -> io::Result<CopyResultWaiter> {
        {
            let members = self.custody().members.lock().await;
            if self.custody().sealed.load(Ordering::Acquire)
                || task.generation != self.identity().generation
            {
                return Err(io::Error::other("copy result belongs to another graph"));
            }
            let record = members
                .writers
                .get(task.index)
                .and_then(|writer| writer.copy.as_ref())
                .filter(|record| record.selection == task)
                .ok_or_else(|| io::Error::other("copy parser is not registered"))?;
            Ok(CopyResultWaiter {
                finished: Arc::clone(&record.finished),
                completion: Arc::clone(&record.completion),
            })
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Original root/task/acquisition IDs, durable arm, parser work and fixed image are validated and transferred atomically; retain the explicit original owners instead of an additional staged owner."
    )]
    pub(in crate::adaptive_resources) async fn spawn_durable_copy_vod(
        &mut self,
        root: GraphRoot,
        root_member_id: Uuid,
        member: Uuid,
        acquisition_id: Uuid,
        arm: super::super::runtime::DurableArm,
        work: &mut Option<CopyWork>,
        image: &mut Option<CopyFixedImage>,
    ) -> io::Result<GraphCopyTask> {
        let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        let custody = self.custody();
        let mut members = custody.members.lock().await;
        if custody.sealed.load(Ordering::Acquire)
            || members.writers.len() >= MAX_WRITERS
            || member.is_nil()
            || image.as_ref().is_none_or(|image| {
                image.root_member != root_member_id
                    || image.task_member != member
                    || image.acquisition_id != acquisition_id
            })
            || root.generation != custody.identity.generation
            || members.writers.iter().any(|writer| {
                writer
                    .copy
                    .as_ref()
                    .is_some_and(|record| record.selection.member == member)
            })
        {
            return Err(io::Error::other("copy task unavailable or already owned"));
        }
        let minimum_memory = work
            .as_ref()
            .ok_or_else(|| io::Error::other("copy work already entered"))?
            .known_memory_bytes()?
            .checked_add(
                u64::try_from(
                    copy_task_structural_bytes()
                        .ok_or_else(|| io::Error::other("copy task structural memory overflow"))?,
                )
                .map_err(io::Error::other)?,
            )
            .ok_or_else(|| io::Error::other("copy task memory overflow"))?;
        budgets::check(
            &self.ledger,
            custody,
            &members,
            budgets::BudgetDelta {
                finite_memory_bytes: minimum_memory,
                ..Default::default()
            },
        )?;
        root_member(custody, &mut members, root)?;
        let owned = &members.roots[root.index];
        if owned.pipes_claimed
            || owned.waited
            || owned.child.stdout.is_none()
            || owned.child.stderr.is_none()
        {
            return Err(io::Error::other("copy root pipes unavailable"));
        }
        begin_acquisition_with_arm(&self.ledger, custody, &mut members, Some(&arm))?;
        let owned = &mut members.roots[root.index];
        owned.pipes_claimed = true;
        drop(owned.child.stdin.take());
        let (pipes, readers) = super::finite_pipes::prepare(
            owned.child.stdout.take().expect("validated stdout"),
            owned.child.stderr.take().expect("validated stderr"),
        );
        let selection = GraphCopyTask {
            generation: custody.identity.generation,
            member,
            index: members.writers.len(),
            root: root.index,
        };
        let finished = Arc::new(StdMutex::new(CopyTaskState::default()));
        let completion = Arc::new(tokio::sync::Notify::new());
        let (start, registered) = tokio::sync::oneshot::channel();
        let task = runtime.spawn(original_copy_task(
            Some(readers),
            work.take(),
            Some(registered),
            Some(Arc::clone(&finished)),
            Some(Arc::clone(&completion)),
            root.registered_control(),
        ));
        members.writers.push(Writer {
            task,
            drain: None,
            finite: None,
            copy: Some(CopyRecord {
                selection,
                pipes,
                finished,
                completion,
                image: image.take().expect("validated fixed copy image"),
                minimum_memory,
                start: Some(start),
                start_disposition: None,
                joined_abandoned: false,
            }),
            joined: false,
        });
        Ok(selection)
    }

    pub(in crate::adaptive_resources) async fn take_copy_start(
        &self,
        task: GraphCopyTask,
    ) -> io::Result<tokio::sync::oneshot::Sender<()>> {
        let mut members = self.custody().members.lock().await;
        if self.custody().sealed.load(Ordering::Acquire)
            || task.generation != self.identity().generation
        {
            return Err(io::Error::other("copy entry sealed"));
        }
        members
            .writers
            .get_mut(task.index)
            .and_then(|writer| writer.copy.as_mut())
            .filter(|record| record.selection == task)
            .and_then(|record| record.start.take())
            .ok_or_else(|| io::Error::other("copy start mismatched or already decided"))
    }
}

impl SealedPhysicalGraph {
    pub(in crate::adaptive_resources) async fn copy_hold_reason(
        &self,
    ) -> Option<CopyTaskHoldReason> {
        self.custody.members.lock().await.copy_hold
    }
    pub(in crate::adaptive_resources) async fn copy_task_hold_reason(
        &self,
        task: GraphCopyTask,
    ) -> Option<CopyTaskHoldReason> {
        let members = self.custody.members.lock().await;
        members.copy_hold.or_else(|| {
            members
                .writers
                .get(task.index)
                .and_then(|writer| writer.copy.as_ref())
                .filter(|record| record.selection == task)
                .and_then(|record| copy_task_hold(&record.finished))
        })
    }
    pub(in crate::adaptive_resources) async fn join_copy_vod(
        &self,
        task: GraphCopyTask,
    ) -> io::Result<CopyTaskTerminal> {
        let mut members = self.custody.members.lock().await;
        if !self.custody.sealed.load(Ordering::Acquire)
            || task.generation != self.custody.identity.generation
        {
            return Err(io::Error::other("copy join requires original sealed graph"));
        }
        if members.copy_hold.is_some() {
            return Err(io::Error::other("copy native uncertainty remains held"));
        }
        let control = members
            .roots
            .get(task.root)
            .and_then(|root| root.job.as_ref())
            .and_then(ChildJob::registered_control);
        let result = async {
            let writer = members
                .writers
                .get_mut(task.index)
                .filter(|writer| {
                    writer
                        .copy
                        .as_ref()
                        .is_some_and(|record| record.selection == task)
                })
                .ok_or(CopyTaskHoldReason::StartDispositionMismatch)?;
            let record = writer.copy.as_mut().expect("validated copy record");
            if record.start_disposition.is_none() {
                record.start_disposition = Some(if let Some(unsent) = record.start.take() {
                    drop(unsent);
                    CopyStartDisposition::DroppedUnsent
                } else {
                    CopyStartDisposition::TakenBeforeSeal
                });
            } else if record.start.is_some() {
                return Err(CopyTaskHoldReason::StartDispositionMismatch);
            }
            let newly_joined = !writer.joined;
            if newly_joined {
                loop {
                    let changed = record.completion.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if let Some(held) = copy_task_hold(&record.finished) {
                        // A failed signal is not lost custody. Let a completed
                        // original task join; otherwise release members so
                        // the caller can kill/wait the SAME retained root.
                        if held.permanent() || !writer.task.is_finished() {
                            return Err(held);
                        }
                    }
                    tokio::select! {
                        joined = &mut writer.task => {
                            writer.joined = true;
                            record.joined_abandoned = joined.is_err();
                            break;
                        },
                        _ = changed => {},
                    }
                }
            }
            Ok((newly_joined, record.joined_abandoned))
        }
        .await;
        let (newly_joined, abandoned) = match result {
            Ok(joined) => joined,
            Err(held) => {
                if held.permanent() {
                    members.copy_hold = Some(held);
                }
                return Err(io::Error::other("copy native uncertainty held"));
            }
        };
        if newly_joined {
            members.joined_writers += 1;
            members.abandoned_writers += usize::from(abandoned);
        }
        if abandoned {
            if let Some(control) = control {
                let _ = control.start_kill();
            }
            members.copy_hold = Some(CopyTaskHoldReason::TaskAbandoned);
            return Err(io::Error::other("original copy task abandoned"));
        }
        // Only a successful join of the original task resolves a failed
        // signal advisory. Poison/lost terminal/checked-close failures remain
        // held; no signal result or leader observation clears those.
        if let Ok(mut state) = members.writers[task.index]
            .copy
            .as_ref()
            .expect("validated copy record")
            .finished
            .lock()
        {
            if state.hold.is_some_and(|held| !held.permanent()) {
                state.hold = None;
            }
        }
        if let Some(held) = copy_task_hold(
            &members.writers[task.index]
                .copy
                .as_ref()
                .expect("validated copy record")
                .finished,
        ) {
            members.copy_hold = Some(held);
            return Err(io::Error::other("copy native uncertainty held"));
        }
        let record = members.writers[task.index]
            .copy
            .as_ref()
            .expect("validated copy record");
        for pipe in &record.pipes {
            if let Err(error) = pipe.close_after_reader_drop() {
                members.copy_hold = Some(CopyTaskHoldReason::PipeCloseFailed);
                return Err(error);
            }
        }
        let terminal = match record.finished.lock() {
            Ok(state) => state.terminal.ok_or(CopyTaskHoldReason::TerminalMissing),
            Err(_) => Err(CopyTaskHoldReason::TerminalPoisoned),
        };
        match terminal {
            Ok(terminal) => Ok(terminal),
            Err(held) => {
                if let Some(control) = control {
                    let _ = control.start_kill();
                }
                members.copy_hold = Some(held);
                Err(io::Error::other("copy terminal uncertainty held"))
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "The original root, member/acquisition identities and fixed image remain explicit move-only native custody; no new staged owner allocation."
    )]
    pub(in crate::adaptive_resources) async fn try_copy_root_only_closure(
        &self,
        root: GraphRoot,
        root_member_id: Uuid,
        task_member_id: Uuid,
        acquisition_id: Uuid,
        image: &mut Option<CopyFixedImage>,
    ) -> io::Result<CopyRootOnlyClosure> {
        let mut members = self.custody.members.lock().await;
        // A runtime task-selector loss is not registration absence. Keep
        // every owner charged and make the mismatch a typed sticky hold.
        if !members.writers.is_empty() {
            members.copy_hold = Some(CopyTaskHoldReason::StartDispositionMismatch);
            return Err(io::Error::other(
                "registered copy writer cannot be root-only",
            ));
        }
        if !cfg!(target_os = "linux")
            || !self.custody.sealed.load(Ordering::Acquire)
            || !members.acquired
            || members.uncertain_launch
            || members.unaudited_handoff
            || members.copy_process_closed
            || members.copy_hold.is_some()
            || members.roots.len() != 1
            || !members.writers.is_empty()
            || root.index != 0
            || root.generation != self.custody.identity.generation
            || image.as_ref().is_none_or(|image| {
                image.root_member != root_member_id
                    || image.task_member != task_member_id
                    || image.acquisition_id != acquisition_id
            })
        {
            return Err(io::Error::other(
                "fixed copy root-only inventory is incomplete",
            ));
        }
        let owned = root_member(&self.custody, &mut members, root)?;
        if !owned.waited
            || owned.exit_status.is_none()
            || owned.job.is_none()
            || owned.pipes_claimed
            || owned.child.stdin.is_some()
            || owned.child.stdout.is_none()
            || owned.child.stderr.is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "original copy root or unclaimed pipes remain unresolved",
            ));
        }
        let stdout = owned
            .child
            .stdout
            .take()
            .expect("validated original stdout");
        let stderr = owned
            .child
            .stderr
            .take()
            .expect("validated original stderr");
        let stdout_close = super::finite_pipes::close_unclaimed_stdout(stdout);
        let stderr_close = super::finite_pipes::close_unclaimed_stderr(stderr);
        if let Err(error) = stdout_close.and(stderr_close) {
            members.copy_hold = Some(CopyTaskHoldReason::PipeCloseFailed);
            return Err(error);
        }
        let image = image.take().expect("validated original fixed copy image");
        let closure = CopyRootOnlyClosure {
            binding: self.custody.binding,
            generation: self.custody.identity.generation,
            root,
            root_member: image.root_member,
            task_member: image.task_member,
            acquisition_id: image.acquisition_id,
            close_operation_id: image.close_operation_id,
            executable_identity: image.identity,
            executable_sha256: image.sha256,
        };
        members.roots.clear();
        members.copy_process_closed = true;
        Ok(closure)
    }

    pub(in crate::adaptive_resources) async fn try_copy_process_closure(
        &self,
        task: GraphCopyTask,
    ) -> io::Result<CopyProcessClosure> {
        let mut members = self.custody.members.lock().await;
        if !cfg!(target_os = "linux")
            || !self.custody.sealed.load(Ordering::Acquire)
            || !members.acquired
            || members.uncertain_launch
            || members.unaudited_handoff
            || members.copy_process_closed
            || members.copy_hold.is_some()
            || members.roots.len() != 1
            || members.writers.len() != 1
            || task.generation != self.custody.identity.generation
            || task.root != 0
            || task.index != 0
        {
            return Err(io::Error::other("fixed copy graph inventory is incomplete"));
        }
        let root = &members.roots[0];
        if !root.waited
            || root.exit_status.is_none()
            || root.job.is_none()
            || root.child.stdin.is_some()
            || root.child.stdout.is_some()
            || root.child.stderr.is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "original copy root remains live",
            ));
        }
        let selected_root = GraphRoot {
            generation: self.custody.identity.generation,
            index: 0,
            process: root.process,
            control: root.job.as_ref().and_then(ChildJob::registered_control),
        };
        let writer = &members.writers[0];
        let record = writer
            .copy
            .as_ref()
            .ok_or_else(|| io::Error::other("copy writer missing"))?;
        if !writer.joined || writer.drain.is_some() || record.selection != task {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "original copy parser remains live",
            ));
        }
        let checked =
            (|| -> Result<(CopyTaskTerminal, CopyStartDisposition), CopyTaskHoldReason> {
                for pipe in &record.pipes {
                    pipe.close_after_reader_drop()
                        .map_err(|_| CopyTaskHoldReason::PipeCloseFailed)?;
                }
                if record.joined_abandoned {
                    return Err(CopyTaskHoldReason::TaskAbandoned);
                }
                let state = record
                    .finished
                    .lock()
                    .map_err(|_| CopyTaskHoldReason::TerminalPoisoned)?;
                if let Some(held) = state.hold {
                    return Err(held);
                }
                let terminal = state.terminal.ok_or(CopyTaskHoldReason::TerminalMissing)?;
                let disposition = record
                    .start_disposition
                    .ok_or(CopyTaskHoldReason::StartDispositionMismatch)?;
                if matches!(terminal, CopyTaskTerminal::Entered(_))
                    && disposition != CopyStartDisposition::TakenBeforeSeal
                {
                    return Err(CopyTaskHoldReason::StartDispositionMismatch);
                }
                Ok((terminal, disposition))
            })();
        let (terminal, start_disposition) = match checked {
            Ok(result) => result,
            Err(held) => {
                members.copy_hold = Some(held);
                return Err(io::Error::other("copy process closure uncertainty held"));
            }
        };
        let record = members.writers[0]
            .copy
            .as_ref()
            .expect("validated copy record");
        let image = &record.image;
        let closure = CopyProcessClosure {
            binding: self.custody.binding,
            generation: self.custody.identity.generation,
            root: selected_root,
            root_member: image.root_member,
            acquisition_id: image.acquisition_id,
            close_operation_id: image.close_operation_id,
            task,
            executable_identity: image.identity,
            executable_sha256: image.sha256,
            terminal,
            start_disposition,
        };
        members.roots.clear();
        members.copy_process_closed = true;
        Ok(closure)
    }
}

// Fixture-only contention capability: it can hold the original membership
// mutex, but cannot seal the graph, take a latch or manufacture native facts.
#[cfg(all(test, feature = "fixtures"))]
pub(in crate::adaptive_resources) struct CopyMembersFixture {
    custody: Arc<GraphCustody>,
}
#[cfg(all(test, feature = "fixtures"))]
impl OwnedPhysicalGraph {
    pub(in crate::adaptive_resources) fn fixture_copy_members(&self) -> CopyMembersFixture {
        CopyMembersFixture {
            custody: Arc::clone(self.custody()),
        }
    }
}
#[cfg(all(test, feature = "fixtures"))]
impl CopyMembersFixture {
    pub(in crate::adaptive_resources) async fn hold(
        &self,
        entered: &tokio::sync::Notify,
        release: &tokio::sync::Notify,
    ) {
        let _members = self.custody.members.lock().await;
        entered.notify_one();
        release.notified().await;
    }
}

// These seams mutate only the SAME retained record for authored failure
// fixtures. They create no native fact, receipt, task, or replacement owner.
#[cfg(all(test, feature = "fixtures"))]
#[derive(Clone, Copy)]
pub(in crate::adaptive_resources) enum CopyFixtureFault {
    PoisonTerminal,
    LoseJoinedTerminal,
    LosePipeCloseResult,
    ContradictStartDisposition,
}
#[cfg(all(test, feature = "fixtures"))]
impl SealedPhysicalGraph {
    /// Delay only: hold this SAME original membership mutex until released.
    /// No native/terminal/proof state is changed or synthesized by the seam.
    pub(in crate::adaptive_resources) async fn fixture_hold_copy_members(
        &self,
        entered: &tokio::sync::Notify,
        release: &tokio::sync::Notify,
    ) {
        let _members = self.custody.members.lock().await;
        entered.notify_one();
        release.notified().await;
    }

    pub(in crate::adaptive_resources) async fn fixture_copy_fault(
        &self,
        task: GraphCopyTask,
        fault: CopyFixtureFault,
    ) {
        let mut members = self.custody.members.lock().await;
        let writer = members
            .writers
            .get_mut(task.index)
            .expect("original writer");
        let record = writer.copy.as_mut().expect("original copy record");
        assert_eq!(record.selection, task);
        match fault {
            CopyFixtureFault::PoisonTerminal => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _guard = record.finished.lock().expect("original result cell");
                    panic!("fixture poisons SAME original terminal cell");
                }));
            }
            CopyFixtureFault::LoseJoinedTerminal => {
                assert!(writer.joined && !record.joined_abandoned);
                record
                    .finished
                    .lock()
                    .expect("original result cell")
                    .terminal = None;
            }
            CopyFixtureFault::LosePipeCloseResult => {
                record.pipes[0].fixture_lose_next_close_result();
            }
            CopyFixtureFault::ContradictStartDisposition => {
                assert!(writer.joined && !record.joined_abandoned);
                record.start_disposition = Some(CopyStartDisposition::DroppedUnsent);
            }
        }
    }
    pub(in crate::adaptive_resources) async fn fixture_copy_task_finished(
        &self,
        task: GraphCopyTask,
    ) -> bool {
        let members = self.custody.members.lock().await;
        let writer = members.writers.get(task.index).expect("original writer");
        assert_eq!(writer.copy.as_ref().expect("original copy").selection, task);
        writer.joined || writer.task.is_finished()
    }
    pub(in crate::adaptive_resources) async fn fixture_copy_pipe_close_calls(
        &self,
        task: GraphCopyTask,
    ) -> [usize; 2] {
        let members = self.custody.members.lock().await;
        let record = members.writers[task.index]
            .copy
            .as_ref()
            .expect("original copy");
        assert_eq!(record.selection, task);
        [
            record.pipes[0].fixture_checked_close_calls(),
            record.pipes[1].fixture_checked_close_calls(),
        ]
    }
    pub(in crate::adaptive_resources) async fn fixture_copy_disposition(
        &self,
        task: GraphCopyTask,
    ) -> Option<CopyStartDisposition> {
        let members = self.custody.members.lock().await;
        let record = members
            .writers
            .get(task.index)
            .expect("original writer")
            .copy
            .as_ref()
            .expect("original copy");
        assert_eq!(record.selection, task);
        record.start_disposition
    }
}
