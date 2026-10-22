//! The graph retains the original concrete pump task and both actual pipes.
//! This task adapter alone supplies no process, source, scratch, or refund proof.
use super::finite_pipes::{resident_memory_bytes, PipeRecord};
use super::*;
use crate::adaptive_resources::runtime::finite_vod::pump::{
    FiniteWriterCompletion, FiniteWriterOutcome, PumpFinished, SoftwarePump,
};
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
pub(super) struct FiniteRecord {
    selection: GraphFiniteVodTask,
    pipes: [PipeRecord; 2],
    finished: Arc<StdMutex<Option<PumpFinished>>>,
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
}

/// A joined task selection that keeps the graph's original terminal allocation
/// alive. Repeated summaries read the same retained result without consuming it.
pub(in crate::adaptive_resources) struct JoinedFiniteVod {
    selection: GraphFiniteVodTask,
    finished: Arc<StdMutex<Option<PumpFinished>>>,
    abandoned: bool,
}
impl JoinedFiniteVod {
    pub(in crate::adaptive_resources) fn selection(&self) -> GraphFiniteVodTask {
        self.selection
    }
    pub(in crate::adaptive_resources) fn summary(&self) -> io::Result<FiniteWriterCompletion> {
        if self.abandoned {
            return Ok(FiniteWriterCompletion {
                outcome: FiniteWriterOutcome::TaskAbandoned,
                produced_through: None,
                completed_output: false,
                diagnostic_bytes: 0,
                diagnostics_truncated: false,
                diagnostic_read_error: None,
                kill_error: None,
            });
        }
        let finished = self.finished.lock().map_err(|_| {
            io::Error::other("finite VOD terminal state retained in poisoned owner")
        })?;
        Ok(finished
            .as_ref()
            .map(PumpFinished::summary)
            .unwrap_or(FiniteWriterCompletion {
                outcome: FiniteWriterOutcome::NotStarted,
                produced_through: None,
                completed_output: false,
                diagnostic_bytes: 0,
                diagnostics_truncated: false,
                diagnostic_read_error: None,
                kill_error: None,
            }))
    }
}

impl OwnedPhysicalGraph {
    pub(in crate::adaptive_resources) async fn spawn_durable_finite_vod(
        &mut self,
        root: GraphRoot,
        member: Uuid,
        arm: super::super::runtime::DurableArm,
        pump: &mut Option<SoftwarePump>,
    ) -> io::Result<(GraphFiniteVodTask, tokio::task::AbortHandle)> {
        let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        let custody = self.custody();
        let mut members = custody.members.lock().await;
        if custody.sealed.load(Ordering::Acquire)
            || members.writers.len() >= MAX_WRITERS
            || member.is_nil()
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
        let finished = Arc::new(StdMutex::new(None));
        let result = Arc::clone(&finished);
        // Only this exact concrete implementation receives the guarded readers.
        // No await or caller factory separates creation from retaining its task.
        let pump = pump.take().expect("validated retained pump");
        let (start, registered) = tokio::sync::oneshot::channel();
        let task = runtime.spawn(async move {
            if registered.await.is_err() {
                return;
            }
            let terminal = pump.run(readers, control).await;
            if let Ok(mut result) = result.lock() {
                *result = Some(terminal);
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
