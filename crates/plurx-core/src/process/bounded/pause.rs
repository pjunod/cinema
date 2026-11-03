//! Per-operation owned process-group suspension. A stop signal is a request;
//! only observed stopped threads permit the daemon's source-slot transition.
use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourcePausePhase {
    Running,
    Stopping,
    Paused,
}

#[derive(Clone, Copy, Debug)]
pub struct SourcePauseObservation {
    pub revision: u64,
    pub phase: SourcePausePhase,
    pub paused_for: Duration,
}

struct State {
    requested: bool,
    revision: u64,
    next_child: u64,
    children: BTreeMap<u64, bool>,
    verified_since: Option<Instant>,
    paused_total: Duration,
}
impl State {
    fn phase(&self) -> SourcePausePhase {
        if !self.requested {
            SourcePausePhase::Running
        } else if self.children.values().all(|stopped| *stopped) {
            SourcePausePhase::Paused
        } else {
            SourcePausePhase::Stopping
        }
    }
}
struct Inner {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
    max_paused: Duration,
    quota_waiters: std::sync::atomic::AtomicUsize,
}

/// One owned scanner attempt explicitly scopes this controller. It never
/// discovers or signals another job's child. All bounded children enlisted
/// in the scope must stop before its aggregate observation becomes Paused.
#[derive(Clone)]
pub struct SourcePauseController(Arc<Inner>);

impl SourcePauseController {
    pub fn new(max_paused: Duration) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State {
                requested: false,
                revision: 0,
                next_child: 0,
                children: BTreeMap::new(),
                verified_since: None,
                paused_total: Duration::ZERO,
            }),
            changed: watch::channel(0).0,
            max_paused,
            quota_waiters: std::sync::atomic::AtomicUsize::new(0),
        }))
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn changed(&self, state: &mut State) {
        if state.phase() == SourcePausePhase::Paused {
            state.verified_since.get_or_insert_with(Instant::now);
        } else if let Some(since) = state.verified_since.take() {
            state.paused_total = state.paused_total.saturating_add(since.elapsed());
        }
        self.0
            .changed
            .send_modify(|value| *value = value.wrapping_add(1));
    }
    pub fn observation(&self) -> SourcePauseObservation {
        let state = self.state();
        SourcePauseObservation {
            revision: state.revision,
            phase: state.phase(),
            paused_for: state
                .verified_since
                .map_or(Duration::ZERO, |since| since.elapsed()),
        }
    }
    /// Verified source-idle time only; stop-request/D-state time still counts
    /// as active execution and cannot inflate measured source capacity.
    pub fn paused_duration(&self) -> Duration {
        let state = self.state();
        state.paused_total.saturating_add(
            state
                .verified_since
                .map_or(Duration::ZERO, |since| since.elapsed()),
        )
    }
    pub fn pause_budget_exhausted(&self) -> bool {
        self.paused_duration() >= self.0.max_paused
    }
    /// Deliberate finite-byte service holds only. This is a request to the
    /// existing owner driver, never a stop acknowledgement or clock credit.
    pub fn finite_quota_wait(&self) -> SourceQuotaWait {
        self.0
            .quota_waiters
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.0
            .changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        SourceQuotaWait(self.clone())
    }
    pub fn quota_waiters(&self) -> usize {
        self.0
            .quota_waiters
            .load(std::sync::atomic::Ordering::Acquire)
    }
    /// Wait for active execution to expire. Only aggregate verified pauses
    /// after this invocation are excluded; stop requests and D-state continue
    /// consuming active time. The existing owner's finite wall limit remains.
    pub async fn wait_active_budget(&self, active_budget: Duration) {
        let started = Instant::now();
        let paused_before = self.paused_duration();
        let wall = self.hard_deadline(active_budget);
        let mut changes = self.subscribe();
        loop {
            let paused = self.paused_duration().saturating_sub(paused_before);
            let deadline = (started + active_budget.saturating_add(paused)).min(wall);
            let now = Instant::now();
            if now >= wall || now >= deadline {
                return;
            }
            let sleep = if self.observation().phase == SourcePausePhase::Paused {
                wall
            } else {
                deadline
            };
            tokio::select! {
                ()=tokio::time::sleep_until(sleep)=>{},
                result=changes.changed()=>{if result.is_err(){return;}},
            }
        }
    }
    /// Returns the exact pause incarnation that a later resume must name.
    pub fn request_pause(&self) -> u64 {
        let mut state = self.state();
        if !state.requested {
            state.requested = true;
            state.revision = state
                .revision
                .checked_add(1)
                .expect("finite pause generations");
            self.changed(&mut state);
        }
        state.revision
    }
    /// Call only after the current fenced source slot was reacquired. A stale
    /// acknowledgement cannot resume a later pause or an unverified D-state.
    pub fn resume(&self, revision: u64) -> bool {
        let mut state = self.state();
        if state.revision != revision || state.phase() != SourcePausePhase::Paused {
            return false;
        }
        state.requested = false;
        for stopped in state.children.values_mut() {
            *stopped = false;
        }
        self.changed(&mut state);
        true
    }
    pub(super) fn subscribe(&self) -> watch::Receiver<u64> {
        self.0.changed.subscribe()
    }
    pub(super) fn hard_deadline(&self, active_budget: Duration) -> Instant {
        Instant::now()
            + active_budget.saturating_add(self.0.max_paused.saturating_sub(self.paused_duration()))
    }
    /// Enlist before spawning a manually owned child. Retain the returned
    /// guard through physical stop/cleanup; call `run_group` while its pipes
    /// are drained. The caller must create a fresh process group for the child.
    pub async fn enlist(&self, cancellation: &CancellationToken) -> io::Result<ChildPauseGuard> {
        let mut changes = self.subscribe();
        let wall = self.hard_deadline(Duration::ZERO);
        loop {
            {
                let mut state = self.state();
                if !state.requested {
                    let child = state.next_child;
                    state.next_child = state
                        .next_child
                        .checked_add(1)
                        .expect("finite owned children");
                    state.children.insert(child, false);
                    self.changed(&mut state);
                    return Ok(ChildPauseGuard {
                        controller: self.clone(),
                        child,
                    });
                }
            }
            tokio::select! {
                () = cancellation.cancelled() => return Err(io::Error::new(io::ErrorKind::Interrupted,"work cancelled")),
                () = tokio::time::sleep_until(wall) => return Err(io::Error::new(io::ErrorKind::TimedOut,"source pause wall age exhausted")),
                result = changes.changed() => { result.map_err(io::Error::other)?; }
            }
        }
    }
}

