//! Node-local playback observations and their bounded Prometheus projection.

use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use plurx_core::domain::{CredentialGeneration, NetworkPriorObservation, PlaybackEvent};
use plurx_core::error::StoreError;
use plurx_core::store::{keys, Store};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// Two buckets above 30 s (C-08 M5 row 1): a p99 that lands in `+Inf` is not
/// a p99, and a cold transcode start on a slow node does reach a minute.
const TTFF_BUCKETS: [i64; 10] = [
    100, 250, 500, 1_000, 2_500, 5_000, 10_000, 30_000, 60_000, 120_000,
];
const METHODS: [&str; 4] = ["direct_play", "remux", "transcode", "unknown"];
/// The closed vocabulary `http::network::client_class` maps a `User-Agent`
/// onto. The classifier returns only these literals, and `other` is the last
/// entry so [`label_index`] folds anything unexpected into it: the label can
/// never carry a raw User-Agent.
pub(crate) const CLIENT_CLASSES: [&str; 7] = [
    "chrome", "safari", "firefox", "edge", "apple", "android", "other",
];
/// Seek command to first presented frame, in ms (C-08 M5 row 2).
const SEEK_BUCKETS: [i64; 8] = [100, 250, 500, 1_000, 2_500, 5_000, 10_000, 30_000];
/// The bounded pools whose admission wait is timed (C-08 M5 row 6). The probe
/// gate is deliberately absent: `plurx_decode_facts_phase_seconds
/// {phase="gate_wait"}` already times exactly that wait.
const ADMISSION_POOLS: [&str; 3] = ["vod_blocked_get", "encode_permit", "image_materialize"];
const ADMISSION_WAIT_BUCKETS_US: [u64; 12] = [
    1_000,
    10_000,
    50_000,
    100_000,
    250_000,
    500_000,
    1_000_000,
    2_500_000,
    5_000_000,
    10_000_000,
    30_000_000,
    u64::MAX,
];
/// Watched-time ledger bounds (C-08 M5 row 3). One entry per viewer and item
/// on this node, forgotten once its last beat is older than the gap.
const WATCH_LEDGER_CAP: usize = 4_096;
const WATCH_BEAT_MAX_GAP: Duration = Duration::from_secs(120);
const WATCH_BEAT_SLACK_MS: u64 = 2_000;
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
    ttff_buckets: [[[AtomicU64; TTFF_BUCKETS.len() + 1]; CLIENT_CLASSES.len()]; METHODS.len()],
    ttff_count: [[AtomicU64; CLIENT_CLASSES.len()]; METHODS.len()],
    ttff_sum: [[AtomicU64; CLIENT_CLASSES.len()]; METHODS.len()],
    seek_buckets: [[AtomicU64; SEEK_BUCKETS.len() + 1]; METHODS.len()],
    seek_sum: [AtomicU64; METHODS.len()],
    seeks: [AtomicU64; METHODS.len()],
    stalls: [AtomicU64; STALL_KINDS.len()],
    stalled_ms: [AtomicU64; STALL_KINDS.len()],
    watched_ms: [AtomicU64; METHODS.len()],
    delivered_bytes: [AtomicU64; METHODS.len()],
    admission_wait: [[AtomicU64; ADMISSION_WAIT_BUCKETS_US.len()]; ADMISSION_POOLS.len()],
    admission_wait_sum_us: [AtomicU64; ADMISSION_POOLS.len()],
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
            ttff_buckets: [const {
                [const { [const { AtomicU64::new(0) }; TTFF_BUCKETS.len() + 1] };
                    CLIENT_CLASSES.len()]
            }; METHODS.len()],
            ttff_count: [const { [const { AtomicU64::new(0) }; CLIENT_CLASSES.len()] };
                METHODS.len()],
            ttff_sum: [const { [const { AtomicU64::new(0) }; CLIENT_CLASSES.len()] };
                METHODS.len()],
            seek_buckets: [const { [const { AtomicU64::new(0) }; SEEK_BUCKETS.len() + 1] };
                METHODS.len()],
            seek_sum: [const { AtomicU64::new(0) }; METHODS.len()],
            seeks: [const { AtomicU64::new(0) }; METHODS.len()],
            stalls: [const { AtomicU64::new(0) }; STALL_KINDS.len()],
            stalled_ms: [const { AtomicU64::new(0) }; STALL_KINDS.len()],
            watched_ms: [const { AtomicU64::new(0) }; METHODS.len()],
            delivered_bytes: [const { AtomicU64::new(0) }; METHODS.len()],
            admission_wait: [const { [const { AtomicU64::new(0) }; ADMISSION_WAIT_BUCKETS_US.len()] };
                ADMISSION_POOLS.len()],
            admission_wait_sum_us: [const { AtomicU64::new(0) }; ADMISSION_POOLS.len()],
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

    #[cfg(test)]
    fn record(&self, event: &PlaybackEvent) {
        self.record_from(event, None);
    }

    /// `client` is the requester's class from `http::network::client_class`,
    /// derived from the request that carried the event; `None` for events the
    /// server raises itself, which folds into `other`.
    fn record_from(&self, event: &PlaybackEvent, client: Option<&str>) {
        match event.event.as_str() {
            "ttff" => {
                let Some(ms) = event.ms.filter(|value| *value >= 0) else {
                    return;
                };
                let method = label_index(event.method.as_deref(), &METHODS);
                let client = label_index(client, &CLIENT_CLASSES);
                let bucket = TTFF_BUCKETS
                    .iter()
                    .position(|upper| ms <= *upper)
                    .unwrap_or(TTFF_BUCKETS.len());
                self.ttff_buckets[method][client][bucket].fetch_add(1, Ordering::Relaxed);
                self.ttff_count[method][client].fetch_add(1, Ordering::Relaxed);
                self.ttff_sum[method][client].fetch_add(ms as u64, Ordering::Relaxed);
            }
            // The client's report that a seek reached a presented frame, with
            // `ms` from the seek command to that frame. Counted in `seeks`
            // too, so `_count / seeks_total` is the share of seeks that got
            // a picture at all.
            "seek_resumed" => {
                let method = label_index(event.method.as_deref(), &METHODS);
                self.seeks[method].fetch_add(1, Ordering::Relaxed);
                let Some(ms) = event.ms.filter(|value| *value >= 0) else {
                    return;
                };
                let bucket = SEEK_BUCKETS
                    .iter()
                    .position(|upper| ms <= *upper)
                    .unwrap_or(SEEK_BUCKETS.len());
                self.seek_buckets[method][bucket].fetch_add(1, Ordering::Relaxed);
                self.seek_sum[method].fetch_add(ms as u64, Ordering::Relaxed);
            }
            // A seek that ended without a picture: superseded by another seek,
            // or the player stopped first. The denominator's other half.
            "seek_abandoned" => {
                let method = label_index(event.method.as_deref(), &METHODS);
                self.seeks[method].fetch_add(1, Ordering::Relaxed);
            }
            "stall" => {
                let kind = label_index(
                    Some(stall_kind(event.detail.as_deref().unwrap_or_default())),
                    &STALL_KINDS,
                );
                self.stalls[kind].fetch_add(1, Ordering::Relaxed);
                // Every first-party client sends the stall's duration in `ms`
                // (web `recordWaitStall`, Android `sampleStall`, Apple
                // `ApplePlaybackStallLog`); the web's diagnosis beacon has
                // none and adds a count without seconds, which is honest.
                // Android and Apple report once, when the stall ends. The web
                // reports a long wait while it is still frozen, at
                // `PERSISTENT_STALL_MS`, with the time so far; the rest of it
                // arrives as `stall_end` below.
                if let Some(ms) = event.ms.filter(|value| *value > 0) {
                    self.stalled_ms[kind].fetch_add(ms as u64, Ordering::Relaxed);
                }
            }
            // The close of a stall that was already reported while it was
            // still going (the web's persistent wait). `ms` is only the time
            // after that report, so crediting it adds the remainder: the two
            // reports together are the stall's whole length, and it is one
            // stall, so the count does not move.
            "stall_end" => {
                let kind = label_index(
                    Some(stall_kind(event.detail.as_deref().unwrap_or_default())),
                    &STALL_KINDS,
                );
                if let Some(ms) = event.ms.filter(|value| *value > 0) {
                    self.stalled_ms[kind].fetch_add(ms as u64, Ordering::Relaxed);
                }
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
            for (client_index, client) in CLIENT_CLASSES.iter().enumerate() {
                let buckets = &self.ttff_buckets[method_index][client_index];
                let mut cumulative = 0;
                for (bucket_index, upper) in TTFF_BUCKETS.iter().enumerate() {
                    cumulative += buckets[bucket_index].load(Ordering::Relaxed);
                    out.push_str(&format!(
                        "plurx_ttff_ms_bucket{{method=\"{method}\",client=\"{client}\",le=\"{upper}\"}} {cumulative}\n"
                    ));
                }
                cumulative += buckets[TTFF_BUCKETS.len()].load(Ordering::Relaxed);
                out.push_str(&format!(
                    "plurx_ttff_ms_bucket{{method=\"{method}\",client=\"{client}\",le=\"+Inf\"}} {cumulative}\n\
                     plurx_ttff_ms_sum{{method=\"{method}\",client=\"{client}\"}} {}\n\
                     plurx_ttff_ms_count{{method=\"{method}\",client=\"{client}\"}} {}\n",
                    self.ttff_sum[method_index][client_index].load(Ordering::Relaxed),
                    self.ttff_count[method_index][client_index].load(Ordering::Relaxed)
                ));
            }
        }
        self.render_release_evidence(&mut out);
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

impl PlaybackMetrics {
    /// The C-08 M5 release-evidence families that live beside the playback
    /// counters. Every label value is from a fixed array above.
    fn render_release_evidence(&self, out: &mut String) {
        out.push_str(
            "# HELP plurx_seek_to_picture_ms Client-reported time from a seek command to the first presented frame.\n\
             # TYPE plurx_seek_to_picture_ms histogram\n",
        );
        for (method_index, method) in METHODS.iter().enumerate() {
            let mut cumulative = 0;
            for (bucket_index, upper) in SEEK_BUCKETS.iter().enumerate() {
                cumulative += self.seek_buckets[method_index][bucket_index].load(Ordering::Relaxed);
                out.push_str(&format!(
                    "plurx_seek_to_picture_ms_bucket{{method=\"{method}\",le=\"{upper}\"}} {cumulative}\n"
                ));
            }
            cumulative +=
                self.seek_buckets[method_index][SEEK_BUCKETS.len()].load(Ordering::Relaxed);
            out.push_str(&format!(
                "plurx_seek_to_picture_ms_bucket{{method=\"{method}\",le=\"+Inf\"}} {cumulative}\n\
                 plurx_seek_to_picture_ms_sum{{method=\"{method}\"}} {}\n\
                 plurx_seek_to_picture_ms_count{{method=\"{method}\"}} {cumulative}\n",
                self.seek_sum[method_index].load(Ordering::Relaxed),
            ));
        }
        render_counters(
            out,
            "plurx_seeks_total",
            "Client-reported seeks that ended, with a picture (seek_resumed) or without one (seek_abandoned).",
            "method",
            &METHODS,
            &self.seeks,
        );
        render_seconds(
            out,
            "plurx_stalled_seconds_total",
            "Seconds of viewer-visible stall reported by clients. Not encoder suspension; see plurx_suspended_seconds_total.",
            "kind",
            &STALL_KINDS,
            &self.stalled_ms,
        );
        render_seconds(
            out,
            "plurx_watched_seconds_total",
            "Seconds this node saw a player's position advance between progress beats, never more than the wall time between them.",
            "method",
            &METHODS,
            &self.watched_ms,
        );
        render_counters(
            out,
            "plurx_delivered_bytes_total",
            "Media bytes handed to viewers' connections, by delivery method.",
            "method",
            &METHODS,
            &self.delivered_bytes,
        );
        out.push_str(
            "# HELP plurx_admission_wait_seconds Time spent waiting for admission to a bounded pool.\n\
             # TYPE plurx_admission_wait_seconds histogram\n",
        );
        for (pool_index, pool) in ADMISSION_POOLS.iter().enumerate() {
            let mut cumulative = 0;
            for (upper, count) in ADMISSION_WAIT_BUCKETS_US
                .iter()
                .zip(&self.admission_wait[pool_index])
            {
                cumulative += count.load(Ordering::Relaxed);
                let upper = if *upper == u64::MAX {
                    "+Inf".to_owned()
                } else {
                    format!("{}", *upper as f64 / 1_000_000.0)
                };
                out.push_str(&format!(
                    "plurx_admission_wait_seconds_bucket{{pool=\"{pool}\",le=\"{upper}\"}} {cumulative}\n"
                ));
            }
            out.push_str(&format!(
                "plurx_admission_wait_seconds_sum{{pool=\"{pool}\"}} {:.6}\n\
                 plurx_admission_wait_seconds_count{{pool=\"{pool}\"}} {cumulative}\n",
                self.admission_wait_sum_us[pool_index].load(Ordering::Relaxed) as f64 / 1_000_000.0,
            ));
        }
    }

    fn record_admission_wait(&self, pool: AdmissionPool, waited: Duration) {
        let pool = pool as usize;
        let micros = u64::try_from(waited.as_micros()).unwrap_or(u64::MAX);
        let bucket = ADMISSION_WAIT_BUCKETS_US
            .iter()
            .position(|upper| micros <= *upper)
            .unwrap_or(ADMISSION_WAIT_BUCKETS_US.len() - 1);
        self.admission_wait[pool][bucket].fetch_add(1, Ordering::Relaxed);
        self.admission_wait_sum_us[pool].fetch_add(micros, Ordering::Relaxed);
    }
}

/// One stall report's kind, from the free-text detail every client sends.
fn stall_kind(detail: &str) -> &'static str {
    if detail.contains("supply") {
        "supply"
    } else if detail.contains("decode") || detail.contains("frame") {
        "decode"
    } else if detail.contains("network") || detail.contains("blocked") {
        "network"
    } else {
        "other"
    }
}

fn render_seconds<const N: usize>(
    out: &mut String,
    name: &str,
    help: &str,
    label_name: &str,
    labels: &[&str; N],
    millis: &[AtomicU64; N],
) {
    out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} counter\n"));
    for (label, value) in labels.iter().zip(millis) {
        out.push_str(&format!(
            "{name}{{{label_name}=\"{label}\"}} {:.3}\n",
            value.load(Ordering::Relaxed) as f64 / 1_000.0
        ));
    }
}

