//! Plurx patch: pre-vote. See `engine/pre_vote.rs` and `docs/cluster/C4-PREVOTE-BUILD.md`.

use std::sync::Arc;
use std::time::Duration;

use maplit::btreeset;
use pretty_assertions::assert_eq;

use crate::core::ServerState;
use crate::engine::testing::UTConfig;
use crate::engine::time_state;
use crate::engine::Command;
use crate::engine::Engine;
use crate::engine::LogIdList;
use crate::raft::VoteRequest;
use crate::raft::VoteResponse;
use crate::raft_state::LogStateReader;
use crate::testing::log_id;
use crate::utime::UTime;
use crate::EffectiveMembership;
use crate::Membership;
use crate::TokioInstant;
use crate::Vote;

const LEASE: Duration = Duration::from_millis(4000);
const ELECTION_TIMEOUT: Duration = Duration::from_millis(3000);

fn m123() -> Membership<u64, ()> {
    Membership::<u64, ()>::new(vec![btreeset! {1,2,3}], None)
}

/// The deployed shape: three voters, node 2 leads term 5 (committed), every log ends at 5-2-10.
/// `heard_ago` is how long ago this node's vote was last touched by the leader.
fn node(id: u64, heard_ago: Duration, membership: Membership<u64, ()>) -> Engine<UTConfig> {
    let mut eng = Engine::<UTConfig>::default();
    eng.state.enable_validation(false);
    eng.config.id = id;
    eng.config.timer_config = time_state::Config {
        election_timeout: ELECTION_TIMEOUT,
        smaller_log_timeout: ELECTION_TIMEOUT * 2,
        leader_lease: LEASE,
    };
    eng.state.vote = UTime::new(ago(heard_ago), Vote::new_committed(5, 2));
    eng.state.server_state = if id == 2 { ServerState::Leader } else { ServerState::Follower };
    eng.state.log_ids = LogIdList::new(vec![log_id(1, 1, 1), log_id(5, 2, 10)]);
    eng.state.membership_state.set_effective(Arc::new(EffectiveMembership::new(Some(log_id(1, 1, 1)), membership)));
    eng.output.take_commands();
    eng
}

/// An instant `d` in the past. Stalls here are a few seconds, so a young monotonic clock is the
/// only way this can fail, and then it says so.
fn ago(d: Duration) -> TokioInstant {
    TokioInstant::now().checked_sub(d).expect("monotonic clock older than the simulated stall")
}

/// Longer than the 8 s committed-follower timer (4 s lease + up to 4 s election timeout).
const STALL: Duration = Duration::from_secs(9);

fn proposed_by_1() -> VoteRequest<u64> {
    VoteRequest::new(Vote::new(6, 1), Some(log_id(5, 2, 10)))
}

fn granted(eng: &Engine<UTConfig>) -> VoteResponse<u64> {
    VoteResponse::new(eng.state.vote_ref(), eng.state.last_log_id().cloned(), true)
}

#[test]
fn election_timeout_starts_a_pre_vote_and_changes_nothing() {
    // Unpatched 0.9.25 elects here: the term would already be 6.
    let mut eng = node(1, STALL, m123());

    eng.handle_election_timeout();

    assert_eq!(Vote::new_committed(5, 2), *eng.state.vote_ref());
    assert_eq!(ServerState::Follower, eng.state.server_state);
    assert!(eng.candidate_ref().is_none());
    assert_eq!(
        vec![Command::SendPreVote {
            vote_req: proposed_by_1()
        }],
        eng.output.take_commands()
    );
}

