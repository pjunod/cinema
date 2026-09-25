//! Node-local playback observations and their bounded Prometheus projection.

use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use plurx_core::domain::{CredentialGeneration, NetworkPriorObservation, PlaybackEvent};
use plurx_core::error::StoreError;
use plurx_core::store::{keys, Store};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const TTFF_BUCKETS: [i64; 8] = [100, 250, 500, 1_000, 2_500, 5_000, 10_000, 30_000];
const METHODS: [&str; 4] = ["direct_play", "remux", "transcode", "unknown"];
const STALL_KINDS: [&str; 4] = ["supply", "decode", "network", "other"];
const STALL_RECOVERY_OUTCOMES: [&str; 4] = ["attempt", "recovered", "failed", "other"];
const HOLD_REASONS: [&str; 4] = ["time", "bytes", "global", "unknown"];
const CACHE_RESULTS: [&str; 3] = ["hit", "miss", "prefix_later"];
const ENCODERS: [&str; 8] = [
    "qsv",
    "nvenc",
    "vaapi",
    "videotoolbox",
    "software",
    "copy",
    "cached",
    "vod",
];
const MARKER_ACTIONS: [&str; 4] = ["offer", "manual_skip", "automatic_skip", "undo_seek_back"];
const MARKER_PREWARM_RESULTS: [&str; 2] = ["hit", "miss"];
const TONE_MAP_PEAK_SOURCES: [&str; 3] = ["cll", "mdcv", "default"];
const QUEUE: usize = 1_024;
const TERMINAL_RESERVE: usize = 128;
const BATCH: usize = 64;
const BATCH_WINDOW: Duration = Duration::from_millis(200);
const DRAIN: Duration = Duration::from_secs(2);
const WRITER_RESTARTS_PER_HOUR: usize = 6;
const SETTINGS_REFRESH: Duration = Duration::from_secs(30);
const BATCH_SIZE_BUCKETS: [u64; 7] = [1, 2, 4, 8, 16, 32, 64];
const BATCH_SECONDS_BUCKETS_US: [u64; 7] = [
    1_000,
    10_000,
    50_000,
    250_000,
    1_000_000,
    5_000_000,
    u64::MAX,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventClass {
    Terminal,
    Lifecycle,
    Sample,
}

impl EventClass {
    const fn index(self) -> usize {
        match self {
            Self::Terminal => 0,
            Self::Lifecycle => 1,
            Self::Sample => 2,
        }
    }
}

#[derive(Debug)]
struct Job {
    class: EventClass,
    event: PlaybackEvent,
    network: Option<NetworkIdentity>,
}

struct QueueMetrics {
    enqueued: [AtomicU64; 3],
    dropped_queue_full: AtomicU64,
    dropped_coalesced: AtomicU64,
    dropped_writer_degraded: AtomicU64,
    dropped_writer_panic: AtomicU64,
    dropped_shutdown: AtomicU64,
    written_ok: AtomicU64,
    written_error: AtomicU64,
    queue_depth: AtomicU64,
    setting_refresh_failures: AtomicU64,
    batch_sizes: [AtomicU64; BATCH_SIZE_BUCKETS.len()],
    batch_seconds: [AtomicU64; BATCH_SECONDS_BUCKETS_US.len()],
    batch_seconds_sum_us: AtomicU64,
}

impl QueueMetrics {
    const fn new() -> Self {
        Self {
            enqueued: [const { AtomicU64::new(0) }; 3],
            dropped_queue_full: AtomicU64::new(0),
            dropped_coalesced: AtomicU64::new(0),
            dropped_writer_degraded: AtomicU64::new(0),
            dropped_writer_panic: AtomicU64::new(0),
            dropped_shutdown: AtomicU64::new(0),
            written_ok: AtomicU64::new(0),
            written_error: AtomicU64::new(0),
            queue_depth: AtomicU64::new(0),
            setting_refresh_failures: AtomicU64::new(0),
            batch_sizes: [const { AtomicU64::new(0) }; BATCH_SIZE_BUCKETS.len()],
            batch_seconds: [const { AtomicU64::new(0) }; BATCH_SECONDS_BUCKETS_US.len()],
            batch_seconds_sum_us: AtomicU64::new(0),
        }
    }

    fn render(&self) -> String {
        let mut out = String::from(
            "# HELP plurx_telemetry_enqueued_total Playback telemetry jobs admitted to the bounded writer queue.\n\
             # TYPE plurx_telemetry_enqueued_total counter\n",
        );
        for (label, value) in ["terminal", "lifecycle", "sample"]
            .iter()
            .zip(&self.enqueued)
        {
            out.push_str(&format!(
                "plurx_telemetry_enqueued_total{{class=\"{label}\"}} {}\n",
                value.load(Ordering::Relaxed)
            ));
        }
        out.push_str(&format!(
            "# HELP plurx_telemetry_dropped_total Playback telemetry jobs discarded by bounded policy.\n\
             # TYPE plurx_telemetry_dropped_total counter\n\
             plurx_telemetry_dropped_total{{reason=\"queue_full\"}} {}\n\
             plurx_telemetry_dropped_total{{reason=\"coalesced\"}} {}\n\
             plurx_telemetry_dropped_total{{reason=\"writer_degraded\"}} {}\n\
             plurx_telemetry_dropped_total{{reason=\"writer_panic\"}} {}\n\
             plurx_telemetry_dropped_total{{reason=\"shutdown\"}} {}\n\
             # HELP plurx_telemetry_written_total Playback telemetry rows offered to node-local storage.\n\
             # TYPE plurx_telemetry_written_total counter\n\
             plurx_telemetry_written_total{{outcome=\"ok\"}} {}\n\
             plurx_telemetry_written_total{{outcome=\"error\"}} {}\n\
             # HELP plurx_telemetry_queue_depth Current playback telemetry writer queue depth.\n\
             # TYPE plurx_telemetry_queue_depth gauge\n\
             plurx_telemetry_queue_depth {}\n\
             # HELP plurx_telemetry_setting_refresh_failures_total Failed refreshes of cached telemetry settings.\n\
             # TYPE plurx_telemetry_setting_refresh_failures_total counter\n\
             plurx_telemetry_setting_refresh_failures_total {}\n",
            self.dropped_queue_full.load(Ordering::Relaxed),
            self.dropped_coalesced.load(Ordering::Relaxed),
            self.dropped_writer_degraded.load(Ordering::Relaxed),
            self.dropped_writer_panic.load(Ordering::Relaxed),
            self.dropped_shutdown.load(Ordering::Relaxed),
            self.written_ok.load(Ordering::Relaxed),
            self.written_error.load(Ordering::Relaxed),
            self.queue_depth.load(Ordering::Relaxed),
            self.setting_refresh_failures.load(Ordering::Relaxed),
        ));
        render_histogram(
            &mut out,
            "plurx_telemetry_batch_size",
            "Playback telemetry jobs handled per storage batch.",
            &BATCH_SIZE_BUCKETS,
            &self.batch_sizes,
            1.0,
            None,
        );
        render_histogram(
            &mut out,
            "plurx_telemetry_batch_seconds",
            "Wall time spent processing playback telemetry batches.",
            &BATCH_SECONDS_BUCKETS_US,
            &self.batch_seconds,
            1_000_000.0,
            Some(self.batch_seconds_sum_us.load(Ordering::Relaxed) as f64 / 1_000_000.0),
        );
        out
    }

    fn record_batch(&self, size: usize, elapsed: Duration) {
        let size = size as u64;
        let size_bucket = BATCH_SIZE_BUCKETS
            .iter()
            .position(|upper| size <= *upper)
            .unwrap_or(BATCH_SIZE_BUCKETS.len() - 1);
        self.batch_sizes[size_bucket].fetch_add(1, Ordering::Relaxed);
        let micros = elapsed.as_micros().min(u64::MAX as u128) as u64;
        let time_bucket = BATCH_SECONDS_BUCKETS_US
            .iter()
            .position(|upper| micros <= *upper)
            .unwrap_or(BATCH_SECONDS_BUCKETS_US.len() - 1);
        self.batch_seconds[time_bucket].fetch_add(1, Ordering::Relaxed);
        self.batch_seconds_sum_us
            .fetch_add(micros, Ordering::Relaxed);
    }
}

fn render_histogram<const N: usize>(
    out: &mut String,
    name: &str,
    help: &str,
    buckets: &[u64; N],
    counts: &[AtomicU64; N],
    divisor: f64,
    sum: Option<f64>,
) {
    out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} histogram\n"));
    let mut cumulative = 0;
    for (index, (upper, count)) in buckets.iter().zip(counts).enumerate() {
        cumulative += count.load(Ordering::Relaxed);
        let upper = if index + 1 == N && *upper == u64::MAX {
            "+Inf".to_owned()
        } else {
            format!("{}", *upper as f64 / divisor)
        };
        out.push_str(&format!("{name}_bucket{{le=\"{upper}\"}} {cumulative}\n"));
    }
    if buckets.last().is_some_and(|upper| *upper != u64::MAX) {
        out.push_str(&format!("{name}_bucket{{le=\"+Inf\"}} {cumulative}\n"));
    }
    if let Some(sum) = sum {
        out.push_str(&format!("{name}_sum {sum:.6}\n"));
    }
    out.push_str(&format!("{name}_count {cumulative}\n"));
}

static QUEUE_METRICS: QueueMetrics = QueueMetrics::new();

/// Bounded labels for the subtitle cluster path. No file, title, node or
/// source-derived value can enter a Prometheus label.
#[allow(dead_code)]
pub(crate) enum SubtitleSourceMetric {
    LookupHit,
    LookupHydrated,
    LookupMiss,
    RequestForeground,
    RequestBackground,
    VerdictKept,
    VerdictEmpty,
    VerdictMalformed,
    VerdictTransient,
    JobBytes(u64),
    HydrationServedBytes(u64),
    HydrationFetchedBytes(u64),
    ForegroundSelfClaim,
    Repair,
}

