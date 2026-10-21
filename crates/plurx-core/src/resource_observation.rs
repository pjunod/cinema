//! Useful service observations, independent of admission and backend counters.
//! Only completed, serveable output may enter a sample. Pacing and lifecycle
//! changes break speed windows; a held/paused graph is not free capacity.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::process::observation::{Metric, UnknownReason};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkPhase {
    Preparing,
    Producing,
    InputPaced,
    BufferPaced,
    ClientBackpressured,
    SourceBackpressured,
    Paused,
    Retrying,
    Retiring,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceUnit {
    MediaMilliseconds,
    Bytes,
}

/// The producer, not a caller's request, supplies generation and frontier.
#[derive(Clone, Copy, Debug)]
pub struct WorkSample {
    pub generation: u64,
    pub phase: WorkPhase,
    pub unit: ServiceUnit,
    pub completed: u64,
    /// Deadline slack and queue residency are distinct from output speed.
    pub deadline_slack_ms: Metric<i64>,
    pub queued_bytes: Metric<u64>,
    pub at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ServiceObservation {
    pub generation: u64,
    pub phase: WorkPhase,
    pub unit: ServiceUnit,
    pub completed: u64,
    pub units_per_second: Metric<u64>,
    pub interval_ms: u64,
    pub advancing_samples: u8,
    pub deadline_slack_ms: Metric<i64>,
    pub queued_bytes: Metric<u64>,
}

/// Bounded, monotonic observation window. This does not infer concurrency from
/// a single stream or turn input-paced 1x into an encoder capacity estimate.
#[derive(Debug, Default)]
pub struct ServiceWindow {
    points: VecDeque<WorkSample>,
    last: Option<WorkSample>,
    last_observation: Option<ServiceObservation>,
}

impl ServiceWindow {
    pub fn observe(&mut self, sample: WorkSample) -> Option<ServiceObservation> {
        let previous = self.last;
        if previous.is_some_and(|old| sample.at <= old.at) {
            // Duplicate/out-of-order observations provide no new evidence.
            return None;
        }
        let changed = previous.is_none_or(|old| {
            old.generation != sample.generation
                || old.phase != sample.phase
                || old.unit != sample.unit
                || old.completed > sample.completed
                || sample.at.duration_since(old.at) > Duration::from_secs(15)
        });
        if changed {
            self.points.clear();
        }
        if !changed
            && previous.is_some_and(|old| sample.at.duration_since(old.at) < Duration::from_secs(1))
        {
            // Events may be more frequent, but a domain's observation cadence
            // is at most once per second. Never manufacture extra trial samples.
            return None;
        }
        self.last = Some(sample);
        while self
            .points
            .front()
            .is_some_and(|point| sample.at.duration_since(point.at) > Duration::from_secs(15))
        {
            self.points.pop_front();
        }
        self.points.push_back(sample);
        while self.points.len() > 16 {
            self.points.pop_front();
        }
        let first = *self.points.front()?;
        let elapsed = sample.at.duration_since(first.at);
        let advancing = self
            .points
            .iter()
            .zip(self.points.iter().skip(1))
            .filter(|(a, b)| b.completed > a.completed)
            .count();
        let rate = if sample.phase != WorkPhase::Producing {
            // Intentional pacing/backpressure never supplies positive encoder
            // capacity evidence or a below-margin capacity failure.
            Metric::Unknown(UnknownReason::InsufficientEvidence)
        } else if self.points.len() < 3 || advancing < 2 || elapsed < Duration::from_secs(2) {
            Metric::Unknown(UnknownReason::InsufficientEvidence)
        } else {
            let delta = sample.completed.checked_sub(first.completed)?;
            let value = u128::from(delta) * 1_000_000_000 / elapsed.as_nanos();
            u64::try_from(value)
                .map(Metric::Known)
                .unwrap_or(Metric::Unknown(UnknownReason::InvalidCounter))
        };
        let observation = ServiceObservation {
            generation: sample.generation,
            phase: sample.phase,
            unit: sample.unit,
            completed: sample.completed,
            units_per_second: rate,
            interval_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            advancing_samples: u8::try_from(advancing).unwrap_or(u8::MAX),
            deadline_slack_ms: sample.deadline_slack_ms,
            queued_bytes: sample.queued_bytes,
        };
        self.last_observation = Some(observation);
        Some(observation)
    }