#[test]
fn the_measured_stall_cannot_depose_a_healthy_leader() {
    // 2026-11-23 20:23Z: voter 1 stalled (85 s measured) while leader 2 kept heartbeating voter 3.
    let mut stalled = node(1, STALL, m123());
    let leader = node(2, STALL, m123());
    let follower = node(3, Duration::from_millis(100), m123());

    stalled.handle_election_timeout();
    let Command::SendPreVote { vote_req } = stalled.output.take_commands().remove(0) else {
        panic!("expected a pre-vote");
    };

    // The leader refuses although its own vote time is old; the follower refuses on its lease.
    let from_leader = leader.handle_pre_vote_req(&vote_req);
    let from_follower = follower.handle_pre_vote_req(&vote_req);
    assert!(!from_leader.vote_granted);
    assert!(!from_follower.vote_granted);

    stalled.handle_pre_vote_resp(2, vote_req.vote, Some(from_leader));
    stalled.handle_pre_vote_resp(3, vote_req.vote, Some(from_follower));

    // No term change anywhere, so nothing can reach the leader to make it step down.
    assert_eq!(Vote::new_committed(5, 2), *stalled.state.vote_ref());
    assert_eq!(ServerState::Follower, stalled.state.server_state);
    assert!(stalled.candidate_ref().is_none());
    assert!(stalled.output.take_commands().is_empty());
    assert_eq!(Vote::new_committed(5, 2), *leader.state.vote_ref());
    assert_eq!(Vote::new_committed(5, 2), *follower.state.vote_ref());
}

#[test]
fn a_heartbeat_during_the_round_makes_it_stale() {
    let mut eng = node(1, STALL, m123());
    eng.handle_election_timeout();
    eng.output.take_commands();

    // The leader's queued append is processed after the resume: it touches the vote.
    eng.vote_handler().update_vote(&Vote::new_committed(5, 2)).unwrap();
    eng.output.take_commands();

    let grant = VoteResponse::new(Vote::new_committed(5, 2), Some(log_id(5, 2, 10)), true);
    eng.handle_pre_vote_resp(3, Vote::new(6, 1), Some(grant.clone()));
    eng.handle_pre_vote_resp(2, Vote::new(6, 1), Some(grant));

    assert!(eng.pre_vote.is_none());
    assert_eq!(Vote::new_committed(5, 2), *eng.state.vote_ref());
    assert!(eng.output.take_commands().is_empty());
}

#[test]
fn a_peer_grants_only_without_a_lease_with_a_good_log_and_vote_and_changes_nothing() {
    let expired = STALL;

    let peer = node(3, expired, m123());
    let utime = peer.state.vote_last_modified();
    let resp = peer.handle_pre_vote_req(&proposed_by_1());
    assert_eq!(granted(&peer), resp);
    assert_eq!(utime, peer.state.vote_last_modified(), "a pre-vote must not touch the timer");
    assert_eq!(Vote::new_committed(5, 2), *peer.state.vote_ref());

    let leased = node(3, Duration::from_millis(100), m123());
    assert!(!leased.handle_pre_vote_req(&proposed_by_1()).vote_granted);

    let behind = VoteRequest::new(Vote::new(6, 1), Some(log_id(5, 2, 9)));
    assert!(!peer.handle_pre_vote_req(&behind).vote_granted);

    let smaller = VoteRequest::new(Vote::new(4, 1), Some(log_id(5, 2, 10)));
    assert!(!peer.handle_pre_vote_req(&smaller).vote_granted);

    let mut leader = node(2, expired, m123());
    assert!(!leader.handle_pre_vote_req(&proposed_by_1()).vote_granted);
    leader.state.vote = UTime::new(ago(expired), Vote::new_committed(5, 1));
    leader.state.server_state = ServerState::Follower;
    assert!(leader.handle_pre_vote_req(&proposed_by_1()).vote_granted, "a follower again");
}

#[test]
fn a_quorum_of_grants_elects_as_before() {
    // Real leader loss: every lease expired.
    let mut eng = node(1, STALL, m123());
    eng.handle_election_timeout();
    eng.output.take_commands();

    let peer = node(3, STALL, m123());
    let resp = peer.handle_pre_vote_req(&proposed_by_1());
    eng.handle_pre_vote_resp(3, Vote::new(6, 1), Some(resp));

    assert!(eng.pre_vote.is_none());
    assert_eq!(Vote::new(6, 1), *eng.state.vote_ref());
    assert_eq!(ServerState::Candidate, eng.state.server_state);
    let commands = eng.output.take_commands();
    assert!(commands.contains(&Command::SaveVote { vote: Vote::new(6, 1) }));
    assert!(commands.contains(&Command::SendVote {
        vote_req: proposed_by_1()
    }));
}

