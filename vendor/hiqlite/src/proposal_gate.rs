//! Plurx patch 30: one gate in front of every database Raft proposal on this
//! node.
//!
//! Every leader-side append on the SQLite Raft — `client_write` from the local
//! client paths and from the API handlers that serve forwarded writes, and the
//! membership calls — takes a shared [`ProposalPermit`] first, through
//! [`crate::app_state::StateRaftDB`]'s helpers. A stopping or handing-off
//! leader takes the exclusive side with [`ProposalGate::hold_until`]: proposals
//! already in flight finish first, and new ones wait (they are not failed)
//! until the [`ProposalHold`] drops. With nothing new appended, a successor can
//! match this leader's last log entry, so its campaign is not refused for being
//! one entry behind.
//!
//! The lock is Tokio's `RwLock`, which queues readers and writers in FIFO
//! order: once the exclusive acquirer waits, later proposals queue behind it,
//! so a steady stream of writers cannot starve it.

use std::sync::Arc;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use tokio::time::Instant;

#[derive(Debug, Default)]
pub(crate) struct ProposalGate {
    lock: Arc<RwLock<()>>,
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

impl ProposalGate {
    pub(crate) async fn permit(&self) -> ProposalPermit {
        ProposalPermit {
            _permit: Arc::clone(&self.lock).read_owned().await,
        }
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

    /// The gate holds only if nothing proposes around it: every database Raft
    /// proposal in this crate goes through `StateRaftDB`'s three helpers, and
    /// those helpers are the only callers of the raw OpenRaft methods.
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
                if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
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
                for call in ["client_write(", "change_membership(", "add_learner("] {
                    assert!(
                        !source.contains(&format!("raft_db.raft.{call}")),
                        "{} proposes to the database Raft around the gate: {call}",
                        path.display()
                    );
                    if path.ends_with("app_state.rs") {
                        let raw = format!("self.raft.{call}");
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
}
