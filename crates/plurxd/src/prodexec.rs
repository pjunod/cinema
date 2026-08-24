//! Turning a [`crate::prodsched::Action`] into something done to a process.
//!
//! [`crate::prodsched`] decides *what* a producer should do from demand and the
//! working set, and it is pure. This is the other half of that split: given
//! what the executor currently believes about the process, which single
//! operation carries the decision out. Also pure, for the same reason — every
//! rule here is about interleavings that are miserable to reproduce against a
//! real encoder and trivial to state as a table.
//!
//! Two facts from the existing daemon shape the whole module, and neither is
//! obvious from the scheduler's side.
//!
//! **A stopped ffmpeg still holds its hardware codec session.** That is
//! already written down in `transcode.rs`, where the producer yields to a live
//! viewer by terminating rather than stopping: "a stopped ffmpeg still holds
//! the hardware codec session, so the viewer this is yielding to would be
//! blocked by a process that is doing nothing." So SIGSTOP is only the right
//! answer for a hold that clears on its own and soon. [`Hold::Ahead`] does —
//! the reader advances and the producer picks up where it left off, which is
//! worth keeping the codec session for, because the alternative is a
//! reposition. [`Hold::WorkingSetFull`] and [`Hold::NoRoom`] do not: both are
//! cleared by something happening in *another* rendition, on no schedule this
//! producer controls. Holding a scarce hardware session indefinitely for a
//! process that is doing nothing, and that may never be resumed, is exactly
//! the failure that comment describes. Those two terminate.
//!
//! **A signal is sent to a pid, and pids are recycled.** Every transition here
//! is idempotent: a producer already stopped is never stopped again, a
//! producer already running is never continued. Not tidiness — a duplicate
//! SIGCONT is only harmless while the pid still belongs to us, and the window
//! where it does not is the window where the daemon has just reaped a child.
//! `transcode.rs` lists "SIGSTOP sent to the wrong pid" among the things that
//! look like a hung encoder, which is the expensive way to find this out.

// M3 attaches this to the transcode path; nothing outside the tests calls it
// yet. The allow comes out with those callers.
#![allow(dead_code)]

use crate::prodsched::{Action, Hold};

/// What the executor believes about the producer process right now.
///
/// Belief, not truth — the process can exit under any of these. That is why
/// [`Producer::Absent`] carries progress too: a producer that died having
/// written through segment 300 is not the same as one that never started, and
/// restarting the second at 301 would skip the first three hundred segments of
/// the film.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Producer {
    /// No process. Either never started, reclaimed, or exited.
    Absent { produced_through: Option<u32> },
    /// Running, spawned positioned at `positioned_at`.
    Running {
        produced_through: Option<u32>,
        positioned_at: u32,
    },
    /// SIGSTOPped, and still holding whatever the running process held —
    /// its file descriptors, its scratch, and its hardware codec session.
    Stopped {
        produced_through: Option<u32>,
        positioned_at: u32,
        reason: Hold,
    },
}

impl Producer {
    pub fn produced_through(&self) -> Option<u32> {
        match *self {
            Producer::Absent { produced_through }
            | Producer::Running {
                produced_through, ..
            }
            | Producer::Stopped {
                produced_through, ..
            } => produced_through,
        }
    }

    /// Where a live process was spawned. `None` when there is no process.
    ///
    /// Kept apart from `produced_through` for the reason `prodsched::Position`
    /// keeps them apart: a producer repositioned to 300 has produced nothing
    /// *yet*, and a decision that cannot tell that from "never produced
    /// anything" repositions it to 300 again, forever.
    pub fn positioned_at(&self) -> Option<u32> {
        match *self {
            Producer::Absent { .. } => None,
            Producer::Running { positioned_at, .. } | Producer::Stopped { positioned_at, .. } => {
                Some(positioned_at)
            }
        }
    }

    /// The next index this process will write if left alone.
    fn reach(&self) -> Option<u32> {
        match self.produced_through() {
            Some(through) => Some(through.saturating_add(1)),
            None => self.positioned_at(),
        }
    }
}

