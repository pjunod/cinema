use super::*;
use crate::resource_custody::InstallationContext;

fn runtime() -> (tempfile::TempDir, Arc<NodeResourceRuntime>) {
    let root = tempfile::tempdir().expect("protected journal fixture");
    let owner = daemon_owner("fixture-node".into(), "fixture-cluster".into());
    let runtime = Arc::new(
        NodeResourceRuntime::open(super::super::super::tests::options(
            root.path(),
            owner,
            InstallationContext::FirstInstallation,
        ))
        .expect("actual journal runtime"),
    );
    runtime
        .ledger
        .observe_capacity(
            Metric::Known(ResourceVector {
                cpu_ns_per_second: 1_000_000_000,
                peak_memory_bytes: 2_000_000_000,
                handles: 10_000,
                processes: 100,
                hardware_slots: 100,
                tuner_slots: 100,
                scratch_bytes: 2_000_000_000,
            })
            .into(),
            std::time::Instant::now(),
        )
        .expect("fresh capacity");
    (root, runtime)
}
async fn idle_service(port: CoreServicePort, _: Arc<()>) {
    assert!(
        port.wait_work().await.is_none(),
        "this structural fixture grants no native R"
    );
}
fn start(
    root: &mut OrdinaryFinalizerLifecycle,
    runtime: &Arc<NodeResourceRuntime>,
    executor: tokio::runtime::Handle,
) -> OrdinaryPoolClient {
    fn requested_for<A, B, F: Future<Output = ()>>(_: fn(A, B) -> F) -> usize {
        requested::<F>().expect("computed actual service layout")
    }
    let profile = FinalizationSupportProfile::ordinary(requested_for(idle_service))
        .expect("private fixed recipe");
    let epoch = runtime.ledger.snapshot().expect("snapshot").limit_epoch;
    root.lease = Some(
        runtime
            .reserve_support(profile, epoch)
            .expect("real census and ledger hold"),
    );
    root.activate(runtime, Arc::new(()), idle_service, executor)
        .expect("actual fixed services and original handle")
}
fn deadline() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(5)
}
fn used(runtime: &NodeResourceRuntime) -> u64 {
    runtime
        .ledger
        .snapshot()
        .expect("snapshot")
        .used
        .peak_memory_bytes
}

#[tokio::test]
async fn collector_adopts_original_guard_and_blocks_final_destruction_until_actual_guard_drop() {
    let (_temp, runtime) = runtime();
    let directory = BuildGateDirectory::default();
    let legacy = directory.gate("key").expect("legacy");
    let guard = legacy.lock_owned().await;
    let mut root = OrdinaryFinalizerLifecycle::new(directory.clone());
    let client = start(&mut root, &runtime, tokio::runtime::Handle::current());
    client
        .control
        .wait_ready(deadline())
        .await
        .expect("first-poll Ready");
    let slot = client.reserve("key").expect("adopt held original");
    assert!(directory
        .gate("key")
        .expect("adopted")
        .try_lock_owned()
        .is_err());
    slot.cancel_unbound().expect("core knows no grant occurred");
    drop(slot);
    let before = directory.inner.drops.load(Ordering::Acquire);
    drop(client);
    assert!(root.shutdown(deadline()).await.is_err());
    assert_eq!(directory.inner.drops.load(Ordering::Acquire), before);
    assert!(used(&runtime) > 0);
    drop(guard);
    root.shutdown(deadline())
        .await
        .expect("all actual originals released");
    assert_eq!(used(&runtime), 0);
    assert!(directory
        .gate("key")
        .expect("released route reopened")
        .try_lock_owned()
        .is_ok());
}

#[tokio::test]
async fn collector_matching_terminal_key_never_forks_an_earlier_free_slot() {
    let (_temp, runtime) = runtime();
    let directory = BuildGateDirectory::default();
    let mut root = OrdinaryFinalizerLifecycle::new(directory.clone());
    let client = start(&mut root, &runtime, tokio::runtime::Handle::current());
    client.control.wait_ready(deadline()).await.expect("Ready");
    let other = client.reserve("other").expect("slot zero");
    let old = client.reserve("key").expect("later slot");
    assert_ne!(other.index, old.index);
    let guard = directory
        .gate("key")
        .expect("original key")
        .lock_owned()
        .await;
    old.cancel_unbound().expect("never-granted terminal");
    other.cancel_unbound().expect("earlier slot terminal");
    drop(other);
    assert!(matches!(
        client.reserve("key"),
        Err(AdmissionRefusal::LedgerFull)
    ));
    assert!(directory
        .gate("key")
        .expect("same original")
        .try_lock_owned()
        .is_err());
    let index = old.index;
    drop(old);
    let restarted = client
        .reserve("key")
        .expect("same key can reuse same held mutex");
    assert_eq!(restarted.index, index);
    assert!(directory
        .gate("key")
        .expect("same mutex on restart")
        .try_lock_owned()
        .is_err());
    restarted.cancel_unbound().expect("never native");
    drop(restarted);
    drop(guard);
    drop(client);
    root.shutdown(deadline()).await.expect("normal stop");
}