/// Lives only around a successfully issued finite allowance's exhausted
/// epoch wait. RPC failures, authority lookup and physical IO never create it.
pub struct SourceQuotaWait(SourcePauseController);
impl Drop for SourceQuotaWait {
    fn drop(&mut self) {
        let prior = self
            .0
             .0
            .quota_waiters
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        debug_assert!(prior > 0);
        self.0
             .0
            .changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

pub struct ChildPauseGuard {
    controller: SourcePauseController,
    child: u64,
}
impl ChildPauseGuard {
    /// Monitor the caller's own fresh child group while retaining the stream
    /// collectors. Errors request owner cleanup; this method never releases
    /// the caller's child/job/descriptors or claims successful reap.
    pub async fn run_group<T>(
        &self,
        process_id: u32,
        active_budget: Duration,
        work: impl std::future::Future<Output = io::Result<T>>,
    ) -> io::Result<T> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (process_id, active_budget, work);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "source pause needs Linux group verification",
            ))
        }
        #[cfg(target_os = "linux")]
        {
            let group = i32::try_from(process_id).map_err(io::Error::other)?;
            let controller = &self.controller;
            let mut changes = controller.subscribe();
            let mut ticks = tokio::time::interval(Duration::from_millis(100));
            let mut active_deadline = Instant::now() + active_budget;
            let wall = controller.hard_deadline(active_budget);
            let mut paused_since = None;
            let mut stop_sent = false;
            let mut stopped_observations = 0;
            tokio::pin!(work);
            loop {
                let observation = controller.observation();
                if observation.phase != SourcePausePhase::Running {
                    if paused_since.is_none() {
                        signal_group(group, libc::SIGSTOP)?;
                        stop_sent = true;
                        if verified_group(group, true).await.unwrap_or(false) {
                            stopped_observations += 1;
                            if stopped_observations >= 2
                                && self.acknowledge(observation.revision)
                                && controller.observation().phase == SourcePausePhase::Paused
                            {
                                // The child alone is insufficient: a daemon
                                // AVIO syscall may still hold an activity guard.
                                // Only aggregate quiescence freezes active time.
                                paused_since = Some(Instant::now());
                            }
                        } else {
                            stopped_observations = 0;
                        }
                    }
                } else if stop_sent {
                    signal_group(group, libc::SIGCONT)?;
                    if let Some(since) = paused_since.take() {
                        active_deadline += since.elapsed();
                    }
                    stop_sent = false;
                    stopped_observations = 0;
                }
                tokio::select! {
                    result=&mut work=>return result,
                    ()=tokio::time::sleep_until(active_deadline),if paused_since.is_none()=>return Err(io::Error::new(io::ErrorKind::TimedOut,"source child active execution timed out")),
                    ()=tokio::time::sleep_until(wall)=>return Err(io::Error::new(io::ErrorKind::TimedOut,"source child paused wall age exhausted")),
                    result=changes.changed()=>{result.map_err(io::Error::other)?;},
                    _=ticks.tick()=>{},
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn acknowledge(&self, revision: u64) -> bool {
        let mut state = self.controller.state();
        if state.requested && state.revision == revision {
            if let Some(stopped) = state.children.get_mut(&self.child) {
                *stopped = true;
            }
            self.controller.changed(&mut state);
            true
        } else {
            false
        }
    }
}
impl Drop for ChildPauseGuard {
    fn drop(&mut self) {
        let mut state = self.controller.state();
        state.children.remove(&self.child);
        self.controller.changed(&mut state);
    }
}

tokio::task_local! { static SOURCE_PAUSE: SourcePauseController; }
/// Explicitly propagate one scanner owner's controller into its directly
/// awaited bounded helpers. Spawned tasks do not inherit this scope.
pub async fn pausable<T>(
    controller: SourcePauseController,
    work: impl std::future::Future<Output = T>,
) -> T {
    SOURCE_PAUSE.scope(controller, work).await
}
/// Capture the explicitly scoped source owner for a spawned helper task or
/// manually owned child. There is no process-global controller fallback.
pub fn source_pause_controller() -> Option<SourcePauseController> {
    SOURCE_PAUSE.try_with(Clone::clone).ok()
}

#[cfg(target_os = "linux")]
fn stat_group_and_state(path: &std::path::Path) -> io::Result<(i32, u8)> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(16 * 1024)
        .read_to_end(&mut bytes)?;
    let end = bytes
        .iter()
        .rposition(|byte| *byte == b')')
        .ok_or_else(|| io::Error::other("invalid process stat"))?;
    let tail = std::str::from_utf8(&bytes[end + 1..]).map_err(io::Error::other)?;
    let mut fields = tail.split_whitespace();
    let state = fields
        .next()
        .and_then(|state| state.as_bytes().first())
        .copied()
        .ok_or_else(|| io::Error::other("missing process state"))?;
    fields
        .next()
        .ok_or_else(|| io::Error::other("missing process parent"))?;
    let group = fields
        .next()
        .ok_or_else(|| io::Error::other("missing process group"))?
        .parse()
        .map_err(io::Error::other)?;
    Ok((group, state))
}

#[cfg(target_os = "linux")]
fn thread_quiescent(state: u8, stopped: bool) -> bool {
    matches!(state, b'Z' | b'X') || (stopped && matches!(state, b'T' | b't'))
}

/// Every member and every thread must be stopped or already dead. A D-state
/// thread, partial enumeration or permission failure never proves quiescence.
#[cfg(target_os = "linux")]
fn group_quiescent(group: i32, stopped: bool) -> io::Result<bool> {
    let mut members = 0usize;
    let mut entries = 0usize;
    for entry in std::fs::read_dir("/proc")? {
        entries += 1;
        if entries > 131_072 {
            return Err(io::Error::other("process inventory exceeded bound"));
        }
        let entry = entry?;
        if !entry
            .file_name()
            .as_encoded_bytes()
            .iter()
            .all(u8::is_ascii_digit)
        {
            continue;
        }
        let path = entry.path();
        let (pgrp, _) = match stat_group_and_state(&path.join("stat")) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if pgrp != group {
            continue;
        }
        members += 1;
        let threads = match std::fs::read_dir(path.join("task")) {
            Ok(threads) => threads,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for thread in threads {
            entries += 1;
            if entries > 131_072 {
                return Err(io::Error::other("thread inventory exceeded bound"));
            }
            let (_, state) = match stat_group_and_state(&thread?.path().join("stat")) {
                Ok(value) => value,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if !thread_quiescent(state, stopped) {
                return Ok(false);
            }
        }
    }
    Ok(!stopped || members > 0)
}

/// Observe all processes/threads in an owned Linux process group. `stopped`
/// admits T/t only after the caller requested a stop; false requires every
/// thread dead/gone. A caller must retain the exact child/group generation.
#[cfg(target_os = "linux")]
pub async fn verified_group(group: i32, stopped: bool) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        tokio::task::spawn_blocking(move || group_quiescent(group, stopped))
            .await
            .map_err(io::Error::other)?
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (group, stopped);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "source pause needs Linux process/thread verification",
        ))
    }
}