struct SubtitleSourceMetrics {
    lookups: [AtomicU64; 3],
    requests: [AtomicU64; 2],
    verdicts: [AtomicU64; 4],
    job_bytes: AtomicU64,
    hydration_bytes: [AtomicU64; 2],
    foreground_self_claims: AtomicU64,
    repairs: AtomicU64,
}

impl SubtitleSourceMetrics {
    const fn new() -> Self {
        Self {
            lookups: [const { AtomicU64::new(0) }; 3],
            requests: [const { AtomicU64::new(0) }; 2],
            verdicts: [const { AtomicU64::new(0) }; 4],
            job_bytes: AtomicU64::new(0),
            hydration_bytes: [const { AtomicU64::new(0) }; 2],
            foreground_self_claims: AtomicU64::new(0),
            repairs: AtomicU64::new(0),
        }
    }

    fn record(&self, event: SubtitleSourceMetric) {
        let one = |counter: &AtomicU64| {
            counter.fetch_add(1, Ordering::Relaxed);
        };
        match event {
            SubtitleSourceMetric::LookupHit => one(&self.lookups[0]),
            SubtitleSourceMetric::LookupHydrated => one(&self.lookups[1]),
            SubtitleSourceMetric::LookupMiss => one(&self.lookups[2]),
            SubtitleSourceMetric::RequestForeground => one(&self.requests[0]),
            SubtitleSourceMetric::RequestBackground => one(&self.requests[1]),
            SubtitleSourceMetric::VerdictKept => one(&self.verdicts[0]),
            SubtitleSourceMetric::VerdictEmpty => one(&self.verdicts[1]),
            SubtitleSourceMetric::VerdictMalformed => one(&self.verdicts[2]),
            SubtitleSourceMetric::VerdictTransient => one(&self.verdicts[3]),
            SubtitleSourceMetric::JobBytes(bytes) => {
                self.job_bytes.fetch_add(bytes, Ordering::Relaxed);
            }
            SubtitleSourceMetric::HydrationServedBytes(bytes) => {
                self.hydration_bytes[0].fetch_add(bytes, Ordering::Relaxed);
            }
            SubtitleSourceMetric::HydrationFetchedBytes(bytes) => {
                self.hydration_bytes[1].fetch_add(bytes, Ordering::Relaxed);
            }
            SubtitleSourceMetric::ForegroundSelfClaim => one(&self.foreground_self_claims),
            SubtitleSourceMetric::Repair => one(&self.repairs),
        }
    }

