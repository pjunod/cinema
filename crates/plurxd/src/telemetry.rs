//! Node-local playback observations and their bounded Prometheus projection.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use plurx_core::domain::{CredentialGeneration, NetworkPriorObservation, PlaybackEvent};
use plurx_core::store::{keys, Store};

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
    tokio::spawn(async move {
        let telemetry_enabled = store
            .get_setting(keys::TELEMETRY_RETAIN_DAYS)
            .await
            .ok()
            .flatten()
            .and_then(|value| value.trim().parse::<i64>().ok())
            .unwrap_or(keys::TELEMETRY_RETAIN_DEFAULT_DAYS)
            > 0;
        let priors_enabled = if network.is_some() {
            store
                .get_setting(keys::PLAYBACK_NETWORK_PRIORS)
                .await
                .ok()
                .flatten()
                .is_some_and(|value| value.trim() == "1")
        } else {
            false
        };

        if telemetry_enabled {
            METRICS.record(&event);
            if let Err(error) = store.record_playback_event(&event).await {
                tracing::warn!(%error, event = %event.event, "recording playback telemetry failed");
            }
        }

        if priors_enabled {
            if let Some(observation) = prior_observation(&event, network.as_ref()) {
                if let Err(error) = store.observe_network_prior(&observation).await {
                    tracing::warn!(%error, event = %event.event, "updating network prior failed");
                }
            }
        }
    });
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
    METRICS.render()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