/// The pools `plurx_admission_wait_seconds` times. The discriminant is the
/// index into [`ADMISSION_POOLS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdmissionPool {
    /// A blocked VOD segment GET, from admission to the pool until its wait
    /// ends (`waitpool::RegisteredWait::wait`).
    VodBlockedGet = 0,
    /// A live start's queue for an encoder slot or software permit, for as
    /// long as its `admission::LiveWait` guard lives.
    EncodePermit = 1,
    /// An artwork derivative waiting for a derive permit.
    ImageMaterialize = 2,
}

/// Record how long one admission waited. Called once per wait, whatever it
/// ended in: a wait that timed out is still a wait, and is the long tail.
pub(crate) fn record_admission_wait(pool: AdmissionPool, waited: Duration) {
    METRICS.record_admission_wait(pool, waited);
    #[cfg(test)]
    THREAD_ADMISSION_WAITS.with(|cells| cells.borrow_mut()[pool as usize] += 1);
}

/// Times one admission wait from its creation until it is dropped, and
/// records it then. A wait is created where the request starts waiting, so
/// every way it can end records exactly once: granted, timed out, refused
/// after the fact, or abandoned because the request's future was dropped (a
/// player that seeks again aborts the GET it had parked). Those abandoned
/// waits are the seek-storm tail row 6 exists to show; a recording placed
/// after the `await` never runs for them.
pub(crate) struct AdmissionWaitTimer {
    pool: AdmissionPool,
    started: Instant,
}

