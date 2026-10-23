//! The graph retains the original concrete pump task and both actual pipes.
//! This task adapter alone supplies no process, source, scratch, or refund proof.
use super::finite_pipes::{resident_memory_bytes, PipeRecord};
use super::*;
use crate::adaptive_resources::runtime::finite_vod::pump::{
    FiniteWriterCompletion, FiniteWriterOutcome, PumpFinished, SoftwarePump,
};
use crate::source_identity::SourceIdentity;
use std::sync::Mutex as StdMutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::adaptive_resources) struct GraphFiniteVodTask {
    generation: Uuid,
    member: Uuid,
    index: usize,
    root: usize,
}
impl GraphFiniteVodTask {
    pub(in crate::adaptive_resources) fn object_identity(self) -> String {
        format!(
            "software-avc-vod-task-v1:{}:{}:{}",
            self.generation, self.member, self.root
        )
    }
}
/// A private, move-only record of the *held* source adapter's verified ELF
/// result. Only the fixed runtime adapter may create it, and the graph consumes
/// it for the exact root/task pair after the original launch has been retained.
#[must_use]
pub(in crate::adaptive_resources) struct FiniteFixedImage {
    root_member: Uuid,
    task_member: Uuid,
    acquisition_id: Uuid,
    close_operation_id: Uuid,
    identity: SourceIdentity,
    sha256: [u8; 32],
}
impl FiniteFixedImage {
    pub(in crate::adaptive_resources) fn from_verified_source(
        root_member: Uuid,
        task_member: Uuid,
        acquisition_id: Uuid,
        executable_identity: SourceIdentity,
        executable_sha256: [u8; 32],
        verified_elf: bool,
    ) -> io::Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "fixed ELF closure requires Linux",
            ));
        }
        if !verified_elf
            || root_member.is_nil()
            || task_member.is_nil()
            || acquisition_id.is_nil()
            || root_member == task_member
            || executable_identity.size == 0
            || executable_identity.size > 512 * 1024 * 1024
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unverified fixed AVC ELF binding",
            ));
        }
        Ok(Self {
            root_member,
            task_member,
            acquisition_id,
            close_operation_id: Uuid::new_v4(),
            identity: executable_identity,
            sha256: executable_sha256,
        })
    }
}

/// Terminal summary survives release of the original diagnostics Vec.
enum FiniteTerminal {
    NotStarted,
    Finished(PumpFinished),
    Closed(FiniteWriterCompletion),
}
fn incomplete_summary(outcome: FiniteWriterOutcome) -> FiniteWriterCompletion {
    FiniteWriterCompletion {
        outcome,
        produced_through: None,
        completed_output: false,
        diagnostic_bytes: 0,
        diagnostics_truncated: false,
        diagnostic_read_error: None,
        kill_error: None,
    }
}

/// Process-only live fact for one trusted fixed no-helper AVC graph. It is
/// neither serializable nor a whole-resource close/refund receipt.
#[must_use]
pub(in crate::adaptive_resources) struct FiniteVodProcessClosure {
    binding: ReservationBinding,
    generation: Uuid,
    root: GraphRoot,
    root_member: Uuid,
    acquisition_id: Uuid,
    close_operation_id: Uuid,
    task: GraphFiniteVodTask,
    executable_identity: SourceIdentity,
    executable_sha256: [u8; 32],
    writer: FiniteWriterCompletion,
}
impl FiniteVodProcessClosure {
    pub(in crate::adaptive_resources) fn matches(&self, graph: &SealedPhysicalGraph) -> bool {
        self.binding == graph.custody.binding
            && self.generation == graph.custody.identity.generation
            && graph.custody.sealed.load(Ordering::Acquire)
    }
    pub(in crate::adaptive_resources) fn generation(&self) -> Uuid {
        self.generation
    }
    pub(in crate::adaptive_resources) fn root(&self) -> GraphRoot {
        self.root
    }
    pub(in crate::adaptive_resources) fn close_operation_id(&self) -> Uuid {
        self.close_operation_id
    }
    pub(in crate::adaptive_resources) fn root_member(&self) -> Uuid {
        self.root_member
    }
    pub(in crate::adaptive_resources) fn acquisition_id(&self) -> Uuid {
        self.acquisition_id
    }
    pub(in crate::adaptive_resources) fn task(&self) -> GraphFiniteVodTask {
        self.task
    }
    pub(in crate::adaptive_resources) fn executable_identity(&self) -> SourceIdentity {
        self.executable_identity
    }
    pub(in crate::adaptive_resources) fn executable_sha256(&self) -> [u8; 32] {
        self.executable_sha256
    }
    pub(in crate::adaptive_resources) fn writer(&self) -> FiniteWriterCompletion {
        self.writer
    }
}