    pub fn snapshot(&self, now: Instant) -> Option<ServiceObservation> {
        let mut observation = self.last_observation?;
        let age = now.checked_duration_since(self.last?.at)?;
        if age > Duration::from_secs(15) {
            observation.units_per_second = Metric::Unknown(UnknownReason::Stale);
            observation.deadline_slack_ms = Metric::Unknown(UnknownReason::Stale);
            observation.queued_bytes = Metric::Unknown(UnknownReason::Stale);
        }
        Some(observation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at: Instant, generation: u64, phase: WorkPhase, completed: u64) -> WorkSample {
        WorkSample {
            at,
            generation,
            phase,
            completed,
            unit: ServiceUnit::MediaMilliseconds,
            deadline_slack_ms: Metric::Unknown(UnknownReason::Inaccessible),
            queued_bytes: Metric::Known(1024),
        }
    }

    #[test]
    fn startup_and_duplicate_events_cannot_supply_trial_evidence() {
        let now = Instant::now();
        let mut window = ServiceWindow::default();
        let a = sample(now, 1, WorkPhase::Producing, 4_000);
        assert_eq!(
            window.observe(a).expect("first").units_per_second,
            Metric::Unknown(UnknownReason::InsufficientEvidence)
        );
        assert!(window.observe(a).is_none());
        assert!(window
            .observe(sample(
                now + Duration::from_millis(500),
                1,
                WorkPhase::Producing,
                6_000
            ))
            .is_none());
        window.observe(sample(
            now + Duration::from_secs(1),
            1,
            WorkPhase::Producing,
            8_000,
        ));
        let proof = window
            .observe(sample(
                now + Duration::from_secs(2),
                1,
                WorkPhase::Producing,
                12_000,
            ))
            .expect("proof");
        assert_eq!(proof.units_per_second, Metric::Known(4_000));
        assert_eq!(proof.interval_ms, 2_000);
        assert_eq!(proof.advancing_samples, 2);
    }

    #[test]
    fn pacing_generation_and_stalls_are_not_positive_capacity_evidence() {
        let now = Instant::now();
        let mut window = ServiceWindow::default();
        for second in 0..4 {
            window.observe(sample(
                now + Duration::from_secs(second),
                1,
                WorkPhase::BufferPaced,
                10_000,
            ));
        }
        let paced = window
            .snapshot(now + Duration::from_secs(3))
            .expect("paced");
        assert_eq!(
            paced.units_per_second,
            Metric::Unknown(UnknownReason::InsufficientEvidence)
        );
        let resumed = window
            .observe(sample(
                now + Duration::from_secs(4),
                1,
                WorkPhase::Producing,
                10_000,
            ))
            .expect("resume");
        assert_eq!(resumed.interval_ms, 0);
        window.observe(sample(
            now + Duration::from_secs(5),
            1,
            WorkPhase::Producing,
            10_000,
        ));
        let stalled = window
            .observe(sample(
                now + Duration::from_secs(6),
                1,
                WorkPhase::Producing,
                10_000,
            ))
            .expect("stall");
        assert_eq!(
            stalled.units_per_second,
            Metric::Unknown(UnknownReason::InsufficientEvidence)
        );
        let successor = window
            .observe(sample(
                now + Duration::from_secs(7),
                2,
                WorkPhase::Producing,
                20_000,
            ))
            .expect("successor");
        assert_eq!(successor.interval_ms, 0);
        let stale = window
            .snapshot(now + Duration::from_secs(23))
            .expect("stale");
        assert_eq!(
            stale.units_per_second,
            Metric::Unknown(UnknownReason::Stale)
        );
        assert_eq!(stale.queued_bytes, Metric::Unknown(UnknownReason::Stale));
    }

    #[test]
    fn copy_bytes_are_never_labelled_media_speed_and_history_is_bounded() {
        let now = Instant::now();
        let mut window = ServiceWindow::default();
        for second in 0..100 {
            let mut point = sample(
                now + Duration::from_secs(second),
                1,
                WorkPhase::Producing,
                second * 1024,
            );
            point.unit = ServiceUnit::Bytes;
            window.observe(point);
        }
        assert!(window.points.len() <= 16);
        let observed = window
            .snapshot(now + Duration::from_secs(99))
            .expect("bytes");
        assert_eq!(observed.unit, ServiceUnit::Bytes);
        assert_eq!(observed.units_per_second, Metric::Known(1024));
    }
}
