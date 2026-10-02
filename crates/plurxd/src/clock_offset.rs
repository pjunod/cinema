//! One serialized full-roster clock observation owner per daemon.
use crate::state::AppState;
use futures_util::{stream, StreamExt};
use plurx_core::cluster::clock::{ClockDecisionTicket, PeerClockOffset, CLOCK_OBSERVATION_MAX_AGE};
use plurx_core::cluster::membership::{
    normalize_internal_http_base, ClockLeadershipIdentity, ClockPeerRoster, MembershipManager,
};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, Semaphore};

const CLOCK_FILTER_DEPTH: usize = 8;
const CLOCK_PROBE_MAX_RTT_MS: i64 = 2_000;
const CLOCK_RTT_QUANTIZATION_FLOOR_US: i64 = 1_000;
const CLOCK_PROBE_INTERVAL: Duration = Duration::from_secs(10);
const CLOCK_ROSTER_BUDGET: Duration = Duration::from_secs(2);
const CLOCK_WAITER_LIMIT: usize = 64;

type PeerDirectory = BTreeMap<String, (u64, String)>;

fn peer_directory(roster: &ClockPeerRoster) -> Option<PeerDirectory> {
    let mut directory = BTreeMap::new();
    for peer in &roster.peers {
        let origin = normalize_internal_http_base(peer.http_base.as_deref()?)?;
        if peer.raft_id == 0
            || directory
                .insert(peer.node_id.clone(), (peer.raft_id, origin))
                .is_some()
        {
            return None;
        }
    }
    Some(directory)
}

#[derive(Clone)]
struct CompletedRound {
    leadership: Option<ClockLeadershipIdentity>,
    directory: PeerDirectory,
    ticket: ClockDecisionTicket,
}

#[derive(Clone, Default)]
struct Completion {
    sequence: u64,
    round: Option<CompletedRound>,
}

struct OwnerControl {
    requests: Option<mpsc::Receiver<()>>,
    running: bool,
}

struct ObserverInner {
    membership: MembershipManager,
    transport: crate::http::peer_transport::PeerTransport,
    requests: mpsc::Sender<()>,
    completion: watch::Sender<Completion>,
    control: Mutex<OwnerControl>,
    waiters: Semaphore,
}

/// The handle is retained across pending-to-normal HTTP activation. Only one
/// caller can claim its receiver, so periodic and demanded work share filters
/// and never publish concurrently. A timed-out waiter does not own the round.
#[derive(Clone)]
pub(crate) struct ClockObserver(Arc<ObserverInner>);

#[derive(Clone)]
pub(crate) struct LearnerClockBarrier {
    leadership: ClockLeadershipIdentity,
    directory: PeerDirectory,
    learner_id: String,
    clock_generation: u64,
}

impl ClockObserver {
    pub(crate) fn new(membership: MembershipManager) -> Self {
        let (requests, receiver) = mpsc::channel(1);
        let (completion, _) = watch::channel(Completion::default());
        Self(Arc::new(ObserverInner {
            transport: crate::http::peer_transport::PeerTransport::new(membership.clone()),
            membership,
            requests,
            completion,
            control: Mutex::new(OwnerControl {
                requests: Some(receiver),
                running: false,
            }),
            waiters: Semaphore::new(CLOCK_WAITER_LIMIT),
        }))
    }

    pub(crate) fn belongs_to(&self, membership: &MembershipManager) -> bool {
        Arc::ptr_eq(&self.0.membership.clock_guard(), &membership.clock_guard())
    }

    fn claim_receiver(&self) -> Option<mpsc::Receiver<()>> {
        self.0
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .requests
            .take()
    }

