//! Plurx patch 31: one gate in front of every database Raft proposal on this
//! node.
//!
//! Every leader-side append on the SQLite Raft — `client_write` from the local
//! client paths and from the API handlers that serve forwarded writes, and the
//! membership calls — takes a shared [`ProposalPermit`] first, through
//! [`crate::app_state::StateRaftDB`]'s helpers. A stopping or handing-off
//! leader takes the exclusive side with [`ProposalGate::hold_until`]: it waits
//! for the permits of proposals in flight, and new ones wait (they are not
//! failed) until the [`ProposalHold`] drops. A caller cancelled after it sent
//! its proposal to Raft drops its permit while the entry may still be queued
//! in the Raft core, so the hold alone does not prove the log has stopped; the
//! handoff's match check against this leader's last log index covers that.
//!
//! The lock is Tokio's `RwLock`, which queues readers and writers in FIFO
//! order: once the exclusive acquirer waits, later proposals queue behind it,
//! so a steady stream of writers cannot starve it.

use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use tokio::time::Instant;

#[derive(Debug, Default)]
pub(crate) struct ProposalGate {
    lock: Arc<RwLock<()>>,
    membership: Arc<RwLock<()>>,
    membership_signal: Arc<MembershipSignal>,
}

/// Held for the whole of one proposal. Dropping it, on any path, releases it.
#[derive(Debug)]
pub(crate) struct ProposalPermit {
    _permit: OwnedRwLockReadGuard<()>,
}

/// While this lives, no new database proposal starts on this node. It releases
/// on drop: success, error, timeout and cancellation alike.
#[derive(Debug)]
pub struct ProposalHold {
    _hold: OwnedRwLockWriteGuard<()>,
}

/// Owned by a real local membership proposal, never an RPC observer.
pub(crate) struct MembershipPermit {
    _permit: OwnedRwLockReadGuard<()>,
}

#[derive(Debug, Default)]
struct MembershipSignal {
    state: Mutex<MembershipSignalState>,
    changed: Notify,
}

#[derive(Debug, Default)]
struct MembershipSignalState {
    generation: u64,
    active: bool,
    stopped: bool,
}

#[derive(Clone)]
pub(crate) struct MembershipStop {
    signal: Arc<MembershipSignal>,
    generation: u64,
}

impl MembershipStop {
    pub(crate) fn is_stopped(&self) -> bool {
        let Ok(state) = self.signal.state.lock() else {
            return true;
        };
        state.generation != self.generation || !state.active || state.stopped
    }

    pub(crate) async fn stopped(&self) {
        loop {
            let mut changed = std::pin::pin!(self.signal.changed.notified());
            changed.as_mut().enable();
            if self.is_stopped() {
                return;
            }
            changed.await;
        }
    }
}

pub(crate) struct MembershipHold {
    _hold: OwnedRwLockWriteGuard<()>,
    stop: MembershipStop,
}

impl MembershipHold {
    pub(crate) fn stop(&self) -> MembershipStop {
        self.stop.clone()
    }
}

impl Drop for MembershipHold {
    fn drop(&mut self) {
        if let Ok(mut state) = self.stop.signal.state.lock() {
            if state.generation == self.stop.generation {
                state.active = false;
                self.stop.signal.changed.notify_waiters();
            }
        }
    }
}

impl ProposalGate {
    /// Exclude only effective local membership proposals. The ordinary
    /// database proposal and incoming consensus paths remain independent.
    pub(crate) fn try_hold_membership(&self) -> Option<MembershipHold> {
        let mut state = self.membership_signal.state.lock().ok()?;
        let hold = Arc::clone(&self.membership).try_write_owned().ok()?;
        let generation = state.generation.checked_add(1)?;
        *state = MembershipSignalState {
            generation,
            active: true,
            stopped: false,
        };
        Some(MembershipHold {
            _hold: hold,
            stop: MembershipStop {
                signal: Arc::clone(&self.membership_signal),
                generation,
            },
        })
    }

    pub(crate) fn try_membership_permit(&self) -> Result<MembershipPermit, crate::Error> {
        // Enrollment and an immediately dispatched membership proposal are
        // serialized here. A refused caller can retry outside its management
        // locks, with its membership admission still unconsumed.
        let mut state = self
            .membership_signal
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(permit) = Arc::clone(&self.membership).try_read_owned() {
            return Ok(MembershipPermit { _permit: permit });
        }
        if state.active {
            state.stopped = true;
            self.membership_signal.changed.notify_waiters();
        }
        Err(crate::Error::RequestNotDispatched(
            "local playback still owns membership exclusion".into(),
        ))
    }

