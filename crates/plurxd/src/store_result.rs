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
    MarkSharedCacheStorageSuspect => "mark_shared_cache_storage_suspect",
    ForgetFencedUnfencedClaim => "forget_fenced_unfenced_claim",
    ForgetFailedFencedOfflineCacheEntry => "forget_failed_fenced_offline_cache_entry",
    ReleaseSharedLookupPinAfterVerification => "release_shared_lookup_pin_after_verification",
    ReleaseExpiredSharedLookupPin => "release_expired_shared_lookup_pin",
    ReleaseSharedLookupPinAfterPreparationFailure => "release_shared_lookup_pin_after_preparation_failure",
    UpdateOfflineProgressExtractingSubtitles => "update_offline_progress_extracting_subtitles",
    UpdateOfflineProgressCachedTranscode => "update_offline_progress_cached_transcode",
    UpdateOfflineProgressTranscoding => "update_offline_progress_transcoding",
    InvalidateCorruptCacheManifest => "invalidate_corrupt_cache_manifest",
    RecordAnalysisRetryWaitPhase => "record_analysis_retry_wait_phase",
    RecordAnalysisFailedPhase => "record_analysis_failed_phase",
    TouchSharedOfflineCacheEntry => "touch_shared_offline_cache_entry",
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

/// Observe a bounded Store operation without treating an inner Store failure
/// as a successful timeout result. Both the deadline and the Store call are
/// classified under the same closed operation label, and both reach the
/// counter and bounded log path.
pub(crate) fn observe_timeout<T, E: Display>(
    operation: Operation,
    severity: Discard,
    result: Result<Result<T, E>, tokio::time::error::Elapsed>,
) {
    match result {
        Ok(result) => observe(operation, severity, result),
        Err(error) => observe(operation, severity, Err::<T, _>(error)),
    }
}

