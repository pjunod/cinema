//! Bounded arithmetic over complete, actually published full-mux media lengths.
//!
//! This reducer does not establish retention or artifact provenance itself.
//! Its caller must bind the identity to source/audio/container/grid/execution
//! and supply every immutable playlist entry, including an audio-only tail.
//! No source packet estimates, payload buffers, scheduler or playback refusal.

const MAX_ENTRIES: usize = 8192;
const MAX_WINDOWS: usize = 131_072;
const MICROS: u128 = 1_000_000;

/// An exact six-decimal EXTINF duration, as rendered on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireDuration(u64);

impl WireDuration {
    pub fn parse(text: &str) -> Option<Self> {
        let (seconds, fraction) = text.split_once('.')?;
        if seconds.is_empty()
            || fraction.len() != 6
            || !seconds.bytes().all(|b| b.is_ascii_digit())
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let micros = seconds
            .parse::<u64>()
            .ok()?
            .checked_mul(1_000_000)?
            .checked_add(fraction.parse::<u64>().ok()?)?;
        (micros > 0).then_some(Self(micros))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnknownReason {
    InvalidPlan,
    InvalidObservation,
    Incomplete,
    Arithmetic,
    WindowLimit,
    NoRfcWindow,
}

/// Values rounded outward in integer bits/second. Burst is deliberately not
/// called RFC peak: RFC8216 peak covers contiguous 0.5–1.5-target windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompleteOutputRates {
    pub identity: [u8; 32],
    pub wire_bytes: u64,
    pub duration_micros: u64,
    pub average_bps: u64,
    pub rfc_peak_bps: u64,
    pub segment_burst_bps: u64,
}

/// Metadata only: at most8192 durations and optional successful wire lengths.
/// A poisoned observation remains Unknown; an unrelated new generation needs
/// a new collector. Identical duplicates are refused as ambiguous publication.
pub struct FullOutputMeasurement {
    identity: [u8; 32],
    target_micros: u64,
    entries: Vec<(WireDuration, Option<u64>)>,
    unknown: Option<UnknownReason>,
}

impl FullOutputMeasurement {
    pub fn new(
        identity: [u8; 32],
        target_duration_seconds: u64,
        durations: &[WireDuration],
    ) -> Self {
        let target = target_duration_seconds.checked_mul(1_000_000);
        let valid = !durations.is_empty()
            && durations.len() <= MAX_ENTRIES
            && target.is_some_and(|value| value > 0);
        Self {
            identity,
            target_micros: target.unwrap_or(0),
            entries: if valid {
                durations.iter().copied().map(|d| (d, None)).collect()
            } else {
                Vec::new()
            },
            unknown: (!valid).then_some(UnknownReason::InvalidPlan),
        }
    }

    /// Call only after an identity-fenced full-mux materialization succeeds.
    /// Zero bytes, wrong identity, duplicate and out-of-plan entries poison the
    /// measurement but do not instruct a caller to reject playable output.
    pub fn observe(&mut self, identity: [u8; 32], entry: usize, wire_bytes: u64) {
        if self.unknown.is_some() {
            return;
        }
        let slot = self.entries.get_mut(entry);
        match slot {
            Some((_, bytes)) if identity == self.identity && wire_bytes > 0 && bytes.is_none() => {
                *bytes = Some(wire_bytes);
            }
            _ => self.unknown = Some(UnknownReason::InvalidObservation),
        }
    }