    pub(crate) async fn permit(&self) -> ProposalPermit {
        ProposalPermit {
            _permit: Arc::clone(&self.lock).read_owned().await,
        }
    }

    /// Sample an idle proposal boundary without queuing ahead of any writer.
    pub(crate) fn try_hold(&self) -> Option<ProposalHold> {
        let hold = Arc::clone(&self.lock).try_write_owned().ok()?;
        Some(ProposalHold { _hold: hold })
    }

    /// Take the exclusive side by `deadline`, or not at all. A proposal that is
    /// in flight when quorum is lost can hang; the caller then proceeds
    /// without a hold instead of waiting past its budget.
    pub(crate) async fn hold_until(&self, deadline: Instant) -> Option<ProposalHold> {
        tokio::time::timeout_at(deadline, Arc::clone(&self.lock).write_owned())
            .await
            .ok()
            .map(|hold| ProposalHold { _hold: hold })
    }
}

#[cfg(test)]
mod tests {
    use super::ProposalGate;
    use std::time::Duration;
    use tokio::time::Instant;

    #[tokio::test(start_paused = true)]
    async fn a_refused_membership_proposal_stops_the_original_without_blocking_sql() {
        let gate = ProposalGate::default();
        let hold = gate.try_hold_membership().expect("original authority");
        let stop = hold.stop();
        assert!(!stop.is_stopped());
        assert!(matches!(
            gate.try_membership_permit(),
            Err(crate::Error::RequestNotDispatched(_))
        ));
        tokio::time::timeout(Duration::from_millis(20), stop.stopped())
            .await
            .expect("membership contention signals the original");
        let sql = tokio::time::timeout(Duration::from_millis(20), gate.permit())
            .await
            .expect("same original can submit durable close");
        assert!(gate.try_hold_membership().is_none());

        // The request's signal is not evidence of closure.
        assert!(matches!(
            gate.try_membership_permit(),
            Err(crate::Error::RequestNotDispatched(_))
        ));
        drop(hold);
        let membership = gate.try_membership_permit().expect("retry after closure");
        assert!(gate.try_hold_membership().is_none());
        drop(membership);
        drop(sql);
        assert!(
            !gate
                .try_hold_membership()
                .expect("next authority")
                .stop()
                .is_stopped()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_hold_waits_for_in_flight_proposals_and_blocks_new_ones() {
        let gate = ProposalGate::default();
        let in_flight = gate.permit().await;
        assert!(
            gate.hold_until(Instant::now() + Duration::from_millis(50))
                .await
                .is_none(),
            "a hold cannot start while a proposal is in flight"
        );
        drop(in_flight);
        let hold = gate
            .hold_until(Instant::now() + Duration::from_millis(50))
            .await
            .expect("the hold starts once the in-flight proposal ends");
        let waiting = tokio::time::timeout(Duration::from_secs(1), gate.permit()).await;
        assert!(waiting.is_err(), "a new proposal waits while the hold lives");
        drop(hold);
        tokio::time::timeout(Duration::from_secs(1), gate.permit())
            .await
            .expect("dropping the hold lets proposals continue");
    }

    #[tokio::test(start_paused = true)]
    async fn a_stuck_proposal_bounds_the_hold_to_its_deadline() {
        let gate = ProposalGate::default();
        let _stuck = gate.permit().await;
        let started = Instant::now();
        let hold = tokio::time::timeout(
            Duration::from_secs(10),
            gate.hold_until(started + Duration::from_secs(3)),
        )
        .await
        .expect("a hold attempt behind a stuck proposal must end at its deadline");
        assert!(hold.is_none());
        assert_eq!(started.elapsed(), Duration::from_secs(3));
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_hold_attempt_and_a_dropped_hold_both_release() {
        let gate = std::sync::Arc::new(ProposalGate::default());
        let attempt = {
            let gate = std::sync::Arc::clone(&gate);
            tokio::spawn(async move {
                let _hold = gate.hold_until(Instant::now() + Duration::from_secs(60)).await;
                std::future::pending::<()>().await;
            })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        attempt.abort();
        let _ = attempt.await;
        tokio::time::timeout(Duration::from_secs(1), gate.permit())
            .await
            .expect("cancelling the holder releases the gate");
    }

    /// The gate holds only if nothing proposes around it. Every proposal call
    /// in this crate must name its receiver as the gated database helper
    /// (`raft_db.client_write(` and its two siblings), the ungated cache Raft
    /// (`raft_cache.raft.`), or, inside the helpers only, `self.raft.`. Any
    /// other receiver, such as a binding to the database `Raft`, fails here.
    #[test]
    fn every_database_proposal_goes_through_the_gate() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![root.clone()];
        let mut helper_calls = 0;
        let mut gated = 0;
        while let Some(directory) = stack.pop() {
            for entry in std::fs::read_dir(&directory).expect("source directory") {
                let path = entry.expect("source entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                // This file proposes nothing; its tests name the calls.
                if path.extension().and_then(|ext| ext.to_str()) != Some("rs")
                    || path.ends_with("proposal_gate.rs")
                {
                    continue;
                }
                let source: String = std::fs::read_to_string(&path)
                    .expect("readable source")
                    .lines()
                    .filter(|line| !line.trim_start().starts_with("//"))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .split_whitespace()
                    .collect();
                let helpers = path.ends_with("app_state.rs");
                for call in [".client_write(", ".change_membership(", ".add_learner("] {
                    for (at, _) in source.match_indices(call) {
                        let receiver = &source[..at];
                        let allowed = receiver.ends_with("raft_db")
                            || receiver.ends_with("raft_cache.raft")
                            || (helpers && receiver.ends_with("self.raft"));
                        assert!(
                            allowed,
                            "{} proposes around the gate: ...{}{call}",
                            path.display(),
                            &receiver[receiver.len().saturating_sub(40)..]
                        );
                    }
                    if helpers {
                        let raw = format!("self.raft{call}");
                        helper_calls += source.matches(&raw).count();
                        gated += source
                            .matches(&format!("self.proposals.permit().await;{raw}"))
                            .count();
                    }
                }
            }
        }
        assert_eq!(helper_calls, 3, "one raw call per helper");
        assert_eq!(gated, 3, "each helper takes its permit before it proposes");
    }

    /// A blocking learner add appends under the permit but waits for the
    /// learner to catch up after the permit drops, so a slow or dead learner
    /// cannot hold off a proposal hold.
    #[test]
    fn a_learner_catch_up_wait_holds_no_permit() {
        let source: String = include_str!("app_state.rs").split_whitespace().collect();
        let helper = source
            .split_once("pub(crate)asyncfnadd_learner(")
            .expect("the add_learner helper")
            .1;
        let append = helper
            .find("{let_permit=self.proposals.permit().await;self.raft.add_learner(id,node,false).await?};")
            .expect("the permit covers a non-blocking append only");
        let released = helper
            .find("drop(membership);")
            .expect("membership released");
        let wait = helper.find(".wait(None)").expect("the catch-up wait");
        assert!(
            append < released && released < wait,
            "both permits drop before catch-up"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_waiting_hold_is_not_starved_by_later_proposals() {
        let gate = std::sync::Arc::new(ProposalGate::default());
        let first = gate.permit().await;
        let holder = {
            let gate = std::sync::Arc::clone(&gate);
            tokio::spawn(async move {
                gate.hold_until(Instant::now() + Duration::from_secs(5))
                    .await
                    .is_some()
            })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        // A proposal arriving after the hold queued must wait behind it.
        let later = tokio::time::timeout(Duration::from_millis(100), gate.permit()).await;
        assert!(later.is_err(), "a later proposal must not overtake the hold");
        drop(first);
        assert!(holder.await.expect("holder task"), "the hold is granted");
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_queued_hold_releases_the_queue() {
        let gate = std::sync::Arc::new(ProposalGate::default());
        let in_flight = gate.permit().await;
        let queued = {
            let gate = std::sync::Arc::clone(&gate);
            tokio::spawn(async move {
                let _ = gate.hold_until(Instant::now() + Duration::from_secs(60)).await;
            })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        let later = {
            let gate = std::sync::Arc::clone(&gate);
            tokio::spawn(async move {
                let _permit = gate.permit().await;
            })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!later.is_finished(), "a later proposal queues behind the hold");
        queued.abort();
        let _ = queued.await;
        tokio::time::timeout(Duration::from_secs(1), later)
            .await
            .expect("cancelling the queued hold lets later proposals proceed")
            .expect("later proposal task");
        drop(in_flight);
    }

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_hold_leaves_the_gate_open() {
        let gate = ProposalGate::default();
        let stuck = gate.permit().await;
        assert!(gate
            .hold_until(Instant::now() + Duration::from_millis(100))
            .await
            .is_none());
        let started = Instant::now();
        let _next = tokio::time::timeout(Duration::from_secs(1), gate.permit())
            .await
            .expect("a timed-out hold leaves nothing queued in the gate");
        assert_eq!(
            started.elapsed(),
            Duration::ZERO,
            "after a timed-out hold, a new proposal starts at once"
        );
        drop(stuck);
    }
}
