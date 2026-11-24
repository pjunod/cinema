//! Plurx patch: pre-vote before a timer-driven election.
//!
//! OpenRaft 0.9.25 raises its term as soon as its election timer expires. A
//! voter whose runtime was stalled observes an expired timer on resume even
//! though the leader stayed healthy. Its higher vote then reaches the leader on
//! an append response, and the leader steps down. Pre-vote asks the voters
//! whether they *would* grant `term + 1` before anything changes; only a quorum
//! of yes answers leads to the unchanged `elect()`.
//!
//! See `vendor/openraft/PLURX-PATCH.md` and `docs/cluster/C4-PREVOTE-BUILD.md`.

use crate::display_ext::DisplayOptionExt;
use crate::engine::Command;
use crate::engine::Engine;
use crate::progress::Progress;
use crate::proposer::Candidate;
use crate::proposer::LeaderQuorumSet;
use crate::raft::VoteRequest;
use crate::raft::VoteResponse;
use crate::raft_state::LogStateReader;
use crate::summary::MessageSummary;
use crate::type_config::alias::InstantOf;
use crate::type_config::TypeConfigExt;
use crate::LogId;
use crate::RaftTypeConfig;
use crate::Vote;

/// One pre-vote round: the vote it proposes, the local state it started from, and the grants.
///
/// The round never changes engine state. It becomes stale, and is discarded, as soon as the local
/// vote, its modification time or the effective membership differ from what the round started
/// with: for example when a heartbeat from a live leader arrives while the round is in flight.
#[derive(Debug)]
pub(crate) struct PreVoteRound<C>
where C: RaftTypeConfig
{
    /// Counts grants for the proposed vote against the effective joint quorum set.
    pub(crate) candidate: Candidate<C, LeaderQuorumSet<C::NodeId>>,
    pub(crate) base_vote: Vote<C::NodeId>,
    pub(crate) base_vote_utime: Option<InstantOf<C>>,
    pub(crate) base_membership: Option<LogId<C::NodeId>>,
    pub(crate) started: InstantOf<C>,
}

impl<C> Engine<C>
where C: RaftTypeConfig
{
    /// The election timer expired on a voter of a multi-voter membership.
    ///
    /// Start a pre-vote round unless one started less than an election timeout ago. A failed round
    /// changes no state and therefore does not reset the vote timer, so this is what paces retries.
    pub(crate) fn handle_election_timeout(&mut self) {
        let now = C::now();
        if let Some(round) = &self.pre_vote {
            if now < round.started + self.config.timer_config.election_timeout {
                tracing::debug!("pre-vote round still in flight; not starting another");
                return;
            }
        }

        let proposed = Vote::new(self.state.vote_ref().leader_id().term + 1, self.config.id.clone());
        let last_log_id = self.state.last_log_id().cloned();
        let effective = self.state.membership_state.effective().clone();
        let membership = effective.membership();

        let mut candidate = Candidate::new(
            now,
            proposed.clone(),
            last_log_id.clone(),
            membership.to_quorum_set(),
            membership.learner_ids(),
        );

        if candidate.grant_by(&self.config.id) {
            // This node alone is a quorum: nothing to ask.
            self.pre_vote = None;
            self.reset_greater_log();
            self.elect();
            return;
        }

        tracing::info!(proposed = display(&proposed), "election timeout: starting a pre-vote round");

        self.pre_vote = Some(PreVoteRound {
            candidate,
            base_vote: self.state.vote_ref().clone(),
            base_vote_utime: self.state.vote_last_modified(),
            base_membership: effective.log_id().clone(),
            started: now,
        });
        self.output.push_command(Command::SendPreVote {
            vote_req: VoteRequest::new(proposed, last_log_id),
        });
    }

    /// Answer a pre-vote: would this node grant `req.vote` now? Changes no state.
    ///
    /// The checks are those of [`Engine::handle_vote_req`] without its vote update, plus one more:
    /// a leader never grants, because its own vote time is not refreshed by its heartbeats and so
    /// says nothing about whether it is alive.
    pub(crate) fn handle_pre_vote_req(&self, req: &VoteRequest<C::NodeId>) -> VoteResponse<C::NodeId> {
        let reject = || VoteResponse::new(self.state.vote_ref(), self.state.last_log_id().cloned(), false);

        if self.leader.is_some() || self.state.is_leader(&self.config.id) {
            tracing::info!(req = display(req.summary()), "reject pre-vote: this node is the leader");
            return reject();
        }

        let vote = self.state.vote_ref();
        if vote.is_committed() {
            let now = C::now();
            let lease = self.config.timer_config.leader_lease;
            let live = self.state.vote_last_modified().is_some_and(|utime| now <= utime + lease);
            if live {
                tracing::info!(req = display(req.summary()), "reject pre-vote: leader lease is live");
                return reject();
            }
        }

        if !(req.last_log_id.as_ref() >= self.state.last_log_id()) {
            tracing::info!(
                req = display(req.summary()),
                my_last_log_id = display(self.state.last_log_id().display()),
                "reject pre-vote: candidate log is behind"
            );
            return reject();
        }

        if !(&req.vote >= vote) {
            tracing::info!(req = display(req.summary()), my_vote = display(vote), "reject pre-vote: smaller vote");
            return reject();
        }

        VoteResponse::new(vote, self.state.last_log_id().cloned(), true)
    }

    /// A pre-vote answer arrived. `resp` is `None` when the target cannot answer pre-vote (an older
    /// peer); that counts as a grant so an older peer never blocks an election it would allow.
    pub(crate) fn handle_pre_vote_resp(
        &mut self,
        target: C::NodeId,
        sender_vote: Vote<C::NodeId>,
        resp: Option<VoteResponse<C::NodeId>>,
    ) {
        let Some(round) = self.pre_vote.as_ref() else {
            return;
        };
        if round.candidate.vote_ref() != &sender_vote {
            // An answer to an earlier round.
            return;
        }

        let membership_log_id = self.state.membership_state.effective().log_id().clone();
        if self.state.vote_ref() != &round.base_vote
            || self.state.vote_last_modified() != round.base_vote_utime
            || membership_log_id != round.base_membership
        {
            tracing::info!("pre-vote round is stale: local vote or membership changed since it started");
            self.pre_vote = None;
            return;
        }

        let granted = match &resp {
            None => true,
            Some(resp) => resp.vote_granted,
        };
        if !granted {
            if let Some(resp) = &resp {
                if resp.last_log_id.as_ref() > self.state.last_log_id() {
                    self.set_greater_log();
                }
            }
            tracing::info!(target = display(&target), "pre-vote rejected");
            return;
        }

        let round = self.pre_vote.as_mut().expect("checked above");
        if round.candidate.progress().try_get(&target).is_none() {
            return;
        }
        if round.candidate.grant_by(&target) {
            tracing::info!("a quorum would grant the vote: electing");
            self.pre_vote = None;
            self.reset_greater_log();
            self.elect();
        }
    }
}