#[test]
fn a_peer_that_cannot_answer_counts_as_a_grant_unless_it_is_the_followed_leader() {
    let mut eng = node(1, STALL, m123());
    eng.handle_election_timeout();
    eng.output.take_commands();

    // Node 2 is the committed leader node 1 last followed: an older leader that is alive would
    // refuse, so "cannot answer" from it is no grant.
    eng.handle_pre_vote_resp(2, Vote::new(6, 1), None);
    assert_eq!(Vote::new_committed(5, 2), *eng.state.vote_ref());

    eng.handle_pre_vote_resp(3, Vote::new(6, 1), None);
    assert_eq!(Vote::new(6, 1), *eng.state.vote_ref());
}

#[test]
fn rounds_are_paced_by_the_election_timeout() {
    let mut eng = node(1, STALL, m123());
    eng.handle_election_timeout();
    assert_eq!(1, eng.output.take_commands().len());

    eng.handle_election_timeout();
    assert!(eng.output.take_commands().is_empty(), "a round is still in flight");

    eng.pre_vote.as_mut().unwrap().started = ago(ELECTION_TIMEOUT + Duration::from_millis(1));
    eng.handle_election_timeout();
    assert_eq!(
        vec![Command::SendPreVote {
            vote_req: proposed_by_1()
        }],
        eng.output.take_commands()
    );
}

#[test]
fn a_joint_membership_needs_a_quorum_of_each_config() {
    let joint = Membership::<u64, ()>::new(vec![btreeset! {1,2,3}, btreeset! {3,4,5}], None);
    let mut eng = node(1, STALL, joint);
    eng.handle_election_timeout();
    eng.output.take_commands();
    let grant = || Some(VoteResponse::new(Vote::new_committed(5, 2), Some(log_id(5, 2, 10)), true));

    eng.handle_pre_vote_resp(2, Vote::new(6, 1), grant());
    assert_eq!(Vote::new_committed(5, 2), *eng.state.vote_ref(), "{{1,2}} is a quorum of one config only");

    eng.handle_pre_vote_resp(3, Vote::new(6, 1), grant());
    assert_eq!(Vote::new_committed(5, 2), *eng.state.vote_ref());

    eng.handle_pre_vote_resp(4, Vote::new(6, 1), grant());
    assert_eq!(Vote::new(6, 1), *eng.state.vote_ref());
}

#[test]
fn answers_from_another_round_or_a_non_voter_are_ignored() {
    let mut eng = node(1, STALL, m123());
    eng.handle_election_timeout();
    eng.output.take_commands();
    let grant = || Some(VoteResponse::new(Vote::new_committed(5, 2), Some(log_id(5, 2, 10)), true));

    eng.handle_pre_vote_resp(2, Vote::new(7, 1), grant());
    eng.handle_pre_vote_resp(9, Vote::new(6, 1), grant());

    assert_eq!(Vote::new_committed(5, 2), *eng.state.vote_ref());
    assert!(eng.pre_vote.is_some());
}

#[test]
fn a_triggered_election_ends_the_round() {
    // Hiqlite patch 4 / C2 shutdown handoff call `elect()` directly.
    let mut eng = node(1, STALL, m123());
    eng.handle_election_timeout();
    eng.output.take_commands();

    eng.elect();

    assert!(eng.pre_vote.is_none());
    assert_eq!(Vote::new(6, 1), *eng.state.vote_ref());
}