impl AdmissionWaitTimer {
    pub(crate) fn start(pool: AdmissionPool) -> Self {
        Self {
            pool,
            started: Instant::now(),
        }
    }
}

impl Drop for AdmissionWaitTimer {
    fn drop(&mut self) {
        record_admission_wait(self.pool, self.started.elapsed());
    }
}

// Per-thread copies of the node-wide cells, for tests that must count exactly
// what their own code path recorded while the rest of the suite runs in
// parallel. A `#[tokio::test]` runs its handler, its response body and its
// drops on its own thread, so these see that test's records and no other's.
#[cfg(test)]
thread_local! {
    static THREAD_DELIVERED: std::cell::RefCell<[u64; METHODS.len()]> =
        const { std::cell::RefCell::new([0; METHODS.len()]) };
    static THREAD_WATCHED: std::cell::RefCell<[u64; METHODS.len()]> =
        const { std::cell::RefCell::new([0; METHODS.len()]) };
    static THREAD_ADMISSION_WAITS: std::cell::RefCell<[u64; ADMISSION_POOLS.len()]> =
        const { std::cell::RefCell::new([0; ADMISSION_POOLS.len()]) };
}

#[cfg(test)]
pub(crate) fn delivered_bytes_on_this_thread(method: &str) -> u64 {
    THREAD_DELIVERED.with(|cells| cells.borrow()[label_index(Some(method), &METHODS)])
}