    /// `complete_tail` is the owner's authoritative completed-output fence,
    /// not producer exit or highest observed index. Every entry is still checked.
    pub fn rates(&self, complete_tail: bool) -> Result<CompleteOutputRates, UnknownReason> {
        if let Some(reason) = self.unknown {
            return Err(reason);
        }
        if !complete_tail || self.entries.iter().any(|(_, bytes)| bytes.is_none()) {
            return Err(UnknownReason::Incomplete);
        }
        let mut total_bytes = 0_u64;
        let mut total_duration = 0_u64;
        let mut burst = 0_u64;
        for (duration, bytes) in &self.entries {
            let bytes = bytes.ok_or(UnknownReason::Incomplete)?;
            total_bytes = total_bytes
                .checked_add(bytes)
                .ok_or(UnknownReason::Arithmetic)?;
            total_duration = total_duration
                .checked_add(duration.0)
                .ok_or(UnknownReason::Arithmetic)?;
            burst = burst.max(rate(bytes, duration.0)?);
        }
        let lower_twice = u128::from(self.target_micros);
        let upper_twice = lower_twice * 3;
        let mut windows = 0_usize;
        let mut peak = None;
        for start in 0..self.entries.len() {
            let mut bytes = 0_u64;
            let mut duration = 0_u64;
            for (entry_duration, entry_bytes) in &self.entries[start..] {
                windows += 1;
                if windows > MAX_WINDOWS {
                    return Err(UnknownReason::WindowLimit);
                }
                bytes = bytes
                    .checked_add(entry_bytes.ok_or(UnknownReason::Incomplete)?)
                    .ok_or(UnknownReason::Arithmetic)?;
                duration = duration
                    .checked_add(entry_duration.0)
                    .ok_or(UnknownReason::Arithmetic)?;
                let twice = u128::from(duration) * 2;
                if twice > upper_twice {
                    break;
                }
                if twice >= lower_twice {
                    let current = rate(bytes, duration)?;
                    peak = Some(peak.map_or(current, |old: u64| old.max(current)));
                }
            }
        }
        Ok(CompleteOutputRates {
            identity: self.identity,
            wire_bytes: total_bytes,
            duration_micros: total_duration,
            average_bps: rate(total_bytes, total_duration)?,
            rfc_peak_bps: peak.ok_or(UnknownReason::NoRfcWindow)?,
            segment_burst_bps: burst,
        })
    }
}

fn rate(bytes: u64, micros: u64) -> Result<u64, UnknownReason> {
    let numerator = u128::from(bytes) * 8 * MICROS;
    let denominator = u128::from(micros);
    let ceiling = numerator
        .checked_add(
            denominator
                .checked_sub(1)
                .ok_or(UnknownReason::Arithmetic)?,
        )
        .ok_or(UnknownReason::Arithmetic)?
        / denominator;
    u64::try_from(ceiling).map_err(|_| UnknownReason::Arithmetic)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn durations(values: &[&str]) -> Vec<WireDuration> {
        values
            .iter()
            .map(|v| WireDuration::parse(v).expect("exact wire duration"))
            .collect()
    }

    #[test]
    fn complete_mux_rates_weight_durations_and_separate_short_tail_burst() {
        let mut measurement = FullOutputMeasurement::new(
            [1; 32],
            2,
            &durations(&["2.000000", "1.000000", "0.100000"]),
        );
        measurement.observe([1; 32], 2, 1000); // full audio tail, out-of-order arrival
        measurement.observe([1; 32], 0, 1000);
        assert_eq!(measurement.rates(true), Err(UnknownReason::Incomplete));
        measurement.observe([1; 32], 1, 2000);
        assert_eq!(measurement.rates(false), Err(UnknownReason::Incomplete));
        let rates = measurement.rates(true).expect("all complete mux entries");
        assert_eq!(rates.wire_bytes, 4000);
        assert_eq!(rates.duration_micros, 3_100_000);
        assert_eq!(rates.average_bps, 10_323);
        assert_eq!(rates.rfc_peak_bps, 21_819); // 1.1s contiguous middle+tail
        assert_eq!(rates.segment_burst_bps, 80_000); // not RFC peak
    }

    #[test]
    fn invalid_identity_duplicates_gaps_overflow_and_bounds_remain_unknown() {
        for text in [
            "0.000000",
            "1.0",
            "-1.000000",
            "1.0000000",
            "NaN",
            "18446744073709551615.000000",
        ] {
            assert_eq!(WireDuration::parse(text), None);
        }
        let one = durations(&["1.000000"]);
        for (identity, index, bytes) in [([2; 32], 0, 1), ([1; 32], 1, 1), ([1; 32], 0, 0)] {
            let mut m = FullOutputMeasurement::new([1; 32], 1, &one);
            m.observe(identity, index, bytes);
            assert_eq!(m.rates(true), Err(UnknownReason::InvalidObservation));
        }
        let mut duplicate = FullOutputMeasurement::new([1; 32], 1, &one);
        duplicate.observe([1; 32], 0, 1);
        duplicate.observe([1; 32], 0, 1);
        assert_eq!(
            duplicate.rates(true),
            Err(UnknownReason::InvalidObservation)
        );
        let mut huge = FullOutputMeasurement::new([1; 32], 1, &one);
        huge.observe([1; 32], 0, u64::MAX);
        assert_eq!(huge.rates(true), Err(UnknownReason::Arithmetic));
        let many = vec![one[0]; MAX_ENTRIES + 1];
        assert_eq!(
            FullOutputMeasurement::new([1; 32], 1, &many).rates(true),
            Err(UnknownReason::InvalidPlan)
        );
        assert_eq!(
            FullOutputMeasurement::new([1; 32], u64::MAX, &one).rates(true),
            Err(UnknownReason::InvalidPlan)
        );
        let tiny = WireDuration::parse("0.000001").expect("positive tail");
        let mut bounded = FullOutputMeasurement::new([1; 32], 1, &vec![tiny; MAX_ENTRIES]);
        for index in 0..MAX_ENTRIES {
            bounded.observe([1; 32], index, 1);
        }
        assert_eq!(bounded.rates(true), Err(UnknownReason::WindowLimit));
    }
}