#[test]
fn a_peer_past_this_term_is_adopted_so_two_survivors_cannot_livelock() {
    // Review finding: leader 2 is gone. Node 3 lost a campaign at term 6 and holds the
    // uncommitted vote (6, 3) with a shorter log; node 1 holds (5, 2) committed, lease expired,
    // with the longer log. Without adopting 3's term, 1 proposes (6, 1) forever (incomparable with
    // (6, 3)) and 3's (7, 3) is refused for its log: nobody is ever elected.
    let mut a = node(1, STALL, m123());
    let mut b = node(3, STALL, m123());
    b.state.vote = UTime::new(ago(STALL), Vote::new(6, 3));
    b.state.server_state = ServerState::Candidate;
    b.state.log_ids = LogIdList::new(vec![log_id(1, 1, 1), log_id(5, 2, 9)]);

    a.handle_election_timeout();
    let Command::SendPreVote { vote_req } = a.output.take_commands().remove(0) else {
        panic!("expected a pre-vote");
    };
    let refusal = b.handle_pre_vote_req(&vote_req);
    assert!(!refusal.vote_granted);
    a.handle_pre_vote_resp(3, vote_req.vote, Some(refusal));

    assert_eq!(Vote::new(6, 3), *a.state.vote_ref(), "adopted, non-committed");
    assert!(a.pre_vote.is_none());
    a.output.take_commands();

    a.handle_election_timeout();
    let Command::SendPreVote { vote_req } = a.output.take_commands().remove(0) else {
        panic!("expected a second pre-vote");
    };
    assert_eq!(Vote::new(7, 1), vote_req.vote);
    let grant = b.handle_pre_vote_req(&vote_req);
    assert!(grant.vote_granted, "(7, 1) outranks (6, 3) and 1's log is longer");
    a.handle_pre_vote_resp(3, vote_req.vote, Some(grant));

    assert_eq!(Vote::new(7, 1), *a.state.vote_ref());
    assert_eq!(ServerState::Candidate, a.state.server_state);
}

#[test]
fn a_refusal_from_a_longer_log_delays_the_next_election() {
    let mut eng = node(1, STALL, m123());
    eng.handle_election_timeout();
    eng.output.take_commands();
    assert!(!eng.is_there_greater_log());

    let refusal = VoteResponse::new(Vote::new_committed(5, 2), Some(log_id(5, 2, 11)), false);
    eng.handle_pre_vote_resp(3, Vote::new(6, 1), Some(refusal));

    assert!(eng.is_there_greater_log());
    assert_eq!(Vote::new_committed(5, 2), *eng.state.vote_ref());
}

/// The body of `fn name` in `source`, up to the next item at the same indentation.
fn body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source.find(signature).unwrap_or_else(|| panic!("{signature} not found"));
    let rest = &source[start..];
    let end = rest[1..].find("\n    fn ").or_else(|| rest[1..].find("\n    async fn ")).map_or(rest.len(), |i| i + 1);
    &rest[..end]
}

#[test]
fn the_election_tick_goes_through_pre_vote() {
    // Reverting the tick to a direct `elect()` must fail a test: the engine tests above call
    // `handle_election_timeout` themselves and would not notice.
    let core = include_str!("../../core/raft_core.rs");
    let tick = body(core, "fn handle_tick_election(&mut self)");
    assert!(tick.contains("self.engine.handle_election_timeout();"));
    assert_eq!(
        1,
        tick.matches("self.engine.elect();").count(),
        "only the single-voter branch elects directly"
    );
    assert!(tick.contains("voter_ids().count() == 1"));
}

#[test]
fn an_in_flight_round_stops_when_elections_are_disabled() {
    let core = include_str!("../../core/raft_core.rs");
    let arm = &core[core.find("\"received Notify::PreVoteResponse\"").expect("pre-vote notify arm")..];
    let arm = &arm[..arm.find("Notify::HigherVote {").expect("next notify arm")];
    assert!(arm.contains("enable_elect.load(Ordering::Relaxed)"));
    assert!(arm.contains("is_voter(&self.id)"));
    assert!(arm.contains("self.engine.pre_vote = None;"));
}
