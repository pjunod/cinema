//! Production bridge while legacy encoder capacity migrates to physical demands.
//! The exclusive slot owns the actual child/job, admission and writer JoinHandle
//! before any registration await. This proves only a producer's narrow reap;
//! it is NOT positive containment or an adaptive physical-close receipt.

use super::*;

impl EmptyProducerSlot {
    /// Launch only into this pre-reserved empty slot. No raw child, job or
    /// writer handle leaves the owner. The writer must not detach work or wait
    /// for its OWN reap receipt; completion metadata belongs to a separate task.
    pub(crate) fn spawn_registered_owned<F, Fut>(
        mut self,
        program: &std::path::Path,
        args: &[String],
        options: crate::producer_spawn::SpawnOptions<'_>,
        at: u32,
        resources: Option<Box<dyn Send>>,
        writer: F,
    ) -> Result<(ProducerRegistration, Option<u32>), String>
    where
        F: FnOnce(
                ProducerRegistration,
                tokio::process::ChildStdout,
                tokio::process::ChildStderr,
            ) -> Fut
            + Send
            + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let crate::producer_spawn::Spawned {
            child,
            child_job,
            stdout,
            stderr,
        } = crate::producer_spawn::spawn(program, args, options)?;
        let pid = child.id(); // Diagnostics only, never a process capability.
        let registration = new_registration(None);
        self.0.child = Some(ProducerChild::Legacy(child));
        self.0.child_job = Some(child_job);
        self.0.resources = resources;
        self.0.registration = Some(registration.clone());
        self.0.belief = after(self.0.belief, Step::Start { at });
        let writer_registration = registration.clone();
        // The factory runs INSIDE the task. A panic constructing its future
        // cannot unwind past child installation and release admission early.
        let task = tokio::spawn(async move { writer(writer_registration, stdout, stderr).await });
        *registration
            .0
            .writers
            .try_lock()
            .expect("unpublished writer registration") = Some(RegisteredWriters::Task(task));
        // No await from launch through child/task custody. Slot operations are
        // fenced by this guard until the actual handle is installed.
        Ok((registration, pid))
    }
}

#[cfg(all(test, unix))]
mod tests;