    fn render(&self) -> String {
        let mut out = String::from(
            "# HELP plurx_subtitle_source_lookups_total Stored subtitle source lookups by result.\n\
             # TYPE plurx_subtitle_source_lookups_total counter\n",
        );
        for (index, label) in ["hit", "hydrated", "miss"].iter().enumerate() {
            out.push_str(&format!(
                "plurx_subtitle_source_lookups_total{{outcome=\"{label}\"}} {}\n",
                self.lookups[index].load(Ordering::Relaxed)
            ));
        }
        out.push_str("# HELP plurx_subtitle_source_requests_total Subtitle extraction requests by trigger.\n# TYPE plurx_subtitle_source_requests_total counter\n");
        for (index, label) in ["foreground", "background"].iter().enumerate() {
            out.push_str(&format!(
                "plurx_subtitle_source_requests_total{{trigger=\"{label}\"}} {}\n",
                self.requests[index].load(Ordering::Relaxed)
            ));
        }
        out.push_str("# HELP plurx_subtitle_source_verdicts_total Subtitle representation extraction verdicts.\n# TYPE plurx_subtitle_source_verdicts_total counter\n");
        for (index, label) in ["kept", "empty", "malformed", "transient"]
            .iter()
            .enumerate()
        {
            out.push_str(&format!(
                "plurx_subtitle_source_verdicts_total{{verdict=\"{label}\"}} {}\n",
                self.verdicts[index].load(Ordering::Relaxed)
            ));
        }
        out.push_str(&format!(
            "# HELP plurx_subtitle_source_job_bytes_total Source bytes read by subtitle extraction jobs.\n\
             # TYPE plurx_subtitle_source_job_bytes_total counter\n\
             plurx_subtitle_source_job_bytes_total {}\n",
            self.job_bytes.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP plurx_subtitle_source_hydration_bytes_total Subtitle artefact bytes transferred over media peers.\n# TYPE plurx_subtitle_source_hydration_bytes_total counter\n");
        for (index, label) in ["served", "fetched"].iter().enumerate() {
            out.push_str(&format!(
                "plurx_subtitle_source_hydration_bytes_total{{direction=\"{label}\"}} {}\n",
                self.hydration_bytes[index].load(Ordering::Relaxed)
            ));
        }
        out.push_str(&format!(
            "# HELP plurx_subtitle_source_foreground_self_claims_total Foreground requests claimed by their waiting node.\n\
             # TYPE plurx_subtitle_source_foreground_self_claims_total counter\n\
             plurx_subtitle_source_foreground_self_claims_total {}\n\
             # HELP plurx_subtitle_source_repairs_total Lost-publication repairs requested.\n\
             # TYPE plurx_subtitle_source_repairs_total counter\n\
             plurx_subtitle_source_repairs_total {}\n",
            self.foreground_self_claims.load(Ordering::Relaxed),
            self.repairs.load(Ordering::Relaxed)
        ));
        out
    }
}

static SUBTITLE_SOURCE_METRICS: SubtitleSourceMetrics = SubtitleSourceMetrics::new();

#[allow(dead_code)]
pub(crate) fn record_subtitle_source(event: SubtitleSourceMetric) {
    SUBTITLE_SOURCE_METRICS.record(event);
}

struct PlaybackMetrics {
    ttff_buckets: [[AtomicU64; TTFF_BUCKETS.len() + 1]; METHODS.len()],
    ttff_count: [AtomicU64; METHODS.len()],
    ttff_sum: [AtomicU64; METHODS.len()],
    stalls: [AtomicU64; STALL_KINDS.len()],
    stall_recoveries: [AtomicU64; STALL_RECOVERY_OUTCOMES.len()],
    suspends: [AtomicU64; HOLD_REASONS.len()],
    suspended_ms: AtomicU64,
    cache_serves: [AtomicU64; CACHE_RESULTS.len()],
    sessions: [AtomicU64; ENCODERS.len() + 1],
    marker_actions: [AtomicU64; MARKER_ACTIONS.len()],
    marker_prewarm: [AtomicU64; MARKER_PREWARM_RESULTS.len()],
    tone_map_peaks: [AtomicU64; TONE_MAP_PEAK_SOURCES.len()],
}

impl PlaybackMetrics {
    const fn new() -> Self {
        Self {
            ttff_buckets: [const { [const { AtomicU64::new(0) }; TTFF_BUCKETS.len() + 1] };
                METHODS.len()],
            ttff_count: [const { AtomicU64::new(0) }; METHODS.len()],
            ttff_sum: [const { AtomicU64::new(0) }; METHODS.len()],
            stalls: [const { AtomicU64::new(0) }; STALL_KINDS.len()],
            stall_recoveries: [const { AtomicU64::new(0) }; STALL_RECOVERY_OUTCOMES.len()],
            suspends: [const { AtomicU64::new(0) }; HOLD_REASONS.len()],
            suspended_ms: AtomicU64::new(0),
            cache_serves: [const { AtomicU64::new(0) }; CACHE_RESULTS.len()],
            sessions: [const { AtomicU64::new(0) }; ENCODERS.len() + 1],
            marker_actions: [const { AtomicU64::new(0) }; MARKER_ACTIONS.len()],
            marker_prewarm: [const { AtomicU64::new(0) }; MARKER_PREWARM_RESULTS.len()],
            tone_map_peaks: [const { AtomicU64::new(0) }; TONE_MAP_PEAK_SOURCES.len()],
        }
    }

    fn record(&self, event: &PlaybackEvent) {
        match event.event.as_str() {
            "ttff" => {
                let Some(ms) = event.ms.filter(|value| *value >= 0) else {
                    return;
                };
                let method = label_index(event.method.as_deref(), &METHODS);
                let bucket = TTFF_BUCKETS
                    .iter()
                    .position(|upper| ms <= *upper)
                    .unwrap_or(TTFF_BUCKETS.len());
                self.ttff_buckets[method][bucket].fetch_add(1, Ordering::Relaxed);
                self.ttff_count[method].fetch_add(1, Ordering::Relaxed);
                self.ttff_sum[method].fetch_add(ms as u64, Ordering::Relaxed);
            }
            "stall" => {
                let detail = event.detail.as_deref().unwrap_or_default();
                let kind = if detail.contains("supply") {
                    "supply"
                } else if detail.contains("decode") || detail.contains("frame") {
                    "decode"
                } else if detail.contains("network") || detail.contains("blocked") {
                    "network"
                } else {
                    "other"
                };
                self.stalls[label_index(Some(kind), &STALL_KINDS)].fetch_add(1, Ordering::Relaxed);
            }
            "stall_recovery" => {
                let outcome = event
                    .detail
                    .as_deref()
                    .unwrap_or_default()
                    .split(':')
                    .next();
                self.stall_recoveries[label_index(outcome, &STALL_RECOVERY_OUTCOMES)]
                    .fetch_add(1, Ordering::Relaxed);
            }
            "suspend" => {
                self.suspends[label_index(event.hold_reason.as_deref(), &HOLD_REASONS)]
                    .fetch_add(1, Ordering::Relaxed);
            }
            "resume" => {
                if let Some(ms) = event.ms.filter(|value| *value > 0) {
                    self.suspended_ms.fetch_add(ms as u64, Ordering::Relaxed);
                }
            }
            "session_start" => {
                let encoder = label_index_with_unknown(event.encoder.as_deref(), &ENCODERS);
                self.sessions[encoder].fetch_add(1, Ordering::Relaxed);
                if let Some(result) = event
                    .extra
                    .as_deref()
                    .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
                    .and_then(|value| {
                        value
                            .get("cache")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned)
                    })
                {
                    if let Some(index) = CACHE_RESULTS.iter().position(|label| *label == result) {
                        self.cache_serves[index].fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            "marker_manual_skip" => {
                self.marker_actions[1].fetch_add(1, Ordering::Relaxed);
            }
            "marker_offer" => {
                self.marker_actions[0].fetch_add(1, Ordering::Relaxed);
            }
            "marker_automatic_skip" => {
                self.marker_actions[2].fetch_add(1, Ordering::Relaxed);
            }
            "marker_undo" | "marker_seek_back" => {
                self.marker_actions[3].fetch_add(1, Ordering::Relaxed);
            }
            "marker_skip" => {
                let slot = usize::from(
                    event
                        .detail
                        .as_deref()
                        .is_some_and(|detail| detail.contains("automatic")),
                ) + 1;
                self.marker_actions[slot].fetch_add(1, Ordering::Relaxed);
            }
            "marker_prewarm" => {
                let result = usize::from(event.detail.as_deref() != Some("hit"));
                self.marker_prewarm[result].fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    fn render(&self) -> String {
        let mut out = String::from(
            "# HELP plurx_ttff_ms Time from play request to first frame.\n\
             # TYPE plurx_ttff_ms histogram\n",
        );
        for (method_index, method) in METHODS.iter().enumerate() {
            let mut cumulative = 0;
            for (bucket_index, upper) in TTFF_BUCKETS.iter().enumerate() {
                cumulative += self.ttff_buckets[method_index][bucket_index].load(Ordering::Relaxed);
                out.push_str(&format!(
                    "plurx_ttff_ms_bucket{{method=\"{method}\",le=\"{upper}\"}} {cumulative}\n"
                ));
            }
            cumulative +=
                self.ttff_buckets[method_index][TTFF_BUCKETS.len()].load(Ordering::Relaxed);
            out.push_str(&format!(
                "plurx_ttff_ms_bucket{{method=\"{method}\",le=\"+Inf\"}} {cumulative}\n\
                 plurx_ttff_ms_sum{{method=\"{method}\"}} {}\n\
                 plurx_ttff_ms_count{{method=\"{method}\"}} {}\n",
                self.ttff_sum[method_index].load(Ordering::Relaxed),
                self.ttff_count[method_index].load(Ordering::Relaxed)
            ));
        }
        render_counters(
            &mut out,
            "plurx_stalls_total",
            "Playback stalls reported by clients.",
            "kind",
            &STALL_KINDS,
            &self.stalls,
        );
        render_counters(
            &mut out,
            "plurx_stall_recoveries_total",
            "Persistent-stall recovery events reported by clients.",
            "outcome",
            &STALL_RECOVERY_OUTCOMES,
            &self.stall_recoveries,
        );
        render_counters(
            &mut out,
            "plurx_suspends_total",
            "Encoder ahead-window suspensions.",
            "reason",
            &HOLD_REASONS,
            &self.suspends,
        );
        out.push_str(&format!(
            "# HELP plurx_suspended_seconds_total Seconds encoders spent suspended.\n\
             # TYPE plurx_suspended_seconds_total counter\n\
             plurx_suspended_seconds_total {:.3}\n",
            self.suspended_ms.load(Ordering::Relaxed) as f64 / 1_000.0
        ));
        render_counters(
            &mut out,
            "plurx_cache_serves_total",
            "Playback session starts by transcode-cache result.",
            "result",
            &CACHE_RESULTS,
            &self.cache_serves,
        );
        let session_labels = ENCODERS
            .iter()
            .copied()
            .chain(std::iter::once("other"))
            .collect::<Vec<_>>();
        render_counters(
            &mut out,
            "plurx_sessions_total",
            "Playback sessions started by encoder.",
            "encoder",
            &session_labels,
            &self.sessions,
        );
        render_counters(
            &mut out,
            "plurx_playback_marker_actions_total",
            "Playback marker offers, skips, and undo or seek-back actions.",
            "action",
            &MARKER_ACTIONS,
            &self.marker_actions,
        );
        render_counters(
            &mut out,
            "plurx_playback_marker_prewarm_total",
            "Skip-destination prewarm outcomes.",
            "result",
            &MARKER_PREWARM_RESULTS,
            &self.marker_prewarm,
        );
        render_counters(
            &mut out,
            "plurx_tone_map_peak_total",
            "CPU tone-map recipes captured by source-luminance provenance.",
            "source",
            &TONE_MAP_PEAK_SOURCES,
            &self.tone_map_peaks,
        );
        let hits = self.marker_prewarm[0].load(Ordering::Relaxed);
        let total = hits.saturating_add(self.marker_prewarm[1].load(Ordering::Relaxed));
        // Publish the ratio only once a hit has been observed.
        //
        // The gauge divides hits by skips, and no first-party client can
        // currently produce a hit: prewarming a marker destination is
        // unbuilt, so every callsite reports a miss and reports it truthfully.
        // Emitting the quotient anyway published a permanent 0.000000, which
        // is the shape of a feature that is failing rather than one that does
        // not exist — and an operator cannot tell those apart from the number.
        //
        // Two readings collapse into that one value, which is the reason a
        // help string could not fix this: `hits == 0` because nothing prewarms,
        // and `total == 0` because this node has served no marker skip since
        // boot. A quiet node and a missing feature are different facts and
        // neither is a rate.
        //
        // Suppression is not "hiding a zero". The counters above carry every
        // miss, so nothing is lost and the quotient stays derivable by anyone
        // who wants it; what is withheld is a server-side claim to have
        // measured a rate it has no numerator for. The gauge appears the first
        // time a hit is recorded, which is exactly when it starts meaning
        // something, and a genuine 0% then publishes normally.
        if hits > 0 {
            out.push_str(&format!(
                "# HELP plurx_playback_marker_prewarm_hit_ratio Skip-destination prewarm hit rate. \
                 Absent until a hit is observed, because a rate with no possible numerator is not a measurement.\n\
                 # TYPE plurx_playback_marker_prewarm_hit_ratio gauge\n\
                 plurx_playback_marker_prewarm_hit_ratio {:.6}\n",
                hits as f64 / total as f64
            ));
        }
        out
    }
}

fn label_index<const N: usize>(value: Option<&str>, labels: &[&str; N]) -> usize {
    labels
        .iter()
        .position(|label| Some(*label) == value)
        .unwrap_or(N - 1)
}

fn label_index_with_unknown<const N: usize>(value: Option<&str>, labels: &[&str; N]) -> usize {
    labels
        .iter()
        .position(|label| Some(*label) == value)
        .unwrap_or(N)
}

fn render_counters<const N: usize>(
    out: &mut String,
    name: &str,
    help: &str,
    label_name: &str,
    labels: &[&str],
    values: &[AtomicU64; N],
) {
    out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} counter\n"));
    for (label, value) in labels.iter().zip(values) {
        out.push_str(&format!(
            "{name}{{{label_name}=\"{label}\"}} {}\n",
            value.load(Ordering::Relaxed)
        ));
    }
}

static METRICS: LazyLock<PlaybackMetrics> = LazyLock::new(PlaybackMetrics::new);

pub(crate) fn record_tone_map_peak(source: plurx_core::transcode::ToneMapPeakSource) {
    let index = match source {
        plurx_core::transcode::ToneMapPeakSource::Cll => 0,
        plurx_core::transcode::ToneMapPeakSource::Mdcv => 1,
        plurx_core::transcode::ToneMapPeakSource::Default => 2,
    };
    METRICS.tone_map_peaks[index].fetch_add(1, Ordering::Relaxed);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NetworkIdentity {
    pub(crate) client_class: String,
    pub(crate) network_fingerprint: String,
    /// Credential-generation key used for prior storage lookups.
    /// Captured at authentication time and never exposed through APIs or logs.
    pub(crate) credential_generation: Option<CredentialGeneration>,
    /// Non-lookup ownership metadata for cross-generation retention bounds.
    pub(crate) user_id: Option<i64>,
}

/// The paired effective-settings answer: the raw
/// `telemetry.retain_days` and `playback.network_priors` values, read
/// together so one consistent read serves both.
type SettingPair = (Option<String>, Option<String>);

/// What the bounded writer needs from durable storage.
///
/// A narrow seam rather than `Arc<dyn Store>`, because the three things this
/// module owns that are worth proving — supervision, drop accounting and the
/// shutdown bound — are only interesting against a store that sleeps, fails
/// or panics, and `Store` is a twenty-seven-trait composite whose
/// hand-written double would be thousands of lines that say nothing about
/// this file.
trait WriterStore: Send + Sync + 'static {
    /// One node-local connection lease for the retained events AND the folded
    /// prior observations. Either slice may be empty; the two opt-ins are
    /// independent and the lease is shared.
    fn write_batch<'a>(
        &'a self,
        events: &'a [PlaybackEvent],
        observations: &'a [NetworkPriorObservation],
    ) -> BoxFuture<'a, Result<u64, StoreError>>;

    /// Both telemetry settings in one consistent read.
    fn setting_pair(&self) -> BoxFuture<'_, Result<SettingPair, StoreError>>;
}

struct DurableWriterStore(Arc<dyn Store>);

impl WriterStore for DurableWriterStore {
    fn write_batch<'a>(
        &'a self,
        events: &'a [PlaybackEvent],
        observations: &'a [NetworkPriorObservation],
    ) -> BoxFuture<'a, Result<u64, StoreError>> {
        self.0.record_playback_batch(events, observations)
    }

    fn setting_pair(&self) -> BoxFuture<'_, Result<SettingPair, StoreError>> {
        self.0
            .get_setting_pair(keys::TELEMETRY_RETAIN_DAYS, keys::PLAYBACK_NETWORK_PRIORS)
    }
}

/// The bounded queue, and the whole of the admission decision.
///
/// Admission has to be atomic with the enqueue it authorises. A depth
/// snapshot followed by a separate send is two decisions with a window
/// between them: concurrent non-terminal producers can every one of them
/// observe the same sub-reserve depth and every one of them send, so the 128
/// slots the C15 remedy reserves for terminal outcomes are reserved only
/// while nothing is contending — which is the only time they are not needed.
///
/// A reserve is also not enough on its own. A queue holding 896 samples and
/// 128 terminals is full, and the next terminal would be refused with 896
/// samples still queued ahead of it: room "reserved" for a terminal that the
/// terminal loses anyway. So a terminal that finds the queue full displaces
/// the OLDEST non-terminal instead of being discarded. Occupancy still never
/// exceeds `QUEUE`, so the memory bound is exactly what it was, and the only
/// queue that can refuse a terminal is one already holding `QUEUE` of them.
struct JobQueue {
    state: Mutex<QueueState>,
    ready: Notify,
    metrics: &'static QueueMetrics,
}

struct QueueState {
    jobs: VecDeque<Job>,
    closed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admission {
    /// Queued, nothing displaced.
    Queued,
    /// A terminal was queued by displacing the oldest non-terminal.
    Displaced,
    /// Refused at this caller's ceiling.
    Refused,
    /// Refused because shutdown has taken ownership of the queue.
    Closed,
}

impl JobQueue {
    fn new(metrics: &'static QueueMetrics) -> Self {
        Self {
            state: Mutex::new(QueueState {
                jobs: VecDeque::with_capacity(QUEUE),
                closed: false,
            }),
            ready: Notify::new(),
            metrics,
        }
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, QueueState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn push(&self, job: Job) -> Admission {
        let admission = {
            let mut state = self.locked();
            if state.closed {
                Admission::Closed
            } else if job.class == EventClass::Terminal {
                if state.jobs.len() < QUEUE {
                    state.jobs.push_back(job);
                    Admission::Queued
                } else if let Some(index) = state
                    .jobs
                    .iter()
                    .position(|queued| queued.class != EventClass::Terminal)
                {
                    state.jobs.remove(index);
                    state.jobs.push_back(job);
                    Admission::Displaced
                } else {
                    Admission::Refused
                }
            } else if state.jobs.len() < QUEUE - TERMINAL_RESERVE {
                state.jobs.push_back(job);
                Admission::Queued
            } else {
                Admission::Refused
            }
        };
        if matches!(admission, Admission::Queued | Admission::Displaced) {
            self.metrics
                .queue_depth
                .store(self.len() as u64, Ordering::Relaxed);
            self.ready.notify_one();
        }
        admission
    }

    fn take_batch(&self, limit: usize) -> Vec<Job> {
        let mut state = self.locked();
        let take = state.jobs.len().min(limit);
        state.jobs.drain(..take).collect()
    }

    fn len(&self) -> usize {
        self.locked().jobs.len()
    }

    fn is_closed(&self) -> bool {
        self.locked().closed
    }

    /// Take ownership of everything still queued, exactly once.
    ///
    /// A second caller gets an empty vector. That is what lets the writer and
    /// the shutdown drain race for the remainder without either of them
    /// counting it twice.
    fn close_and_drain(&self) -> Vec<Job> {
        let mut state = self.locked();
        if state.closed {
            return Vec::new();
        }
        state.closed = true;
        state.jobs.drain(..).collect()
    }
}

struct TelemetrySink {
    queue: Arc<JobQueue>,
    /// The batch the writer is currently responsible for.
    ///
    /// Jobs move here the moment they leave the queue, and leave only by
    /// `Option::take`. That is what makes their accounting exactly-once: the
    /// writer takes them back to record the write, the supervisor takes them
    /// to count a batch a panic destroyed, and a shutdown that gives up takes
    /// them to count them lost. Whoever takes them first accounts for them;
    /// everyone else finds `None` and accounts for nothing.
    inflight: Mutex<Option<Vec<Job>>>,
    degraded: AtomicBool,
    draining: AtomicBool,
    shutdown: CancellationToken,
    stopped: Notify,
    settings: RwLock<EffectiveSettings>,
    metrics: &'static QueueMetrics,
}

#[derive(Clone, Copy)]
struct EffectiveSettings {
    retain: bool,
    priors: bool,
    read_at: Option<Instant>,
}

static SINKS: LazyLock<Mutex<HashMap<usize, Arc<TelemetrySink>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn store_key(store: &Arc<dyn Store>) -> usize {
    Arc::as_ptr(store) as *const () as usize
}

/// Start the single writer for this Store before the listener can accept.
fn ensure_sink(store: Arc<dyn Store>) -> Arc<TelemetrySink> {
    let key = store_key(&store);
    let mut sinks = SINKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(sink) = sinks.get(&key) {
        return Arc::clone(sink);
    }
    let sink = new_sink(&QUEUE_METRICS);
    let writer: Arc<dyn WriterStore> = Arc::new(DurableWriterStore(store));
    tokio::spawn(supervise_writer(writer, Arc::clone(&sink)));
    sinks.insert(key, Arc::clone(&sink));
    sink
}

fn new_sink(metrics: &'static QueueMetrics) -> Arc<TelemetrySink> {
    Arc::new(TelemetrySink {
        queue: Arc::new(JobQueue::new(metrics)),
        inflight: Mutex::new(None),
        degraded: AtomicBool::new(false),
        draining: AtomicBool::new(false),
        shutdown: CancellationToken::new(),
        stopped: Notify::new(),
        settings: RwLock::new(EffectiveSettings {
            retain: true,
            priors: false,
            read_at: None,
        }),
        metrics,
    })
}

/// Seed settings before the listener accepts the first telemetry producer.
pub(crate) async fn initialize(store: Arc<dyn Store>) -> Result<(), StoreError> {
    let sink = ensure_sink(Arc::clone(&store));
    sink.refresh_settings(&DurableWriterStore(store), true)
        .await
}

/// Register a sink for `store` whose writer is `writer` and whose counters
/// are `metrics`.
///
/// Production registers during boot through [`ensure_sink`]. A test needs the
/// same registration so `emit` takes the real path, but against a store that
/// sleeps, fails or panics, and against counters no concurrently running test
/// is also moving.
#[cfg(test)]
fn install_test_sink(
    store: &Arc<dyn Store>,
    writer: Arc<dyn WriterStore>,
    metrics: &'static QueueMetrics,
) -> Arc<TelemetrySink> {
    let sink = new_sink(metrics);
    tokio::spawn(supervise_writer(writer, Arc::clone(&sink)));
    SINKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(store_key(store), Arc::clone(&sink));
    sink
}

fn sink_for(store: Arc<dyn Store>) -> Arc<TelemetrySink> {
    let key = store_key(&store);
    // The lookup binds to a `let` before anything branches on it, and that is
    // load-bearing: a temporary `MutexGuard` in an `if let` scrutinee lives to
    // the end of the WHOLE `if let` in this edition, so building the sink in
    // an `else` arm would re-lock `SINKS` on the thread already holding it and
    // deadlock `emit` outright.
    let registered = SINKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
        .cloned();
    // Production registers during boot. Lazy construction keeps isolated
    // handler tests on the same bounded path without making emit async.
    registered.unwrap_or_else(|| ensure_sink(store))
}

impl TelemetrySink {
    fn put_inflight(&self, jobs: Vec<Job>) {
        *self
            .inflight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(jobs);
    }

    fn take_inflight(&self) -> Option<Vec<Job>> {
        self.inflight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    async fn refresh_settings(
        &self,
        store: &dyn WriterStore,
        force: bool,
    ) -> Result<(), StoreError> {
        let due = self
            .settings
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .read_at
            .is_none_or(|at| at.elapsed() >= SETTINGS_REFRESH);
        if !force && !due {
            return Ok(());
        }
        match store.setting_pair().await {
            Ok((retain, priors)) => {
                *self
                    .settings
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = EffectiveSettings {
                    retain: retain
                        .as_deref()
                        .and_then(|value| value.trim().parse::<i64>().ok())
                        .unwrap_or(keys::TELEMETRY_RETAIN_DEFAULT_DAYS)
                        > 0,
                    priors: priors.as_deref() == Some("1"),
                    read_at: Some(Instant::now()),
                };
                Ok(())
            }
            Err(error) => {
                self.metrics
                    .setting_refresh_failures
                    .fetch_add(1, Ordering::Relaxed);
                Err(error)
            }
        }
    }

    fn invalidate_settings(&self) {
        self.settings
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .read_at = None;
    }
}

pub(crate) fn invalidate_settings(store: &Arc<dyn Store>) {
    let key = store_key(store);
    if let Some(sink) = SINKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
    {
        sink.invalidate_settings();
    }
}

async fn supervise_writer(store: Arc<dyn WriterStore>, sink: Arc<TelemetrySink>) {
    let mut restarts = Vec::<Instant>::new();
    loop {
        let outcome = AssertUnwindSafe(writer_loop(Arc::clone(&store), &sink))
            .catch_unwind()
            .await;
        // Fail closed before anything restarts. A store future that panics
        // after the batch has left the queue destroys jobs this node has
        // already told its producers it accepted; leaving that silent is the
        // one way a bounded, counted queue still loses events without saying
        // so. `take_inflight` is what makes the accounting exactly once --
        // a batch a shutdown drain already took is gone from the slot and is
        // not counted a second time here.
        if let Some(abandoned) = sink.take_inflight() {
            sink.metrics
                .dropped_writer_panic
                .fetch_add(abandoned.len() as u64, Ordering::Relaxed);
            tracing::error!(
                lost = abandoned.len(),
                "playback telemetry writer lost its in-flight batch"
            );
        }
        match outcome {
            Ok(()) => {
                sink.stopped.notify_one();
                return;
            }
            Err(_) => {
                let now = Instant::now();
                restarts.retain(|at| now.duration_since(*at) < Duration::from_secs(3_600));
                restarts.push(now);
                tracing::error!(
                    restarts = restarts.len(),
                    "playback telemetry writer panicked; restarting"
                );
                if restarts.len() >= WRITER_RESTARTS_PER_HOUR {
                    sink.degraded.store(true, Ordering::Release);
                    tracing::error!(
                        "playback telemetry writer exceeded restart budget; sink degraded"
                    );
                }
            }
        }
    }
}

async fn writer_loop(store: Arc<dyn WriterStore>, sink: &TelemetrySink) {
    loop {
        let batch = sink.queue.take_batch(BATCH);
        if batch.is_empty() {
            if sink.queue.is_closed() {
                return;
            }
            if sink.shutdown.is_cancelled() {
                // Shutdown was asked for and there is nothing left to write.
                // Closing here is what stops a producer racing in behind the
                // drain, and it goes through the same one-shot close the
                // timeout path uses.
                finish_drain(sink);
                return;
            }
            tokio::select! {
                _ = tokio::time::timeout(BATCH_WINDOW, sink.queue.ready.notified()) => {}
                () = sink.shutdown.cancelled() => {}
            }
            continue;
        }
        process_batch(&store, sink, batch).await;
    }
}

/// Close the queue once and count whatever closing it found.
fn finish_drain(sink: &TelemetrySink) {
    let remainder = sink.queue.close_and_drain();
    if !remainder.is_empty() {
        sink.metrics
            .dropped_shutdown
            .fetch_add(remainder.len() as u64, Ordering::Relaxed);
    }
    sink.metrics.queue_depth.store(0, Ordering::Relaxed);
}

async fn process_batch(store: &Arc<dyn WriterStore>, sink: &TelemetrySink, jobs: Vec<Job>) {
    let started = Instant::now();
    // Ownership moves to the sink before anything that can panic or block, so
    // the jobs are always somewhere an unwinding supervisor or a giving-up
    // drain can still find them.
    sink.put_inflight(jobs);
    if let Err(error) = sink.refresh_settings(store.as_ref(), false).await {
        tracing::warn!(%error, "refreshing playback telemetry settings failed; keeping cached values");
    }
    let settings = *sink
        .settings
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (events, observations, size) = {
        let mut held = sink
            .inflight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(jobs) = held.as_mut() else {
            // A shutdown timeout took the batch while the settings read was
            // in flight, and has already counted it.
            return;
        };
        coalesce_samples(jobs, sink.metrics);
        let events = if settings.retain {
            jobs.iter().map(|job| job.event.clone()).collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        // One batch, one lease: the priors are folded by the same Store call
        // that writes the retained rows rather than one call per event, which
        // is what kept the per-event sidecar contention alive.
        let observations = if settings.priors {
            jobs.iter()
                .filter_map(|job| prior_observation(&job.event, job.network.as_ref()))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        (events, observations, jobs.len())
    };
    let outcome = if events.is_empty() && observations.is_empty() {
        Ok(0)
    } else {
        store.write_batch(&events, &observations).await
    };
    if sink.take_inflight().is_none() {
        // The drain gave up on this batch and counted it as lost. The rows
        // may still land -- `spawn_blocking` is not cancellable -- but the
        // counter states the pessimistic answer once and never twice.
        return;
    }
    match outcome {
        Ok(written) => {
            sink.metrics
                .written_ok
                .fetch_add(written, Ordering::Relaxed);
        }
        Err(error) => {
            sink.metrics
                .written_error
                .fetch_add(events.len() as u64, Ordering::Relaxed);
            tracing::warn!(%error, count = events.len(), "recording playback telemetry batch failed");
        }
    }
    sink.metrics
        .queue_depth
        .store(sink.queue.len() as u64, Ordering::Relaxed);
    sink.metrics.record_batch(size, started.elapsed());
}

fn coalesce_samples(batch: &mut Vec<Job>, metrics: &QueueMetrics) {
    let mut retained: Vec<Job> = Vec::with_capacity(batch.len());
    for job in batch.drain(..) {
        let replace = retained.last().is_some_and(|previous| {
            previous.class == EventClass::Sample
                && job.class == EventClass::Sample
                && previous.event.session_id == job.event.session_id
                && previous.event.event == job.event.event
        });
        if replace {
            *retained.last_mut().expect("checked above") = job;
            metrics.dropped_coalesced.fetch_add(1, Ordering::Relaxed);
        } else {
            retained.push(job);
        }
    }
    *batch = retained;
}

pub(crate) async fn drain_for_shutdown(store: &Arc<dyn Store>) {
    let key = store_key(store);
    let sink = SINKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
        .cloned();
    let Some(sink) = sink else {
        return;
    };
    drain_sink(&sink).await;
}

/// The bounded shutdown drain, and the only owner of the two-second bound.
///
/// The writer keeps draining until the queue is closed; this is what closes
/// it if the writer cannot. Both endings go through `close_and_drain` and
/// `take_inflight`, each of which yields its contents to exactly one caller,
/// so the remainder is counted once: a writer that returns after the bound
/// finds its batch already taken and adds nothing, and a producer that
/// arrives after the close is refused rather than queued behind a writer that
/// is no longer going to run.
async fn drain_sink(sink: &Arc<TelemetrySink>) {
    sink.draining.store(true, Ordering::Release);
    let stopped = sink.stopped.notified();
    sink.shutdown.cancel();
    sink.queue.ready.notify_one();
    if tokio::time::timeout(DRAIN, stopped).await.is_err() {
        let mut remaining = sink.queue.close_and_drain().len() as u64;
        if let Some(jobs) = sink.take_inflight() {
            remaining += jobs.len() as u64;
        }
        sink.metrics
            .dropped_shutdown
            .fetch_add(remaining, Ordering::Relaxed);
        sink.metrics.queue_depth.store(0, Ordering::Relaxed);
        tracing::warn!(
            remaining,
            "playback telemetry drain reached its two-second bound"
        );
    }
}

fn classify(event: &PlaybackEvent) -> EventClass {
    if event.level.as_deref() == Some("error") {
        return EventClass::Terminal;
    }
    match event.event.as_str() {
        "control_terminal"
        | "control_retry_resource"
        | "control_hold_withheld"
        | "control_action_suppressed" => EventClass::Terminal,
        "ttff" | "session_start" | "stall" | "stall_recovery" | "suspend" | "resume" => {
            EventClass::Lifecycle
        }
        _ => EventClass::Sample,
    }
}

/// Record metrics and persist an event without delaying the caller. The
/// retention setting is read inside the task so setting `0` makes this a true
/// no-op while HTTP ingest can still return its existing 204 immediately.
pub fn emit(store: Arc<dyn Store>, event: PlaybackEvent) {
    emit_with_network(store, event, None);
}

/// Persist the N0 event and, independently when opted in, fold its bounded
/// network measurement into the matching prior. Keeping the two switches
/// independent means an operator may retain only the aggregate prior without
/// retaining raw playback events.
pub(crate) fn emit_with_network(
    store: Arc<dyn Store>,
    event: PlaybackEvent,
    network: Option<NetworkIdentity>,
) {
    // Metrics describe what this node observed, independently of whether raw
    // retention, queue admission, or the node-local sidecar succeeds.
    METRICS.record(&event);
    let sink = sink_for(store);
    let class = classify(&event);
    if sink.draining.load(Ordering::Acquire) && class == EventClass::Sample {
        sink.metrics
            .dropped_shutdown
            .fetch_add(1, Ordering::Relaxed);
        return;
    }
    if sink.degraded.load(Ordering::Acquire) && class != EventClass::Terminal {
        sink.metrics
            .dropped_writer_degraded
            .fetch_add(1, Ordering::Relaxed);
        return;
    }
    // One decision, taken inside the queue's own lock: see [`JobQueue`].
    match sink.queue.push(Job {
        class,
        event,
        network,
    }) {
        Admission::Queued => {
            sink.metrics.enqueued[class.index()].fetch_add(1, Ordering::Relaxed);
        }
        Admission::Displaced => {
            // The terminal is queued; the oldest sample it displaced is the
            // drop, and it is exactly the trade the reserve exists to make.
            sink.metrics.enqueued[class.index()].fetch_add(1, Ordering::Relaxed);
            sink.metrics
                .dropped_queue_full
                .fetch_add(1, Ordering::Relaxed);
        }
        Admission::Refused => {
            sink.metrics
                .dropped_queue_full
                .fetch_add(1, Ordering::Relaxed);
        }
        Admission::Closed => {
            sink.metrics
                .dropped_shutdown
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn prior_observation(
    event: &PlaybackEvent,
    network: Option<&NetworkIdentity>,
) -> Option<NetworkPriorObservation> {
    let network = network?;
    let credential_generation = network.credential_generation.as_ref()?;
    let user_id = network.user_id?;
    let client_kbps = event
        .bandwidth_kbps
        .filter(|value| *value > 0)
        .map(|value| u32::try_from(value).unwrap_or(u32::MAX));
    let delivered_kbps = event
        .delivered_bps
        .filter(|value| *value > 0)
        .map(|value| u32::try_from(value / 1_000).unwrap_or(u32::MAX));
    let throughput_kbps = match (client_kbps, delivered_kbps) {
        (Some(client), Some(delivered)) => Some(client.min(delivered)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
    let detail = event.detail.as_deref().unwrap_or_default();
    let starved = event.event == "stall"
        && (detail.contains("supply")
            || detail.contains("network")
            || detail.contains("blocked")
            || detail.contains("kind=buffering")
            || event.runway_ds.is_some_and(|runway_ds| runway_ds <= 15));
    let starved_rung_height = if starved {
        event.height.filter(|height| *height > 0)
    } else {
        None
    };
    (throughput_kbps.is_some() || starved_rung_height.is_some()).then(|| NetworkPriorObservation {
        user_id,
        credential_generation: credential_generation.clone(),
        client_class: network.client_class.clone(),
        network_fingerprint: network.network_fingerprint.clone(),
        throughput_kbps,
        starved_rung_height,
        observed_at_ms: event.at_unix_ms,
    })
}

pub fn prometheus() -> String {
    let mut metrics = METRICS.render();
    metrics.push_str(&QUEUE_METRICS.render());
    metrics.push_str(&SUBTITLE_SOURCE_METRICS.render());
    metrics
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitle_source_metrics_render_bounded_labels_and_bytes() {
        let metrics = SubtitleSourceMetrics::new();
        metrics.record(SubtitleSourceMetric::LookupHydrated);
        metrics.record(SubtitleSourceMetric::RequestBackground);
        metrics.record(SubtitleSourceMetric::VerdictMalformed);
        metrics.record(SubtitleSourceMetric::JobBytes(123));
        metrics.record(SubtitleSourceMetric::HydrationFetchedBytes(17));
        metrics.record(SubtitleSourceMetric::ForegroundSelfClaim);
        metrics.record(SubtitleSourceMetric::Repair);
        let rendered = metrics.render();
        assert!(rendered.contains("plurx_subtitle_source_lookups_total{outcome=\"hydrated\"} 1"));
        assert!(rendered.contains("plurx_subtitle_source_requests_total{trigger=\"background\"} 1"));
        assert!(rendered.contains("plurx_subtitle_source_verdicts_total{verdict=\"malformed\"} 1"));
        assert!(rendered.contains("plurx_subtitle_source_job_bytes_total 123"));
        assert!(rendered
            .contains("plurx_subtitle_source_hydration_bytes_total{direction=\"fetched\"} 17"));
        assert!(rendered.contains("plurx_subtitle_source_foreground_self_claims_total 1"));
        assert!(rendered.contains("plurx_subtitle_source_repairs_total 1"));
        assert!(!rendered.contains("node_id="));
        assert!(!rendered.contains("file_id="));
    }

    fn metric_value(rendered: &str, prefix: &str) -> u64 {
        rendered
            .lines()
            .find_map(|line| {
                line.strip_prefix(prefix)
                    .and_then(|value| value.trim().parse().ok())
            })
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn metrics_are_recorded_with_retention_off() {
        let store: Arc<dyn Store> =
            Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("telemetry store"));
        store
            .put_setting(keys::TELEMETRY_RETAIN_DAYS, "0")
            .await
            .expect("disable retention");
        initialize(Arc::clone(&store)).await.expect("seed settings");
        let prefix = "plurx_ttff_ms_count{method=\"remux\"} ";
        let before = metric_value(&prometheus(), prefix);

        emit(
            Arc::clone(&store),
            PlaybackEvent {
                event: "ttff".into(),
                method: Some("remux".into()),
                ms: Some(321),
                ..PlaybackEvent::default()
            },
        );
        tokio::time::sleep(Duration::from_millis(250)).await;

        assert_eq!(metric_value(&prometheus(), prefix), before + 1);
        assert!(store
            .playback_events(&plurx_core::domain::PlaybackEventQuery {
                limit: 10,
                ..Default::default()
            })
            .await
            .expect("retained events")
            .is_empty());
    }

    fn job(event: &str, session_id: &str, level: Option<&str>) -> Job {
        let event = PlaybackEvent {
            event: event.to_owned(),
            session_id: Some(session_id.to_owned()),
            level: level.map(str::to_owned),
            ..Default::default()
        };
        Job {
            class: classify(&event),
            event,
            network: None,
        }
    }

    #[test]
    fn durable_outcomes_and_error_levels_are_terminal() {
        for event in [
            "control_terminal",
            "control_retry_resource",
            "control_hold_withheld",
            "control_action_suppressed",
        ] {
            assert_eq!(classify(&job(event, "s", None).event), EventClass::Terminal);
        }
        assert_eq!(
            classify(&job("future_outcome", "s", Some("error")).event),
            EventClass::Terminal
        );
    }

    fn isolated_metrics() -> &'static QueueMetrics {
        Box::leak(Box::new(QueueMetrics::new()))
    }

    /// The reserve, exercised on the queue itself rather than on a predicate.
    ///
    /// A queue holding `QUEUE - TERMINAL_RESERVE` non-terminals refuses the
    /// next one and still accepts `TERMINAL_RESERVE` terminals, which is what
    /// "128 slots reserved" has to mean.
    #[test]
    fn the_reserve_is_what_the_queue_does_not_what_a_predicate_says() {
        let queue = JobQueue::new(isolated_metrics());
        for index in 0..QUEUE - TERMINAL_RESERVE {
            assert_eq!(
                queue.push(job("producer_pass", &format!("s{index}"), None)),
                Admission::Queued
            );
        }
        assert_eq!(
            queue.push(job("producer_pass", "overflow", None)),
            Admission::Refused
        );
        assert_eq!(
            queue.push(job("stall", "overflow", None)),
            Admission::Refused,
            "lifecycle is non-terminal and stops at the same ceiling"
        );
        for index in 0..TERMINAL_RESERVE {
            assert_eq!(
                queue.push(job("control_terminal", &format!("t{index}"), None)),
                Admission::Queued
            );
        }
        assert_eq!(queue.len(), QUEUE);
    }

    /// The case a reserve alone does not cover.
    ///
    /// With 896 samples and 128 terminals the queue is full, and a reserve
    /// that only refuses non-terminals refuses the next terminal too -- with
    /// 896 samples still queued in front of it. A terminal is the statement
    /// an operator reads after a restart; a periodic sample is not. So the
    /// terminal displaces the oldest sample, occupancy stays at `QUEUE`, and
    /// the only queue that can turn a terminal away is one already full of
    /// terminals.
    #[test]
    fn a_terminal_is_never_discarded_while_a_sample_is_queued() {
        let metrics = isolated_metrics();
        let queue = JobQueue::new(metrics);
        for index in 0..QUEUE - TERMINAL_RESERVE {
            assert_eq!(
                queue.push(job("producer_pass", &format!("s{index}"), None)),
                Admission::Queued
            );
        }
        for index in 0..TERMINAL_RESERVE {
            assert_eq!(
                queue.push(job("control_terminal", &format!("t{index}"), None)),
                Admission::Queued
            );
        }
        assert_eq!(queue.len(), QUEUE);

        assert_eq!(
            queue.push(job("control_terminal", "late", None)),
            Admission::Displaced,
            "a full queue must make room for a terminal, not refuse it"
        );
        assert_eq!(queue.len(), QUEUE, "and the memory bound is unchanged");
        let drained = queue.take_batch(QUEUE);
        assert_eq!(
            drained
                .iter()
                .filter(|queued| queued.event.session_id.as_deref() == Some("late"))
                .count(),
            1,
            "the late terminal is in the queue, not in the drop counter"
        );
        assert_eq!(
            drained
                .iter()
                .filter(|queued| queued.class == EventClass::Sample)
                .count(),
            QUEUE - TERMINAL_RESERVE - 1,
            "exactly one sample was displaced"
        );
        assert!(
            !drained
                .iter()
                .any(|queued| queued.event.session_id.as_deref() == Some("s0")),
            "and it was the oldest one"
        );
    }

    /// A queue of nothing but terminals is the only one that refuses a
    /// terminal, and it still never exceeds `QUEUE`.
    #[test]
    fn a_queue_of_terminals_refuses_a_terminal_and_stays_bounded() {
        let queue = JobQueue::new(isolated_metrics());
        for index in 0..QUEUE {
            assert_eq!(
                queue.push(job("control_terminal", &format!("t{index}"), None)),
                Admission::Queued
            );
        }
        assert_eq!(
            queue.push(job("control_terminal", "late", None)),
            Admission::Refused
        );
        assert_eq!(queue.len(), QUEUE);
    }

    /// Admission is atomic with the enqueue it authorises.
    ///
    /// Eight threads racing to fill the queue must admit EXACTLY
    /// `QUEUE - TERMINAL_RESERVE` non-terminals between them. A depth
    /// snapshot taken before a separate send cannot promise that: every
    /// racing producer can read the same sub-reserve depth and every one of
    /// them can then send, which is how the reserved slots get spent by the
    /// work they are reserved from.
    #[test]
    fn concurrent_producers_cannot_overrun_the_reserve() {
        let queue = Arc::new(JobQueue::new(isolated_metrics()));
        let admitted = Arc::new(AtomicU64::new(0));
        std::thread::scope(|scope| {
            for thread in 0..8 {
                let queue = Arc::clone(&queue);
                let admitted = Arc::clone(&admitted);
                scope.spawn(move || {
                    for index in 0..4_000 {
                        if queue.push(job("producer_pass", &format!("s{thread}-{index}"), None))
                            == Admission::Queued
                        {
                            admitted.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        assert_eq!(
            admitted.load(Ordering::Relaxed) as usize,
            QUEUE - TERMINAL_RESERVE,
            "non-terminal admissions must stop exactly at the reserve boundary"
        );
        assert_eq!(queue.len(), QUEUE - TERMINAL_RESERVE);
        for index in 0..TERMINAL_RESERVE {
            assert_eq!(
                queue.push(job("control_terminal", &format!("t{index}"), None)),
                Admission::Queued,
                "the reserve survived the race intact"
            );
        }
    }

    #[test]
    fn only_consecutive_samples_for_the_same_session_and_name_coalesce() {
        let before = QUEUE_METRICS.dropped_coalesced.load(Ordering::Relaxed);
        let mut jobs = vec![
            job("producer_pass", "one", None),
            job("producer_pass", "one", None),
            job("stall", "one", None),
            job("producer_pass", "one", None),
            job("producer_pass", "two", None),
            job("control_terminal", "one", None),
            job("control_terminal", "one", None),
        ];

        coalesce_samples(&mut jobs, &QUEUE_METRICS);

        assert_eq!(jobs.len(), 6);
        assert_eq!(
            QUEUE_METRICS.dropped_coalesced.load(Ordering::Relaxed),
            before + 1
        );
        assert_eq!(
            jobs.iter()
                .filter(|job| job.class == EventClass::Terminal)
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn shutdown_drain_returns_inside_its_bound() {
        let store: Arc<dyn Store> =
            Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("telemetry store"));
        initialize(Arc::clone(&store)).await.expect("seed settings");
        for index in 0..16 {
            emit(
                Arc::clone(&store),
                PlaybackEvent {
                    event: "producer_pass".into(),
                    session_id: Some(format!("session-{index}")),
                    ..Default::default()
                },
            );
        }
        let started = Instant::now();
        drain_for_shutdown(&store).await;
        assert!(started.elapsed() <= DRAIN + Duration::from_millis(250));
    }

    #[tokio::test]
    async fn settings_seed_precedes_the_first_event_and_local_changes_invalidate() {
        let store: Arc<dyn Store> =
            Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("telemetry store"));
        store
            .put_setting(keys::TELEMETRY_RETAIN_DAYS, "0")
            .await
            .expect("disable retention");
        store
            .put_setting(keys::PLAYBACK_NETWORK_PRIORS, "1")
            .await
            .expect("enable priors");

        initialize(Arc::clone(&store)).await.expect("seed settings");
        let sink = sink_for(Arc::clone(&store));
        let seeded = *sink.settings.read().expect("settings");
        assert!(!seeded.retain);
        assert!(seeded.priors);
        assert!(seeded.read_at.is_some());

        store
            .put_setting(keys::TELEMETRY_RETAIN_DAYS, "30")
            .await
            .expect("enable retention");
        invalidate_settings(&store);
        assert!(sink.settings.read().expect("settings").read_at.is_none());
        sink.refresh_settings(&DurableWriterStore(Arc::clone(&store)), false)
            .await
            .expect("refresh invalidated settings");
        assert!(sink.settings.read().expect("settings").retain);
    }

    #[tokio::test]
    async fn a_fresh_cache_skips_replicated_setting_reads_for_its_window() {
        let store: Arc<dyn Store> =
            Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("telemetry store"));
        initialize(Arc::clone(&store)).await.expect("seed settings");
        let sink = sink_for(Arc::clone(&store));
        let first_read = sink.settings.read().expect("settings").read_at;

        sink.refresh_settings(&DurableWriterStore(Arc::clone(&store)), false)
            .await
            .expect("cached refresh");

        assert_eq!(sink.settings.read().expect("settings").read_at, first_read);
    }

    #[test]
    fn playback_metrics_render_bounded_labels_and_counts() {
        let metrics = PlaybackMetrics::new();
        metrics.record(&PlaybackEvent {
            event: "ttff".into(),
            method: Some("remux".into()),
            ms: Some(684),
            ..PlaybackEvent::default()
        });
        metrics.record(&PlaybackEvent {
            event: "stall".into(),
            detail: Some("supply".into()),
            ..PlaybackEvent::default()
        });
        metrics.record(&PlaybackEvent {
            event: "stall_recovery".into(),
            detail: Some("attempt:transcode".into()),
            ..PlaybackEvent::default()
        });
        metrics.record(&PlaybackEvent {
            event: "stall_recovery".into(),
            detail: Some("recovered".into()),
            ..PlaybackEvent::default()
        });
        metrics.record(&PlaybackEvent {
            event: "suspend".into(),
            hold_reason: Some("time".into()),
            ..PlaybackEvent::default()
        });
        metrics.record(&PlaybackEvent {
            event: "resume".into(),
            ms: Some(1_500),
            ..PlaybackEvent::default()
        });
        for (event, detail) in [
            ("marker_offer", Some("intro")),
            ("marker_manual_skip", None),
            ("marker_automatic_skip", None),
            ("marker_seek_back", None),
            ("marker_prewarm", Some("hit")),
            ("marker_prewarm", Some("miss")),
        ] {
            metrics.record(&PlaybackEvent {
                event: event.to_owned(),
                detail: detail.map(str::to_owned),
                ..PlaybackEvent::default()
            });
        }
        metrics.record(&PlaybackEvent {
            event: "marker_prewarm".to_owned(),
            detail: Some("miss".to_owned()),
            method: Some("direct_play".to_owned()),
            ..PlaybackEvent::default()
        });
        let text = metrics.render();
        assert!(text.contains("plurx_ttff_ms_count{method=\"remux\"} 1"));
        assert!(text.contains("plurx_ttff_ms_bucket{method=\"remux\",le=\"1000\"} 1"));
        assert!(text.contains("plurx_stalls_total{kind=\"supply\"} 1"));
        assert!(text.contains("plurx_stall_recoveries_total{outcome=\"attempt\"} 1"));
        assert!(text.contains("plurx_stall_recoveries_total{outcome=\"recovered\"} 1"));
        assert!(text.contains("plurx_suspends_total{reason=\"time\"} 1"));
        assert!(text.contains("plurx_suspended_seconds_total 1.500"));
        assert!(text.contains("plurx_playback_marker_actions_total{action=\"offer\"} 1"));
        assert!(text.contains("plurx_playback_marker_actions_total{action=\"manual_skip\"} 1"));
        assert!(text.contains("plurx_playback_marker_actions_total{action=\"automatic_skip\"} 1"));
        assert!(text.contains("plurx_playback_marker_actions_total{action=\"undo_seek_back\"} 1"));
        assert!(text.contains("plurx_playback_marker_prewarm_total{result=\"hit\"} 1"));
        assert!(text.contains("plurx_playback_marker_prewarm_total{result=\"miss\"} 2"));
        assert!(text.contains("plurx_playback_marker_prewarm_hit_ratio 0.333333"));
        assert!(!text.contains("title="));
        assert!(!text.contains("user="));
        assert!(!text.contains("path="));
    }

    #[test]
    fn the_prewarm_ratio_stays_absent_until_a_hit_is_observed() {
        // This is the fleet's real state. Prewarming a marker destination is
        // unbuilt, so every callsite on every client reports a miss and does
        // so truthfully -- and a published quotient would sit at 0.000000
        // forever, which is indistinguishable from a feature that is failing.
        // The counters still carry every miss, so the suppression withholds a
        // claim rather than data.
        let metrics = PlaybackMetrics::new();
        for _ in 0..3 {
            metrics.record(&PlaybackEvent {
                event: "marker_prewarm".into(),
                detail: Some("miss".into()),
                ..PlaybackEvent::default()
            });
        }
        let text = metrics.render();
        assert!(text.contains("plurx_playback_marker_prewarm_total{result=\"miss\"} 3"));
        assert!(text.contains("plurx_playback_marker_prewarm_total{result=\"hit\"} 0"));
        assert!(!text.contains("plurx_playback_marker_prewarm_hit_ratio"));

        // A quiet node is the other reading the single value conflated: no
        // marker skip since boot is not a rate of zero either.
        let idle = PlaybackMetrics::new();
        assert!(!idle
            .render()
            .contains("plurx_playback_marker_prewarm_hit_ratio"));

        // The first hit is what makes the gauge mean something, and a genuine
        // zero rate publishes normally once one exists.
        metrics.record(&PlaybackEvent {
            event: "marker_prewarm".into(),
            detail: Some("hit".into()),
            ..PlaybackEvent::default()
        });
        assert!(metrics
            .render()
            .contains("plurx_playback_marker_prewarm_hit_ratio 0.250000"));
    }

    #[test]
    fn prior_observation_uses_conservative_throughput_and_supply_stalls_only() {
        let network = NetworkIdentity {
            client_class: "chrome".to_owned(),
            network_fingerprint: "192.0.2.0/24".to_owned(),
            credential_generation: Some(CredentialGeneration::from("test-gen".to_owned())),
            user_id: Some(42),
        };
        let event = PlaybackEvent {
            at_unix_ms: 123,
            user_id: Some(4),
            event: "stall".to_owned(),
            height: Some(720),
            bandwidth_kbps: Some(8_000),
            delivered_bps: Some(5_000_000),
            detail: Some("supply:empty".to_owned()),
            ..PlaybackEvent::default()
        };
        let observation = prior_observation(&event, Some(&network)).expect("observation");
        assert_eq!(observation.throughput_kbps, Some(5_000));
        assert_eq!(observation.starved_rung_height, Some(720));
        assert_eq!(observation.observed_at_ms, 123);

        let mut decode = event;
        decode.detail = Some("decode:late_frames".to_owned());
        let observation = prior_observation(&decode, Some(&network)).expect("throughput remains");
        assert_eq!(observation.starved_rung_height, None);

        decode.height = None;
        let observation = prior_observation(&decode, Some(&network))
            .expect("a throughput sample does not require a rung");
        assert_eq!(observation.throughput_kbps, Some(5_000));
    }

    /// How the writer called its Store, recorded call by call.
    #[derive(Default)]
    struct WriterCalls {
        /// `(events, observations)` per `write_batch` call.
        batches: Vec<(usize, usize)>,
    }

    /// A `WriterStore` that records its calls and can be told to sleep or to
    /// panic on the first of them.
    struct ScriptedWriterStore {
        calls: Arc<Mutex<WriterCalls>>,
        priors_enabled: bool,
        sleep: Option<Duration>,
        panic_first: AtomicBool,
    }

    impl ScriptedWriterStore {
        fn new(priors_enabled: bool) -> Self {
            Self {
                calls: Arc::new(Mutex::new(WriterCalls::default())),
                priors_enabled,
                sleep: None,
                panic_first: AtomicBool::new(false),
            }
        }

        fn sleeping(duration: Duration) -> Self {
            Self {
                sleep: Some(duration),
                ..Self::new(false)
            }
        }

        fn panicking_once() -> Self {
            let store = Self::new(false);
            store.panic_first.store(true, Ordering::Release);
            store
        }

        fn batches(&self) -> Vec<(usize, usize)> {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .batches
                .clone()
        }
    }

    impl WriterStore for ScriptedWriterStore {
        fn write_batch<'a>(
            &'a self,
            events: &'a [PlaybackEvent],
            observations: &'a [NetworkPriorObservation],
        ) -> BoxFuture<'a, Result<u64, StoreError>> {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .batches
                .push((events.len(), observations.len()));
            Box::pin(async move {
                if self.panic_first.swap(false, Ordering::AcqRel) {
                    panic!("injected node-local store panic");
                }
                if let Some(sleep) = self.sleep {
                    tokio::time::sleep(sleep).await;
                }
                Ok(events.len() as u64)
            })
        }

        fn setting_pair(&self) -> BoxFuture<'_, Result<SettingPair, StoreError>> {
            let priors = self.priors_enabled;
            Box::pin(async move {
                Ok((
                    Some("30".to_owned()),
                    Some(if priors { "1" } else { "0" }.to_owned()),
                ))
            })
        }
    }

    fn network_identity() -> NetworkIdentity {
        NetworkIdentity {
            client_class: "chrome".to_owned(),
            network_fingerprint: "192.0.2.0/24".to_owned(),
            credential_generation: Some(CredentialGeneration::from("c06-gen".to_owned())),
            user_id: Some(42),
        }
    }

    fn measured_job(session: &str) -> Job {
        let event = PlaybackEvent {
            event: "producer_pass".to_owned(),
            session_id: Some(session.to_owned()),
            bandwidth_kbps: Some(9_000),
            delivered_bps: Some(6_000_000),
            ..PlaybackEvent::default()
        };
        Job {
            class: classify(&event),
            event,
            network: Some(network_identity()),
        }
    }

    async fn wait_for(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..400 {
            if done() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        done()
    }

    /// One batch is one Store call even with the network-prior opt-in on.
    ///
    /// The batch is queued before the writer starts so the batch boundary is
    /// the test's, not the scheduler's. Folding the priors event by event --
    /// one `observe_network_prior` per job after the batched insert -- would
    /// make this 65 calls, and on the hiqlite sidecar 65 acquisitions of the
    /// one mutex that telemetry shares with fragment-index work.
    #[tokio::test]
    async fn a_batch_with_priors_on_is_one_store_call_not_one_per_event() {
        let metrics = isolated_metrics();
        let sink = new_sink(metrics);
        for index in 0..BATCH {
            assert_eq!(
                sink.queue.push(measured_job(&format!("session-{index}"))),
                Admission::Queued
            );
        }
        let store = Arc::new(ScriptedWriterStore::new(true));
        let observer = Arc::clone(&store);
        tokio::spawn(supervise_writer(store, Arc::clone(&sink)));

        assert!(
            wait_for(|| !observer.batches().is_empty()).await,
            "the writer never ran"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            observer.batches(),
            vec![(BATCH, BATCH)],
            "64 events and their 64 prior observations must cross the Store \
             boundary once, not once plus one per event"
        );
    }

    /// Retention off and priors on stays one call carrying only the priors:
    /// the switches are independent, the lease is shared.
    #[tokio::test]
    async fn the_two_opt_ins_stay_independent_inside_the_shared_batch() {
        let metrics = isolated_metrics();
        let sink = new_sink(metrics);
        sink.settings
            .write()
            .expect("settings")
            .clone_from(&EffectiveSettings {
                retain: false,
                priors: true,
                read_at: Some(Instant::now()),
            });
        for index in 0..8 {
            sink.queue.push(measured_job(&format!("session-{index}")));
        }
        // A cached read this fresh is never refreshed, so the sink keeps the
        // retention-off, priors-on pair the operator set.
        let store = Arc::new(ScriptedWriterStore::new(true));
        let observer = Arc::clone(&store);
        tokio::spawn(supervise_writer(store, Arc::clone(&sink)));

        assert!(wait_for(|| !observer.batches().is_empty()).await);
        assert_eq!(observer.batches(), vec![(0, 8)]);
    }

    /// A store future that panics destroys the batch the writer took out of
    /// the queue. Whatever else happens, that loss is counted.
    #[tokio::test]
    async fn a_panicking_store_accounts_for_the_batch_it_destroys() {
        let metrics = isolated_metrics();
        let sink = new_sink(metrics);
        for index in 0..8 {
            sink.queue.push(measured_job(&format!("doomed-{index}")));
        }
        let store = Arc::new(ScriptedWriterStore::panicking_once());
        let observer = Arc::clone(&store);
        tokio::spawn(supervise_writer(store, Arc::clone(&sink)));

        assert!(
            wait_for(|| metrics.dropped_writer_panic.load(Ordering::Relaxed) == 8).await,
            "a batch lost to a panicking store must raise a drop reason, not vanish; \
             saw {}",
            metrics.dropped_writer_panic.load(Ordering::Relaxed)
        );
        assert_eq!(metrics.written_ok.load(Ordering::Relaxed), 0);

        // And the writer is back: the restart is not a silent stop either.
        for index in 0..4 {
            sink.queue.push(measured_job(&format!("after-{index}")));
        }
        assert!(
            wait_for(|| metrics.written_ok.load(Ordering::Relaxed) == 4).await,
            "the supervisor did not restart the writer"
        );
        assert_eq!(observer.batches().len(), 2);
    }

    /// The shutdown bound transfers ownership; it does not guess from
    /// occupancy.
    ///
    /// A writer wedged inside a Store call still holds the batch it took. The
    /// remainder is therefore the queue AND that batch, counted once, by
    /// taking both away from the writer -- so a writer that returns after the
    /// bound cannot count them again as written.
    #[tokio::test]
    async fn a_wedged_writer_hands_shutdown_its_remainder_exactly_once() {
        let metrics = isolated_metrics();
        let sink = new_sink(metrics);
        for index in 0..QUEUE - TERMINAL_RESERVE {
            sink.queue.push(measured_job(&format!("sample-{index}")));
        }
        for index in 0..TERMINAL_RESERVE {
            sink.queue
                .push(job("control_terminal", &format!("terminal-{index}"), None));
        }
        assert_eq!(sink.queue.len(), QUEUE);

        let store = Arc::new(ScriptedWriterStore::sleeping(Duration::from_secs(4)));
        let observer = Arc::clone(&store);
        tokio::spawn(supervise_writer(store, Arc::clone(&sink)));
        assert!(
            wait_for(|| !observer.batches().is_empty()).await,
            "the writer never took a batch"
        );
        assert_eq!(sink.queue.len(), QUEUE - BATCH);

        drain_sink(&sink).await;
        assert_eq!(
            metrics.dropped_shutdown.load(Ordering::Relaxed),
            QUEUE as u64,
            "the queue remainder AND the wedged writer's in-flight batch are \
             the remainder; counting sender occupancy alone loses the batch"
        );

        // The store finally returns. The batch it was holding has already
        // been accounted for, so nothing is counted a second time.
        tokio::time::sleep(Duration::from_millis(2_500)).await;
        assert_eq!(
            metrics.dropped_shutdown.load(Ordering::Relaxed),
            QUEUE as u64
        );
        assert_eq!(
            metrics.written_ok.load(Ordering::Relaxed),
            0,
            "a batch reported dropped must not also be reported written"
        );
    }

    /// `emit` returns on the caller's thread whatever the store is doing, and
    /// the queue it feeds stays bounded.
    #[tokio::test]
    async fn emit_stays_synchronous_under_a_wedged_writer_and_the_queue_stays_bounded() {
        let metrics = isolated_metrics();
        let store: Arc<dyn Store> =
            Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("telemetry store"));
        let sink = install_test_sink(
            &store,
            Arc::new(ScriptedWriterStore::sleeping(Duration::from_secs(30))),
            metrics,
        );

        let started = Instant::now();
        for index in 0..5_000 {
            emit(
                Arc::clone(&store),
                PlaybackEvent {
                    event: "producer_pass".into(),
                    session_id: Some(format!("session-{index}")),
                    ..PlaybackEvent::default()
                },
            );
        }
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "5,000 emits took {elapsed:?} against a store that sleeps 30s"
        );
        assert!(
            sink.queue.len() <= QUEUE,
            "queue depth {} exceeded its bound",
            sink.queue.len()
        );
        assert!(
            metrics.dropped_queue_full.load(Ordering::Relaxed) > 0,
            "a full queue must count what it refuses"
        );
        assert_eq!(metrics.written_ok.load(Ordering::Relaxed), 0);

        // And the reserve is still there for the outcome that matters.
        emit(
            Arc::clone(&store),
            PlaybackEvent {
                event: "control_terminal".into(),
                session_id: Some("terminal".into()),
                ..PlaybackEvent::default()
            },
        );
        assert_eq!(
            metrics.enqueued[EventClass::Terminal.index()].load(Ordering::Relaxed),
            1,
            "a terminal was refused by a queue full of samples"
        );
    }

    /// `emit` against a Store boot never registered must build the sink and
    /// return.
    ///
    /// It runs on its own thread with a timeout because the failure it guards
    /// is a deadlock, and a deadlock does not fail a test -- it hangs the
    /// whole binary, which is how this reached a review as a passing branch.
    #[test]
    fn an_unregistered_store_registers_its_sink_without_relocking() {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let store: Arc<dyn Store> = Arc::new(
                    plurx_core::store::SqliteStore::open_in_memory().expect("telemetry store"),
                );
                emit(
                    Arc::clone(&store),
                    PlaybackEvent {
                        event: "ttff".into(),
                        ..PlaybackEvent::default()
                    },
                );
                let registered = SINKS
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .contains_key(&store_key(&store));
                let _ = sender.send(registered);
            });
        });
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(10)),
            Ok(true),
            "emit against an unregistered Store must build its sink and return"
        );
    }

    #[test]
    fn every_drop_reason_is_rendered_with_a_fixed_label() {
        let metrics = QueueMetrics::new();
        let rendered = metrics.render();
        for reason in [
            "queue_full",
            "coalesced",
            "writer_degraded",
            "writer_panic",
            "shutdown",
        ] {
            assert!(
                rendered.contains(&format!(
                    "plurx_telemetry_dropped_total{{reason=\"{reason}\"}}"
                )),
                "missing drop reason {reason}"
            );
        }
        assert!(!rendered.contains("session"));
    }
}
