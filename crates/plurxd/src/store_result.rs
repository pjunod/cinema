//! Closed-cardinality accounting for store results callers intentionally do
//! not propagate.

use std::fmt::{Display, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const ERROR_LOG_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug)]
pub(crate) enum Discard {
    LostWork,
    BestEffort,
    Cancelled,
}

impl Discard {
    const ALL: [Self; 3] = [Self::LostWork, Self::BestEffort, Self::Cancelled];

    const fn index(self) -> usize {
        match self {
            Self::LostWork => 0,
            Self::BestEffort => 1,
            Self::Cancelled => 2,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::LostWork => "lost_work",
            Self::BestEffort => "best_effort",
            Self::Cancelled => "cancelled",
        }
    }
}

macro_rules! operations {
    ($($variant:ident => $label:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug)]
        pub(crate) enum Operation { $($variant),+ }

        impl Operation {
            const ALL: [Self; operations!(@count $($variant),+)] = [$(Self::$variant),+];

            const fn index(self) -> usize { self as usize }

            const fn label(self) -> &'static str {
                match self { $(Self::$variant => $label),+ }
            }
        }
    };
    (@count $($variant:ident),+) => { <[()]>::len(&[$(operations!(@one $variant)),+]) };
    (@one $variant:ident) => { () };
}

operations! {
    ForgetIndexAfterLocalRemoval => "forget_index_after_local_removal",
    ForgetCorruptLocalIndex => "forget_corrupt_local_index",
    ForgetMissingPeerIndex => "forget_missing_peer_index",
    ForgetCorruptPeerIndex => "forget_corrupt_peer_index",
    ReleaseClassificationLease => "release_classification_lease",
    DeleteExpiredDvrReminder => "delete_expired_dvr_reminder",
    ReleaseUnverifiedCacheGcLease => "release_unverified_cache_gc_lease",
    ReleaseCacheGcLease => "release_cache_gc_lease",
    ResetDvQueueCursorAfterLoss => "reset_dv_queue_cursor_after_loss",
    ResetDvQueueCursorAfterEmptyPage => "reset_dv_queue_cursor_after_empty_page",
    ResetDvQueueCursorAfterExhaustion => "reset_dv_queue_cursor_after_exhaustion",
    AdvanceDvQueueCursor => "advance_dv_queue_cursor",
    ResetDvRecoveryCursor => "reset_dv_recovery_cursor",
    ResetDvGuardCursor => "reset_dv_guard_cursor",
    MergeProbeChapters => "merge_probe_chapters",
    RecordPublishedAnalysisPhase => "record_published_analysis_phase",
    SettleAnalysisAfterQueueAdmission => "settle_analysis_after_queue_admission",
    RecordFragmentIndexSource => "record_fragment_index_source",
    SettleAnalysisAfterHydration => "settle_analysis_after_hydration",
    RequeueFragmentIndexNoHolder => "requeue_fragment_index_no_holder",
    TouchApiKey => "touch_api_key",
    ForgetMissingInternalIndex => "forget_missing_internal_index",
    ForgetCorruptInternalIndex => "forget_corrupt_internal_index",
    PruneChannelSubjects => "prune_channel_subjects",
    UpdateOfflineProgressStarted => "update_offline_progress_started",
    ForgetUnfencedCacheEntry => "forget_unfenced_cache_entry",
    TouchSharedCacheEntry => "touch_shared_cache_entry",
    TouchCacheEntry => "touch_cache_entry",
    ForgetFailedOfflineCacheEntry => "forget_failed_offline_cache_entry",
}

const OUTCOMES: [&str; 2] = ["ok", "error"];
const CELLS: usize = Operation::ALL.len() * Discard::ALL.len() * OUTCOMES.len();

struct Metrics {
    cells: [AtomicU64; CELLS],
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            cells: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl Metrics {
    const fn index(operation: Operation, severity: Discard, error: bool) -> usize {
        (operation.index() * Discard::ALL.len() + severity.index()) * OUTCOMES.len()
            + error as usize
    }

    fn record(&self, operation: Operation, severity: Discard, error: bool) {
        let cell = &self.cells[Self::index(operation, severity, error)];
        let _ = cell.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            (current != u64::MAX).then(|| current + 1)
        });
    }

    fn render(&self) -> String {
        let mut out = String::from(
            "# HELP plurx_store_discarded_results_total Store results the caller did not propagate.\n\
             # TYPE plurx_store_discarded_results_total counter\n",
        );
        for operation in Operation::ALL {
            for severity in Discard::ALL {
                for (error, outcome) in OUTCOMES.iter().enumerate() {
                    let count = self.cells[Self::index(operation, severity, error == 1)]
                        .load(Ordering::Relaxed);
                    let _ = writeln!(
                        out,
                        "plurx_store_discarded_results_total{{operation=\"{}\",severity=\"{}\",outcome=\"{}\"}} {}",
                        operation.label(), severity.label(), outcome, count
                    );
                }
            }
        }
        out
    }
}

