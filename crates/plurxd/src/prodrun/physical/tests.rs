use super::*;
use plurx_core::adaptive_resources::{ResourceLedger, ResourceVector, WorkloadDemand};
use plurx_core::process::observation::Metric;

fn ledger() -> ResourceLedger {
    ResourceLedger::new(
        1,
        Metric::Known(ResourceVector {
            processes: 2,
            ..Default::default()
        }),
    )
    .expect("bounded ledger")
}

async fn launch<F, Fut>(
    slot: &ProducerSlot,
    ledger: &ResourceLedger,
    trial: bool,
    writer: F,
) -> Result<ProducerRegistration, String>
where
    F: FnOnce(
        ProducerRegistration,
        plurx_core::adaptive_resources::physical_graph::RootPipes,
    ) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (reservation, _cleanup) = ledger
        .reserve(WorkloadDemand {
            resources: Metric::Known(ResourceVector {
                processes: 1,
                ..Default::default()
            }),
            limit_epoch: 1,
            trial,
        })
        .expect("physical reservation");
    let empty = slot.try_reserve_empty().expect("exclusive pre-spawn slot");
    let cache = tempfile::tempdir().expect("owned runtime cache");
    empty
        .spawn_registered_custodied(
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
                work: crate::process_control::ChildWork::background("custodied producer fixture"),
            },
            reservation,
            0,
            writer,
        )
        .await
}

#[tokio::test]
async fn actual_writer_join_precedes_narrow_receipt_but_does_not_refund_graph() {
    let ledger = ledger();
    let slot = Arc::new(ProducerSlot::new());
    let (started, start) = tokio::sync::oneshot::channel();
    let (finish, finished) = tokio::sync::oneshot::channel();
    let registration = launch(&slot, &ledger, false, |_, pipes| async move {
        use tokio::io::AsyncBufReadExt as _;
        let mut stdout = tokio::io::BufReader::new(pipes.stdout.expect("real stdout")).lines();
        loop {
            let line = stdout
                .next_line()
                .await
                .expect("read child")
                .expect("child reports readiness before EOF");
            if let Some(pid) = line.trim().strip_prefix("pid=") {
                assert!(pid.parse::<u32>().expect("real numeric child PID") > 0);
                break;
            }
        }
        started.send(()).expect("fixture alive");
        let _pipes = (stdout, pipes.stderr);
        finished.await.expect("release actual writer");
    })
    .await
    .expect("launch via real producer and slot bridge");
    tokio::time::timeout(std::time::Duration::from_secs(5), start)
        .await
        .expect("bounded child readiness")
        .expect("writer started");
    assert!(slot.try_reserve_empty().is_err());
    assert_eq!(ledger.snapshot().expect("charged").held, 1);
    assert!(slot
        .request_registered_retirement(&registration)
        .await
        .expect("retire"));
    tokio::task::yield_now().await;
    assert!(registration.confirmed_reap().is_none());
    let waiter = tokio::spawn({
        let registration = registration.clone();
        async move { registration.wait_confirmed_reap().await }
    });
    waiter.abort();
    assert!(registration.confirmed_reap().is_none());
    finish.send(()).expect("actual join can finish");
    let receipt = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        registration.wait_confirmed_reap(),
    )
    .await
    .expect("narrow producer retirement completes despite unsupported physical proof");
    assert!(receipt.matches(&registration));
    assert_eq!(receipt.writers(), WriterSettlement::Settled);
    let snapshot = ledger.snapshot().expect("physical hold");
    assert_eq!(snapshot.held, 1);
    assert_eq!(snapshot.unknown_held, 1);
    let graphs = ledger
        .retained_physical_graphs()
        .expect("retained core owner");
    assert_eq!(graphs.len(), 1);
    assert_eq!(graphs[0].snapshot().await.joined_writers, 1);
}

#[tokio::test]
async fn actual_writer_panic_is_joined_abandonment_not_physical_release() {
    let ledger = ledger();
    let slot = ProducerSlot::new();
    let registration = launch(&slot, &ledger, false, |_, _pipes| async move {
        panic!("intentional graph-owned writer failure");
    })
    .await
    .expect("launched graph");
    slot.request_registered_retirement(&registration)
        .await
        .expect("retire");
    let receipt = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        registration.wait_confirmed_reap(),
    )
    .await
    .expect("actual JoinError settles the narrow event");
    assert_eq!(receipt.writers(), WriterSettlement::Abandoned);
    assert_eq!(ledger.snapshot().expect("physical hold").unknown_held, 1);
}

#[tokio::test]
async fn unsupported_trial_never_launches_or_installs_a_slot_generation() {
    let ledger = ledger();
    let slot = ProducerSlot::new();
    let result = launch(&slot, &ledger, true, |_, _| async {
        panic!("unsupported trial must refuse before creating a task");
    })
    .await;
    assert!(result.is_err());
    assert!(slot.try_reserve_empty().is_ok());
    let snapshot = ledger.snapshot().expect("unacquired refund");
    assert_eq!(snapshot.held, 0);
    assert!(!snapshot.trial_held);
}
