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
//!   until `wait`, and the only `wait` is in here, under the same lock.
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
//! [`Step::Terminate`] and [`Step::Restart`] are `Child::kill`, which is
//! SIGKILL and a reap. Not a graceful signal: a stopped process does not run a
//! `SIGTERM` handler until something continues it, so terminating a suspended
//! producer politely is a wait that never ends. What this layer cannot do is
//! spawn — the command line belongs to the caller — so [`Step::Start`] and the
//! respawn half of [`Step::Restart`] come back as [`Performed::NeedsSpawn`],
//! and [`ProducerSlot::attach`] records the spawn once it has happened.

use std::io;

use tokio::process::Child;
use tokio::sync::Mutex;

use crate::prodexec::{after, Producer, Step, Termination};

/// One producer slot: the child (if any) and the recorded belief about it.
///
/// The lock is the second defence named above; every operation holds it
/// across the whole signal-then-record sequence, so no step can act on a
/// child another step is in the middle of reaping.
pub struct ProducerSlot {
    inner: Mutex<Inner>,
}

struct Inner {
    child: Option<Child>,
    belief: Producer,
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
            inner: Mutex::new(Inner {
                child: None,
                belief: Producer::Absent {
                    produced_through: None,
                },
            }),
        }
    }

    /// What the executor currently believes about the producer process.
    pub async fn belief(&self) -> Producer {
        self.inner.lock().await.belief
    }

    /// Attach a freshly spawned child positioned at `at` — the caller's half
    /// of a [`Performed::NeedsSpawn`]. Records the belief via [`after`], as
    /// the successful completion of the `Start` this spawn is.
    pub async fn attach(&self, child: Child, at: u32) {
        let mut inner = self.inner.lock().await;
        debug_assert!(
            inner.child.is_none(),
            "attach expects the empty slot NeedsSpawn left behind"
        );
        inner.child = Some(child);
        inner.belief = after(inner.belief, Step::Start { at });
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
        let mut inner = self.inner.lock().await;
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
                signal(attached_pid(&inner)?, libc::SIGSTOP)?;
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
                signal(attached_pid(&inner)?, libc::SIGCONT)?;
                // Clock first, belief second, exactly as `apply_ahead_window`
                // resumes: the watchdog must never observe "running" beside a
                // motion clock that still spans the suspension.
                touch();
                inner.belief = after(inner.belief, step);
                Ok(Performed::Done)
            }

            Step::Terminate { .. } => {
                let child = inner.child.as_mut().ok_or_else(no_child)?;
                // SIGKILL and a reap. `Child::kill` is `start_kill` then
                // `wait`, and SIGKILL still works on a stopped process.
                child.kill().await?;
                inner.child = None;
                inner.belief = after(inner.belief, step);
                Ok(Performed::Done)
            }

            Step::Restart { at } => {
                if let Some(child) = inner.child.as_mut() {
                    child.kill().await?;
                    inner.child = None;
                }
                // This layer performs only the terminate half of a restart;
                // the start half is the caller's spawn, recorded by `attach`.
                // `after` reads nothing from the `why`, so `Idle` here records
                // exactly what happened either way: the process is gone, its
                // progress remembered. Composed with `attach`'s `Start { at }`
                // this lands on the same belief `after(_, Restart { at })`
                // answers — but only once the spawn is real.
                inner.belief = after(
                    inner.belief,
                    Step::Terminate {
                        why: Termination::Idle,
                    },
                );
                Ok(Performed::NeedsSpawn { at })
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

fn signal(pid: u32, signal: libc::c_int) -> io::Result<()> {
    // SAFETY: `kill(2)` with a pid this slot owns and a signal constant. The
    // child is held un-reaped under the slot lock, so the pid cannot have
    // been recycled; a race with its exit yields ESRCH, which the return
    // check turns into an error the caller keeps its old belief over.
    if unsafe { libc::kill(pid as libc::pid_t, signal) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
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
