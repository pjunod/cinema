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
    verified_total: Duration,
    // Slot-gated owners exclude time only while a finite quota wait is live
    // and their exact pause RPC has acknowledged the verified stop.
    slot_gated: bool,
    slot_acknowledged: bool,
    credited_since: Option<Instant>,
    credited_total: Duration,
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
        Self::with_slot_gate(max_paused, false)
    }
    /// A source-slot owner separates physical stop proof from timeout credit.
    /// Call `confirm_pause_slot` only after its pause RPC acknowledges the
    /// exact revision. The quota guard closes credit synchronously on drop.
    pub fn new_slot_gated(max_paused: Duration) -> Self {
        Self::with_slot_gate(max_paused, true)
    }
    fn with_slot_gate(max_paused: Duration, slot_gated: bool) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State {
                requested: false,
                revision: 0,
                next_child: 0,
                children: BTreeMap::new(),
                verified_since: None,
                verified_total: Duration::ZERO,
                slot_gated,
                slot_acknowledged: false,
                credited_since: None,
                credited_total: Duration::ZERO,
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
        let now = Instant::now();
        if state.phase() == SourcePausePhase::Paused {
            state.verified_since.get_or_insert(now);
        } else if let Some(since) = state.verified_since.take() {
            state.verified_total = state
                .verified_total
                .saturating_add(now.duration_since(since));
        }
        let creditable = state.phase() == SourcePausePhase::Paused
            && (!state.slot_gated
                || (state.slot_acknowledged
                    && self
                        .0
                        .quota_waiters
                        .load(std::sync::atomic::Ordering::Acquire)
                        > 0));
        if creditable {
            state.credited_since.get_or_insert(now);
        } else if let Some(since) = state.credited_since.take() {
            state.credited_total = state
                .credited_total
                .saturating_add(now.duration_since(since));
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
    /// Active-time credit. A slot-gated owner earns this only after both
    /// physical quiescence and its pause RPC acknowledge a live finite hold.
    pub fn paused_duration(&self) -> Duration {
        let state = self.state();
        state.credited_total.saturating_add(
            state
                .credited_since
                .map_or(Duration::ZERO, |since| since.elapsed()),
        )
    }
    fn verified_duration(&self) -> Duration {
        let state = self.state();
        state.verified_total.saturating_add(
            state
                .verified_since
                .map_or(Duration::ZERO, |since| since.elapsed()),
        )
    }
    fn pause_credit_active(&self) -> bool {
        self.state().credited_since.is_some()
    }
    pub fn pause_budget_exhausted(&self) -> bool {
        self.verified_duration() >= self.0.max_paused
    }
    /// Deliberate finite-byte service holds only. This is a request to the
    /// existing owner driver, never a stop acknowledgement or clock credit.
    pub fn finite_quota_wait(&self) -> SourceQuotaWait {
        self.0
            .quota_waiters
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let mut state = self.state();
        self.changed(&mut state);
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
            let sleep = if self.pause_credit_active() {
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
            state.slot_acknowledged = false;
            state.revision = state
                .revision
                .checked_add(1)
                .expect("finite pause generations");
            self.changed(&mut state);
        }
        state.revision
    }
    /// The existing owner has received the exact pause RPC acknowledgement.
    /// A stale result cannot enable credit for a successor revision.
    pub fn confirm_pause_slot(&self, revision: u64) -> bool {
        let mut state = self.state();
        if !state.slot_gated
            || state.revision != revision
            || state.phase() != SourcePausePhase::Paused
        {
            return false;
        }
        if state.slot_acknowledged {
            return true;
        }
        state.slot_acknowledged = true;
        self.changed(&mut state);
        true
    }
    /// Stop credit before validation and the store RPC that reacquires the
    /// source slot. The group stays physically stopped until `resume` succeeds.
    pub fn suspend_pause_credit(&self, revision: u64) -> bool {
        let mut state = self.state();
        if !state.slot_gated || state.revision != revision || !state.requested {
            return false;
        }
        if !state.slot_acknowledged {
            return true;
        }
        state.slot_acknowledged = false;
        self.changed(&mut state);
        true
    }
    /// Call only after the current fenced source slot was reacquired. A stale
    /// acknowledgement cannot resume a later pause or an unverified D-state.
    pub fn resume(&self, revision: u64) -> bool {
        let mut state = self.state();
        if state.revision != revision || state.phase() != SourcePausePhase::Paused {
            return false;
        }
        state.requested = false;
        state.slot_acknowledged = false;
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
            + active_budget
                .saturating_add(self.0.max_paused.saturating_sub(self.verified_duration()))
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
        let mut state = self.0.state();
        self.0.changed(&mut state);
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
            let started = Instant::now();
            let paused_before = controller.paused_duration();
            let wall = controller.hard_deadline(active_budget);
            let mut stop_sent = false;
            let mut stop_revision = None;
            let mut acknowledged_revision = None;
            let mut stopped_observations = 0;
            tokio::pin!(work);
            loop {
                let observation = controller.observation();
                if observation.phase != SourcePausePhase::Running {
                    // A Running watch update may coalesce with a newer pause.
                    // The old stopped group is not an acknowledgement of the
                    // new revision, even when it never observed SIGCONT.
                    if stop_revision != Some(observation.revision) {
                        signal_group(group, libc::SIGSTOP)?;
                        stop_sent = true;
                        stop_revision = Some(observation.revision);
                        acknowledged_revision = None;
                        stopped_observations = 0;
                    }
                    if acknowledged_revision != Some(observation.revision) {
                        if verified_group(group, true).await.unwrap_or(false) {
                            stopped_observations += 1;
                            if stopped_observations >= 2 && self.acknowledge(observation.revision) {
                                acknowledged_revision = Some(observation.revision);
                            }
                        } else {
                            stopped_observations = 0;
                        }
                    }
                } else if stop_sent {
                    // resume(revision) is the slot-reacquisition CAS and has
                    // already happened before the controller says Running.
                    signal_group(group, libc::SIGCONT)?;
                    stop_sent = false;
                    stop_revision = None;
                    acknowledged_revision = None;
                    stopped_observations = 0;
                }
                // Credit comes only from aggregate verified quiescence. A
                // stop request or an outstanding daemon read cannot extend it.
                let paused = controller.paused_duration().saturating_sub(paused_before);
                let active_deadline = started + active_budget.saturating_add(paused);
                let aggregate_paused = controller.pause_credit_active();
                // The owner may have awaited a store RPC while this future
                // was not polled. A later finite hold cannot erase that spent
                // active time merely because credit is active now.
                if Instant::now() >= active_deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "source child active execution timed out",
                    ));
                }
                tokio::select! {
                    result=&mut work=>return result,
                    ()=tokio::time::sleep_until(active_deadline),if !aggregate_paused=>return Err(io::Error::new(io::ErrorKind::TimedOut,"source child active execution timed out")),
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

    #[tokio::test]
    async fn slot_rpc_waits_spend_active_time() {
        let control = SourcePauseController::new_slot_gated(Duration::from_secs(1));
        let quota = control.finite_quota_wait();
        let revision = control.request_pause();
        assert_eq!(control.observation().phase, SourcePausePhase::Paused);

        // Physical stop plus a finite wait is insufficient before the exact
        // pause RPC acknowledges the slot transition.
        tokio::time::timeout(
            Duration::from_millis(100),
            control.wait_active_budget(Duration::from_millis(30)),
        )
        .await
        .expect("pause RPC wait consumes active budget");
        assert_eq!(control.paused_duration(), Duration::ZERO);

        assert!(control.confirm_pause_slot(revision));
        let clock = control.wait_active_budget(Duration::from_millis(30));
        tokio::pin!(clock);
        assert!(tokio::time::timeout(Duration::from_millis(60), &mut clock)
            .await
            .is_err());
        assert!(control.paused_duration() >= Duration::from_millis(40));

        // The group remains stopped during the resume RPC, but its active
        // clock must run. No CONT is authorized until resume(revision).
        assert!(control.suspend_pause_credit(revision));
        tokio::time::timeout(Duration::from_millis(100), clock)
            .await
            .expect("resume RPC wait consumes active budget");
        assert_eq!(control.observation().phase, SourcePausePhase::Paused);
        drop(quota);
        assert!(control.resume(revision));
    }

    #[tokio::test]
    async fn verified_pressure_without_finite_quota_still_spends_active_time() {
        let control = SourcePauseController::new_slot_gated(Duration::from_secs(1));
        let revision = control.request_pause();
        assert!(control.confirm_pause_slot(revision));
        assert_eq!(control.observation().phase, SourcePausePhase::Paused);
        tokio::time::timeout(
            Duration::from_millis(100),
            control.wait_active_budget(Duration::from_millis(30)),
        )
        .await
        .expect("unidentified pressure is not finite quota service");
        assert_eq!(control.paused_duration(), Duration::ZERO);
        assert!(control.resume(revision));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "current_thread")]
    async fn coalesced_resume_and_repause_reacknowledges_new_revision() {
        let control = SourcePauseController::new(Duration::from_secs(2));
        let guard = control
            .enlist(&CancellationToken::new())
            .await
            .expect("enlist owned child");
        let mut command = tokio::process::Command::new("sleep");
        command.arg("30").process_group(0).kill_on_drop(true);
        let mut child = command.spawn().expect("spawn isolated child group");
        let process_id = child.id().expect("child process identity");
        let driver = tokio::spawn(async move {
            guard
                .run_group(process_id, Duration::from_millis(400), async {
                    std::future::pending::<io::Result<()>>().await
                })
                .await
        });
        let first = control.request_pause();
        tokio::time::timeout(Duration::from_secs(2), async {
            while control.observation().phase != SourcePausePhase::Paused {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first revision reaches verified pause");

        // There is no await between these transitions on this single-thread
        // runtime. The driver can see only the newer Stopping observation.
        assert!(control.resume(first));
        let second = control.request_pause();
        assert!(second > first);
        tokio::time::timeout(Duration::from_secs(2), async {
            while control.observation().phase != SourcePausePhase::Paused {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("coalesced newer revision is reverified and acknowledged");
        assert_eq!(control.observation().revision, second);
        assert!(control.resume(second));
        let result = tokio::time::timeout(Duration::from_millis(1200), driver)
            .await
            .expect("active clock resumes before the longer wall limit")
            .expect("owned driver joins");
        assert_eq!(
            result
                .expect_err("pending work reaches active budget")
                .kind(),
            io::ErrorKind::TimedOut
        );
        child.start_kill().expect("stop fixture child");
        child.wait().await.expect("reap fixture child");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uninterruptible_or_running_threads_never_acknowledge_source_quiescence() {
        for &state in b"DRSI" {
            assert!(!thread_quiescent(state, true));
            assert!(!thread_quiescent(state, false));
        }
        for &state in b"Tt" {
            assert!(thread_quiescent(state, true));
            assert!(!thread_quiescent(state, false), "stopped is not reaped");
        }
        assert!(thread_quiescent(b'Z', false));
        assert!(thread_quiescent(b'X', false));
    }
}
