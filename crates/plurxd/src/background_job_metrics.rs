//! Process-local worker observations. No Store access or dynamic metric labels.
//! Counts describe acknowledged transitions; lost acknowledgements can undercount.
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use plurx_core::store::background_jobs::{JobKind, JOB_METRIC_KINDS};

#[derive(Clone, Copy)]
pub(super) enum Event {
    ClaimWrite,
    ClaimAccepted,
    ClaimRefused,
    ClaimError,
    Takeover,
    RenewWrite,
    ChargedFailure,
    Yield,
    Cancel,
    Published,
    FencedPublication,
}
const EVENTS: [&str; 11] = [
    "claim_write",
    "claim_accepted",
    "claim_refused",
    "claim_error",
    "takeover",
    "renew_write",
    "charged_failure",
    "yield",
    "cancel",
    "published",
    "fenced_publication",
];
const BOUNDS_MS: [u64; 10] = [
    1, 10, 100, 1000, 5000, 30_000, 120_000, 600_000, 3_600_000, 86_400_000,
];

fn kind_slot(kind: JobKind) -> usize {
    match kind {
        JobKind::TranscodePrepare => 0,
        JobKind::FragmentIndexBuild => 1,
        JobKind::ArtifactHydrate => 2,
        JobKind::SubtitleExtract => 3,
        JobKind::LibraryScan => 4,
        JobKind::MetadataRefresh => 5,
        JobKind::ArtifactVerify => 6,
        JobKind::ArtworkDerivative => 7,
        JobKind::SemanticEmbedding => 8,
        JobKind::MediaProbe => 9,
        JobKind::CopyOutputPrepare => 10,
        JobKind::EncodedOutputPrepare => 11,
    }
}

struct Histogram {
    buckets: [AtomicU64; 11],
    sum_ms: AtomicU64,
}
impl Histogram {
    const fn new() -> Self {
        Self {
            buckets: [const { AtomicU64::new(0) }; 11],
            sum_ms: AtomicU64::new(0),
        }
    }
    fn observe(&self, duration: Duration) {
        let ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        self.sum_ms.fetch_add(ms, Ordering::Relaxed);
        let slot = BOUNDS_MS
            .iter()
            .position(|bound| ms <= *bound)
            .unwrap_or(BOUNDS_MS.len());
        self.buckets[slot].fetch_add(1, Ordering::Relaxed);
    }
    fn render(&self, out: &mut String, name: &str, kind: &str) {
        let mut count = 0;
        for (index, bucket) in self.buckets.iter().enumerate() {
            count += bucket.load(Ordering::Relaxed);
            let bound = BOUNDS_MS.get(index).map_or_else(
                || "+Inf".to_owned(),
                |value| (*value as f64 / 1000.0).to_string(),
            );
            let _ = writeln!(
                out,
                "{name}_bucket{{kind=\"{kind}\",le=\"{bound}\"}} {count}"
            );
        }
        let _ = writeln!(out, "{name}_count{{kind=\"{kind}\"}} {count}");
        let _ = writeln!(
            out,
            "{name}_sum{{kind=\"{kind}\"}} {}",
            self.sum_ms.load(Ordering::Relaxed) as f64 / 1000.0
        );
    }
}

static COUNTERS: [[AtomicU64; 11]; JOB_METRIC_KINDS.len()] =
    [const { [const { AtomicU64::new(0) }; 11] }; JOB_METRIC_KINDS.len()];
static CLAIM_LATENCY: [Histogram; JOB_METRIC_KINDS.len()] =
    [const { Histogram::new() }; JOB_METRIC_KINDS.len()];
static QUEUE_WAIT: [Histogram; JOB_METRIC_KINDS.len()] =
    [const { Histogram::new() }; JOB_METRIC_KINDS.len()];
static EXECUTION: [Histogram; JOB_METRIC_KINDS.len()] =
    [const { Histogram::new() }; JOB_METRIC_KINDS.len()];

pub(super) fn event(kind: JobKind, event: Event) {
    COUNTERS[kind_slot(kind)][event as usize].fetch_add(1, Ordering::Relaxed);
}
pub(super) fn claim_latency(kind: JobKind, elapsed: Duration) {
    CLAIM_LATENCY[kind_slot(kind)].observe(elapsed);
}
pub(super) fn queue_wait(kind: JobKind, elapsed: Duration) {
    QUEUE_WAIT[kind_slot(kind)].observe(elapsed);
}
pub(super) fn execution(kind: JobKind, elapsed: Duration) {
    EXECUTION[kind_slot(kind)].observe(elapsed);
}

pub(crate) fn accepted_claims(kinds: &[JobKind]) -> u64 {
    kinds.iter().fold(0_u64, |total, kind| {
        total.saturating_add(
            COUNTERS[kind_slot(*kind)][Event::ClaimAccepted as usize].load(Ordering::Relaxed),
        )
    })
}

pub(crate) fn prometheus() -> String {
    let mut out = String::from("# HELP plurx_background_worker_events_total Process worker events; acknowledged transitions may undercount lost replies.\n# TYPE plurx_background_worker_events_total counter\n");
    for (kind, counters) in JOB_METRIC_KINDS.iter().zip(COUNTERS.iter()) {
        for (event, counter) in EVENTS.iter().zip(counters.iter()) {
            let _ = writeln!(
                out,
                "plurx_background_worker_events_total{{kind=\"{kind}\",outcome=\"{event}\"}} {}",
                counter.load(Ordering::Relaxed)
            );
        }
    }
    for (name, help, histograms) in [
        (
            "plurx_background_claim_seconds",
            "Claim dispatch and ambiguous-reply resolution latency.",
            &CLAIM_LATENCY,
        ),
        (
            "plurx_background_queue_wait_seconds",
            "Age of work at accepted claims, including prior attempts.",
            &QUEUE_WAIT,
        ),
        (
            "plurx_background_execution_seconds",
            "Physical worker lifetime through child join and retirement.",
            &EXECUTION,
        ),
    ] {
        let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} histogram");
        for (kind, histogram) in JOB_METRIC_KINDS.iter().zip(histograms.iter()) {
            histogram.render(&mut out, name, kind);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn histogram_boundaries_are_cumulative_and_include_infinity() {
        let histogram = Histogram::new();
        for ms in [0, 1, 2, 10, 100, 86_400_001] {
            histogram.observe(Duration::from_millis(ms));
        }
        let mut out = String::new();
        histogram.render(&mut out, "test_seconds", "artifact_hydrate");
        for row in [
            "test_seconds_bucket{kind=\"artifact_hydrate\",le=\"0.001\"} 2",
            "test_seconds_bucket{kind=\"artifact_hydrate\",le=\"0.01\"} 4",
            "test_seconds_bucket{kind=\"artifact_hydrate\",le=\"+Inf\"} 6",
            "test_seconds_count{kind=\"artifact_hydrate\"} 6",
        ] {
            assert!(out.contains(row), "{out}");
        }
    }
}