#[derive(Default)]
struct LogWindow {
    last: Option<Instant>,
    suppressed: u64,
}

impl LogWindow {
    fn admit(&mut self, now: Instant) -> Option<u64> {
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < ERROR_LOG_INTERVAL)
        {
            self.suppressed = self.suppressed.saturating_add(1);
            return None;
        }
        let suppressed = std::mem::take(&mut self.suppressed);
        self.last = Some(now);
        Some(suppressed)
    }
}

static METRICS: LazyLock<Metrics> = LazyLock::new(Metrics::default);
static LOG_WINDOWS: LazyLock<Vec<Mutex<LogWindow>>> = LazyLock::new(|| {
    (0..Operation::ALL.len())
        .map(|_| Mutex::new(LogWindow::default()))
        .collect()
});

/// Record a deliberately unpropagated store result. Error logs are bounded to
/// one per operation per 30 seconds; the next emitted row reports how many
/// were suppressed in the preceding window.
pub(crate) fn observe<T, E: Display>(
    operation: Operation,
    severity: Discard,
    result: Result<T, E>,
) {
    let error = match result {
        Ok(_) => {
            METRICS.record(operation, severity, false);
            return;
        }
        Err(error) => error,
    };
    METRICS.record(operation, severity, true);
    let now = Instant::now();
    let mut window = LOG_WINDOWS[operation.index()]
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(suppressed) = window.admit(now) else {
        return;
    };
    drop(window);
    match severity {
        Discard::LostWork => tracing::error!(
            operation = operation.label(),
            suppressed,
            %error,
            "store result discarded after work was lost"
        ),
        Discard::BestEffort => tracing::debug!(
            operation = operation.label(),
            suppressed,
            %error,
            "best-effort store result discarded"
        ),
        Discard::Cancelled => tracing::debug!(
            operation = operation.label(),
            suppressed,
            %error,
            "store result discarded while caller was already unwinding"
        ),
    }
}

pub(crate) fn prometheus() -> String {
    METRICS.render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_three_severities_have_bounded_metric_rows() {
        assert_eq!(Operation::ALL.len(), 29, "one fixed label per audited site");
        let metrics = Metrics::default();
        for severity in Discard::ALL {
            metrics.record(Operation::RequeueFragmentIndexNoHolder, severity, true);
        }
        let rendered = metrics.render();
        for severity in ["lost_work", "best_effort", "cancelled"] {
            assert!(rendered.contains(&format!(
                "operation=\"requeue_fragment_index_no_holder\",severity=\"{severity}\",outcome=\"error\"}} 1"
            )));
        }
    }

    #[test]
    fn log_window_suppresses_until_the_interval_expires() {
        let now = Instant::now();
        let mut window = LogWindow::default();
        assert_eq!(window.admit(now), Some(0));
        assert_eq!(window.admit(now + Duration::from_secs(1)), None);
        assert_eq!(window.suppressed, 1);
        assert_eq!(window.admit(now + ERROR_LOG_INTERVAL), Some(1));
        assert_eq!(window.suppressed, 0);
    }

    #[test]
    fn production_store_results_never_use_bare_let_underscore() {
        fn visit(path: &std::path::Path, offenders: &mut Vec<String>) {
            for entry in std::fs::read_dir(path).expect("source directory") {
                let entry = entry.expect("source entry");
                let path = entry.path();
                if path.is_dir() {
                    visit(&path, offenders);
                } else if path.extension().and_then(std::ffi::OsStr::to_str) == Some("rs") {
                    let source = std::fs::read_to_string(&path).expect("Rust source");
                    for (line, suffix) in source.match_indices("let _ =") {
                        let sample = suffix
                            .chars()
                            .take(360)
                            .collect::<String>()
                            .split_whitespace()
                            .collect::<String>();
                        if sample.contains("store.") && sample.contains(".await") {
                            offenders.push(format!(
                                "{}:{}",
                                path.display(),
                                source[..line].matches('\n').count() + 1
                            ));
                        }
                    }
                }
            }
        }

        let mut offenders = Vec::new();
        visit(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut offenders,
        );
        assert!(
            offenders.is_empty(),
            "store results must go through store_result::observe: {offenders:?}"
        );
    }
}