#[cfg(test)]
pub(crate) fn watched_ms_on_this_thread(method: &str) -> u64 {
    THREAD_WATCHED.with(|cells| cells.borrow()[label_index(Some(method), &METHODS)])
}

#[cfg(test)]
pub(crate) fn admission_waits_on_this_thread(pool: AdmissionPool) -> u64 {
    THREAD_ADMISSION_WAITS.with(|cells| cells.borrow()[pool as usize])
}

/// Credit media bytes as they are handed to a viewer's connection.
/// `method` is one of the playback vocabulary's values; anything else is
/// `unknown`.
pub(crate) fn record_delivered_bytes(method: &str, bytes: u64) {
    crate::media_pool::observe_media_io(bytes, 0, None);
    METRICS.delivered_bytes[label_index(Some(method), &METHODS)]
        .fetch_add(bytes, Ordering::Relaxed);
    #[cfg(test)]
    THREAD_DELIVERED.with(|cells| cells.borrow_mut()[label_index(Some(method), &METHODS)] += bytes);
}

/// Watched time from progress beats (C-08 M5 row 3's denominator).
///
/// The progress beat is the one signal every first-party player sends every
/// few seconds while it is open, playing or paused, and each beat reaches
/// exactly one node. A beat credits the smaller of the position's advance and
/// the wall time since the viewer's previous beat for that item. So: a paused
/// player (position unchanged) credits nothing; a stall (position stuck)
/// credits nothing; a seek (an advance more than twice the wall time, plus
/// slack) credits nothing; a gap longer than [`WATCH_BEAT_MAX_GAP`] credits
/// nothing. The credit is never more than the wall time between two beats.
///
/// "The previous beat" is cluster-wide. A viewer's beats are not held to one
/// node (the routing contract lets any non-HLS request go anywhere, and the
/// Android client keeps no cookie), so a node's own last beat for the viewer
/// may be older than one another node has already credited. Each beat
/// therefore also looks at the durable progress row the handler read before
/// writing: when another writer moved that row after this node's last beat,
/// the row is the baseline and this node credits only the time since it.
/// Beats alternating between N nodes are then credited once, not N times.
/// With only one node in play the row is this node's own write and the
/// monotonic local baseline is used, which is exact.
#[derive(Default)]
pub(crate) struct WatchLedger {
    beats: Mutex<HashMap<(i64, i64), LedgerBeat>>,
}

/// This node's last beat for one viewer and item.
#[derive(Clone, Copy)]
struct LedgerBeat {
    position_ms: i64,
    /// The position of the beat before this one. A beat that arrives less
    /// than a second after another can find the durable row still holding
    /// that earlier beat of this node's own.
    prior_position_ms: Option<i64>,
    at: Instant,
    /// The same moment on the wall clock, to compare with the durable row's
    /// `updated_at` (whole seconds, written by whichever node committed it).
    wall_ms: i64,
}

impl WatchLedger {
    #[cfg(test)]
    fn beat(&self, user_id: i64, item_id: i64, position_ms: i64, now: Instant) -> u64 {
        self.beat_after(user_id, item_id, position_ms, now, 0, None)
    }

