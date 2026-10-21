use super::*;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

struct ReaderClosed(Arc<AtomicBool>);
impl Drop for ReaderClosed {
    fn drop(&mut self) {
        self.0.store(true, SeqCst);
    }
}

async fn session_with_pending_drain(
    dir: &std::path::Path,
) -> (
    Arc<Session>,
    tokio::sync::oneshot::Sender<()>,
    Arc<AtomicBool>,
) {
    let mut session = test_session(dir.to_owned());
    let mut placeholder = session.child.get_mut().take().expect("fixture child");
    placeholder.kill().await.expect("kill fixture child");
    placeholder
        .wait_for_settlement()
        .await
        .expect("join fixture supervisor");

    let (release, released) = tokio::sync::oneshot::channel();
    let closed = Arc::new(AtomicBool::new(false));
    let guard = ReaderClosed(Arc::clone(&closed));
    let progress = tokio::spawn(async move {
        let _guard = guard;
        released.await.expect("release owned progress reader");
    });
    let reader = tokio::spawn(async {
        crate::decoder_health::HealthAccumulator::with_qualified_grammar(true)
    });
    let diagnostics = crate::decoder_health::ObservedDiagnostics::new(
        "rolling-task-settlement".to_owned(),
        Some("owned-test-grammar".to_owned()),
        Some(reader),
        Some(progress),
    );
    let child = AttemptChild::new(
        0,
        successful_child(),
        session.control.clone(),
        Some(diagnostics),
    );
    tokio::time::timeout(Duration::from_secs(5), child.wait_for_terminal())
        .await
        .expect("root exits independently of diagnostic reader")
        .expect("actual root wait");
    assert!(!closed.load(SeqCst));
    *session.child.get_mut() = Some(child);
    (Arc::new(session), release, closed)
}

#[tokio::test]
async fn process_terminal_is_early_but_task_settlement_joins_the_reader() {
    let dir = crate::test_tempdir().expect("scratch");
    let (session, release, closed) = session_with_pending_drain(dir.path()).await;
    let slot = session.child.lock().await;
    let child = slot.as_ref().expect("installed child");
    assert!(child.terminal_exit_snapshot().is_some());
    assert!(
        tokio::time::timeout(Duration::from_millis(1), child.wait_for_settlement())
            .await
            .is_err()
    );
    assert!(!closed.load(SeqCst));
    release.send(()).expect("same actual reader remains owned");
    child.wait_for_settlement().await.expect("supervisor join");
    assert!(closed.load(SeqCst), "actual reader ended before settlement");
    child.wait_for_settlement().await.expect("idempotent join");
}

#[tokio::test]
async fn cancelled_prepublication_cleanup_retains_slot_and_admission_until_drain_join() {
    let dir = crate::test_tempdir().expect("scratch");
    let (session, release, closed) = session_with_pending_drain(dir.path()).await;
    let admissions = Admissions::new();
    reserve_test_admissions(&session, &admissions);
    assert!(tokio::time::timeout(
        Duration::from_millis(1),
        terminate_exact_prepublication_child(&session, 0),
    )
    .await
    .is_err());
    assert!(session.child.lock().await.is_some());
    assert_eq!(admissions.in_use(), 1);
    assert_eq!(admissions.software_in_use(), 2);
    assert!(!closed.load(SeqCst));

    release
        .send(())
        .expect("cancelled waiter did not abort reader");
    terminate_exact_prepublication_child(&session, 0)
        .await
        .expect("retry joins retained supervisor");
    assert!(closed.load(SeqCst));
    assert!(session.child.lock().await.is_none());
    // This exact-child helper does not itself return the caller's permits.
    assert_eq!(admissions.in_use(), 1);
    assert_eq!(admissions.software_in_use(), 2);
    session.release_hardware_after_confirmed_reap();
    session.release_software_after_confirmed_reap();
    assert_eq!(admissions.in_use(), 0);
    assert_eq!(admissions.software_in_use(), 0);
}

#[tokio::test]
async fn published_completion_holds_capacity_through_actual_supervisor_join() {
    let dir = crate::test_tempdir().expect("scratch");
    let (session, release, closed) = session_with_pending_drain(dir.path()).await;
    let admissions = Admissions::new();
    reserve_test_admissions(&session, &admissions);
    let mut settlement = spawn_published_completion_cleanup_owner(
        &session,
        0,
        crate::playback_control::RollingProducerCompletionDisposition::CompleteUnverifiedDuration,
        "rolling-drain-join",
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(1), &mut settlement)
            .await
            .is_err()
    );
    assert_eq!(admissions.in_use(), 1);
    assert_eq!(admissions.software_in_use(), 2);
    assert!(!closed.load(SeqCst));
    release.send(()).expect("owned reader was not detached");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), settlement)
            .await
            .expect("cleanup finishes")
            .expect("cleanup result"),
        PublishedFailureCleanupOutcome::Reaped
    );
    assert!(closed.load(SeqCst));
    assert!(session.child.lock().await.is_none());
    assert_eq!(admissions.in_use(), 0);
    assert_eq!(admissions.software_in_use(), 0);
}

#[tokio::test]
async fn cancelled_published_settlement_waiter_cannot_skip_owned_drain() {
    let dir = crate::test_tempdir().expect("scratch");
    let (session, release, closed) = session_with_pending_drain(dir.path()).await;
    let admissions = Admissions::new();
    reserve_test_admissions(&session, &admissions);
    let mut settlement = spawn_published_completion_cleanup_owner(
        &session,
        0,
        crate::playback_control::RollingProducerCompletionDisposition::CompleteUnverifiedDuration,
        "rolling-cancelled-drain-join",
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(1), &mut settlement)
            .await
            .is_err()
    );
    drop(settlement);
    assert_eq!(admissions.in_use(), 1);
    assert_eq!(admissions.software_in_use(), 2);
    assert!(!closed.load(SeqCst));
    release.send(()).expect("cleanup owner retains reader");
    tokio::time::timeout(Duration::from_secs(5), async {
        while session.child.lock().await.is_some()
            || admissions.in_use() != 0
            || admissions.software_in_use() != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("independent cleanup joins and settles despite cancelled waiter");
    assert!(closed.load(SeqCst));
}

#[tokio::test]
async fn off_runtime_attempt_drop_transfers_supervisor_join_to_its_original_runtime() {
    let dir = crate::test_tempdir().expect("scratch");
    let (session, release, closed) = session_with_pending_drain(dir.path()).await;
    let child = session.child.lock().await.take().expect("owned attempt");
    std::thread::spawn(move || {
        assert!(tokio::runtime::Handle::try_current().is_err());
        drop(child);
    })
    .join()
    .expect("attempt Drop does not require an entered runtime");
    release.send(()).expect("supervisor still owns real drain");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !closed.load(SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("original runtime completes the owned reader");
}
