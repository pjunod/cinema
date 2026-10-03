//! Carrying a [`crate::prodexec::Step`] out against a real child process.
//!
//! [`crate::prodexec`] is pure on purpose: `next_step` decides the single
//! operation a decision needs, `after` says what to believe once it succeeds,
//! and every interleaving rule lives there as a table. This is the thin layer
//! that actually performs the step, and it is thin on purpose too — it decides
//! nothing. It sends the signal the step names, and it routes every belief
//! change back through [`after`], so a reader asking "when does the executor
//! believe X" has exactly one file to read, and it is not this one.
//!
//! Two disciplines from `transcode.rs`'s `apply_ahead_window` are load-bearing
//! here, because this layer exists to repeat that function's correctness
//! without repeating its body:
//!
//! - **The whole signal-then-record sequence runs under the slot lock.** The
//!   idempotence in `next_step` is the first defence against signalling a pid
//!   that is no longer ours; the lock is the second, and there is no third. A
//!   pid is only certainly ours while the locked slot still holds the
//!   [`tokio::process::Child`] un-reaped — the kernel keeps the pid reserved
//!   until `wait`. Retirement transfers it under that lock to one detached
//!   owner, then keeps the slot reserved until successful wait and registered
//!   writer settlement. Waiters never hold the slot lock across that barrier.
//! - **Belief is recorded only after the operation succeeds.** A failed
//!   `kill(2)` returns the error and changes nothing — a producer recorded as
//!   stopped that is actually running produces past every horizon; one
//!   recorded as running that is actually stopped never resumes.
//!
//! The `touch` hook is the motion clock. `apply_ahead_window` calls
//! `progress.touch()` before its suspension flag flips, so a watchdog can
//! never observe "running" beside a motion clock that spans time the process
//! was not scheduled — that read would fail a healthy session at the moment of
//! its resume. This layer keeps the same order — signal, touch, belief — on
//! both edges, and never touches when it did not signal: a repeated `Stop`
//! against an already-stopped belief must not reset a watchdog's clock, for
//! the same reason `apply_ahead_window`'s `want_suspend == suspended` guard
//! exists. The hook is a closure so the caller can pass its own clock in
//! without this module learning anything about session internals.
//!
//! [`Step::Terminate`] and [`Step::Restart`] send SIGKILL and await the owned
//! child reaper and registered writer barrier. A stopped process does not run a
//! `SIGTERM` handler until something continues it, so terminating a suspended
//! producer politely is a wait that never ends. What this layer cannot do is
//! spawn — the command line belongs to the caller — so [`Step::Start`] and the
//! respawn half of [`Step::Restart`] come back as [`Performed::NeedsSpawn`],
//! and [`ProducerSlot::attach`] records the spawn once it has happened.

use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex, Weak,
    },
};

use tokio::process::Child;
use tokio::sync::{Mutex, Notify};

use crate::prodexec::{after, Producer, Step, Termination};

/// Identity minted only while attaching an actual job-owned producer.
/// It has no wire representation and cannot be constructed from an epoch.
#[derive(Clone)]
pub(crate) struct ProducerRegistration(Arc<GenerationLifetime>);

struct GenerationLifetime {
    identity: Arc<()>,
    writers: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    receipt: StdMutex<Option<ConfirmedProducerReap>>,
    settled: Notify,
}

/// Owned only by the actual stdout/diagnostic task. Dropping without settlement
/// deliberately leaves retirement unavailable, rather than certifying a panic.
pub(crate) struct ProducerWriters(tokio::sync::oneshot::Sender<()>);
impl ProducerWriters {
    pub(crate) fn settled(self) {
        let _ = self.0.send(());
    }
}