    /// `durable` is the viewer's progress row as it stood before this beat
    /// was written, or `None` when there is none (or the caller has none).
    fn beat_after(
        &self,
        user_id: i64,
        item_id: i64,
        position_ms: i64,
        now: Instant,
        now_wall_ms: i64,
        durable: Option<&plurx_core::domain::WatchState>,
    ) -> u64 {
        let Ok(mut beats) = self.beats.lock() else {
            return 0;
        };
        let key = (user_id, item_id);
        let previous = beats.get(&key).copied();
        if previous.is_none() && beats.len() >= WATCH_LEDGER_CAP {
            beats.retain(|_, beat| now.saturating_duration_since(beat.at) <= WATCH_BEAT_MAX_GAP);
            if beats.len() >= WATCH_LEDGER_CAP {
                // Full of live viewers: this one is not tracked, and is not
                // credited, rather than evicting someone mid-film.
                return 0;
            }
        }
        beats.insert(
            key,
            LedgerBeat {
                position_ms,
                prior_position_ms: previous.map(|previous| previous.position_ms),
                at: now,
                wall_ms: now_wall_ms,
            },
        );
        if let Some(row) = durable.filter(|row| moved_by_another_writer(row, previous)) {
            let since_ms = now_wall_ms.saturating_sub(row.updated_at.saturating_mul(1_000));
            let elapsed = Duration::from_millis(u64::try_from(since_ms).unwrap_or(0));
            return watched_credit_ms(row.position_ms, elapsed, position_ms);
        }
        previous.map_or(0, |previous| {
            watched_credit_ms(
                previous.position_ms,
                now.saturating_duration_since(previous.at),
                position_ms,
            )
        })
    }
}

/// True when the durable row is a newer beat than this node's own last one:
/// its position is neither of the two this node last saw (this node's own
/// commits and coalesced flushes write one of those), and it was written no
/// earlier than the last of them. `updated_at` is whole seconds, so "no earlier"
/// allows the second it was truncated from. With no local beat at all, the
/// row is the only baseline there is; the gap rule still ignores a stale one.
fn moved_by_another_writer(
    row: &plurx_core::domain::WatchState,
    previous: Option<LedgerBeat>,
) -> bool {
    previous.is_none_or(|previous| {
        row.position_ms != previous.position_ms
            && Some(row.position_ms) != previous.prior_position_ms
            && row.updated_at.saturating_mul(1_000).saturating_add(1_000) > previous.wall_ms
    })
}

fn watched_credit_ms(previous_ms: i64, elapsed: Duration, position_ms: i64) -> u64 {
    if elapsed > WATCH_BEAT_MAX_GAP {
        return 0;
    }
    let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    let Some(advance) = position_ms
        .checked_sub(previous_ms)
        .filter(|advance| *advance > 0)
        .and_then(|advance| u64::try_from(advance).ok())
    else {
        return 0;
    };
    if advance
        > elapsed_ms
            .saturating_mul(2)
            .saturating_add(WATCH_BEAT_SLACK_MS)
    {
        return 0;
    }
    advance.min(elapsed_ms)
}

/// Read one node-wide release-evidence cell. Tests elsewhere in the crate use
/// these to prove their call site feeds the family, without parsing text.
#[cfg(test)]
pub(crate) fn delivered_bytes_for_test(method: &str) -> u64 {
    METRICS.delivered_bytes[label_index(Some(method), &METHODS)].load(Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn admission_waits_for_test(pool: AdmissionPool) -> u64 {
    METRICS.admission_wait[pool as usize]
        .iter()
        .map(|count| count.load(Ordering::Relaxed))
        .sum()
}

#[cfg(test)]
pub(crate) fn watched_ms_for_test(method: &str) -> u64 {
    METRICS.watched_ms[label_index(Some(method), &METHODS)].load(Ordering::Relaxed)
}

/// One live progress beat (never an offline replay, which carries its own
/// clock). `method` is what the client says it is playing through; a value
/// outside the playback vocabulary, or none, is `unknown`.
///
/// `durable` is the viewer's progress row as the handler read it before
/// writing this beat; see [`WatchLedger`] for why the credit needs it.
pub(crate) fn record_progress_beat(
    ledger: &WatchLedger,
    user_id: i64,
    item_id: i64,
    position_ms: i64,
    method: Option<&str>,
    durable: Option<&plurx_core::domain::WatchState>,
) {
    let wall_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        });
    let credited = record_progress_beat_at(
        ledger,
        &METRICS,
        BeatAt {
            user_id,
            item_id,
            position_ms,
            method,
            now: Instant::now(),
            wall_ms,
            durable,
        },
    );
    #[cfg(test)]
    THREAD_WATCHED.with(|cells| cells.borrow_mut()[label_index(method, &METHODS)] += credited);
    #[cfg(not(test))]
    let _ = credited;
}

struct BeatAt<'a> {
    user_id: i64,
    item_id: i64,
    position_ms: i64,
    method: Option<&'a str>,
    now: Instant,
    wall_ms: i64,
    durable: Option<&'a plurx_core::domain::WatchState>,
}