#[cfg(target_os = "linux")]
pub(super) fn signal_group(group: i32, signal: i32) -> io::Result<()> {
    #[cfg(unix)]
    {
        if unsafe { libc::kill(-group, signal) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (group, signal);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "source pause needs process groups",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_stale_pause_acknowledgement_cannot_resume_a_successor() {
        let control = SourcePauseController::new(Duration::from_secs(60));
        let first = control.request_pause();
        assert_eq!(control.observation().phase, SourcePausePhase::Paused);
        assert!(!control.resume(first + 1));
        assert!(control.resume(first));
        let second = control.request_pause();
        assert!(second > first);
        assert!(!control.resume(first));
        assert_eq!(control.observation().phase, SourcePausePhase::Paused);
        assert!(control.resume(second));
    }

    #[tokio::test]
    async fn quota_request_alone_never_excludes_active_time() {
        let control = SourcePauseController::new(Duration::from_secs(1));
        let child = control
            .enlist(&CancellationToken::new())
            .await
            .expect("daemon read activity");
        let quota = control.finite_quota_wait();
        control.request_pause();
        assert_eq!(control.observation().phase, SourcePausePhase::Stopping);
        tokio::time::timeout(
            Duration::from_secs(1),
            control.wait_active_budget(Duration::from_millis(20)),
        )
        .await
        .expect("unverified stop still spends active budget");
        assert_eq!(control.paused_duration(), Duration::ZERO);
        assert_eq!(control.quota_waiters(), 1);
        drop(quota);
        assert_eq!(control.quota_waiters(), 0);
        drop(child);
    }

    #[tokio::test]
    async fn aggregate_verified_pause_freezes_active_budget_until_exact_resume() {
        let control = SourcePauseController::new(Duration::from_secs(1));
        let revision = control.request_pause();
        let clock = control.wait_active_budget(Duration::from_millis(40));
        tokio::pin!(clock);
        assert!(tokio::time::timeout(Duration::from_millis(70), &mut clock)
            .await
            .is_err());
        assert!(control.resume(revision));
        tokio::time::timeout(Duration::from_secs(1), clock)
            .await
            .expect("active clock resumes");
    }

    #[tokio::test]
    async fn verified_pause_keeps_a_finite_wall_and_prior_history_is_not_new_credit() {
        let control = SourcePauseController::new(Duration::from_millis(50));
        let revision = control.request_pause();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(control.resume(revision));
        tokio::time::timeout(
            Duration::from_millis(45),
            control.wait_active_budget(Duration::from_millis(10)),
        )
        .await
        .expect("old pause history cannot extend a new active helper");
        control.request_pause();
        tokio::time::timeout(
            Duration::from_millis(100),
            control.wait_active_budget(Duration::from_millis(10)),
        )
        .await
        .expect("verified pause cannot exceed remaining finite wall credit");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uninterruptible_or_running_threads_never_acknowledge_source_quiescence() {
        for state in [b'D', b'R', b'S', b'I'] {
            assert!(!thread_quiescent(state, true));
            assert!(!thread_quiescent(state, false));
        }
        for state in [b'T', b't'] {
            assert!(thread_quiescent(state, true));
            assert!(!thread_quiescent(state, false), "stopped is not reaped");
        }
        assert!(thread_quiescent(b'Z', false));
        assert!(thread_quiescent(b'X', false));
    }
}
