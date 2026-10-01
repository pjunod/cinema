//! Node-local clock evidence. No HTTP, Store calls or enforcement consumers.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const CLOCK_OFFSET_REFUSAL_MS: i64 = 2_000;
pub const CLOCK_OBSERVATION_MAX_AGE: Duration = Duration::from_secs(25);
pub const CLOCK_LOCAL_DISCONTINUITY_TOLERANCE_MS: i64 = 250;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerClockOffset {
    Bounded {
        offset_us: i64,
        uncertainty_us: i64,
        observed_at: Instant,
    },
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounded(now: Instant) -> BTreeMap<String, PeerClockOffset> {
        BTreeMap::from([(
            "peer".into(),
            PeerClockOffset::Bounded {
                offset_us: 0,
                uncertainty_us: 1000,
                observed_at: now,
            },
        )])
    }

    #[test]
    fn local_discontinuity_invalidates_generation() {
        // Before the next probe, during t1/t4, after a decision ticket, and
        // a common-mode peer step all use the same serialized generation fence.
        for stage in [
            "before probe",
            "during exchange",
            "after ticket",
            "common mode",
        ] {
            let guard = ClusterClockGuard::new(true);
            let ticket = guard.roster(&["peer".into()]);
            assert!(guard.publish(ticket, bounded(Instant::now())));
            let old = guard.ticket();
            {
                let mut inner = guard.inner.lock().expect("clock state lock");
                let before = Instant::now();
                let current_wall = inner.anchor_wall_ms.expect("fixture anchor") + 15_000;
                ClusterClockGuard::continuity(&mut inner, before, Some(current_wall), before);
                assert_eq!(
                    inner.snapshot.clock_generation,
                    old.clock_generation + 1,
                    "{stage}"
                );
                assert!(matches!(
                    inner.snapshot.state,
                    ClusterClockState::Incomplete { .. }
                ));
                assert_eq!(inner.snapshot.peers["peer"], PeerClockOffset::Unknown);
            }
            assert!(!guard.publish(old, bounded(Instant::now())), "{stage}");
        }
    }

    #[test]
    fn fixed_anchor_and_unrepresentable_wall_invalidate() {
        let guard = ClusterClockGuard::new(true);
        let mut inner = guard.inner.lock().expect("clock state lock");
        let mono = inner.anchor_mono;
        let wall = inner.anchor_wall_ms.expect("fixture anchor");
        for delta in [100, 200] {
            ClusterClockGuard::continuity(&mut inner, mono, Some(wall + delta), mono);
            assert_eq!(inner.snapshot.clock_generation, 0);
        }
        ClusterClockGuard::continuity(&mut inner, mono, Some(wall + 300), mono);
        assert_eq!(inner.snapshot.clock_generation, 1);
        ClusterClockGuard::continuity(&mut inner, mono, None, mono);
        assert_eq!(inner.snapshot.clock_generation, 2);
    }

    #[test]
    fn roster_failure_expiry_and_passive_metrics_preserve_unknown() {
        let guard = ClusterClockGuard::new(true);
        assert!(matches!(
            guard.snapshot().state,
            ClusterClockState::Incomplete { .. }
        ));
        let ticket = guard.roster(&["peer".into()]);
        assert!(guard.publish(ticket, bounded(Instant::now())));
        assert!(guard
            .prometheus()
            .contains("plurx_cluster_clock_offset_seconds{peer=\"peer\"} 0\n"));
        let old = guard.ticket();
        guard.roster_failed();
        assert!(!guard.publish(old, bounded(Instant::now())));
        assert!(!guard
            .prometheus()
            .contains("plurx_cluster_clock_offset_seconds{peer=\"peer\"}"));
        let ticket = guard.roster(&["peer".into()]);
        assert!(guard.publish(ticket, bounded(Instant::now() - Duration::from_secs(26))));
        assert!(matches!(
            guard.snapshot().state,
            ClusterClockState::Incomplete { .. }
        ));
        let ticket = guard.roster(&[]);
        assert!(guard.publish(ticket, BTreeMap::new()));
        assert_eq!(guard.snapshot().state, ClusterClockState::NoPeers);
        assert!(!guard.prometheus().contains("peer=\"peer\""));
        assert_eq!(
            ClusterClockGuard::new(false).snapshot().state,
            ClusterClockState::NoPeers
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClusterClockState {
    NoPeers,
    Bounded {
        worst_abs_upper_us: i64,
    },
    Incomplete {
        unknown_peers: usize,
        worst_abs_upper_us: i64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockDecisionTicket {
    pub clock_generation: u64,
    pub state_generation: u64,
    pub now_ms: i64,
}

#[derive(Clone, Debug)]
pub struct ClockSnapshot {
    pub state: ClusterClockState,
    pub peers: BTreeMap<String, PeerClockOffset>,
    pub clock_generation: u64,
    pub state_generation: u64,
    pub discontinuities: u64,
    pub unknown_rounds: u64,
}

struct ClockInner {
    snapshot: ClockSnapshot,
    roster_proved: bool,
    anchor_wall_ms: Option<i64>,
    anchor_mono: Instant,
}

pub struct ClusterClockGuard {
    inner: Mutex<ClockInner>,
    authority_reads: AtomicU64,
}

fn wall_ms() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
}

impl ClusterClockGuard {
    #[must_use]
    pub fn new(replicated: bool) -> Self {
        let mono = Instant::now();
        Self {
            authority_reads: AtomicU64::new(0),
            inner: Mutex::new(ClockInner {
                snapshot: ClockSnapshot {
                    state: if replicated {
                        ClusterClockState::Incomplete {
                            unknown_peers: 1,
                            worst_abs_upper_us: 0,
                        }
                    } else {
                        ClusterClockState::NoPeers
                    },
                    peers: BTreeMap::new(),
                    clock_generation: 0,
                    state_generation: 0,
                    discontinuities: 0,
                    unknown_rounds: 0,
                },
                roster_proved: !replicated,
                anchor_wall_ms: wall_ms(),
                anchor_mono: mono,
            }),
        }
    }

    /// Synchronously check fixed wall/monotonic continuity, serialized with evidence.
    #[must_use]
    pub fn ticket(&self) -> ClockDecisionTicket {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = Instant::now();
        let wall = wall_ms();
        let after = Instant::now();
        Self::continuity(&mut inner, before, wall, after)
    }

    fn continuity(
        inner: &mut ClockInner,
        before: Instant,
        wall: Option<i64>,
        after: Instant,
    ) -> ClockDecisionTicket {
        let valid = inner
            .anchor_wall_ms
            .zip(wall)
            .is_some_and(|(anchor, current)| {
                let delta = i128::from(current) - i128::from(anchor);
                let lower = before
                    .saturating_duration_since(inner.anchor_mono)
                    .as_millis() as i128
                    - i128::from(CLOCK_LOCAL_DISCONTINUITY_TOLERANCE_MS);
                let upper = after
                    .saturating_duration_since(inner.anchor_mono)
                    .as_millis() as i128
                    + i128::from(CLOCK_LOCAL_DISCONTINUITY_TOLERANCE_MS);
                (lower..=upper).contains(&delta)
            });
        if !valid {
            inner.snapshot.clock_generation += 1;
            inner.snapshot.state_generation += 1;
            inner.snapshot.discontinuities += 1;
            for peer in inner.snapshot.peers.values_mut() {
                *peer = PeerClockOffset::Unknown;
            }
            inner.roster_proved = false;
            inner.anchor_wall_ms = wall;
            inner.anchor_mono = before;
        }
        Self::expire(inner, after);
        Self::recompute(inner);
        ClockDecisionTicket {
            clock_generation: inner.snapshot.clock_generation,
            state_generation: inner.snapshot.state_generation,
            now_ms: wall.unwrap_or_default(),
        }
    }

    fn expire(inner: &mut ClockInner, now: Instant) {
        let mut changed = false;
        for peer in inner.snapshot.peers.values_mut() {
            if matches!(peer, PeerClockOffset::Bounded { observed_at, .. } if now.saturating_duration_since(*observed_at) > CLOCK_OBSERVATION_MAX_AGE)
            {
                *peer = PeerClockOffset::Unknown;
                changed = true;
            }
        }
        if changed {
            inner.snapshot.state_generation += 1;
        }
    }

    fn recompute(inner: &mut ClockInner) {
        let mut unknown = 0;
        let mut worst = 0;
        for peer in inner.snapshot.peers.values() {
            match peer {
                PeerClockOffset::Unknown => unknown += 1,
                PeerClockOffset::Bounded {
                    offset_us,
                    uncertainty_us,
                    ..
                } => {
                    match offset_us
                        .checked_abs()
                        .and_then(|value| value.checked_add(*uncertainty_us))
                    {
                        Some(value) if *uncertainty_us >= 0 => worst = worst.max(value),
                        _ => unknown += 1,
                    }
                }
            }
        }
        inner.snapshot.state = if !inner.roster_proved || unknown > 0 {
            ClusterClockState::Incomplete {
                unknown_peers: unknown.max(usize::from(!inner.roster_proved)),
                worst_abs_upper_us: worst,
            }
        } else if inner.snapshot.peers.is_empty() {
            ClusterClockState::NoPeers
        } else {
            ClusterClockState::Bounded {
                worst_abs_upper_us: worst,
            }
        };
    }

    /// Install the exact committed roster before starting fanout. New members are Unknown.
    pub fn roster(&self, peers: &[String]) -> ClockDecisionTicket {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = Instant::now();
        let wall = wall_ms();
        let ticket = Self::continuity(&mut inner, before, wall, Instant::now());
        let changed = !inner.roster_proved || inner.snapshot.peers.keys().ne(peers.iter());
        if changed {
            inner.snapshot.peers.retain(|id, _| peers.contains(id));
            for id in peers {
                inner
                    .snapshot
                    .peers
                    .entry(id.clone())
                    .or_insert(PeerClockOffset::Unknown);
            }
            inner.roster_proved = true;
            inner.snapshot.state_generation += 1;
        }
        Self::recompute(&mut inner);
        ClockDecisionTicket {
            state_generation: inner.snapshot.state_generation,
            ..ticket
        }
    }

    /// A round publishes atomically only if continuity and roster remained identical.
    pub fn publish(
        &self,
        ticket: ClockDecisionTicket,
        peers: BTreeMap<String, PeerClockOffset>,
    ) -> bool {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = Instant::now();
        let wall = wall_ms();
        let current = Self::continuity(&mut inner, before, wall, Instant::now());
        if current.clock_generation != ticket.clock_generation
            || current.state_generation != ticket.state_generation
            || inner.snapshot.clock_generation != ticket.clock_generation
            || inner.snapshot.state_generation != ticket.state_generation
            || inner.snapshot.peers.keys().ne(peers.keys())
        {
            return false;
        }
        let failed = peers
            .values()
            .any(|peer| matches!(peer, PeerClockOffset::Unknown));
        inner.snapshot.peers = peers;
        if failed {
            inner.snapshot.unknown_rounds += 1;
        }
        // Every completed round invalidates tickets, including a newly failed peer.
        inner.snapshot.state_generation += 1;
        Self::recompute(&mut inner);
        true
    }

    pub fn roster_failed(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.roster_proved = false;
        for peer in inner.snapshot.peers.values_mut() {
            *peer = PeerClockOffset::Unknown;
        }
        inner.snapshot.state_generation += 1;
        inner.snapshot.unknown_rounds += 1;
        Self::recompute(&mut inner);
    }

    #[must_use]
    pub fn snapshot(&self) -> ClockSnapshot {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::expire(&mut inner, Instant::now());
        Self::recompute(&mut inner);
        inner.snapshot.clone()
    }

    #[cfg(feature = "hiqlite-store")]
    pub(crate) fn record_authority_read(&self) {
        self.authority_reads.fetch_add(1, Ordering::Relaxed);
    }

    /// Passive scrape: no continuity sampling, membership lookup or Store.
    #[must_use]
    pub fn prometheus(&self) -> String {
        let snapshot = self.snapshot();
        let mut out = String::from("# HELP plurx_cluster_clock_observation_state Per-peer offset observation state.\n# TYPE plurx_cluster_clock_observation_state gauge\n# HELP plurx_cluster_clock_offset_seconds Measured clock offset to a committed peer.\n# TYPE plurx_cluster_clock_offset_seconds gauge\n# HELP plurx_cluster_clock_offset_uncertainty_seconds Filtered network half-delay plus quantization.\n# TYPE plurx_cluster_clock_offset_uncertainty_seconds gauge\n");
        for (id, observation) in &snapshot.peers {
            let peer = id
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n");
            let bounded = matches!(observation, PeerClockOffset::Bounded { .. });
            out.push_str(&format!("plurx_cluster_clock_observation_state{{peer=\"{peer}\",state=\"bounded\"}} {}\nplurx_cluster_clock_observation_state{{peer=\"{peer}\",state=\"unknown\"}} {}\n", u8::from(bounded), u8::from(!bounded)));
            if let PeerClockOffset::Bounded {
                offset_us,
                uncertainty_us,
                ..
            } = observation
            {
                out.push_str(&format!("plurx_cluster_clock_offset_seconds{{peer=\"{peer}\"}} {}\nplurx_cluster_clock_offset_uncertainty_seconds{{peer=\"{peer}\"}} {}\n", *offset_us as f64 / 1_000_000.0, *uncertainty_us as f64 / 1_000_000.0));
            }
        }
        out.push_str(&format!("# HELP plurx_cluster_clock_discontinuities_total Local wall/monotonic discontinuities observed.\n# TYPE plurx_cluster_clock_discontinuities_total counter\nplurx_cluster_clock_discontinuities_total {}\n# HELP plurx_cluster_clock_unknown_rounds_total Completed failed or incomplete observation rounds.\n# TYPE plurx_cluster_clock_unknown_rounds_total counter\nplurx_cluster_clock_unknown_rounds_total {}\n# HELP plurx_cluster_clock_authority_reads_total Consistent authority reads attributable to inbound clock requests.\n# TYPE plurx_cluster_clock_authority_reads_total counter\nplurx_cluster_clock_authority_reads_total {}\n# HELP plurx_cluster_clock_refusals_total Decisions refused because the clock could not be bounded (measurement-only emits zero).\n# TYPE plurx_cluster_clock_refusals_total counter\n", snapshot.discontinuities, snapshot.unknown_rounds, self.authority_reads.load(Ordering::Relaxed)));
        for decision in ["takeover", "membership_change", "expiry_scan"] {
            for cause in [
                "offset",
                "unknown",
                "local_discontinuity",
                "generation_changed",
            ] {
                out.push_str(&format!("plurx_cluster_clock_refusals_total{{decision=\"{decision}\",cause=\"{cause}\"}} 0\n"));
            }
        }
        out
    }
}
