use super::*;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

struct Capacity(Arc<AtomicUsize>);
impl Drop for Capacity {
    fn drop(&mut self) {
        self.0.fetch_add(1, SeqCst);
    }
}

fn launch<F, Fut>(
    slot: &ProducerSlot,
    releases: &Arc<AtomicUsize>,
    writer: F,
) -> (ProducerRegistration, Option<u32>)
where
    F: FnOnce(
            ProducerRegistration,
            tokio::process::ChildStdout,
            tokio::process::ChildStderr,
        ) -> Fut
        + Send
        + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let cache = crate::test_tempdir().expect("owned cache");
    slot.try_reserve_empty()
        .expect("reserve BEFORE launch")
        .spawn_registered_owned(
            &std::env::current_exe().expect("test executable"),
            &[
                "--exact".to_owned(),
                "producer_spawn::tests::child_main".to_owned(),
                "--nocapture".to_owned(),
            ],
            crate::producer_spawn::SpawnOptions {
                runtime_cache: cache.path(),
                progress: crate::producer_spawn::Progress::None,
                descriptors: crate::producer_spawn::Descriptors::default(),
                env: &[("PLURX_PRODUCER_CHILD_MODE", std::ffi::OsStr::new("hold"))],
                work: crate::process_control::ChildWork::background("owned writer fixture"),
            },
            0,
            Some(Box::new(Capacity(Arc::clone(releases)))),
            writer,
        )
        .expect("real launch")
}

async fn receipt(registration: &ProducerRegistration) -> ConfirmedProducerReap {
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        registration.wait_confirmed_reap(),
    )
    .await
    .expect("bounded actual reap")
}

#[tokio::test]
async fn production_slot_joins_actual_writer_after_result_and_waiter_cancellation() {
    let slot = ProducerSlot::new();
    let releases = Arc::new(AtomicUsize::new(0));
    let (reported, result) = tokio::sync::oneshot::channel();
    let (finish, finished) = tokio::sync::oneshot::channel();
    let (registration, pid) = launch(&slot, &releases, |_, stdout, stderr| async move {
        // Reporting completion is not task completion: real writes/resources
        // can still be retained until the owned JoinHandle actually returns.
        reported.send(()).expect("observer alive");
        let _pipes = (stdout, stderr);
        finished.await.expect("test releases writer");
    });
    result.await.expect("actual task started");
    assert!(pid.is_some_and(|pid| pid > 0));
    assert!(
        slot.try_reserve_empty().is_err(),
        "no second launch under one slot"
    );
    slot.request_registered_retirement(&registration)
        .await
        .expect("request only, not self join");
    let waiter = tokio::spawn({
        let registration = registration.clone();
        async move { registration.wait_confirmed_reap().await }
    });
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    assert_eq!(releases.load(SeqCst), 0);
    assert!(registration.confirmed_reap().is_none());
    finish.send(()).expect("actual writer retained");
    assert_eq!(
        receipt(&registration).await.writers(),
        WriterSettlement::Settled
    );
    assert_eq!(releases.load(SeqCst), 1);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if slot.try_reserve_empty().is_ok() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("slot becomes available after actual joins");
}

#[tokio::test]
async fn writer_factory_panic_is_joined_after_child_installation() {
    let slot = ProducerSlot::new();
    let releases = Arc::new(AtomicUsize::new(0));
    let (registration, _) = launch(&slot, &releases, |_, _, _| {
        // Panic while CONSTRUCTING the future, not just when polling it.
        panic!("intentional factory panic");
        #[allow(unreachable_code)]
        std::future::ready(())
    });
    assert_eq!(releases.load(SeqCst), 0);
    slot.request_registered_retirement(&registration)
        .await
        .expect("retire installed child");
    assert_eq!(
        receipt(&registration).await.writers(),
        WriterSettlement::Abandoned
    );
    assert_eq!(releases.load(SeqCst), 1);
}

#[tokio::test]
async fn dropping_production_slot_retains_writer_and_admission_until_join() {
    let slot = ProducerSlot::new();
    let releases = Arc::new(AtomicUsize::new(0));
    let (finish, finished) = tokio::sync::oneshot::channel();
    let (registration, _) = launch(&slot, &releases, |_, stdout, stderr| async move {
        let _pipes = (stdout, stderr);
        finished.await.expect("writer retained after slot drop");
    });
    drop(slot);
    assert_eq!(releases.load(SeqCst), 0);
    assert!(registration.confirmed_reap().is_none());
    finish.send(()).expect("actual task alive");
    assert_eq!(
        receipt(&registration).await.writers(),
        WriterSettlement::Settled
    );
    assert_eq!(releases.load(SeqCst), 1);
}

#[tokio::test]
async fn failed_launch_releases_unacquired_slot_without_invoking_writer() {
    let slot = ProducerSlot::new();
    let releases = Arc::new(AtomicUsize::new(0));
    let cache = crate::test_tempdir().expect("owned fixture");
    let result = slot
        .try_reserve_empty()
        .expect("empty")
        .spawn_registered_owned(
            &cache.path().join("missing-executable"),
            &[],
            crate::producer_spawn::SpawnOptions {
                runtime_cache: cache.path(),
                progress: crate::producer_spawn::Progress::None,
                descriptors: crate::producer_spawn::Descriptors::default(),
                env: &[],
                work: crate::process_control::ChildWork::background("failed fixture"),
            },
            0,
            Some(Box::new(Capacity(Arc::clone(&releases)))),
            |_, _, _| async { panic!("no task may run after failed launch") },
        );
    assert!(result.is_err());
    assert_eq!(releases.load(SeqCst), 1);
    assert!(slot.try_reserve_empty().is_ok());
}