/// The one operation that carries a decision out.
///
/// Exactly one, never a list. A caller that had to run two of these in order
/// would have to decide what happens when the second fails, and the answer
/// would be a partially applied decision — a producer stopped for a hold that
/// was supposed to terminate it, holding a codec session nothing will release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The process is already in the state the decision asks for.
    Nothing,
    /// Spawn a producer positioned at this index.
    Start { at: u32 },
    /// SIGCONT.
    Resume,
    /// SIGSTOP. Only ever for a hold that clears on its own and soon.
    Stop,
    /// SIGKILL, and give up the codec session with it.
    ///
    /// `SIGKILL` rather than a graceful signal on purpose: a stopped process
    /// does not run a `SIGTERM` handler until something continues it, so
    /// terminating a suspended producer politely is a wait that never ends.
    Terminate { why: Termination },
    /// Kill and respawn positioned somewhere else. The scheduler already
    /// decided this beats reading forward.
    Restart { at: u32 },
    /// Free bytes before producing anything more. The process is left exactly
    /// as it is: eviction touches segments this producer is not writing, and
    /// stopping for a sweep that usually takes milliseconds is churn.
    MakeRoom { wanted: u64 },
    /// A stall worth saying out loud. Nothing about the process changes,
    /// because nothing about the process is the problem.
    Report { hold: Hold },
}

/// Why a producer is being killed rather than stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    /// Nothing is attached. Reclaim it.
    Idle,
    /// Held on a bound that only another rendition can clear, so the codec
    /// session it is sitting on has no scheduled end.
    IndefiniteHold,
}

/// Which holds are worth keeping a process — and a codec session — for.
///
/// [`Hold::Ahead`] clears when a reader advances, which happens by itself and
/// soon, and resuming costs nothing where restarting costs a reposition. The
/// other two are cleared by another rendition releasing bytes, on no schedule
/// this producer controls or can predict.
fn clears_on_its_own(hold: Hold) -> bool {
    match hold {
        Hold::Ahead { .. } => true,
        Hold::WorkingSetFull { .. } | Hold::NoRoom { .. } => false,
    }
}

/// The single operation that carries `action` out against `producer`.
pub fn next_step(producer: Producer, action: Action) -> Step {
    match action {
        Action::Idle => match producer {
            // Reclaimed rather than stopped. A producer nobody is attached to
            // has nothing to resume for, and a stopped one goes on holding
            // everything a running one held.
            Producer::Running { .. } | Producer::Stopped { .. } => Step::Terminate {
                why: Termination::Idle,
            },
            Producer::Absent { .. } => Step::Nothing,
        },

        Action::Produce { next } => match producer {
            Producer::Absent { .. } => Step::Start { at: next },
            // Already going. The scheduler names the next gap every time it is
            // asked; that is not an instruction to restart at it.
            Producer::Running { .. } => Step::Nothing,
            Producer::Stopped {
                produced_through, ..
            } => {
                // Resume only if this producer is actually positioned to reach
                // `next` by carrying on. It usually is — a producer stops
                // ahead of its readers, so the gap it wakes to is the one in
                // front of it. When it is not, continuing would have it write
                // forward from where it stopped and never reach the segment
                // that is owed.
                match produced_through {
                    Some(through) if next > through => Step::Resume,
                    // Never produced anything, so there is no "where it left
                    // off" to carry on from.
                    None if next == 0 => Step::Resume,
                    _ => Step::Restart { at: next },
                }
            }
        },

        Action::Reposition { to } => match producer {
            Producer::Absent { .. } => Step::Start { at: to },
            // Already there and yet to write anything. Restarting would kill a
            // process that is about to produce exactly what was asked for, and
            // the producer it spawns is in the identical state — which is a
            // respawn loop, not a reposition. This is why `positioned_at` is
            // tracked apart from `produced_through` at all.
            _ if producer.reach() == Some(to) && producer.produced_through().is_none() => {
                match producer {
                    Producer::Stopped { .. } => Step::Resume,
                    _ => Step::Nothing,
                }
            }
            // Kill and respawn. A stopped producer is killed outright rather
            // than continued first: `Step::Terminate` is SIGKILL, which a
            // stopped process cannot ignore, and continuing it beforehand
            // would let it write a fragment from the position it is about to
            // stop being at.
            Producer::Running { .. } | Producer::Stopped { .. } => Step::Restart { at: to },
        },

        Action::MakeRoom { wanted } => Step::MakeRoom { wanted },

        Action::Suspend { reason, .. } => {
            if clears_on_its_own(reason) {
                match producer {
                    Producer::Running { .. } => Step::Stop,
                    // Already stopped, and for a reason that will clear. Do
                    // not re-signal: the pid is only certainly ours while the
                    // process is certainly alive.
                    Producer::Stopped { .. } => Step::Nothing,
                    Producer::Absent { .. } => Step::Nothing,
                }
            } else {
                match producer {
                    // Including a producer already stopped for `Ahead`: the
                    // reason it is held has changed to one with no scheduled
                    // end, so the codec session has to go back.
                    Producer::Running { .. } | Producer::Stopped { .. } => Step::Terminate {
                        why: Termination::IndefiniteHold,
                    },
                    // Nothing to terminate. Say why anyway — a rendition that
                    // wants to produce and cannot is worth a line whether or
                    // not it has a process.
                    Producer::Absent { .. } => Step::Report { hold: reason },
                }
            }
        }
    }
}

