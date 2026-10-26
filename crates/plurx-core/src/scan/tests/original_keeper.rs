use super::*;
use crate::process::bounded::{
    with_original_work_keeper, OriginalWorkKeeper, OriginalWorkUncertainty,
};
use std::sync::{Arc, Condvar, Mutex};

struct Keeper {
    dropped: Mutex<Option<tokio::sync::oneshot::Sender<std::thread::ThreadId>>>,
}
impl OriginalWorkKeeper for Keeper {
    fn retain_uncertainty(&self, _reason: OriginalWorkUncertainty) {
        // This fixture enters a real filesystem worker, never a native child.
        // It must not manufacture a child closure or uncertainty receipt.
        panic!("filesystem worker fixture does not enter a child collector");
    }
}
impl Drop for Keeper {
    fn drop(&mut self) {
        if let Some(sender) = self
            .dropped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = sender.send(std::thread::current().id());
        }
    }
}

struct ReleaseWalker(Arc<(Mutex<bool>, Condvar)>);
impl Drop for ReleaseWalker {
    fn drop(&mut self) {
        let (released, changed) = &*self.0;
        *released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        changed.notify_all();
    }
}

#[tokio::test]
async fn dropped_scan_waiter_retains_same_keeper_in_original_filesystem_worker() {
    let root = tempfile::tempdir().expect("actual library directory");
    std::fs::write(root.path().join("movie.mkv"), b"walker fixture").expect("actual entry");
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let original = Arc::new(Keeper {
        dropped: Mutex::new(Some(dropped_tx)),
    });
    let same_original = Arc::downgrade(&original);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let release = ReleaseWalker(Arc::clone(&gate));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered_tx = Mutex::new(Some(entered_tx));
    let cancel = tokio_util::sync::CancellationToken::new();
    let operation_cancel = cancel.clone();
    let roots = vec![root.path().to_path_buf()];
    let keeper: Arc<dyn OriginalWorkKeeper> = original.clone();
    let caller = tokio::spawn(with_original_work_keeper(
        keeper,
        crate::process::bounded::cancellable(operation_cancel, async move {
            let (pages, walker) = spawn_walker_with_entry_hook(
                roots,
                WalkFilter {
                    kind: LibraryKind::Movies,
                },
                move || {
                    if let Some(entered) = entered_tx.lock().expect("entry witness").take() {
                        // The real worker has visited a real WalkDir entry. It
                        // explicitly owns the keeper; task locals are absent.
                        assert!(crate::process::bounded::original_keeper().is_none());
                        let _ = entered.send(std::thread::current().id());
                        let (released, changed) = &*gate;
                        let (released, timeout) = changed
                            .wait_timeout_while(
                                released.lock().expect("walker gate"),
                                Duration::from_secs(5),
                                |released| !*released,
                            )
                            .expect("same original filesystem worker gate");
                        assert!(*released && !timeout.timed_out(), "release original worker");
                    }
                },
            );
            // The same actual handle and receiver remain in this caller. Drop
            // cancels this wait, not the already-running blocking worker.
            walker.await.expect("original walker joined");
            drop(pages);
        }),
    ));
    drop(original);
    let native_thread = tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .expect("actual filesystem entry")
        .expect("entry witness");
    assert_ne!(native_thread, std::thread::current().id());
    cancel.cancel();
    caller.abort();
    assert!(caller
        .await
        .expect_err("original caller dropped")
        .is_cancelled());
    assert!(
        same_original.upgrade().is_some(),
        "original worker still retains original keeper"
    );
    drop(release);
    let drop_thread = tokio::time::timeout(Duration::from_secs(5), dropped_rx)
        .await
        .expect("original worker exited")
        .expect("drop witness");
    assert_eq!(
        drop_thread, native_thread,
        "last owner was same actual filesystem worker"
    );
    assert!(same_original.upgrade().is_none());
    // A retained owner/drop trace is not a proof of native-group extinction,
    // positive restart readiness, closed source roots or finite settlement.
}