    fn request_round(&self) -> Result<(), ()> {
        let control = self
            .0
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if control.running {
            return Ok(());
        }
        match self.0.requests.try_send(()) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(())) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(())) => Err(()),
        }
    }

    fn begin_round(&self, requests: &mut mpsc::Receiver<()>) {
        let mut control = self
            .0
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        control.running = true;
        // A periodic tick and queued callers join this same round.
        while requests.try_recv().is_ok() {}
    }

    fn finish_round(&self, round: Option<CompletedRound>) {
        let mut control = self
            .0
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.0.completion.send_modify(|completed| {
            completed.sequence = completed.sequence.saturating_add(1);
            completed.round = round;
        });
        control.running = false;
    }

    /// Only a real current leader receiving a committed non-voter's clock
    /// request waits. Inverse requests received by learners stay immediate.
    pub(crate) async fn learner_barrier(
        &self,
        learner_id: &str,
    ) -> Result<Option<LearnerClockBarrier>, ()> {
        let Some(leadership) = self.0.membership.clock_leadership_identity() else {
            return if self.0.membership.is_replicated() {
                Err(())
            } else {
                Ok(None)
            };
        };
        if leadership.current_leader != Some(leadership.membership.local_node) {
            return Ok(None);
        }
        let guard = self.0.membership.clock_guard();
        let clock_generation = guard.ticket().clock_generation;
        let roster = self.0.membership.clock_peers().await.map_err(|_| ())?;
        if roster.membership.as_ref() != Some(&leadership.membership)
            || self.0.membership.clock_leadership_identity().as_ref() != Some(&leadership)
            || guard.ticket().clock_generation != clock_generation
        {
            return Err(());
        }
        let directory = peer_directory(&roster).ok_or(())?;
        let (raft_id, _) = directory.get(learner_id).ok_or(())?;
        if leadership.membership.voters.contains(raft_id) {
            return Ok(None);
        }
        if !leadership.membership.members.contains(raft_id) {
            return Err(());
        }
        Ok(Some(LearnerClockBarrier {
            leadership,
            directory,
            learner_id: learner_id.to_owned(),
            clock_generation,
        }))
    }

    fn current_completion(&self, barrier: &LearnerClockBarrier) -> Option<CompletedRound> {
        let round = self.0.completion.borrow().round.clone()?;
        let guard = self.0.membership.clock_guard();
        let current = guard.ticket();
        if round.leadership.as_ref() != Some(&barrier.leadership)
            || round.directory != barrier.directory
            || !round.directory.contains_key(&barrier.learner_id)
            || round.ticket.clock_generation != barrier.clock_generation
            || current.clock_generation != round.ticket.clock_generation
            || current.state_generation != round.ticket.state_generation
            || self.0.membership.clock_leadership_identity().as_ref() != Some(&barrier.leadership)
            || guard.acquire().is_err()
        {
            return None;
        }
        Some(round)
    }

    pub(crate) async fn observe_learner(
        &self,
        barrier: &LearnerClockBarrier,
    ) -> Result<ClockDecisionTicket, ()> {
        let _waiter = self.0.waiters.try_acquire().map_err(|_| ())?;
        let mut completed = self.0.completion.subscribe();
        let sequence = completed.borrow_and_update().sequence;
        if let Some(round) = self.current_completion(barrier) {
            return Ok(round.ticket);
        }
        self.request_round()?;
        loop {
            completed.changed().await.map_err(|_| ())?;
            if let Some(round) = self.current_completion(barrier) {
                return Ok(round.ticket);
            }
            if completed.borrow_and_update().sequence != sequence {
                return Err(());
            }
        }
    }

    /// After the last awaited directory read, no further await may occur
    /// before the response stamps/signature. This does not reserve promotion.
    pub(crate) async fn revalidate_learner(
        &self,
        barrier: &LearnerClockBarrier,
        completed: ClockDecisionTicket,
    ) -> Result<(), ()> {
        let roster = self.0.membership.clock_peers().await.map_err(|_| ())?;
        if roster.membership.as_ref() != Some(&barrier.leadership.membership)
            || peer_directory(&roster).as_ref() != Some(&barrier.directory)
            || self.0.membership.clock_leadership_identity().as_ref() != Some(&barrier.leadership)
        {
            return Err(());
        }
        let current = self.current_completion(barrier).ok_or(())?;
        if current.ticket.clock_generation != completed.clock_generation
            || current.ticket.state_generation != completed.state_generation
        {
            return Err(());
        }
        Ok(())
    }

    pub(crate) async fn run(&self, shutdown: tokio_util::sync::CancellationToken) {
        let receiver = self.claim_receiver();
        let Some(mut requests) = receiver else {
            return;
        };
        let mut filters = BTreeMap::<String, Filter>::new();
        let mut previous_directory = PeerDirectory::new();
        let mut generation = self.0.membership.clock_guard().ticket().clock_generation;
        let mut membership = None;
        let mut interval = tokio::time::interval(CLOCK_PROBE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = interval.tick() => {},
                request = requests.recv() => if request.is_none() { break; },
            }
            self.begin_round(&mut requests);
            let round = tokio::select! {
                () = shutdown.cancelled() => break,
                round = observation_round(
                    &self.0.membership,
                    &self.0.transport,
                    &mut filters,
                    &mut previous_directory,
                    &mut generation,
                    &mut membership,
                ) => round,
            };
            self.finish_round(round);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sample {
    offset_us: i64,
    uncertainty_us: i64,
    round_trip_us: i64,
    observed_at: Instant,
}

impl Sample {
    fn exchange(t1: i64, t2: i64, t3: i64, t4: i64, now: Instant) -> Option<Self> {
        if t3 < t2 {
            return None;
        }
        let service = t3.checked_sub(t2)?;
        let rtt = t4.checked_sub(t1)?.checked_sub(service)?;
        if !(0..=CLOCK_PROBE_MAX_RTT_MS).contains(&rtt) {
            return None;
        }
        let offset_us = t2
            .checked_sub(t1)?
            .checked_add(t3.checked_sub(t4)?)?
            .checked_mul(500)?;
        let uncertainty_us = rtt.checked_mul(500)?.checked_add(1_000)?;
        // A representable offset must also have a representable conservative interval.
        offset_us.checked_abs()?.checked_add(uncertainty_us)?;
        Some(Self {
            offset_us,
            uncertainty_us,
            round_trip_us: rtt.checked_mul(1_000)?,
            observed_at: now,
        })
    }

    fn intersects(self, other: Self) -> bool {
        i128::from(self.offset_us) - i128::from(self.uncertainty_us)
            <= i128::from(other.offset_us) + i128::from(other.uncertainty_us)
            && i128::from(other.offset_us) - i128::from(other.uncertainty_us)
                <= i128::from(self.offset_us) + i128::from(self.uncertainty_us)
    }

    fn observation(self) -> PeerClockOffset {
        PeerClockOffset::Bounded {
            offset_us: self.offset_us,
            uncertainty_us: self.uncertainty_us,
            observed_at: self.observed_at,
        }
    }
}

#[derive(Default)]
struct Filter(VecDeque<Sample>);

impl Filter {
    fn observe(&mut self, candidate: Option<Sample>, now: Instant) -> PeerClockOffset {
        self.0.retain(|sample| {
            now.saturating_duration_since(sample.observed_at) <= CLOCK_OBSERVATION_MAX_AGE
        });
        let Some(candidate) = candidate else {
            return PeerClockOffset::Unknown;
        };
        if let Some(selected) = self.0.iter().min_by_key(|sample| sample.round_trip_us) {
            if !candidate.intersects(*selected) {
                self.0.clear();
            } else if candidate.round_trip_us.max(CLOCK_RTT_QUANTIZATION_FLOOR_US)
                > selected.round_trip_us.max(CLOCK_RTT_QUANTIZATION_FLOOR_US) * 4
            {
                return PeerClockOffset::Unknown;
            }
        }
        self.0.push_back(candidate);
        if self.0.len() > CLOCK_FILTER_DEPTH {
            self.0.pop_front();
        }
        self.0
            .iter()
            .min_by_key(|sample| sample.round_trip_us)
            .copied()
            .expect("candidate was inserted")
            .observation()
    }
}

pub(crate) async fn run(state: AppState, shutdown: tokio_util::sync::CancellationToken) {
    state.clock_observer.run(shutdown).await;
}

async fn observation_round(
    membership_manager: &MembershipManager,
    transport: &crate::http::peer_transport::PeerTransport,
    filters: &mut BTreeMap<String, Filter>,
    previous_directory: &mut PeerDirectory,
    generation: &mut u64,
    membership: &mut Option<plurx_core::cluster::clock::ClockMembershipIdentity>,
) -> Option<CompletedRound> {
    let guard = membership_manager.clock_guard();
    let leadership = membership_manager.clock_leadership_identity();
    let peer_roster =
        match tokio::time::timeout(CLOCK_ROSTER_BUDGET, membership_manager.clock_peers()).await {
            Ok(Ok(peers)) => peers,
            _ => {
                guard.roster_failed();
                filters.clear();
                return None;
            }
        };
    let Some(directory) = peer_directory(&peer_roster) else {
        guard.roster_failed();
        filters.clear();
        return None;
    };
    let roster_ticket = if membership_manager.is_replicated() {
        guard.roster_for_peer_directory(&peer_roster)
    } else {
        guard.roster_for_membership(&[], None)
    };
    let ticket = match roster_ticket {
        Ok(ticket) => ticket,
        Err(_) => {
            filters.clear();
            return None;
        }
    };
    if ticket.clock_generation != *generation || peer_roster.membership != *membership {
        filters.clear();
        *generation = ticket.clock_generation;
        *membership = peer_roster.membership.clone();
    }
    // A UUID keeping its name but changing Raft identity/origin must not
    // inherit a previous endpoint's minimum-delay filter.
    filters.retain(|id, _| {
        directory.contains_key(id) && directory.get(id) == previous_directory.get(id)
    });
    *previous_directory = directory.clone();
    let results = stream::iter(peer_roster.peers.iter().cloned())
        .map(|peer| {
            let transport = transport.clone();
            async move {
                let sample = if let Some(base) = peer.http_base {
                    match transport.clock_request(&peer.node_id, &base).await {
                        Ok(response) if response.status == reqwest::StatusCode::OK => {
                            response.clock_timing.and_then(|(t1, t4, stamp)| {
                                if stamp.clock_generation != ticket.clock_generation
                                    || stamp.state_generation != ticket.state_generation
                                {
                                    return None;
                                }
                                let answer = serde_json::from_slice::<
                                    crate::http::internal_clock::ClockResponse,
                                >(&response.body)
                                .ok()?;
                                if answer.node_id != peer.node_id {
                                    return None;
                                }
                                Sample::exchange(
                                    t1,
                                    answer.received_unix_ms,
                                    answer.sent_unix_ms,
                                    t4,
                                    Instant::now(),
                                )
                            })
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                (peer.node_id, sample)
            }
        })
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;
    // Bind all UUID/Raft/origin facts, not merely an unchanged UUID set.
    let final_peers =
        tokio::time::timeout(CLOCK_ROSTER_BUDGET, membership_manager.clock_peers()).await;
    let same_roster = match final_peers {
        Ok(Ok(final_roster)) => {
            peer_directory(&final_roster).as_ref() == Some(&directory)
                && final_roster.membership == peer_roster.membership
                && membership_manager.clock_leadership_identity() == leadership
        }
        _ => false,
    };
    if !same_roster {
        let current = guard.ticket();
        if current.clock_generation == ticket.clock_generation
            && current.state_generation == ticket.state_generation
        {
            guard.roster_failed();
        }
        filters.clear();
        return None;
    }
    let now = Instant::now();
    let observations = results
        .into_iter()
        .map(|(id, candidate)| {
            let observation = filters
                .entry(id.clone())
                .or_default()
                .observe(candidate, now);
            (id, observation)
        })
        .collect();
    if !guard.publish(ticket, observations) {
        // publish already invalidates a failed CURRENT round. An obsolete
        // ticket cannot erase evidence belonging to a newer generation.
        filters.clear();
        return None;
    }
    let completed = guard.ticket();
    if completed.clock_generation != ticket.clock_generation
        || ticket.state_generation.checked_add(1) != Some(completed.state_generation)
    {
        return None;
    }
    Some(CompletedRound {
        leadership,
        directory,
        ticket: completed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn k06_clock_observer_single_owner_and_bounded_coalescing() {
        let observer = ClockObserver::new(MembershipManager::unavailable());
        let mut receiver = observer.claim_receiver().expect("one owner");
        assert!(observer.clone().claim_receiver().is_none());
        for _ in 0..128 {
            observer.request_round().expect("bounded coalesced demand");
        }
        assert_eq!(receiver.len(), 1);
        observer.begin_round(&mut receiver);
        for _ in 0..128 {
            observer.request_round().expect("join actual active round");
        }
        assert_eq!(receiver.len(), 0, "active work never queues a replay");
        observer.finish_round(None);
        assert_eq!(observer.0.completion.borrow().sequence, 1);
        observer.request_round().expect("next independent demand");
        assert_eq!(receiver.len(), 1);
        drop(receiver);
        assert!(observer.request_round().is_err());
    }

    #[test]
    fn k06_clock_directory_binds_raft_and_normalized_origin() {
        use plurx_core::cluster::membership::ActivityPeer;
        let mut roster = ClockPeerRoster {
            membership: None,
            peers: vec![ActivityPeer {
                node_id: "actual-peer".into(),
                raft_id: 7,
                http_base: Some("http://localhost:8096/".into()),
                reachable: false,
            }],
        };
        let original = peer_directory(&roster).expect("valid directory");
        roster.peers[0].http_base = Some("http://localhost:8096".into());
        roster.peers[0].reachable = true;
        assert_eq!(peer_directory(&roster).as_ref(), Some(&original));
        roster.peers[0].raft_id = 8;
        assert_ne!(peer_directory(&roster).as_ref(), Some(&original));
        roster.peers[0].raft_id = 7;
        roster.peers[0].http_base = Some("http://localhost:8097".into());
        assert_ne!(peer_directory(&roster).as_ref(), Some(&original));
        roster.peers[0].http_base = None;
        assert!(peer_directory(&roster).is_none());
        roster.peers[0].http_base = Some("http://localhost:8096".into());
        roster.peers.push(roster.peers[0].clone());
        assert!(
            peer_directory(&roster).is_none(),
            "duplicates fail whole roster"
        );
    }

    #[test]
    fn four_timestamp_contract() {
        let now = Instant::now();
        for (times, offset, uncertainty) in [
            ([1000, 1011, 1013, 1024], 0, 12000),
            ([1000, 1050, 1052, 1053], 24500, 26500),
            ([1000, 3510, 3512, 1022], 2500000, 11000),
            ([1000, 3100, 3102, 2202], 1500000, 601000),
        ] {
            let sample = Sample::exchange(times[0], times[1], times[2], times[3], now)
                .expect("valid timestamp fixture");
            assert_eq!(
                (sample.offset_us, sample.uncertainty_us),
                (offset, uncertainty)
            );
        }
        assert!(Sample::exchange(0, 10, 9, 20, now).is_none());
        assert!(Sample::exchange(0, 0, 0, 2001, now).is_none());
        assert!(Sample::exchange(i64::MIN, 0, 0, i64::MAX, now).is_none());
        // Responder service time is excluded, including a cold authorization read.
        assert_eq!(
            Sample::exchange(1000, 1011, 1513, 1524, now)
                .expect("service-time fixture")
                .uncertainty_us,
            12000
        );
    }

    #[test]
    fn step_resets_minimum_delay_window() {
        let now = Instant::now();
        let mut filter = Filter::default();
        filter.observe(Sample::exchange(1000, 1000, 1000, 1000, now), now);
        let stepped = Sample::exchange(2000, 17000, 17000, 2000, now).expect("peer step fixture");
        assert_eq!(filter.observe(Some(stepped), now), stepped.observation());
        assert_eq!(filter.0.len(), 1);
    }

    #[test]
    fn zero_then_one_ms_recovers() {
        let now = Instant::now();
        let mut filter = Filter::default();
        filter.observe(Sample::exchange(1000, 1000, 1000, 1000, now), now);
        assert!(matches!(
            filter.observe(Sample::exchange(2000, 2000, 2000, 2001, now), now),
            PeerClockOffset::Bounded { .. }
        ));
        assert_eq!(filter.0.len(), 2);
        assert_eq!(filter.observe(None, now), PeerClockOffset::Unknown);
    }

    #[test]
    fn expired_minimum_cannot_poison_window() {
        let now = Instant::now();
        let mut filter = Filter::default();
        filter.observe(Sample::exchange(1000, 1000, 1000, 1000, now), now);
        assert_eq!(
            filter.observe(Sample::exchange(2000, 2000, 2000, 2010, now), now),
            PeerClockOffset::Unknown
        );
        let later = now + Duration::from_secs(26);
        assert!(matches!(
            filter.observe(Sample::exchange(3000, 3000, 3000, 3010, later), later),
            PeerClockOffset::Bounded { .. }
        ));
        assert_eq!(filter.0.len(), 1);
    }
}