/// What the executor should believe after `step` succeeds.
///
/// Separate from [`next_step`] so a caller that fails to send a signal keeps
/// its old belief rather than recording an operation that did not happen. A
/// producer recorded as stopped that is actually running produces past every
/// horizon; one recorded as running that is actually stopped never resumes.
pub fn after(producer: Producer, step: Step) -> Producer {
    let through = producer.produced_through();
    match step {
        Step::Nothing | Step::MakeRoom { .. } | Step::Report { .. } => producer,
        Step::Start { at } | Step::Restart { at } => Producer::Running {
            // A repositioned producer has not produced anything *in its new
            // run*, and saying otherwise would have the next decision measure
            // an ahead window from a place this process never wrote. Where it
            // is, is `positioned_at`; what it has made, is nothing.
            produced_through: None,
            positioned_at: at,
        },
        Step::Resume => Producer::Running {
            produced_through: through,
            positioned_at: producer.positioned_at().unwrap_or(0),
        },
        Step::Stop => Producer::Stopped {
            produced_through: through,
            positioned_at: producer.positioned_at().unwrap_or(0),
            // Only `Ahead` reaches `Step::Stop`; the others terminate.
            reason: Hold::Ahead { horizon: 0 },
        },
        Step::Terminate { .. } => Producer::Absent {
            produced_through: through,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ahead() -> Hold {
        Hold::Ahead { horizon: 26 }
    }

    fn full() -> Hold {
        Hold::WorkingSetFull { free_bytes: 5_000 }
    }

    fn no_room() -> Hold {
        Hold::NoRoom { wanted: 5_000 }
    }

    fn running(through: u32) -> Producer {
        Producer::Running {
            produced_through: Some(through),
            positioned_at: 0,
        }
    }

    fn stopped(through: u32, reason: Hold) -> Producer {
        Producer::Stopped {
            produced_through: Some(through),
            positioned_at: 0,
            reason,
        }
    }

    /// A live process positioned at `at` that has produced nothing yet — what
    /// a freshly repositioned producer actually is.
    fn positioned(at: u32) -> Producer {
        Producer::Running {
            produced_through: None,
            positioned_at: at,
        }
    }

    fn absent(through: Option<u32>) -> Producer {
        Producer::Absent {
            produced_through: through,
        }
    }

    // ---- the codec session ------------------------------------------------

    #[test]
    fn an_indefinite_hold_terminates_rather_than_stopping() {
        // A stopped ffmpeg still holds its hardware codec session, and both of
        // these holds are cleared by another rendition on no schedule this one
        // controls. Stopping would sit on a scarce session for a process that
        // is doing nothing and may never be resumed.
        for reason in [full(), no_room()] {
            assert_eq!(
                next_step(
                    running(40),
                    Action::Suspend {
                        produced_through: 40,
                        reason
                    }
                ),
                Step::Terminate {
                    why: Termination::IndefiniteHold
                },
                "{reason:?}"
            );
        }
    }

    #[test]
    fn an_ahead_hold_stops_because_a_reader_will_clear_it() {
        // This one clears by itself and soon, and resuming costs nothing where
        // restarting costs a reposition.
        assert_eq!(
            next_step(
                running(40),
                Action::Suspend {
                    produced_through: 40,
                    reason: ahead()
                }
            ),
            Step::Stop
        );
    }

    #[test]
    fn a_hold_that_turns_indefinite_gives_the_codec_back() {
        // Stopped for the ahead window, then the node fills up. The reason it
        // is held has changed to one with no scheduled end, so what was the
        // right answer a moment ago is now a process sitting on a codec
        // session nothing will release.
        assert_eq!(
            next_step(
                stopped(40, ahead()),
                Action::Suspend {
                    produced_through: 40,
                    reason: full()
                }
            ),
            Step::Terminate {
                why: Termination::IndefiniteHold
            }
        );
    }

    // ---- never signal twice -----------------------------------------------

    #[test]
    fn a_producer_already_stopped_is_not_stopped_again() {
        // A pid is only certainly ours while the process is certainly alive.
        assert_eq!(
            next_step(
                stopped(40, ahead()),
                Action::Suspend {
                    produced_through: 40,
                    reason: ahead()
                }
            ),
            Step::Nothing
        );
    }

    #[test]
    fn a_producer_already_running_is_not_started_again() {
        // The scheduler names the next gap every time it is asked. That is not
        // an instruction to restart at it.
        assert_eq!(
            next_step(running(40), Action::Produce { next: 41 }),
            Step::Nothing
        );
    }

    #[test]
    fn nothing_is_signalled_at_a_process_that_is_not_there() {
        assert_eq!(
            next_step(
                absent(Some(40)),
                Action::Suspend {
                    produced_through: 40,
                    reason: ahead()
                }
            ),
            Step::Nothing
        );
        assert_eq!(next_step(absent(None), Action::Idle), Step::Nothing);
    }

    #[test]
    fn a_stall_with_no_process_is_still_reported() {
        // A rendition that wants to produce and cannot is worth a line whether
        // or not it has a process to show for it.
        assert_eq!(
            next_step(
                absent(Some(40)),
                Action::Suspend {
                    produced_through: 40,
                    reason: no_room()
                }
            ),
            Step::Report { hold: no_room() }
        );
    }

    // ---- resuming vs restarting -------------------------------------------

    #[test]
    fn a_stopped_producer_resumes_when_the_gap_is_in_front_of_it() {
        assert_eq!(
            next_step(stopped(40, ahead()), Action::Produce { next: 41 }),
            Step::Resume
        );
    }

    #[test]
    fn a_stopped_producer_restarts_when_the_gap_is_behind_it() {
        // Continuing would have it write forward from 41 and never reach 12.
        // A reader blocked on 12 would wait for a producer that is running.
        assert_eq!(
            next_step(stopped(40, ahead()), Action::Produce { next: 12 }),
            Step::Restart { at: 12 }
        );
    }

    #[test]
    fn a_reposition_kills_from_either_state() {
        assert_eq!(
            next_step(running(40), Action::Reposition { to: 300 }),
            Step::Restart { at: 300 }
        );
        // Not continued first: SIGKILL cannot be ignored by a stopped process,
        // and continuing would let it write a fragment from the position it is
        // about to stop being at.
        assert_eq!(
            next_step(stopped(40, ahead()), Action::Reposition { to: 300 }),
            Step::Restart { at: 300 }
        );
    }

    // ---- reclaiming -------------------------------------------------------

    #[test]
    fn idle_reclaims_a_stopped_producer_too() {
        // Nothing to resume for, and a stopped process goes on holding
        // everything a running one held.
        assert_eq!(
            next_step(stopped(40, ahead()), Action::Idle),
            Step::Terminate {
                why: Termination::Idle
            }
        );
    }

    // ---- what the executor believes afterwards ----------------------------

    #[test]
    fn a_restart_forgets_progress_the_new_process_did_not_make() {
        // The old process produced through 40. The new one is positioned at
        // 300 and has written nothing. Carrying 40 forward would have the next
        // decision measure an ahead window from a place this process never
        // wrote, and `stop` would answer `Idle` for a producer that is running.
        let after_restart = after(running(40), Step::Restart { at: 300 });
        assert_eq!(after_restart, positioned(300));
    }

    #[test]
    fn a_producer_started_at_the_beginning_has_produced_nothing() {
        assert_eq!(after(absent(None), Step::Start { at: 0 }), positioned(0));
    }

    #[test]
    fn a_resume_keeps_the_progress_the_process_actually_made() {
        assert_eq!(after(stopped(40, ahead()), Step::Resume), running(40));
    }

    #[test]
    fn a_terminated_producer_is_remembered_as_having_got_somewhere() {
        // A producer that died having written through 300 is not the same as
        // one that never started. Restarting the second at 301 would skip the
        // first three hundred segments of the film.
        assert_eq!(
            after(
                running(300),
                Step::Terminate {
                    why: Termination::Idle
                }
            ),
            Producer::Absent {
                produced_through: Some(300)
            }
        );
    }

    #[test]
    fn a_step_that_changes_nothing_changes_nothing() {
        for step in [
            Step::Nothing,
            Step::MakeRoom { wanted: 1_000 },
            Step::Report { hold: no_room() },
        ] {
            assert_eq!(after(running(40), step), running(40), "{step:?}");
            assert_eq!(
                after(stopped(40, ahead()), step),
                stopped(40, ahead()),
                "{step:?}"
            );
        }
    }

    #[test]
    fn make_room_leaves_the_process_exactly_as_it_is() {
        // Eviction touches segments this producer is not writing, and stopping
        // for a sweep that usually takes milliseconds is churn.
        for producer in [running(40), stopped(40, ahead()), absent(Some(40))] {
            assert_eq!(
                next_step(producer, Action::MakeRoom { wanted: 2_000 }),
                Step::MakeRoom { wanted: 2_000 },
                "{producer:?}"
            );
        }
    }

    // ---- the loop converges -----------------------------------------------

    #[test]
    fn a_repositioned_producer_is_left_alone_to_reach_where_it_was_sent() {
        // The respawn loop, in one assertion. A producer sent to 300 has
        // produced nothing yet; an executor that reads only `produced_through`
        // cannot tell that from "never produced anything", kills the process
        // it just spawned, and spawns an identical one — forever, while a
        // reader waits on a segment nobody is making.
        assert_eq!(
            next_step(positioned(300), Action::Reposition { to: 300 }),
            Step::Nothing
        );
        // And a producer sent somewhere it has *not* reached still moves.
        assert_eq!(
            next_step(positioned(300), Action::Reposition { to: 12 }),
            Step::Restart { at: 12 }
        );
    }

    #[test]
    fn a_producer_stopped_before_it_ever_produced_resumes_rather_than_respawning() {
        // Positioned at 300, stopped for the ahead window before writing
        // anything, and now wanted at 300. Killing it would throw away a
        // process that is already exactly where it needs to be.
        let held = Producer::Stopped {
            produced_through: None,
            positioned_at: 300,
            reason: ahead(),
        };
        assert_eq!(
            next_step(held, Action::Reposition { to: 300 }),
            Step::Resume
        );
    }

    #[test]
    fn every_step_is_a_fixed_point_when_repeated() {
        // Applying a decision and asking again must not produce a second
        // operation for the same decision. Anything else is a signal storm
        // against a live encoder.
        let cases = [
            (running(40), Action::Produce { next: 41 }),
            (
                running(40),
                Action::Suspend {
                    produced_through: 40,
                    reason: ahead(),
                },
            ),
            (running(40), Action::Reposition { to: 300 }),
            (positioned(300), Action::Reposition { to: 300 }),
            (running(40), Action::Idle),
            (absent(None), Action::Produce { next: 0 }),
        ];
        for (producer, action) in cases {
            let first = next_step(producer, action);
            let settled = after(producer, first);
            let second = next_step(settled, action);
            assert_eq!(
                second,
                Step::Nothing,
                "{action:?} against {producer:?} asked for {first:?} and then {second:?}"
            );
        }
    }
}