pub(crate) fn prometheus() -> String {
    METRICS.render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use syn::visit::Visit;

    fn test_only(attributes: &[syn::Attribute]) -> bool {
        attributes.iter().any(|attribute| {
            if attribute.path().is_ident("test") {
                return true;
            }
            if attribute.path().segments.last().is_some_and(|segment| {
                segment.ident == "test" && attribute.path().segments.len() > 1
            }) {
                return true;
            }
            matches!(
                &attribute.meta,
                syn::Meta::List(list)
                    if list.path.is_ident("cfg") && list.tokens.to_string() == "test"
            )
        })
    }

    #[derive(Default)]
    struct StoreMethodCollector {
        names: BTreeSet<String>,
    }

    impl<'ast> Visit<'ast> for StoreMethodCollector {
        fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
            for member in &item.items {
                if let syn::TraitItem::Fn(method) = member {
                    if method.sig.asyncness.is_some()
                        && matches!(
                            &method.sig.output,
                            syn::ReturnType::Type(_, ty)
                                if matches!(ty.as_ref(), syn::Type::Path(path)
                                    if path.path.segments.last().is_some_and(|segment| segment.ident == "Result"))
                        )
                    {
                        self.names.insert(method.sig.ident.to_string());
                    }
                }
            }
        }
    }

    struct CallCollector<'a> {
        store_methods: &'a BTreeSet<String>,
        calls: BTreeSet<String>,
    }

    impl<'ast> Visit<'ast> for CallCollector<'_> {
        fn visit_expr_method_call(&mut self, expression: &'ast syn::ExprMethodCall) {
            let name = expression.method.to_string();
            if self.store_methods.contains(&name) {
                self.calls.insert(name);
            }
            syn::visit::visit_expr_method_call(self, expression);
        }

        fn visit_expr_call(&mut self, expression: &'ast syn::ExprCall) {
            if let syn::Expr::Path(path) = expression.func.as_ref() {
                if let Some(segment) = path.path.segments.last() {
                    let name = segment.ident.to_string();
                    if self.store_methods.contains(&name) {
                        self.calls.insert(name);
                    }
                }
            }
            syn::visit::visit_expr_call(self, expression);
        }
    }

    struct DiscardCollector<'a> {
        path: &'a Path,
        store_methods: &'a BTreeSet<String>,
        offenders: Vec<String>,
    }

    fn wildcard(pattern: &syn::Pat) -> bool {
        match pattern {
            syn::Pat::Wild(_) => true,
            syn::Pat::Type(typed) => wildcard(&typed.pat),
            _ => false,
        }
    }

    impl<'ast> Visit<'ast> for DiscardCollector<'_> {
        fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
            if !test_only(&item.attrs) {
                syn::visit::visit_item_mod(self, item);
            }
        }

        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            if !test_only(&item.attrs) {
                syn::visit::visit_item_fn(self, item);
            }
        }

        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            if !test_only(&item.attrs) {
                syn::visit::visit_impl_item_fn(self, item);
            }
        }

        fn visit_local(&mut self, local: &'ast syn::Local) {
            if wildcard(&local.pat) {
                if let Some(initializer) = &local.init {
                    let mut calls = CallCollector {
                        store_methods: self.store_methods,
                        calls: BTreeSet::new(),
                    };
                    calls.visit_expr(&initializer.expr);
                    for call in calls.calls {
                        self.offenders
                            .push(format!("{}: discarded {call}", self.path.display()));
                    }
                }
            }
            syn::visit::visit_local(self, local);
        }
    }

    fn rust_sources(root: &Path, sources: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(root).expect("source directory") {
            let entry = entry.expect("source entry");
            let path = entry.path();
            if path.is_dir() {
                rust_sources(&path, sources);
            } else if path.extension().and_then(std::ffi::OsStr::to_str) == Some("rs") {
                sources.push(path);
            }
        }
    }

    fn parse(path: &Path) -> syn::File {
        syn::parse_file(&std::fs::read_to_string(path).expect("Rust source"))
            .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
    }

    fn store_method_names(workspace: &Path) -> BTreeSet<String> {
        let mut sources = Vec::new();
        rust_sources(&workspace.join("crates/plurx-core/src/store"), &mut sources);
        sources.sort();
        let mut collector = StoreMethodCollector::default();
        for source in sources {
            collector.visit_file(&parse(&source));
        }
        collector.names
    }

    fn discarded_store_calls(
        path: &Path,
        source: &str,
        store_methods: &BTreeSet<String>,
    ) -> Vec<String> {
        let syntax = syn::parse_file(source).expect("discard fixture is valid Rust");
        let mut collector = DiscardCollector {
            path,
            store_methods,
            offenders: Vec::new(),
        };
        collector.visit_file(&syntax);
        collector.offenders
    }

    #[test]
    fn every_classified_failure_has_a_bounded_metric_row() {
        assert_eq!(Operation::ALL.len(), 42, "one fixed label per audited site");
        let metrics = Metrics::default();
        for operation in Operation::ALL {
            for severity in Discard::ALL {
                metrics.record(operation, severity, true);
            }
        }
        let rendered = metrics.render();
        for operation in Operation::ALL {
            for severity in ["lost_work", "best_effort", "cancelled"] {
                assert!(rendered.contains(&format!(
                    "operation=\"{}\",severity=\"{severity}\",outcome=\"error\"}} 1",
                    operation.label()
                )));
            }
        }
    }

    fn metric_value(operation: Operation, severity: Discard, outcome: &str) -> u64 {
        let prefix = format!(
            "plurx_store_discarded_results_total{{operation=\"{}\",severity=\"{}\",outcome=\"{outcome}\"}} ",
            operation.label(),
            severity.label()
        );
        prometheus()
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .expect("metric row")
            .parse()
            .expect("metric value")
    }

    #[tokio::test]
    async fn timeout_observation_counts_inner_and_deadline_failures_as_errors() {
        let operation = Operation::MarkSharedCacheStorageSuspect;
        let severity = Discard::LostWork;
        let before = metric_value(operation, severity, "error");
        observe_timeout(operation, severity, Ok(Err::<(), _>("inner Store failure")));
        let deadline = tokio::time::timeout(
            Duration::ZERO,
            std::future::pending::<Result<(), &'static str>>(),
        )
        .await;
        observe_timeout(operation, severity, deadline);
        assert_eq!(
            metric_value(operation, severity, "error"),
            before + 2,
            "an outer timeout and an inner Store error are both classified failures"
        );
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
    fn discarded_store_result_guard_recognizes_receiver_independent_shapes() {
        let methods = BTreeSet::from([
            "forget_cache_entry".to_owned(),
            "mark_cache_storage_suspect".to_owned(),
        ]);
        let source = r#"
            async fn direct(store: &Store) {
                let _ = store.forget_cache_entry("a", "b", "c").await;
            }
            async fn wrapped(store: &Store, fence: Fence) {
                let _ = PublicationStore::fenced(store, fence)
                    .forget_cache_entry("a", "b", "c").await;
            }
            async fn nested(store: &Store) {
                let _ = timeout(DURATION,
                    store.mark_cache_storage_suspect("a", "b", 1)).await;
            }
            async fn unrelated(directory: &Directory) {
                let _ = directory.unlink_child("a").await;
            }
        "#;
        let offenders = discarded_store_calls(Path::new("fixture.rs"), source, &methods);
        assert_eq!(offenders.len(), 3, "{offenders:?}");
        assert!(offenders
            .iter()
            .any(|entry| entry.contains("mark_cache_storage_suspect")));
        assert_eq!(
            offenders
                .iter()
                .filter(|entry| entry.contains("forget_cache_entry"))
                .count(),
            2
        );
    }

    #[test]
    fn production_store_results_never_use_bare_let_underscore() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let store_methods = store_method_names(workspace);
        assert!(
            store_methods.contains("forget_cache_entry")
                && store_methods.contains("mark_cache_storage_suspect"),
            "the semantic guard must derive wrapper and nested methods from the Store traits"
        );
        let mut sources = Vec::new();
        for crate_dir in std::fs::read_dir(workspace.join("crates")).expect("workspace crates") {
            let source_root = crate_dir.expect("crate directory").path().join("src");
            if source_root.is_dir() {
                rust_sources(&source_root, &mut sources);
            }
        }
        sources.sort();
        let mut offenders = Vec::new();
        for source in sources {
            let syntax = parse(&source);
            let mut collector = DiscardCollector {
                path: &source,
                store_methods: &store_methods,
                offenders: Vec::new(),
            };
            collector.visit_file(&syntax);
            offenders.extend(collector.offenders);
        }
        assert!(
            offenders.is_empty(),
            "store results must go through store_result::observe: {offenders:?}"
        );
    }
}