#[tokio::test]
async fn collector_weak_header_refusal_destroys_no_unique_prefix() {
    let (_temp, runtime) = runtime();
    let directory = BuildGateDirectory::default();
    let mut root = OrdinaryFinalizerLifecycle::new(directory.clone());
    let client = start(&mut root, &runtime, tokio::runtime::Handle::current());
    client.control.wait_ready(deadline()).await.expect("Ready");
    let weak = {
        let meta = client.payload.records[N - 1]
            .meta
            .lock()
            .expect("last gate");
        Arc::downgrade(&meta.gate)
    };
    let before = directory.inner.drops.load(Ordering::Acquire);
    drop(client);
    assert!(root.shutdown(deadline()).await.is_err());
    assert_eq!(directory.inner.drops.load(Ordering::Acquire), before);
    let upgraded = weak.upgrade().expect("same original survived");
    drop(weak);
    assert!(root.shutdown(deadline()).await.is_err());
    assert_eq!(directory.inner.drops.load(Ordering::Acquire), before);
    drop(upgraded);
    root.shutdown(deadline())
        .await
        .expect("atomic uniqueness after aliases gone");
    assert_eq!(directory.inner.drops.load(Ordering::Acquire), before + N);
}

#[tokio::test]
async fn collector_closing_route_has_no_legacy_fallback_while_actual_client_survives() {
    let (_temp, runtime) = runtime();
    let directory = BuildGateDirectory::default();
    let mut root = OrdinaryFinalizerLifecycle::new(directory.clone());
    let client = start(&mut root, &runtime, tokio::runtime::Handle::current());
    client.control.wait_ready(deadline()).await.expect("Ready");
    assert!(root.shutdown(deadline()).await.is_err());
    let other = directory.clone();
    std::thread::spawn(move || {
        for _ in 0..100 {
            assert!(matches!(
                other.gate("never-seen-key"),
                Err(AdmissionRefusal::Unavailable)
            ));
        }
    })
    .join()
    .expect("concurrent lookup");
    assert!(used(&runtime) > 0);
    drop(client);
    root.shutdown(deadline()).await.expect("same state resumed");
    assert_eq!(used(&runtime), 0);
}

#[tokio::test]
async fn collector_cancelled_ready_waiter_keeps_same_original_actor() {
    let (_temp, runtime) = runtime();
    let directory = BuildGateDirectory::default();
    let mut root = OrdinaryFinalizerLifecycle::new(directory);
    let client = start(&mut root, &runtime, tokio::runtime::Handle::current());
    let state = root
        .lease
        .as_ref()
        .expect("lease")
        .original_state()
        .expect("state");
    let id = state
        .inner
        .lock()
        .expect("counts")
        .task
        .as_ref()
        .expect("original JH")
        .id();
    drop(state);
    {
        let waiter = client.control.wait_ready(deadline());
        tokio::pin!(waiter);
        assert!(futures_util::poll!(waiter.as_mut()).is_pending());
    }
    assert!(client.reserve("unready").is_err());
    client
        .control
        .wait_ready(deadline())
        .await
        .expect("same actor Ready");
    let state = root
        .lease
        .as_ref()
        .expect("lease")
        .original_state()
        .expect("state");
    assert_eq!(
        state
            .inner
            .lock()
            .expect("counts")
            .task
            .as_ref()
            .expect("same JH")
            .id(),
        id
    );
    drop(state);
    drop(client);
    root.shutdown(deadline()).await.expect("normal stop");
}

#[tokio::test]
async fn collector_closed_executor_never_ready_and_keeps_actual_unknown_hold() {
    let (_temp, runtime) = runtime();
    let directory = BuildGateDirectory::default();
    let mut root = OrdinaryFinalizerLifecycle::new(directory);
    let executor = std::thread::spawn(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("executor");
        let handle = runtime.handle().clone();
        drop(runtime);
        handle
    })
    .join()
    .expect("executor owner");
    let client = start(&mut root, &runtime, executor);
    assert!(client.control.wait_ready(deadline()).await.is_err());
    assert!(client.reserve("no-R").is_err());
    assert!(used(&runtime) > 0);
    assert_eq!(
        runtime
            .ledger
            .snapshot()
            .expect("snapshot")
            .finalization_support
            .expect("support")
            .phase,
        FinalizationSupportPhase::UnknownHeld
    );
    assert!(root.shutdown(deadline()).await.is_err());
}

