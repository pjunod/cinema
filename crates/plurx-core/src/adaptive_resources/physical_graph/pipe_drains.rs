//! Audited task adapter: discard a root's stdout/stderr, close its stdin.
//!
//! The future receives only pipes already owned by this graph. It cannot spawn
//! children, open files, export handles or delegate remote work. Two fixed read
//! buffers bound memory independently of output size. Abort is a request; only
//! joining the retained task proves that its future and pipe handles are gone.
//! This is task-local closure, NEVER whole-process/descendant extinction.
use super::*;
use tokio::io::AsyncReadExt;

const BUFFER_BYTES: usize = 8192;
pub(super) const TASK_BYTES: u64 = (BUFFER_BYTES * 2) as u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::adaptive_resources) struct GraphPipeDrain {
    generation: Uuid,
    member: Uuid,
    index: usize,
    root: usize,
}
impl GraphPipeDrain {
    pub(in crate::adaptive_resources) fn object_identity(self) -> String {
        format!(
            "pipe-drain-v1:{}:{}:{}",
            self.generation, self.member, self.root
        )
    }
}

pub(super) struct DrainRecord {
    selection: GraphPipeDrain,
}

impl DrainRecord {
    pub(super) fn matches_writer(&self, generation: Uuid, index: usize, roots: &[Root]) -> bool {
        self.selection.generation == generation
            && self.selection.index == index
            && roots
                .get(self.selection.root)
                .is_some_and(|root| root.pipes_claimed)
    }

    pub(super) fn owns_root(&self, index: usize) -> bool {
        self.selection.root == index
    }
}

async fn discard<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>) {
    if let Some(mut pipe) = pipe {
        let mut bytes = [0u8; BUFFER_BYTES];
        // Read failure still drops this adapter's sole handle. It is not
        // reported as successful producer/output completion.
        while let Ok(count) = pipe.read(&mut bytes).await {
            if count == 0 {
                break;
            }
        }
    }
}

impl OwnedPhysicalGraph {
    pub(in crate::adaptive_resources) async fn spawn_durable_pipe_drain(
        &mut self,
        root: GraphRoot,
        member: Uuid,
        arm: super::super::runtime::DurableArm,
    ) -> io::Result<GraphPipeDrain> {
        let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        let custody = self.custody();
        let mut members = custody.members.lock().await;
        if custody.sealed.load(Ordering::Acquire)
            || members.writers.len() >= MAX_WRITERS
            || member.is_nil()
            || root.generation != custody.identity.generation
            || members.writers.iter().any(|writer| {
                writer
                    .drain
                    .as_ref()
                    .is_some_and(|drain| drain.selection.member == member)
            })
        {
            return Err(io::Error::other("pipe drain unavailable or already owned"));
        }
        // Share headroom with every retained source read, not just other tasks.
        budgets::check(
            &self.ledger,
            custody,
            &members,
            budgets::BudgetDelta {
                drains: 1,
                ..Default::default()
            },
        )?;
        // Validation must precede begin_acquisition; an invalid foreign root
        // cannot consume this named member or acquire through another graph.
        root_member(custody, &mut members, root)?;
        let owned_root = &members.roots[root.index];
        if owned_root.pipes_claimed || owned_root.waited {
            return Err(io::Error::other("root pipes already claimed or waited"));
        }
        begin_acquisition_with_arm(&self.ledger, custody, &mut members, Some(&arm))?;
        let owned_root = &mut members.roots[root.index];
        owned_root.pipes_claimed = true;
        let pipes = RootPipes {
            stdin: owned_root.child.stdin.take(),
            stdout: owned_root.child.stdout.take(),
            stderr: owned_root.child.stderr.take(),
        };
        let selection = GraphPipeDrain {
            generation: custody.identity.generation,
            member,
            index: members.writers.len(),
            root: root.index,
        };
        // No await from pipe take to retaining the real JoinHandle. No user
        // factory/future can run here. Cancellation loses only the selection.
        let task = runtime.spawn(async move {
            drop(pipes.stdin);
            tokio::join!(discard(pipes.stdout), discard(pipes.stderr));
        });
        members.writers.push(Writer {
            task,
            drain: Some(DrainRecord { selection }),
            #[cfg(unix)]
            finite: None,
            joined: false,
        });
        Ok(selection)
    }
}

impl SealedPhysicalGraph {
    /// Idempotent positive closure of ONLY this audited task. An aborted-but-not-
    /// joined task is still owned; a cancelled waiter retains its JoinHandle.
    pub(in crate::adaptive_resources) async fn close_pipe_drain(
        &self,
        task: GraphPipeDrain,
    ) -> io::Result<()> {
        let mut members = self.custody.members.lock().await;
        if !self.custody.sealed.load(Ordering::Acquire)
            || task.generation != self.custody.identity.generation
        {
            return Err(io::Error::other(
                "pipe drain requires original sealed graph",
            ));
        }
        let writer = members
            .writers
            .get_mut(task.index)
            .filter(|writer| {
                writer
                    .drain
                    .as_ref()
                    .is_some_and(|drain| drain.selection == task)
            })
            .ok_or_else(|| io::Error::other("pipe drain belongs to another adapter"))?;
        if !writer.joined {
            writer.task.abort();
            let abandoned = (&mut writer.task).await.is_err();
            writer.joined = true;
            members.joined_writers += 1;
            members.abandoned_writers += usize::from(abandoned);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