fn record_progress_beat_at(ledger: &WatchLedger, metrics: &PlaybackMetrics, beat: BeatAt) -> u64 {
    let credited = ledger.beat_after(
        beat.user_id,
        beat.item_id,
        beat.position_ms,
        beat.now,
        beat.wall_ms,
        beat.durable,
    );
    if credited > 0 {
        metrics.watched_ms[label_index(beat.method, &METHODS)]
            .fetch_add(credited, Ordering::Relaxed);
    }
    credited
}

#[cfg(test)]
fn record_progress_beat_into(
    ledger: &WatchLedger,
    metrics: &PlaybackMetrics,
    user_id: i64,
    item_id: i64,
    position_ms: i64,
    method: Option<&str>,
    now: Instant,
) {
    record_progress_beat_at(
        ledger,
        metrics,
        BeatAt {
            user_id,
            item_id,
            position_ms,
            method,
            now,
            wall_ms: 0,
            durable: None,
        },
    );
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

/// C-08 M5 row 4: how each finite-playback start ended. The attempt itself is
/// kept by [`crate::playstart::StartAttempts`], which resolves every attempt it
/// opens exactly once; these cells are only where the resolutions land.
pub(crate) const START_OUTCOMES: [&str; 4] = ["ok", "refused", "failed", "cancelled"];
/// The terminal client events that arrived with no attempt on this node to
/// resolve (see [`crate::playstart::StartAttempts`]): a first frame, or a
/// failure report, the ledger could not pair.
pub(crate) const START_UNPAIRED: [&str; 2] = ["ok", "failed"];

struct StartOutcomeCounters {
    outcomes: [[AtomicU64; START_OUTCOMES.len()]; METHODS.len()],
    unpaired: [AtomicU64; START_UNPAIRED.len()],
}

impl StartOutcomeCounters {
    const fn new() -> Self {
        Self {
            outcomes: [const { [const { AtomicU64::new(0) }; START_OUTCOMES.len()] };
                METHODS.len()],
            unpaired: [const { AtomicU64::new(0) }; START_UNPAIRED.len()],
        }
    }

    fn render(&self) -> String {
        let mut out = String::from(
            "# HELP plurx_start_outcomes_total Finite-playback start attempts by how they ended: a first frame (ok), a server refusal of the start request (refused), a client failure report before any frame (failed), or neither within the start deadline (cancelled).\n\
             # TYPE plurx_start_outcomes_total counter\n",
        );
        for (method_index, method) in METHODS.iter().enumerate() {
            for (outcome_index, outcome) in START_OUTCOMES.iter().enumerate() {
                out.push_str(&format!(
                    "plurx_start_outcomes_total{{method=\"{method}\",outcome=\"{outcome}\"}} {}\n",
                    self.outcomes[method_index][outcome_index].load(Ordering::Relaxed)
                ));
            }
        }
        out.push_str(
            "# HELP plurx_start_outcomes_unpaired_total First-frame and start-failure reports this node could not pair with a start attempt it opened.\n\
             # TYPE plurx_start_outcomes_unpaired_total counter\n",
        );
        for (index, outcome) in START_UNPAIRED.iter().enumerate() {
            out.push_str(&format!(
                "plurx_start_outcomes_unpaired_total{{outcome=\"{outcome}\"}} {}\n",
                self.unpaired[index].load(Ordering::Relaxed)
            ));
        }
        out
    }
}

static START_OUTCOME_COUNTERS: StartOutcomeCounters = StartOutcomeCounters::new();

/// Count one resolved start attempt. `method` outside the playback vocabulary
/// is `unknown`; `outcome` outside [`START_OUTCOMES`] is a programming error
/// and is dropped rather than folded into a real outcome.
pub(crate) fn record_start_outcome(method: &str, outcome: &str) {
    let Some(outcome) = START_OUTCOMES.iter().position(|label| *label == outcome) else {
        return;
    };
    START_OUTCOME_COUNTERS.outcomes[label_index(Some(method), &METHODS)][outcome]
        .fetch_add(1, Ordering::Relaxed);
}

/// Count one terminal client event the start ledger could not pair.
pub(crate) fn record_start_unpaired(outcome: &str) {
    if let Some(index) = START_UNPAIRED.iter().position(|label| *label == outcome) {
        START_OUTCOME_COUNTERS.unpaired[index].fetch_add(1, Ordering::Relaxed);
    }
}

/// The pools a live start has been admitted over, in index order.
pub(crate) const BACKGROUND_OVERRUN_POOLS: [&str; 2] = ["hardware", "software"];

static BACKGROUND_OVERRUNS: [AtomicU64; BACKGROUND_OVERRUN_POOLS.len()] =
    [const { AtomicU64::new(0) }; BACKGROUND_OVERRUN_POOLS.len()];

/// Count one live start admitted over background ownership that did not yield
/// inside the cooperative admission window. Zero is the design working; a
/// rising count names a background worker holding a permit through a phase
/// that never checks the pool.
pub(crate) fn record_background_overrun(pool: &str) {
    if let Some(index) = BACKGROUND_OVERRUN_POOLS
        .iter()
        .position(|label| *label == pool)
    {
        BACKGROUND_OVERRUNS[index].fetch_add(1, Ordering::Relaxed);
    }
}

/// Orphaned shared-playback receiver recovery outcomes, in index order.
pub(crate) const RECEIVER_ORPHAN_OUTCOMES: [&str; 3] = ["retired", "stranded", "lost"];

static RECEIVER_ORPHANS: [AtomicU64; RECEIVER_ORPHAN_OUTCOMES.len()] =
    [const { AtomicU64::new(0) }; RECEIVER_ORPHAN_OUTCOMES.len()];

/// Count one orphaned receiver route outcome: retired after a confirmed
/// Source End, newly stranded (kept and reported), or lost to another owner.
pub(crate) fn record_receiver_orphan(outcome: &str) {
    if let Some(index) = RECEIVER_ORPHAN_OUTCOMES
        .iter()
        .position(|label| *label == outcome)
    {
        RECEIVER_ORPHANS[index].fetch_add(1, Ordering::Relaxed);
    }
}

fn render_receiver_orphans(out: &mut String) {
    render_counters(
        out,
        "plurx_sharing_receiver_orphan_total",
        "Orphaned shared-playback receiver routes by recovery outcome: retired after a confirmed Source End, newly stranded and kept, or lost to another owner.",
        "outcome",
        &RECEIVER_ORPHAN_OUTCOMES,
        &RECEIVER_ORPHANS,
    );
}

fn render_background_overruns(out: &mut String) {
    render_counters(
        out,
        "plurx_transcode_background_overrun_total",
        "Live starts admitted over a background encoder permit that did not yield within the cooperative admission window, by the pool taken.",
        "pool",
        &BACKGROUND_OVERRUN_POOLS,
        &BACKGROUND_OVERRUNS,
    );
}

/// Queued output-preparation rows cancelled because `vod.output_preparation`
/// no longer admits their kind, by kind.
pub(crate) const OUTPUT_PREPARATION_DRAIN_KINDS: [&str; 2] =
    ["copy_output_prepare", "encoded_output_prepare"];

static OUTPUT_PREPARATION_DRAINS: [AtomicU64; OUTPUT_PREPARATION_DRAIN_KINDS.len()] =
    [const { AtomicU64::new(0) }; OUTPUT_PREPARATION_DRAIN_KINDS.len()];

pub(crate) fn record_output_preparation_drained(kind: plurx_core::store::background_jobs::JobKind) {
    use plurx_core::store::background_jobs::JobKind;
    let index = match kind {
        JobKind::CopyOutputPrepare => 0,
        JobKind::EncodedOutputPrepare => 1,
        _ => return,
    };
    OUTPUT_PREPARATION_DRAINS[index].fetch_add(1, Ordering::Relaxed);
}

fn render_output_preparation_drains(out: &mut String) {
    render_counters(
        out,
        "plurx_output_preparation_drained_total",
        "Queued complete-output preparation rows cancelled because vod.output_preparation no longer admits their kind (reason output_preparation_disabled).",
        "kind",
        &OUTPUT_PREPARATION_DRAIN_KINDS,
        &OUTPUT_PREPARATION_DRAINS,
    );
}

/// Why a rolling start skipped its retained-output lookup instead of failing
/// Play, in index order. The lookup is optional: a miss starts a producer.
pub(crate) const ROLLING_RETAINED_LOOKUP_SKIP_REASONS: [&str; 1] = ["plan_error"];

static ROLLING_RETAINED_LOOKUP_SKIPS: [AtomicU64; ROLLING_RETAINED_LOOKUP_SKIP_REASONS.len()] =
    [const { AtomicU64::new(0) }; ROLLING_RETAINED_LOOKUP_SKIP_REASONS.len()];

pub(crate) fn record_rolling_retained_lookup_skipped(reason: &str) {
    if let Some(index) = ROLLING_RETAINED_LOOKUP_SKIP_REASONS
        .iter()
        .position(|label| *label == reason)
    {
        ROLLING_RETAINED_LOOKUP_SKIPS[index].fetch_add(1, Ordering::Relaxed);
    }
}

fn render_rolling_retained_lookup_skips(out: &mut String) {
    render_counters(
        out,
        "plurx_rolling_retained_lookup_skipped_total",
        "Rolling starts that skipped the optional retained-output lookup instead of failing Play, by reason.",
        "reason",
        &ROLLING_RETAINED_LOOKUP_SKIP_REASONS,
        &ROLLING_RETAINED_LOOKUP_SKIPS,
    );
}

/// Why a started session's complete-output preparation was not handed to
/// [`crate::transcode::TranscodeManager::output_enqueue_loop`], in index order.
pub(crate) const OUTPUT_ENQUEUE_DROP_REASONS: [&str; 2] = ["queue_full", "worker_stopped"];

static OUTPUT_ENQUEUE_DROPS: [AtomicU64; OUTPUT_ENQUEUE_DROP_REASONS.len()] =
    [const { AtomicU64::new(0) }; OUTPUT_ENQUEUE_DROP_REASONS.len()];

/// Count one complete-output preparation the create could not hand off. The
/// title's next start offers the same deduplicated job again, so a drop costs
/// time, not correctness; a rising count names a worker that is behind
/// (`queue_full`) or gone (`worker_stopped`).
pub(crate) fn record_output_enqueue_drop(reason: &str) {
    if let Some(index) = OUTPUT_ENQUEUE_DROP_REASONS
        .iter()
        .position(|label| *label == reason)
    {
        OUTPUT_ENQUEUE_DROPS[index].fetch_add(1, Ordering::Relaxed);
    }
}

fn render_output_enqueue_drops(out: &mut String) {
    render_counters(
        out,
        "plurx_transcode_output_enqueue_dropped_total",
        "Complete-output preparations a started session could not hand to the output-enqueue worker, by reason.",
        "reason",
        &OUTPUT_ENQUEUE_DROP_REASONS,
        &OUTPUT_ENQUEUE_DROPS,
    );
}

#[cfg(test)]
fn render_start_outcomes_for_test(record: impl FnOnce(&StartOutcomeCounters)) -> String {
    let counters = StartOutcomeCounters::new();
    record(&counters);
    counters.render()
}

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
    diagnostics: Arc<crate::diagnostics::Recorder>,
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

// Keep the allocation identity alive without retaining the Store itself.
// A writer can disappear with its runtime; its old queue must not be handed
// to a later Store whose allocation reuses the same address.
struct RegisteredSink {
    store: Weak<dyn Store>,
    sink: Arc<TelemetrySink>,
}

static SINKS: LazyLock<Mutex<HashMap<usize, RegisteredSink>>> =
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
    if let Some(registered) = sinks.get(&key) {
        return Arc::clone(&registered.sink);
    }
    sinks.retain(|_, registered| registered.store.strong_count() > 0);
    let store_identity = Arc::downgrade(&store);
    let sink = new_sink(&QUEUE_METRICS);
    let writer: Arc<dyn WriterStore> = Arc::new(DurableWriterStore(store));
    tokio::spawn(supervise_writer(writer, Arc::clone(&sink)));
    sinks.insert(
        key,
        RegisteredSink {
            store: store_identity,
            sink: Arc::clone(&sink),
        },
    );
    sink
}

