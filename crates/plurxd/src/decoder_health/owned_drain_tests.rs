use super::*;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;

struct Closed {
    closed: Arc<AtomicUsize>,
    notify: Arc<tokio::sync::Notify>,
}
impl Drop for Closed {
    fn drop(&mut self) {
        self.closed.fetch_add(1, SeqCst);
        self.notify.notify_one();
    }
}

fn qualified_accumulator() -> HealthAccumulator {
    let accumulator = HealthAccumulator::with_qualified_grammar(true);
    assert!(
        accumulator
            .settle_receipt(
                "plan".to_owned(),
                Some("contract".to_owned()),
                ExitDisposition::CleanEnd
            )
            .permits_reuse(),
        "control is eligible before an owned-reader failure"
    );
    accumulator
}

#[tokio::test]
async fn progress_panic_cannot_be_hidden_by_successful_diagnostic_drain() {
    let progress = tokio::spawn(async { panic!("intentional progress reader failure") });
    let reader = tokio::spawn(async { qualified_accumulator() });
    let receipt = ObservedDiagnostics::new(
        "plan".to_owned(),
        Some("contract".to_owned()),
        Some(reader),
        Some(progress),
    )
    .settle(Duration::from_secs(1), ExitDisposition::CleanEnd)
    .await;
    assert!(!receipt.observation_complete);
    assert!(!receipt.permits_reuse());
}

#[tokio::test]
async fn progress_deadline_joins_actual_task_before_incomplete_receipt() {
    let closed = Arc::new(AtomicUsize::new(0));
    let guard = Closed {
        closed: Arc::clone(&closed),
        notify: Arc::new(tokio::sync::Notify::new()),
    };
    let progress = tokio::spawn(async move {
        let _guard = guard;
        std::future::pending::<()>().await;
    });
    let reader = tokio::spawn(async { qualified_accumulator() });
    let receipt = ObservedDiagnostics::new(
        "plan".to_owned(),
        Some("contract".to_owned()),
        Some(reader),
        Some(progress),
    )
    .settle(Duration::from_millis(1), ExitDisposition::CleanEnd)
    .await;
    assert_eq!(
        closed.load(SeqCst),
        1,
        "abort is actually joined, no post-receipt yield"
    );
    assert!(!receipt.observation_complete);
    assert!(!receipt.permits_reuse());
}

#[tokio::test]
async fn cancelling_drain_waiter_does_not_detach_taken_reader_handles() {
    let closed = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());
    let progress_guard = Closed {
        closed: Arc::clone(&closed),
        notify: Arc::clone(&notify),
    };
    let reader_guard = Closed {
        closed: Arc::clone(&closed),
        notify: Arc::clone(&notify),
    };
    let progress = tokio::spawn(async move {
        let _guard = progress_guard;
        std::future::pending::<()>().await;
    });
    let reader = tokio::spawn(async move {
        let _guard = reader_guard;
        std::future::pending::<HealthAccumulator>().await
    });
    let diagnostics = ObservedDiagnostics::new(
        "plan".to_owned(),
        Some("contract".to_owned()),
        Some(reader),
        Some(progress),
    );
    let mut waiter =
        Box::pin(diagnostics.settle(Duration::from_millis(1), ExitDisposition::CleanEnd));
    // A real first poll establishes cancellation-independent custody; do not
    // guess whether a spawned waiter has reached its first await by sleeping.
    assert!(futures_util::poll!(&mut waiter).is_pending());
    drop(waiter);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let changed = notify.notified();
            if closed.load(SeqCst) == 2 {
                break;
            }
            changed.await;
        }
    })
    .await
    .expect("both actual tasks die despite cancelled observer");
}

#[tokio::test]
async fn optional_progress_absence_does_not_invalidate_real_qualified_diagnostics() {
    let reader = tokio::spawn(async { qualified_accumulator() });
    let receipt = ObservedDiagnostics::new(
        "plan".to_owned(),
        Some("contract".to_owned()),
        Some(reader),
        None,
    )
    .settle(Duration::from_secs(1), ExitDisposition::CleanEnd)
    .await;
    assert!(receipt.observation_complete);
    assert!(receipt.permits_reuse());
}

fn pending_readers() -> (
    ObservedDiagnostics,
    Arc<AtomicUsize>,
    Arc<tokio::sync::Notify>,
) {
    let closed = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());
    let reader_guard = Closed {
        closed: Arc::clone(&closed),
        notify: Arc::clone(&notify),
    };
    let progress_guard = Closed {
        closed: Arc::clone(&closed),
        notify: Arc::clone(&notify),
    };
    let reader = tokio::spawn(async move {
        let _guard = reader_guard;
        std::future::pending::<HealthAccumulator>().await
    });
    let progress = tokio::spawn(async move {
        let _guard = progress_guard;
        std::future::pending::<()>().await;
    });
    (
        ObservedDiagnostics::new("abandoned".to_owned(), None, Some(reader), Some(progress)),
        closed,
        notify,
    )
}

async fn await_both_reader_closures(closed: &AtomicUsize, notify: &tokio::sync::Notify) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let changed = notify.notified();
            if closed.load(SeqCst) == 2 {
                return;
            }
            changed.await;
        }
    })
    .await
    .expect("both actual abandoned reader tasks close");
}

#[tokio::test]
async fn off_runtime_drop_aborts_readers_and_transfers_actual_joins_without_panicking() {
    let (diagnostics, closed, notify) = pending_readers();
    std::thread::spawn(move || {
        assert!(tokio::runtime::Handle::try_current().is_err());
        drop(diagnostics);
    })
    .join()
    .expect("drop uses the original reader runtime, not a missing current one");
    await_both_reader_closures(&closed, &notify).await;
}

#[tokio::test]
async fn supervisor_unwind_transfers_abandoned_reader_joins_without_a_receipt() {
    let (diagnostics, closed, notify) = pending_readers();
    let failed_owner = tokio::spawn(async move {
        let _diagnostics = diagnostics;
        panic!("intentional producer owner unwind");
    });
    assert!(failed_owner.await.expect_err("owner fails").is_panic());
    await_both_reader_closures(&closed, &notify).await;
}