/// Original fixed root closure when task preflight never registered a writer.
/// This is deliberately distinct from FiniteVodProcessClosure: no task was
/// acquired or joined, so it cannot satisfy the typed producer settlement.
#[must_use]
pub(in crate::adaptive_resources) struct FiniteVodRootOnlyClosure {
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
impl FiniteVodRootOnlyClosure {
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

/// Original fixed graph never entered process acquisition. It proves absence
/// of both root and writer without borrowing a process identity or PID.
#[must_use]
pub(in crate::adaptive_resources) struct FiniteVodNeverRootClosure {
    binding: ReservationBinding,
    generation: Uuid,
    root_member: Uuid,
    task_member: Uuid,
    acquisition_id: Uuid,
    close_operation_id: Uuid,
}
impl FiniteVodNeverRootClosure {
    pub(in crate::adaptive_resources) fn matches(&self, graph: &SealedPhysicalGraph) -> bool {
        self.binding == graph.custody.binding
            && self.generation == graph.custody.identity.generation
            && graph.custody.sealed.load(Ordering::Acquire)
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
}

pub(super) struct FiniteRecord {
    selection: GraphFiniteVodTask,
    pipes: [PipeRecord; 2],
    finished: Arc<StdMutex<FiniteTerminal>>,
    fixed_image: FiniteFixedImage,
    minimum_memory: u64,
    start: Option<tokio::sync::oneshot::Sender<()>>,
    joined_abandoned: bool,
}
impl FiniteRecord {
    pub(super) fn minimum_memory_bytes(&self) -> u64 {
        self.minimum_memory
    }
    pub(super) fn close_joined_pipes(&self) -> io::Result<()> {
        for pipe in &self.pipes {
            pipe.close_after_reader_drop()?;
        }
        Ok(())
    }
    pub(super) fn note_join(&mut self, abandoned: bool) {
        self.joined_abandoned |= abandoned;
    }
    fn compact_terminal(&self) -> io::Result<FiniteWriterCompletion> {
        let mut terminal = self.finished.lock().map_err(|_| {
            io::Error::other("finite VOD terminal state retained in poisoned owner")
        })?;
        let summary = if self.joined_abandoned {
            incomplete_summary(FiniteWriterOutcome::TaskAbandoned)
        } else {
            match &*terminal {
                FiniteTerminal::NotStarted => incomplete_summary(FiniteWriterOutcome::NotStarted),
                FiniteTerminal::Finished(finished) => finished.summary(),
                FiniteTerminal::Closed(summary) => *summary,
            }
        };
        *terminal = FiniteTerminal::Closed(summary);
        Ok(summary)
    }
}

/// A joined task selection that keeps the graph's original terminal allocation
/// alive. Repeated summaries read the same retained result without consuming it.
pub(in crate::adaptive_resources) struct JoinedFiniteVod {
    selection: GraphFiniteVodTask,
    finished: Arc<StdMutex<FiniteTerminal>>,
    abandoned: bool,
}
impl JoinedFiniteVod {
    pub(in crate::adaptive_resources) fn selection(&self) -> GraphFiniteVodTask {
        self.selection
    }
    pub(in crate::adaptive_resources) fn summary(&self) -> io::Result<FiniteWriterCompletion> {
        let finished = self.finished.lock().map_err(|_| {
            io::Error::other("finite VOD terminal state retained in poisoned owner")
        })?;
        if self.abandoned {
            return Ok(incomplete_summary(FiniteWriterOutcome::TaskAbandoned));
        }
        Ok(match &*finished {
            FiniteTerminal::NotStarted => incomplete_summary(FiniteWriterOutcome::NotStarted),
            FiniteTerminal::Finished(finished) => finished.summary(),
            FiniteTerminal::Closed(summary) => *summary,
        })
    }
}

impl OwnedPhysicalGraph {
    pub(in crate::adaptive_resources) async fn spawn_durable_finite_vod(
        &mut self,
        root: GraphRoot,
        root_member_id: Uuid,
        member: Uuid,
        acquisition_id: Uuid,
        arm: super::super::runtime::DurableArm,
        pump: &mut Option<SoftwarePump>,
        fixed_image: &mut Option<FiniteFixedImage>,
    ) -> io::Result<(GraphFiniteVodTask, tokio::task::AbortHandle)> {
        let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        let custody = self.custody();
        let mut members = custody.members.lock().await;
        if custody.sealed.load(Ordering::Acquire)
            || members.writers.len() >= MAX_WRITERS
            || member.is_nil()
            || fixed_image.as_ref().is_none_or(|image| {
                image.root_member != root_member_id
                    || image.task_member != member
                    || image.acquisition_id != acquisition_id
            })
            || root.generation != custody.identity.generation
            || members.writers.iter().any(|writer| {
                writer
                    .finite
                    .as_ref()
                    .is_some_and(|record| record.selection.member == member)
            })
        {
            return Err(io::Error::other(
                "finite VOD task unavailable or already owned",
            ));
        }
        let minimum_memory = pump
            .as_ref()
            .ok_or_else(|| io::Error::other("finite VOD pump already entered"))?
            .minimum_memory_bytes()?
            .checked_add(u64::try_from(resident_memory_bytes()?).map_err(io::Error::other)?)
            .and_then(|n| n.checked_add(std::mem::size_of::<FiniteRecord>() as u64))
            .and_then(|n| n.checked_add(std::mem::size_of::<StdMutex<FiniteTerminal>>() as u64))
            .and_then(|n| n.checked_add((2 * std::mem::size_of::<usize>()) as u64))
            .ok_or_else(|| io::Error::other("finite VOD task memory overflow"))?;
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
            return Err(io::Error::other("finite VOD root pipes unavailable"));
        }
        let control = owned
            .job
            .as_ref()
            .and_then(ChildJob::registered_control)
            .ok_or_else(|| io::Error::other("finite VOD root registration unavailable"))?;
        begin_acquisition_with_arm(&self.ledger, custody, &mut members, Some(&arm))?;
        let owned = &mut members.roots[root.index];
        owned.pipes_claimed = true;
        drop(owned.child.stdin.take());
        let (pipes, readers) = super::finite_pipes::prepare(
            owned.child.stdout.take().expect("validated stdout"),
            owned.child.stderr.take().expect("validated stderr"),
        );
        let selection = GraphFiniteVodTask {
            generation: custody.identity.generation,
            member,
            index: members.writers.len(),
            root: root.index,
        };
        let finished = Arc::new(StdMutex::new(FiniteTerminal::NotStarted));
        let result = Arc::clone(&finished);
        // Only this exact concrete implementation receives the guarded readers.
        // No await or caller factory separates creation from retaining its task.
        let pump = pump.take().expect("validated retained pump");
        let fixed_image = fixed_image.take().expect("validated fixed image");
        let (start, registered) = tokio::sync::oneshot::channel();
        let task = runtime.spawn(async move {
            if registered.await.is_err() {
                return;
            }
            let terminal = pump.run(readers, control).await;
            if let Ok(mut result) = result.lock() {
                *result = FiniteTerminal::Finished(terminal);
            }
        });
        let abort = task.abort_handle();
        members.writers.push(Writer {
            task,
            drain: None,
            finite: Some(FiniteRecord {
                selection,
                pipes,
                finished,
                fixed_image,
                minimum_memory,
                start: Some(start),
                joined_abandoned: false,
            }),
            joined: false,
        });
        Ok((selection, abort))
    }
}

impl OwnedPhysicalGraph {
    pub(in crate::adaptive_resources) async fn take_finite_vod_start(
        &self,
        task: GraphFiniteVodTask,
    ) -> io::Result<tokio::sync::oneshot::Sender<()>> {
        let mut members = self.custody().members.lock().await;
        if self.custody().sealed.load(Ordering::Acquire)
            || task.generation != self.identity().generation
        {
            return Err(io::Error::other("finite VOD entry was sealed"));
        }
        members
            .writers
            .get_mut(task.index)
            .and_then(|writer| writer.finite.as_mut())
            .filter(|record| record.selection == task)
            .and_then(|record| record.start.take())
            .ok_or_else(|| io::Error::other("finite VOD start already decided or mismatched"))
    }
}

impl SealedPhysicalGraph {
    /// Call without a runtime bundle lock: the running writer may need that
    /// same bundle for publication. A cancelled join keeps the original handle.
    pub(in crate::adaptive_resources) async fn join_finite_vod(
        &self,
        task: GraphFiniteVodTask,
        abort: bool,
    ) -> io::Result<JoinedFiniteVod> {
        let mut members = self.custody.members.lock().await;
        if !self.custody.sealed.load(Ordering::Acquire)
            || task.generation != self.custody.identity.generation
        {
            return Err(io::Error::other(
                "finite VOD join requires original sealed graph",
            ));
        }
        let writer = members
            .writers
            .get_mut(task.index)
            .filter(|writer| {
                writer
                    .finite
                    .as_ref()
                    .is_some_and(|record| record.selection == task)
            })
            .ok_or_else(|| io::Error::other("finite VOD task belongs to another owner"))?;
        let mut abandoned = false;
        let newly_joined = !writer.joined;
        if newly_joined {
            if abort {
                writer.task.abort();
            }
            abandoned = (&mut writer.task).await.is_err();
            writer.joined = true;
        }
        let record = writer.finite.as_mut().expect("validated concrete record");
        if newly_joined {
            record.note_join(abandoned);
        }
        let pipes_closed = record.close_joined_pipes();
        let joined = JoinedFiniteVod {
            selection: record.selection,
            finished: Arc::clone(&record.finished),
            abandoned: record.joined_abandoned,
        };
        if newly_joined {
            members.joined_writers += 1;
            members.abandoned_writers += usize::from(abandoned);
        }
        pipes_closed?;
        Ok(joined)
    }
}

impl SealedPhysicalGraph {
    /// Mint a one-shot absence witness only from the sealed original graph.
    /// An acquisition window, unknown spawn result, generic handoff, or any
    /// retained native graph member refuses; no numeric PID guess is involved.
    pub(in crate::adaptive_resources) async fn try_finite_vod_never_root_closure(
        &self,
        root_member: Uuid,
        task_member: Uuid,
        acquisition_id: Uuid,
    ) -> io::Result<FiniteVodNeverRootClosure> {
        let mut members = self.custody.members.lock().await;
        if !cfg!(target_os = "linux")
            || !self.custody.sealed.load(Ordering::Acquire)
            || members.acquired
            || members.uncertain_launch
            || members.unaudited_handoff
            || members.receipt_issued
            || members.finite_process_closed
            || !members.roots.is_empty()
            || !members.writers.is_empty()
            || !members.source_files.is_empty()
            || members.joined_writers != 0
            || members.abandoned_writers != 0
            || root_member.is_nil()
            || task_member.is_nil()
            || acquisition_id.is_nil()
            || root_member == task_member
        {
            return Err(io::Error::other(
                "fixed AVC no-root inventory is incomplete",
            ));
        }
        members.finite_process_closed = true;
        Ok(FiniteVodNeverRootClosure {
            binding: self.custody.binding,
            generation: self.custody.identity.generation,
            root_member,
            task_member,
            acquisition_id,
            close_operation_id: Uuid::new_v4(),
        })
    }
}

impl SealedPhysicalGraph {
    /// Close the actual waited root's unclaimed pipes and registered job only
    /// when the graph inventory proves that no writer was ever installed.
    /// A failed native close consumes its descriptor once and leaves no
    /// retryable positive proof; the original reservation remains held.
    pub(in crate::adaptive_resources) async fn try_finite_vod_root_only_closure(
        &self,
        root: GraphRoot,
        root_member_id: Uuid,
        task_member_id: Uuid,
        acquisition_id: Uuid,
        image: &mut Option<FiniteFixedImage>,
    ) -> io::Result<FiniteVodRootOnlyClosure> {
        let mut members = self.custody.members.lock().await;
        if !cfg!(target_os = "linux")
            || !self.custody.sealed.load(Ordering::Acquire)
            || !members.acquired
            || members.uncertain_launch
            || members.unaudited_handoff
            || members.finite_process_closed
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
                "fixed AVC root-only inventory is incomplete",
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
                "original fixed AVC root or unclaimed pipe remains unresolved",
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
        stdout_close?;
        stderr_close?;
        let image = image.take().expect("validated fixed image");
        let closure = FiniteVodRootOnlyClosure {
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
        // Dispose original waited Child and ChildJob only after both native
        // closes returned success. This never marks a writer joined.
        members.roots.clear();
        members.finite_process_closed = true;
        Ok(closure)
    }
}

impl SealedPhysicalGraph {
    /// Positive process-only result for the one retained trusted fixed software
    /// AVC graph. This cannot close source/scratch owners or refund its ledger.
    pub(in crate::adaptive_resources) async fn try_finite_vod_process_closure(
        &self,
        task: GraphFiniteVodTask,
    ) -> io::Result<FiniteVodProcessClosure> {
        let mut members = self.custody.members.lock().await;
        if !cfg!(target_os = "linux") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "fixed AVC process closure requires Linux",
            ));
        }
        if !self.custody.sealed.load(Ordering::Acquire)
            || !members.acquired
            || members.uncertain_launch
            || members.unaudited_handoff
            || members.finite_process_closed
            || members.roots.len() != 1
            || members.writers.len() != 1
            || task.generation != self.custody.identity.generation
            || task.root != 0
            || task.index != 0
        {
            return Err(io::Error::other("fixed AVC graph inventory is incomplete"));
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
                "original fixed AVC root or pipe owner remains live",
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
            .finite
            .as_ref()
            .ok_or_else(|| io::Error::other("fixed AVC writer record missing"))?;
        if !writer.joined || writer.drain.is_some() || record.selection != task {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "original fixed AVC writer is not joined",
            ));
        }
        // A reader can keep a pipe after task completion. Do not drop the
        // diagnostic allocation or native owner until both original pipes close.
        record.close_joined_pipes()?;
        let summary = record.compact_terminal()?;
        let image = &record.fixed_image;
        let closure = FiniteVodProcessClosure {
            binding: self.custody.binding,
            generation: self.custody.identity.generation,
            root: selected_root,
            root_member: image.root_member,
            acquisition_id: image.acquisition_id,
            close_operation_id: image.close_operation_id,
            task,
            executable_identity: image.identity,
            executable_sha256: image.sha256,
            writer: summary,
        };
        // The original waited Child and ChildJob (including its registered
        // pidfd) are disposed only after the fixed graph's terminal checks.
        members.roots.clear();
        members.finite_process_closed = true;
        Ok(closure)
    }
}
