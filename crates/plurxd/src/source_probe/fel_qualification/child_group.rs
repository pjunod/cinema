//! One qualification pipe graph, paused and retired as one physical owner.
use plurx_core::process::{
    bounded::{source_group_quiescent as verified_group, source_pause_controller, ChildPauseGuard},
    ChildJob, ChildWork,
};
use std::{future::Future, io, process::ExitStatus, time::Duration};
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

pub(super) struct Group {
    id: i32,
    guard: Option<ChildPauseGuard>,
    children: Vec<Child>,
    jobs: Vec<ChildJob>,
}
impl Group {
    #[cfg(test)]
    pub(super) async fn start(
        command: &mut Command,
        cancel: &CancellationToken,
        work: ChildWork,
    ) -> Result<Self, String> {
        Self::start_inner(command, cancel, work, None).await
    }
    pub(super) async fn start_with_manifest(
        command: &mut Command,
        cancel: &CancellationToken,
        work: ChildWork,
        manifest: std::sync::Arc<crate::producer_spawn::DescriptorManifest>,
    ) -> Result<Self, String> {
        Self::start_inner(command, cancel, work, Some(manifest)).await
    }
    async fn start_inner(
        command: &mut Command,
        cancel: &CancellationToken,
        work: ChildWork,
        manifest: Option<std::sync::Arc<crate::producer_spawn::DescriptorManifest>>,
    ) -> Result<Self, String> {
        // One enlistment owns both pipe partners. A second enlistment between
        // spawns could wait for a pause that the first unmonitored child cannot acknowledge.
        let guard = match source_pause_controller() {
            Some(controller) => Some(controller.enlist(cancel).await.map_err(|e| e.to_string())?),
            None => None,
        };
        command.process_group(0);
        let (child, job) = match manifest {
            Some(manifest) => crate::producer_spawn::with_manifest(&manifest, command, |command| {
                plurx_core::process::spawn_job_owned(command, work)
            }),
            None => plurx_core::process::spawn_job_owned(command, work),
        }
        .map_err(|e| e.to_string())?;
        let id = i32::try_from(child.id().ok_or("qualification child has no PID")?)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            id,
            guard,
            children: vec![child],
            jobs: vec![job],
        })
    }
    #[cfg(test)]
    pub(super) fn add(&mut self, command: &mut Command, work: ChildWork) -> Result<(), String> {
        self.add_inner(command, work, None)
    }
    pub(super) fn add_with_manifest(
        &mut self,
        command: &mut Command,
        work: ChildWork,
        manifest: &std::sync::Arc<crate::producer_spawn::DescriptorManifest>,
    ) -> Result<(), String> {
        self.add_inner(command, work, Some(manifest))
    }
    fn add_inner(
        &mut self,
        command: &mut Command,
        work: ChildWork,
        manifest: Option<&std::sync::Arc<crate::producer_spawn::DescriptorManifest>>,
    ) -> Result<(), String> {
        command.process_group(self.id);
        let (child, job) = match manifest {
            Some(manifest) => crate::producer_spawn::with_manifest(manifest, command, |command| {
                plurx_core::process::spawn_job_owned(command, work)
            }),
            None => plurx_core::process::spawn_job_owned(command, work),
        }
        .map_err(|e| e.to_string())?;
        let pid = child.id().ok_or("qualification partner has no PID")? as i32;
        self.children.push(child);
        self.jobs.push(job);
        // Neither leader has been waited/reaped, so the original PGID cannot
        // be recycled while a partner is joined or a signal is issued.
        // SAFETY: getpgid only observes this owned, unreaped child identifier.
        if unsafe { libc::getpgid(pid) } != self.id {
            return Err("qualification pipe partner did not join its owned group".into());
        }
        Ok(())
    }
    pub(super) fn parts(&mut self) -> (Option<&ChildPauseGuard>, i32, &mut [Child]) {
        (self.guard.as_ref(), self.id, &mut self.children)
    }
    pub(super) async fn close(mut self) -> Vec<io::Result<ExitStatus>> {
        kill(self.id, &mut self.children);
        wait_dead(self.id).await;
        let mut statuses = Vec::new();
        for child in &mut self.children {
            statuses.push(Ok(reap_owned(child).await));
        }
        // No signal or group lookup follows reap: the pinned generation ends here.
        self.id = 0;
        self.children.clear();
        self.jobs.clear();
        drop(self.guard.take());
        statuses
    }
}
async fn reap_owned(child: &mut Child) -> ExitStatus {
    loop {
        match child.wait().await {
            Ok(status) => return status,
            Err(error) => {
                tracing::warn!(%error, "FEL group child join failed; retaining owner");
                let _ = child.start_kill();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}
fn kill(id: i32, children: &mut [Child]) {
    if id > 0 {
        // The unreaped group leader pins this exact generation.
        // SAFETY: the unreaped owned leader pins this process-group identity.
        unsafe {
            libc::kill(-id, libc::SIGKILL);
        }
    }
    for child in children {
        let _ = child.start_kill();
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        if self.id <= 0 {
            return;
        }
        kill(self.id, &mut self.children);
        let id = self.id;
        let mut children = std::mem::take(&mut self.children);
        let jobs = std::mem::take(&mut self.jobs);
        let guard = self.guard.take();
        // Cooperative callers await close. This fallback still retains the
        // pause guard and process registrations until detached cleanup settles.
        tokio::spawn(async move {
            wait_dead(id).await;
            for child in &mut children {
                reap_owned(child).await;
            }
            drop((jobs, guard));
        });
    }
}
pub(super) async fn wait_dead(id: i32) {
    while !verified_group(id, false).await.unwrap_or(false) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
pub(super) async fn supervise<T>(
    guard: Option<&ChildPauseGuard>,
    id: i32,
    cancel: &CancellationToken,
    work: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    match guard {
        Some(guard) => tokio::select! {
            result = guard.run_group(id as u32, super::STAGE_DEADLINE, async { work.await.map_err(io::Error::other) }) => result.map_err(|e| e.to_string()),
            () = cancel.cancelled() => Err("FEL qualification was preempted or cancelled".into()),
        },
        None => super::supervised(cancel, work).await,
    }
}