/// Minted inside the detached process owner after successful wait and joined
/// writers. It proves one producer generation, not viewer or DB retirement.
#[derive(Clone)]
pub(crate) struct ConfirmedProducerReap(Arc<()>);
impl ConfirmedProducerReap {
    pub(crate) fn matches(&self, generation: &ProducerRegistration) -> bool {
        Arc::ptr_eq(&self.0, &generation.0.identity)
    }
}
impl ProducerRegistration {
    pub(crate) fn same_generation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    /// A detached viewer can retain its exact old generation even after the
    /// slot moves on. Cancellation drops only the waiter, never the reaper.
    pub(crate) async fn wait_confirmed_reap(&self) -> ConfirmedProducerReap {
        loop {
            let changed = self.0.settled.notified();
            if let Some(receipt) = self.confirmed_reap() {
                return receipt;
            }
            changed.await;
        }
    }
    pub(crate) fn confirmed_reap(&self) -> Option<ConfirmedProducerReap> {
        self.0
            .receipt
            .lock()
            .expect("producer receipt lock")
            .clone()
    }
}

struct ReapOperation {
    finished: AtomicBool,
    changed: Notify,
}
impl ReapOperation {
    async fn wait(&self) {
        loop {
            let changed = self.changed.notified();
            if self.finished.load(Ordering::Acquire) {
                return;
            }
            changed.await;
        }
    }
}

/// The fault/await seam uses the same owned child in production and tests.
pub(crate) trait ProducerReapHooks: Send + Sync {
    fn wait<'a>(
        &'a self,
        child: &'a mut Child,
    ) -> Pin<Box<dyn Future<Output = io::Result<std::process::ExitStatus>> + Send + 'a>>;
}
struct ActualProducerWait;
impl ProducerReapHooks for ActualProducerWait {
    fn wait<'a>(
        &'a self,
        child: &'a mut Child,
    ) -> Pin<Box<dyn Future<Output = io::Result<std::process::ExitStatus>> + Send + 'a>> {
        Box::pin(child.wait())
    }
}

fn owned_reap(
    mut child: Child,
    child_job: Option<crate::process_control::ChildJob>,
    resources: Option<Box<dyn Send>>,
    generation: Option<ProducerRegistration>,
    hooks: Arc<dyn ProducerReapHooks>,
    inner: Weak<Mutex<Inner>>,
    next: Producer,
) -> Arc<ReapOperation> {
    let operation = Arc::new(ReapOperation {
        finished: AtomicBool::new(false),
        changed: Notify::new(),
    });
    let completed = Arc::clone(&operation);
    let _ = child.start_kill();
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move{
            // One owned task retains admission and descendant ownership across
            // retries and cancellation. Errors never mint a reap receipt.
            let mut failures=0u64;
            loop {
                match hooks.wait(&mut child).await {
                    Ok(_)=>break,
                    Err(error)=>{
                        if failures==0 {tracing::warn!(target:"plurxd::prodrun",%error,"producer wait failed; admission retained");}
                        failures=failures.saturating_add(1);
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        let _=child.start_kill();
                    }
                }
            }
            if let Some(generation)=generation.as_ref(){
                let receiver=generation.0.writers.lock().await.take();
                if let Some(receiver)=receiver {
                    if receiver.await.is_err(){
                        tracing::error!(target:"plurxd::prodrun","producer writer task did not confirm settlement; admission retained");
                        std::future::pending::<()>().await;
                    }
                }
            }
            drop(child_job);
            drop(resources);
            if let Some(generation)=generation {
                *generation.0.receipt.lock().expect("producer receipt lock")=Some(ConfirmedProducerReap(Arc::clone(&generation.0.identity)));
                generation.0.settled.notify_waiters();
            }
            if let Some(inner)=inner.upgrade(){
                let mut state=inner.lock().await;
                if state.reaping.as_ref().is_some_and(|active|Arc::ptr_eq(active,&completed)){
                    state.reaping=None;
                    state.registration=None;
                    state.belief=next;
                }
            }
            completed.finished.store(true,Ordering::Release);
            completed.changed.notify_waiters();
        });
    } else {
        // Runtime shutdown cannot confirm a wait. Retain the bounded owner's
        // resources until process exit; never advertise an unproven release.
        std::mem::forget((child, child_job, resources, generation));
    }
    operation
}