fn new_sink(metrics: &'static QueueMetrics) -> Arc<TelemetrySink> {
    Arc::new(TelemetrySink {
        diagnostics: Arc::new(crate::diagnostics::Recorder::default()),
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
        .insert(
            store_key(store),
            RegisteredSink {
                store: Arc::downgrade(store),
                sink: Arc::clone(&sink),
            },
        );
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
        .map(|registered| Arc::clone(&registered.sink));
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
        sink.sink.invalidate_settings();
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
        .map(|registered| Arc::clone(&registered.sink));
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
        "ttff" | "session_start" | "stall" | "stall_end" | "stall_recovery" | "suspend"
        | "resume" => EventClass::Lifecycle,
        _ => EventClass::Sample,
    }
}

/// Record metrics and persist an event without delaying the caller. The
/// retention setting is read inside the task so setting `0` makes this a true
/// no-op while HTTP ingest can still return its existing 204 immediately.
pub fn emit(store: Arc<dyn Store>, event: PlaybackEvent) {
    emit_with_network(store, event, None, None);
}

/// Persist the N0 event and, independently when opted in, fold its bounded
/// network measurement into the matching prior. Keeping the two switches
/// independent means an operator may retain only the aggregate prior without
/// retaining raw playback events.
pub(crate) fn emit_with_network(
    store: Arc<dyn Store>,
    event: PlaybackEvent,
    network: Option<NetworkIdentity>,
    client: Option<&'static str>,
) {
    // Metrics describe what this node observed, independently of whether raw
    // retention, queue admission, or the node-local sidecar succeeds.
    METRICS.record_from(&event, client);
    let sink = sink_for(store);
    sink.diagnostics.observe(&event);
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
    let detail = event.detail.as_deref().unwrap_or_default();
    // CPU encode pressure, decoder failure, deliberate hold or lost authority say nothing about
    // this credential's network. Even accompanying throughput must not turn
    // those observations into a shared quality ceiling.
    let cause = detail.split_once(':').map(|(cause, _)| cause);
    if matches!(
        cause,
        Some("encode" | "decode" | "hold" | "authority" | "unknown")
    ) {
        return None;
    }
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
    let starved = event.event == "stall"
        && (cause == Some("link")
            || detail.contains("supply")
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
        // ClientLog supplies a smoothed meter and aggregate delivery facts,
        // not a completed, unpaced network body's timing/provenance. Even a
        // `link:` label must remain legacy evidence until a real producer
        // supplies the typed completed-transfer proof.
        measured_link: None,
    })
}

pub fn prometheus() -> String {
    let mut metrics = METRICS.render();
    metrics.push_str(&QUEUE_METRICS.render());
    metrics.push_str(&SUBTITLE_SOURCE_METRICS.render());
    metrics.push_str(&START_OUTCOME_COUNTERS.render());
    render_background_overruns(&mut metrics);
    render_receiver_orphans(&mut metrics);
    render_rolling_retained_lookup_skips(&mut metrics);
    render_output_preparation_drains(&mut metrics);
    render_output_enqueue_drops(&mut metrics);
    metrics
}

/// Existing telemetry ownership also owns the node-local diagnostic projection.
pub(crate) fn diagnostics(store: Arc<dyn Store>) -> Arc<crate::diagnostics::Recorder> {
    Arc::clone(&sink_for(store).diagnostics)
}

#[cfg(test)]
mod tests;