#[tokio::test]
async fn collector_abandoned_envelope_drop_keeps_original_charge_and_blocks_reuse() {
    let (_temp, runtime) = runtime();
    let directory = BuildGateDirectory::default();
    let mut root = OrdinaryFinalizerLifecycle::new(directory);
    let client = start(&mut root, &runtime, tokio::runtime::Handle::current());
    client.control.wait_ready(deadline()).await.expect("Ready");
    let slot = client.reserve("key").expect("slot");
    let guard = slot.begin_envelope_drop();
    drop(guard);
    drop(slot);
    assert!(client.reserve("other").is_err());
    assert_eq!(
        runtime
            .ledger
            .snapshot()
            .expect("snapshot")
            .finalization_support
            .expect("support")
            .phase,
        FinalizationSupportPhase::UnknownHeld
    );
    assert!(used(&runtime) > 0);
}

#[tokio::test]
async fn collector_admission_drain_preserves_existing_generic_gates_before_final_seal() {
    let (_temp, runtime) = runtime();
    let directory = BuildGateDirectory::default();
    let mut root = OrdinaryFinalizerLifecycle::new(directory.clone());
    let client = start(&mut root, &runtime, tokio::runtime::Handle::current());
    client.control.wait_ready(deadline()).await.expect("Ready");
    root.begin_drain();
    assert!(client.reserve("new").is_err());
    assert!(directory
        .gate("legacy-cleanup")
        .expect("old cleanup gate still available")
        .try_lock_owned()
        .is_ok());
    drop(client);
    root.shutdown(deadline())
        .await
        .expect("final seal after old cleanup");
}

#[tokio::test]
async fn collector_final_commit_refusal_preserves_storage_proof_without_reopening_route() {
    let (_temp, runtime) = runtime();
    let directory = BuildGateDirectory::default();
    let mut root = OrdinaryFinalizerLifecycle::new(directory.clone());
    let client = start(&mut root, &runtime, tokio::runtime::Handle::current());
    client.control.wait_ready(deadline()).await.expect("Ready");
    client
        .payload
        .exhaust_on_destroy
        .store(true, Ordering::Release);
    drop(client);
    assert!(root.shutdown(deadline()).await.is_err());
    assert!(used(&runtime) > 0);
    assert!(matches!(
        directory.gate("blocked"),
        Err(AdmissionRefusal::Unavailable)
    ));
    let slot = runtime
        .finalization_support
        .lock()
        .expect("actual inline slot");
    assert!(matches!(
        &slot.inner,
        SupportCustody::Held {
            state: None,
            destroyed: Some(_),
            ..
        }
    ));
}

#[tokio::test]
async fn collector_only_matching_postcommit_witness_reopens_directory_once() {
    let (_temp, runtime) = runtime();
    let directory = BuildGateDirectory::default();
    let mut root = OrdinaryFinalizerLifecycle::new(directory.clone());
    let client = start(&mut root, &runtime, tokio::runtime::Handle::current());
    client.control.wait_ready(deadline()).await.expect("Ready");
    let control = Arc::clone(&client.control);
    directory
        .inner
        .seal(control.seal)
        .expect("seal original route");
    control.stop.store(true, Ordering::Release);
    client.request_all_close();
    drop(client);
    root.lease
        .as_mut()
        .expect("lease")
        .join_original()
        .await
        .expect("same original join");
    drop(root.client.take());
    let released = root
        .lease
        .as_mut()
        .expect("lease")
        .try_close_released()
        .expect("actual numeric/census commit and slot clear");
    assert_eq!(used(&runtime), 0);
    assert!(matches!(
        directory.gate("blocked"),
        Err(AdmissionRefusal::Unavailable)
    ));
    let seal = released.collector.expect("directory bound proof");
    let identity = released.identity;
    let mut wrong = seal;
    wrong.epoch += 1;
    assert!(directory
        .inner
        .reopen(FinalizationSupportReleased {
            identity,
            collector: Some(wrong)
        })
        .is_err());
    directory
        .inner
        .reopen(released)
        .expect("consume actual released identity");
    assert!(directory
        .inner
        .reopen(FinalizationSupportReleased {
            identity,
            collector: Some(seal)
        })
        .is_err());
}