/// One producer slot: the child (if any) and the recorded belief about it.
///
/// The lock is the second defence named above; every operation holds it
/// across the whole signal-then-record sequence, so no step can act on a
/// child another step is in the middle of reaping.
pub struct ProducerSlot {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    child: Option<Child>,
    /// Windows descendant ownership follows the exact child through reap.
    /// On Unix this is a zero-sized proof that the shared spawn path was used.
    child_job: Option<crate::process_control::ChildJob>,
    /// Capacity follows the exact child, not its pipe reader or epoch.
    resources: Option<Box<dyn Send>>,
    belief: Producer,
    registration: Option<ProducerRegistration>,
    reaping: Option<Arc<ReapOperation>>,
    hooks: Arc<dyn ProducerReapHooks>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            owned_reap(
                child,
                self.child_job.take(),
                self.resources.take(),
                self.registration.take(),
                Arc::clone(&self.hooks),
                Weak::new(),
                self.belief,
            );
        }
    }
}

/// What the caller must do because this layer cannot: spawn a new child.
#[derive(Debug, PartialEq, Eq)]
pub enum Performed {
    /// The step is fully carried out and the belief already reflects it.
    Done,
    /// The old child (if any) is killed and reaped, and the slot is empty.
    /// The caller spawns a producer positioned at plan index `at`, then calls
    /// [`ProducerSlot::attach`]. Until it does, the belief honestly says
    /// absent — a spawn that has not happened is not recorded.
    NeedsSpawn { at: u32 },
}

impl ProducerSlot {
    /// An empty slot: no child, and the belief [`prodexec`](crate::prodexec)
    /// starts from — absent, having produced nothing.
    pub fn new() -> ProducerSlot {
        ProducerSlot {
            inner: Arc::new(Mutex::new(Inner {
                child: None,
                child_job: None,
                resources: None,
                registration: None,
                reaping: None,
                hooks: Arc::new(ActualProducerWait),
                belief: Producer::Absent {
                    produced_through: None,
                },
            })),
        }
    }

    /// Nonblocking diagnostic read; contention means unknown, never authority.
    pub fn try_belief(&self) -> Option<Producer> {
        self.inner.try_lock().ok().map(|state| state.belief)
    }

    /// What the executor currently believes about the producer process.
    pub async fn belief(&self) -> Producer {
        self.inner.lock().await.belief
    }

    #[cfg(test)]
    async fn owns_child_job(&self) -> bool {
        self.inner.lock().await.child_job.is_some()
    }

    /// Attach a freshly spawned child positioned at `at` — the caller's half
    /// of a [`Performed::NeedsSpawn`]. Records the belief via [`after`], as
    /// the successful completion of the `Start` this spawn is.
    #[cfg(test)]
    pub async fn attach(&self, child: Child, at: u32) {
        self.attach_owned(child, at, None).await;
    }

    /// Retain admission through confirmed termination, including slot drop.
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn attach_owned(&self, child: Child, at: u32, resources: Option<Box<dyn Send>>) {
        self.attach_resources(child, None, at, resources).await;
    }

    /// Attach a child together with its descendant-lifetime guard.
    #[cfg(test)]
    pub async fn attach_job_owned(
        &self,
        child: Child,
        child_job: crate::process_control::ChildJob,
        at: u32,
        resources: Option<Box<dyn Send>>,
    ) {
        self.attach_resources(child, Some(child_job), at, resources)
            .await;
    }

