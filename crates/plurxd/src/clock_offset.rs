//! Measurement-only peer clock observation. Decisions do not consume it yet.
use crate::state::AppState;
use futures_util::{stream, StreamExt};
use plurx_core::cluster::clock::{PeerClockOffset, CLOCK_OBSERVATION_MAX_AGE};
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

const CLOCK_FILTER_DEPTH: usize = 8;
const CLOCK_PROBE_MAX_RTT_MS: i64 = 2_000;
const CLOCK_RTT_QUANTIZATION_FLOOR_US: i64 = 1_000;
const CLOCK_PROBE_INTERVAL: Duration = Duration::from_secs(10);

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
    run_membership(state.membership, shutdown).await;
}

/// The observation phase needs no media/store AppState or normal background
/// loops. It shares the eventual manager's exact auth caches and clock guard.
pub(crate) async fn run_membership(
    membership_manager: plurx_core::cluster::membership::MembershipManager,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let guard = membership_manager.clock_guard();
    let transport = crate::http::peer_transport::PeerTransport::new(membership_manager.clone());
    let mut filters = BTreeMap::<String, Filter>::new();
    let mut generation = guard.ticket().clock_generation;
    let mut membership = None;
    let mut interval = tokio::time::interval(CLOCK_PROBE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! { () = shutdown.cancelled() => break, _ = interval.tick() => {} }
        // Roster discovery is bounded independently of the common 2 s peer deadline.
        let peer_roster =
            match tokio::time::timeout(Duration::from_secs(2), membership_manager.clock_peers())
                .await
            {
                Ok(Ok(peers)) => peers,
                _ => {
                    guard.roster_failed();
                    filters.clear();
                    continue;
                }
            };
        let mut roster = peer_roster
            .peers
            .iter()
            .map(|peer| peer.node_id.clone())
            .collect::<Vec<_>>();
        roster.sort();
        let ticket = match guard.roster_for_membership(&roster, peer_roster.membership.as_ref()) {
            Ok(ticket) => ticket,
            Err(_) => {
                filters.clear();
                continue;
            }
        };
        if ticket.clock_generation != generation || peer_roster.membership != membership {
            filters.clear();
            generation = ticket.clock_generation;
            membership = peer_roster.membership.clone();
        }
        filters.retain(|id, _| roster.contains(id));
        let results = stream::iter(peer_roster.peers)
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
                                    let answer =
                                        serde_json::from_slice::<
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
        // A membership change during fanout cannot publish complete coverage for the old roster.
        let final_peers =
            tokio::time::timeout(Duration::from_secs(2), membership_manager.clock_peers()).await;
        let same_roster = match final_peers {
            Ok(Ok(final_roster)) => {
                let mut ids = final_roster
                    .peers
                    .iter()
                    .map(|peer| peer.node_id.clone())
                    .collect::<Vec<_>>();
                ids.sort();
                ids == roster && final_roster.membership == peer_roster.membership
            }
            _ => false,
        };
        if !same_roster {
            guard.roster_failed();
            filters.clear();
            continue;
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
            filters.clear();
            guard.roster_failed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