    pub(crate) async fn attach_registered_job_owned(
        &self,
        child: Child,
        child_job: crate::process_control::ChildJob,
        at: u32,
        resources: Option<Box<dyn Send>>,
    ) -> (ProducerRegistration, ProducerWriters) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let registration = ProducerRegistration(Arc::new(GenerationLifetime {
            identity: Arc::new(()),
            writers: Mutex::new(Some(receiver)),
            receipt: StdMutex::new(None),
            settled: Notify::new(),
        }));
        self.attach_resources_registered(
            child,
            Some(child_job),
            at,
            resources,
            Some(registration.clone()),
        )
        .await;
        (registration, ProducerWriters(sender))
    }
    #[cfg(test)]
    pub(crate) async fn set_reap_hooks(&self, hooks: Arc<dyn ProducerReapHooks>) {
        self.inner.lock().await.hooks = hooks;
    }

    async fn attach_resources(
        &self,
        child: Child,
        child_job: Option<crate::process_control::ChildJob>,
        at: u32,
        resources: Option<Box<dyn Send>>,
    ) {
        self.attach_resources_registered(child, child_job, at, resources, None)
            .await;
    }
    async fn attach_resources_registered(
        &self,
        child: Child,
        child_job: Option<crate::process_control::ChildJob>,
        at: u32,
        resources: Option<Box<dyn Send>>,
        registration: Option<ProducerRegistration>,
    ) {
        loop {
            let mut inner = self.inner.lock().await;
            if let Some(reaping) = inner.reaping.clone() {
                drop(inner);
                reaping.wait().await;
                continue;
            }
            assert!(
                inner.child.is_none(),
                "attach expects the empty slot NeedsSpawn left behind"
            );
            inner.child = Some(child);
            inner.child_job = child_job;
            inner.resources = resources;
            inner.registration = registration;
            inner.belief = after(inner.belief, Step::Start { at });
            return;
        }
    }

    /// Record produced-through progress from a running generation's sink.
    ///
    /// Only a producer believed `Running` advances — progress reported by a
    /// generation the executor no longer believes in (killed, reaped, or
    /// replaced) must not resurrect a belief `after` already settled. The
    /// value only ever moves forward, because a generation's segments leave
    /// the segmenter in plan order and a late report must not walk the
    /// frontier back.
    pub async fn produced(&self, through: u32) {
        let mut inner = self.inner.lock().await;
        if let Producer::Running {
            produced_through, ..
        } = &mut inner.belief
        {
            *produced_through = Some(produced_through.map_or(through, |sofar| sofar.max(through)));
        }
    }

    /// Perform one step. Signals are sent under the slot lock; belief is
    /// updated only after the operation succeeds, and only through [`after`].
    ///
    /// `touch` is the caller's motion clock (`progress.touch()` on the live
    /// path). It runs after a successful SIGSTOP or SIGCONT and before the
    /// belief flips — a failed operation must not move the clock, and no
    /// observer may see the new belief beside a clock that predates it — and
    /// it never runs when nothing was signalled.
    ///
    /// A signalling step with no child attached is an error and leaves the
    /// belief exactly as it was: the belief and the slot disagreeing is the
    /// caller's bug, and recording an operation that did not happen would
    /// paper over it in the worst possible way.
    pub async fn perform(&self, step: Step, touch: impl FnOnce()) -> io::Result<Performed> {
        self.perform_for_generation(None, step, touch).await
    }
    /// Start exact-generation retirement without waiting for its own writers.
    /// Returns whether this call acquired the actual process for retirement.
    pub(crate) async fn request_registered_retirement(
        &self,
        generation: &ProducerRegistration,
    ) -> io::Result<bool> {
        let mut state = self.inner.lock().await;
        if !state
            .registration
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(&current.0, &generation.0))
        {
            return if generation.confirmed_reap().is_some() {
                Ok(false)
            } else {
                Err(no_child())
            };
        }
        if state.reaping.is_some() {
            return Ok(false);
        }
        let child = state.child.take().ok_or_else(no_child)?;
        let next = after(
            state.belief,
            Step::Terminate {
                why: Termination::Idle,
            },
        );
        let operation = owned_reap(
            child,
            state.child_job.take(),
            state.resources.take(),
            state.registration.clone(),
            Arc::clone(&state.hooks),
            Arc::downgrade(&self.inner),
            next,
        );
        state.reaping = Some(operation);
        Ok(true)
    }
    pub(crate) async fn wait_registered_retirement(
        &self,
        generation: &ProducerRegistration,
    ) -> io::Result<ConfirmedProducerReap> {
        loop {
            if let Some(receipt) = generation.confirmed_reap() {
                return Ok(receipt);
            }
            let state = self.inner.lock().await;
            if !state
                .registration
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(&current.0, &generation.0))
            {
                return Err(no_child());
            }
            let operation = state.reaping.clone().ok_or_else(no_child)?;
            drop(state);
            operation.wait().await;
        }
    }
    async fn perform_for_generation(
        &self,
        generation: Option<&ProducerRegistration>,
        step: Step,
        touch: impl FnOnce(),
    ) -> io::Result<Performed> {
        let mut inner = loop {
            let state = self.inner.lock().await;
            if let Some(generation) = generation {
                if !state
                    .registration
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(&current.0, &generation.0))
                {
                    return Err(no_child());
                }
            }
            if let Some(reaping) = state.reaping.clone() {
                drop(state);
                reaping.wait().await;
                continue;
            }
            break state;
        };
        match step {
            // Nothing about the process changes; the belief routing still
            // goes through `after` so this file decides nothing.
            Step::Nothing | Step::MakeRoom { .. } | Step::Report { .. } => {
                inner.belief = after(inner.belief, step);
                Ok(Performed::Done)
            }

            Step::Start { at } => {
                if inner.child.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Start against a slot that already holds a child",
                    ));
                }
                // The belief stays absent until `attach` records the spawn.
                Ok(Performed::NeedsSpawn { at })
            }

            Step::Stop => {
                // `next_step` never asks to stop a stopped producer; this
                // guard is the same second defence `apply_ahead_window`'s
                // `want_suspend == suspended` check is. No signal, and no
                // touch — a repeat must not reset a watchdog's motion clock.
                if matches!(inner.belief, Producer::Stopped { .. }) {
                    inner.belief = after(inner.belief, step);
                    return Ok(Performed::Done);
                }
                crate::process_control::signal(
                    attached_pid(&inner)?,
                    crate::process_control::ProcessSignal::Suspend,
                )?;
                touch();
                inner.belief = after(inner.belief, step);
                Ok(Performed::Done)
            }

            Step::Resume => {
                // Symmetric with `Stop`: already running means no signal.
                if matches!(inner.belief, Producer::Running { .. }) {
                    inner.belief = after(inner.belief, step);
                    return Ok(Performed::Done);
                }
                crate::process_control::signal(
                    attached_pid(&inner)?,
                    crate::process_control::ProcessSignal::Resume,
                )?;
                // Clock first, belief second, exactly as `apply_ahead_window`
                // resumes: the watchdog must never observe "running" beside a
                // motion clock that still spans the suspension.
                touch();
                inner.belief = after(inner.belief, step);
                Ok(Performed::Done)
            }

            Step::Terminate { .. } | Step::Restart { .. } => {
                let next = match step {
                    Step::Restart { .. } => after(
                        inner.belief,
                        Step::Terminate {
                            why: Termination::Idle,
                        },
                    ),
                    _ => after(inner.belief, step),
                };
                let Some(child) = inner.child.take() else {
                    return match step {
                        Step::Restart { at } => Ok(Performed::NeedsSpawn { at }),
                        _ => Err(no_child()),
                    };
                };
                let operation = owned_reap(
                    child,
                    inner.child_job.take(),
                    inner.resources.take(),
                    inner.registration.clone(),
                    Arc::clone(&inner.hooks),
                    Arc::downgrade(&self.inner),
                    next,
                );
                inner.reaping = Some(Arc::clone(&operation));
                drop(inner);
                operation.wait().await;
                Ok(match step {
                    Step::Restart { at } => Performed::NeedsSpawn { at },
                    _ => Performed::Done,
                })
            }
        }
    }
}

impl Default for ProducerSlot {
    fn default() -> ProducerSlot {
        ProducerSlot::new()
    }
}

/// The pid of the attached child, or the error every signalling step answers
/// when the slot is empty. `Child::id` is `None` once the child has been
/// waited, which for this slot means the same thing: nothing to signal.
fn attached_pid(inner: &Inner) -> io::Result<u32> {
    inner
        .child
        .as_ref()
        .and_then(|child| child.id())
        .ok_or_else(no_child)
}

fn no_child() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "no child is attached to this producer slot",
    )
}

#[cfg(all(test, unix))]
mod tests {
    #[tokio::test]
    async fn a_vod_generation_carries_a_child_job_until_reap() {
        let slot = super::ProducerSlot::new();
        let child = sleeper();
        let child_job = crate::process_control::ChildJob::attach(&child).expect("child job");
        slot.attach_job_owned(child, child_job, 0, None).await;
        assert!(slot.owns_child_job().await);
        slot.perform(
            super::Step::Terminate {
                why: super::Termination::Idle,
            },
            || {},
        )
        .await
        .expect("reap exact producer");
        assert!(!slot.owns_child_job().await);
    }

    #[tokio::test]
    async fn encoded_capacity_is_released_only_after_slot_reap() {
        let admissions = crate::admission::Admissions::new();
        let slot = super::ProducerSlot::new();
        let child = sleeper();
        let pid = child.id().expect("child pid");
        let permit = admissions
            .try_admit_software(2, 2, crate::admission::Priority::Live)
            .expect("encoder permit");
        slot.attach_owned(child, 0, Some(Box::new(permit))).await;
        assert_eq!(admissions.software_in_use(), 2);
        slot.perform(
            super::Step::Terminate {
                why: super::Termination::Idle,
            },
            || {},
        )
        .await
        .expect("reap exact producer");
        assert_eq!(admissions.software_in_use(), 0);
        assert!(is_reaped(pid).await);
    }

    #[tokio::test]
    async fn dropping_encoded_slot_transfers_capacity_to_its_reaper() {
        let admissions = crate::admission::Admissions::new();
        let slot = super::ProducerSlot::new();
        let child = sleeper();
        let pid = child.id().expect("child pid");
        let permit = admissions
            .try_admit_software(2, 2, crate::admission::Priority::Live)
            .expect("encoder permit");
        slot.attach_owned(child, 0, Some(Box::new(permit))).await;
        drop(slot);
        // Drop transfers the child and permit together; cancellation of its
        // former task cannot advertise capacity before wait has completed.
        tokio::time::timeout(Duration::from_secs(5), async {
            while admissions.software_in_use() > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned reaper releases capacity");
        assert!(is_reaped(pid).await);
    }

    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    use std::time::Duration;

    use crate::prodsched::Hold;

    /// A real child that will sit quietly until killed — the whole point of
    /// these tests is observing real process states, not a mock's.
    fn sleeper() -> Child {
        let mut command = tokio::process::Command::new("sleep");
        command.arg("300").kill_on_drop(true);
        command.spawn().expect("spawn sleep")
    }

    /// The process state letter from `/proc/<pid>/stat` — field three, read
    /// from after the closing paren so a comm with spaces cannot shift it.
    #[cfg(target_os = "linux")]
    async fn proc_state(pid: u32) -> Option<char> {
        let stat = tokio::fs::read_to_string(format!("/proc/{pid}/stat"))
            .await
            .ok()?;
        let (_, rest) = stat.rsplit_once(')')?;
        rest.split_whitespace().next()?.chars().next()
    }

    /// Darwin has no procfs. Its `ps` state column uses the same leading
    /// process-state letters these assertions need (`T`, `S`, and `Z`).
    #[cfg(target_os = "macos")]
    async fn proc_state(pid: u32) -> Option<char> {
        let pid = pid.to_string();
        let output = tokio::process::Command::new("ps")
            .args(["-o", "state=", "-p", &pid])
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .chars()
            .next()
    }

    /// Signal delivery is fast but not instant; poll briefly rather than
    /// asserting against a race with the kernel.
    async fn settles_into(pid: u32, wanted: char) -> bool {
        for _ in 0..500 {
            if proc_state(pid).await == Some(wanted) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    async fn is_reaped(pid: u32) -> bool {
        // After `wait` the kernel forgets the pid entirely — no zombie left.
        for _ in 0..500 {
            let state = proc_state(pid).await;
            if state.is_none() || state == Some('Z') {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    fn stop() -> Step {
        Step::Stop
    }

    fn terminate() -> Step {
        Step::Terminate {
            why: Termination::Idle,
        }
    }

    #[tokio::test]
    async fn a_real_child_is_stopped_resumed_and_reaped_through_the_slot() {
        let slot = ProducerSlot::new();
        let child = sleeper();
        let pid = child.id().expect("a fresh child has a pid");
        slot.attach(child, 0).await;
        assert_eq!(
            slot.belief().await,
            Producer::Running {
                produced_through: None,
                positioned_at: 0
            }
        );

        // Suspend: the process actually enters the stopped state.
        assert_eq!(
            slot.perform(stop(), || {}).await.expect("stop"),
            Performed::Done
        );
        assert!(settles_into(pid, 'T').await, "the child never stopped");
        assert!(matches!(slot.belief().await, Producer::Stopped { .. }));

        // Resume: the process actually leaves it.
        assert_eq!(
            slot.perform(Step::Resume, || {}).await.expect("resume"),
            Performed::Done
        );
        assert!(settles_into(pid, 'S').await, "the child never resumed");
        assert!(matches!(slot.belief().await, Producer::Running { .. }));

        // Terminate: killed, reaped, and remembered as absent.
        assert_eq!(
            slot.perform(terminate(), || {}).await.expect("terminate"),
            Performed::Done
        );
        assert!(is_reaped(pid).await, "the child was never reaped");
        assert_eq!(
            slot.belief().await,
            Producer::Absent {
                produced_through: None
            }
        );
    }

    #[tokio::test]
    async fn a_terminate_reaches_a_child_that_is_stopped() {
        // The reason the step is SIGKILL at all: a stopped process does not
        // run a SIGTERM handler, so this is the case a graceful signal loses.
        let slot = ProducerSlot::new();
        let child = sleeper();
        let pid = child.id().expect("pid");
        slot.attach(child, 0).await;
        slot.perform(stop(), || {}).await.expect("stop");
        assert!(settles_into(pid, 'T').await);

        slot.perform(terminate(), || {}).await.expect("terminate");
        assert!(is_reaped(pid).await, "a stopped child must still die");
        assert_eq!(
            slot.belief().await,
            Producer::Absent {
                produced_through: None
            }
        );
    }

    #[tokio::test]
    async fn a_restart_reaps_the_old_child_and_asks_the_caller_to_spawn() {
        let slot = ProducerSlot::new();
        let child = sleeper();
        let pid = child.id().expect("pid");
        slot.attach(child, 0).await;

        let performed = slot
            .perform(Step::Restart { at: 300 }, || {})
            .await
            .expect("restart");
        assert_eq!(performed, Performed::NeedsSpawn { at: 300 });
        assert!(is_reaped(pid).await, "the old child must be gone first");
        // Honest in the gap: nothing is running until the caller spawns.
        assert_eq!(
            slot.belief().await,
            Producer::Absent {
                produced_through: None
            }
        );

        slot.attach(sleeper(), 300).await;
        assert_eq!(
            slot.belief().await,
            Producer::Running {
                produced_through: None,
                positioned_at: 300
            }
        );
        slot.perform(terminate(), || {}).await.expect("cleanup");
    }

    #[tokio::test]
    async fn a_signalling_step_with_no_child_is_an_error_that_leaves_belief_alone() {
        let slot = ProducerSlot::new();
        let before = slot.belief().await;
        let touched = AtomicUsize::new(0);

        for step in [stop(), Step::Resume, terminate()] {
            let error = slot
                .perform(step, || {
                    touched.fetch_add(1, Relaxed);
                })
                .await
                .expect_err("nothing to signal");
            assert_eq!(error.kind(), io::ErrorKind::NotFound, "{step:?}");
        }
        assert_eq!(slot.belief().await, before, "belief must not be corrupted");
        assert_eq!(
            touched.load(Relaxed),
            0,
            "a failed operation must not move the motion clock"
        );
    }

    #[tokio::test]
    async fn a_second_stop_signals_nothing_and_does_not_move_the_clock() {
        // `next_step` answers `Nothing` for a stopped producer being stopped;
        // if a `Stop` arrives anyway, this layer must neither re-signal nor
        // reset the clock — the exact discipline behind `apply_ahead_window`'s
        // `want_suspend == suspended` guard.
        let slot = ProducerSlot::new();
        let child = sleeper();
        let pid = child.id().expect("pid");
        slot.attach(child, 0).await;

        let touched = AtomicUsize::new(0);
        let touch = || {
            touched.fetch_add(1, Relaxed);
        };
        slot.perform(stop(), touch).await.expect("first stop");
        assert!(settles_into(pid, 'T').await);
        assert_eq!(touched.load(Relaxed), 1, "the first stop touches");

        let belief = slot.belief().await;
        slot.perform(stop(), touch).await.expect("second stop");
        assert_eq!(slot.belief().await, belief, "belief must not flip");
        assert_eq!(touched.load(Relaxed), 1, "the second stop must not touch");
        assert!(settles_into(pid, 'T').await, "still stopped, still ours");

        slot.perform(terminate(), || {}).await.expect("cleanup");
    }

    #[tokio::test]
    async fn the_clock_is_touched_on_both_edges_and_before_the_belief_flips() {
        let slot = ProducerSlot::new();
        slot.attach(sleeper(), 0).await;

        let touched = AtomicUsize::new(0);
        let touch = || {
            touched.fetch_add(1, Relaxed);
        };
        slot.perform(stop(), touch).await.expect("stop");
        assert_eq!(touched.load(Relaxed), 1);
        slot.perform(Step::Resume, touch).await.expect("resume");
        assert_eq!(touched.load(Relaxed), 2);

        slot.perform(terminate(), || {}).await.expect("cleanup");
    }

    #[tokio::test]
    async fn steps_that_do_not_touch_the_process_still_route_belief_through_after() {
        let slot = ProducerSlot::new();
        for step in [
            Step::Nothing,
            Step::MakeRoom { wanted: 1_000 },
            Step::Report {
                hold: Hold::NoRoom { wanted: 1_000 },
            },
        ] {
            assert_eq!(
                slot.perform(step, || {}).await.expect("no-op"),
                Performed::Done,
                "{step:?}"
            );
        }
        assert_eq!(
            slot.belief().await,
            Producer::Absent {
                produced_through: None
            }
        );
    }

    #[tokio::test]
    async fn a_start_asks_the_caller_to_spawn_and_records_nothing_until_attach() {
        let slot = ProducerSlot::new();
        assert_eq!(
            slot.perform(Step::Start { at: 7 }, || {})
                .await
                .expect("start"),
            Performed::NeedsSpawn { at: 7 }
        );
        assert_eq!(
            slot.belief().await,
            Producer::Absent {
                produced_through: None
            },
            "a spawn that has not happened is not recorded"
        );

        slot.attach(sleeper(), 7).await;
        assert_eq!(
            slot.belief().await,
            Producer::Running {
                produced_through: None,
                positioned_at: 7
            }
        );
        slot.perform(terminate(), || {}).await.expect("cleanup");
    }

    #[tokio::test]
    async fn a_start_against_an_occupied_slot_is_refused() {
        let slot = ProducerSlot::new();
        slot.attach(sleeper(), 0).await;
        let error = slot
            .perform(Step::Start { at: 0 }, || {})
            .await
            .expect_err("the slot already holds a child");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(matches!(slot.belief().await, Producer::Running { .. }));
        slot.perform(terminate(), || {}).await.expect("cleanup");
    }
}
