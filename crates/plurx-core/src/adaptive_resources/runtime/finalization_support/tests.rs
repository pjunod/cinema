use super::*;
use crate::adaptive_resources::owner_inventory::{OwnerFamily, OwnerKind};
use crate::resource_custody::InstallationContext;
use std::sync::atomic::AtomicUsize;
use std::task::Poll;

fn runtime() -> (tempfile::TempDir, Arc<NodeResourceRuntime>) {
    let root = tempfile::tempdir().expect("durable protected TMPDIR");
    let owner = daemon_owner("fixture-node".into(), "fixture-cluster".into());
    let runtime = Arc::new(
        NodeResourceRuntime::open(super::super::tests::options(
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
                cpu_ns_per_second: 1_000_000,
                peak_memory_bytes: 1_000_000,
                handles: 100,
                processes: 100,
                hardware_slots: 100,
                tuner_slots: 100,
                scratch_bytes: 1_000_000,
            })
            .into(),
            std::time::Instant::now(),
        )
        .expect("fresh known RAM");
    (root, runtime)
}
fn used(runtime: &NodeResourceRuntime) -> u64 {
    runtime
        .ledger
        .snapshot()
        .expect("ledger snapshot")
        .used
        .peak_memory_bytes
}
struct Payload(Arc<AtomicUsize>);
impl Drop for Payload {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn support_unallocated_close_and_replacement_require_original_identity() {
    let (_root, runtime) = runtime();
    let mut lease = runtime.reserve_fixture_support().expect("support");
    let original = lease.identity;
    assert_eq!(used(&runtime), 64 * 1024);
    assert!(runtime.has_unreconciled_custody());
    assert_eq!(runtime.ledger.snapshot().expect("snapshot").held, 0);
    lease
        .try_close()
        .expect("sealed allocation was never possible");
    assert_eq!(used(&runtime), 0);
    assert_eq!(lease.try_close(), Err(AdmissionRefusal::InvalidTransition));
    let mut replacement = runtime
        .reserve_fixture_support()
        .expect("new original support");
    assert_ne!(replacement.identity, original);
    replacement.try_close().expect("new unallocated close");
    assert_eq!(
        runtime
            .affected_owners
            .snapshot()
            .expect("census")
            .records()
            .count(),
        0
    );
}

#[tokio::test]
async fn support_positive_join_actual_payload_and_state_aliases_precede_release() {
    let (_root, runtime) = runtime();
    let count = Arc::new(AtomicUsize::new(0));
    let mut lease = runtime.reserve_fixture_support().expect("support");
    let payload = lease
        .activate_fixture(
            || Payload(Arc::clone(&count)),
            |owned| async move {
                let _actual = owned;
            },
        )
        .expect("actual registration");
    lease.join_original().await.expect("original positive join");
    assert_eq!(lease.try_close(), Err(AdmissionRefusal::InvalidTransition));
    assert_eq!(count.load(Ordering::Relaxed), 0);
    drop(payload);
    let state = lease.original_state().expect("same original state alias");
    assert_eq!(lease.try_close(), Err(AdmissionRefusal::InvalidTransition));
    let restored = lease.original_state().expect("same Arc restored");
    assert!(Arc::ptr_eq(&state, &restored));
    drop((state, restored));
    assert_eq!(used(&runtime), 64 * 1024);
    lease
        .try_close()
        .expect("actual storage and Arc headers destroyed before release");
    assert_eq!(count.load(Ordering::Relaxed), 1);
    assert_eq!(used(&runtime), 0);
}

#[tokio::test]
async fn support_join_waiter_cancellation_retains_exact_original_handle() {
    let (_root, runtime) = runtime();
    let (send, receive) = tokio::sync::oneshot::channel::<()>();
    let mut lease = runtime.reserve_fixture_support().expect("support");
    let payload = lease
        .activate_fixture(
            || (),
            |_| async move {
                receive.await.expect("release original task");
            },
        )
        .expect("registration");
    {
        let wait = lease.join_original();
        tokio::pin!(wait);
        std::future::poll_fn(|cx| {
            assert!(wait.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    assert_eq!(used(&runtime), 64 * 1024);
    assert_eq!(lease.try_close(), Err(AdmissionRefusal::InvalidTransition));
    send.send(()).expect("original remains live");
    lease
        .join_original()
        .await
        .expect("retry joins same original");
    drop(payload);
    lease.try_close().expect("release after original join");
}

#[tokio::test]
async fn support_failed_join_is_sticky_and_never_repolls_completed_handle() {
    let (_root, runtime) = runtime();
    let mut lease = runtime.reserve_fixture_support().expect("support");
    let payload = lease
        .activate_fixture(
            || (),
            |_| async {
                panic!("actual fixture task panic");
            },
        )
        .expect("registration");
    drop(payload);
    assert_eq!(
        lease.join_original().await,
        Err(AdmissionRefusal::Unavailable)
    );
    assert_eq!(
        lease.join_original().await,
        Err(AdmissionRefusal::Unavailable)
    );
    assert!(lease
        .original_state()
        .expect("original custody")
        .inner
        .lock()
        .expect("counts")
        .task
        .is_some());
    assert!(lease.try_close().is_err());
    assert_eq!(used(&runtime), 64 * 1024);
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
}

#[tokio::test]
async fn support_destructor_panic_keeps_inline_original_runtime_pin_and_unknown_charge() {
    struct Panics;
    impl Drop for Panics {
        fn drop(&mut self) {
            panic!("actual payload destructor panic");
        }
    }
    let (_root, runtime) = runtime();
    let weak = Arc::downgrade(&runtime);
    let mut lease = runtime.reserve_fixture_support().expect("support");
    let payload = lease
        .activate_fixture(|| Panics, |_| async {})
        .expect("registration");
    lease.join_original().await.expect("positive join");
    drop(payload);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lease.try_close())).is_err());
    assert!(runtime.finalization_support.try_lock().is_ok());
    assert!(runtime.ledger.inner.try_lock().is_ok());
    assert_eq!(used(&runtime), 64 * 1024);
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
    drop((lease, runtime));
    assert!(
        weak.upgrade().is_some(),
        "inline cycle pins actual journal/daemon-lock owner after State destruction panics"
    );
}

#[test]
fn support_selector_drop_under_census_and_ledger_locks_only_marks_atomics() {
    let (_root, runtime) = runtime();
    let lease = runtime.reserve_fixture_support().expect("support");
    runtime
        .affected_owners
        .with_runtime_mutation(|_| {
            let _ledger = runtime.ledger.inner.lock().expect("ledger already held");
            drop(lease);
        })
        .expect("no reentrant locks");
    assert_eq!(used(&runtime), 64 * 1024);
    let census = runtime
        .affected_owners
        .snapshot()
        .expect("reconcile deferred marker");
    assert!(census
        .records()
        .any(|row| row.phase == OwnerPhase::UnknownHeld));
}

#[test]
fn support_full_census_refuses_without_partial_charge_or_overflow_mutation() {
    let (_root, runtime) = runtime();
    let owners: Vec<_> = (0..crate::adaptive_resources::owner_inventory::MAX_INVENTORY_OWNERS)
        .map(|_| {
            runtime
                .affected_owners
                .register_original(
                    Uuid::new_v4(),
                    OwnerFamily::NativeCleanup,
                    OwnerKind::Residency,
                )
                .expect("fill fixed census")
        })
        .collect();
    let before = runtime.affected_owners.snapshot().expect("census");
    assert!(runtime.reserve_fixture_support().is_err());
    assert_eq!(used(&runtime), 0);
    assert_eq!(
        runtime
            .affected_owners
            .snapshot()
            .expect("unchanged census")
            .epoch(),
        before.epoch()
    );
    drop(owners);
}

#[tokio::test]
async fn support_oversized_fixture_refuses_before_payload_or_task_allocation() {
    let (_root, runtime) = runtime();
    let mut lease = runtime.reserve_fixture_support().expect("support");
    let calls = AtomicUsize::new(0);
    assert!(lease
        .activate_fixture(
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                [0u8; 65536]
            },
            |_| async {}
        )
        .is_err());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    lease.try_close().expect("allocation never became possible");
    assert_eq!(used(&runtime), 0);
}

#[tokio::test]
async fn support_activation_epoch_exhaustion_refuses_before_factory_and_all_phase_changes() {
    let (_root, runtime) = runtime();
    let mut lease = runtime.reserve_fixture_support().expect("support");
    runtime
        .affected_owners
        .with_runtime_mutation(|inventory| inventory.exhaust_epoch_for_support_fixture())
        .expect("exhaust after reservation");
    let calls = AtomicUsize::new(0);
    assert!(lease
        .activate_fixture(
            || {
                calls.fetch_add(1, Ordering::Relaxed);
            },
            |_| async {}
        )
        .is_err());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let snapshot = runtime.ledger.snapshot().expect("ledger");
    assert_eq!(
        snapshot.finalization_support.expect("support").phase,
        FinalizationSupportPhase::ReservedUnallocated
    );
    let census = runtime.affected_owners.snapshot().expect("census");
    assert!(census
        .records()
        .all(|row| row.phase == OwnerPhase::Registered));
    let slot = runtime.finalization_support.lock().expect("slot");
    assert!(matches!(
        &slot.inner,
        SupportCustody::Held {
            allocation_possible: false,
            state: None,
            ..
        }
    ));
}

#[tokio::test]
async fn support_wrapped_future_boundary_refuses_before_both_factories() {
    const FIXED: usize = std::mem::size_of::<SupportState>()
        + std::mem::size_of::<SupportPayload<()>>()
        + std::mem::size_of::<OwnedPayload<()>>()
        + 8 * std::mem::size_of::<usize>();
    const RAW: usize = 65536 - FIXED;
    struct Boundary([u8; RAW]);
    impl Future for Boundary {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
            let _ = self.0[0];
            Poll::Ready(())
        }
    }
    assert!(
        FinalizationSupportProfile::fixture().fits_inline(FIXED + std::mem::size_of::<Boundary>())
    );
    assert!(!FinalizationSupportProfile::fixture()
        .fits_inline(FIXED + registered_future_size::<Boundary>()));
    let (_root, runtime) = runtime();
    let mut lease = runtime.reserve_fixture_support().expect("support");
    let calls = AtomicUsize::new(0);
    assert!(lease
        .activate_fixture(
            || {
                calls.fetch_add(1, Ordering::Relaxed);
            },
            |_| {
                calls.fetch_add(1, Ordering::Relaxed);
                Boundary([0; RAW])
            }
        )
        .is_err());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        runtime
            .ledger
            .snapshot()
            .expect("snapshot")
            .finalization_support
            .expect("support")
            .phase,
        FinalizationSupportPhase::ReservedUnallocated
    );
    assert_eq!(used(&runtime), 64 * 1024);
    lease.try_close().expect("allocation remained impossible");
}

#[tokio::test]
async fn support_original_self_join_refuses_without_poisoning_external_close() {
    let (_root, runtime) = runtime();
    let shared = Arc::new(tokio::sync::Mutex::new(None::<FinalizationSupportLease>));
    let original_shared = Arc::clone(&shared);
    let (start, started) = tokio::sync::oneshot::channel::<()>();
    let (result, observed) = tokio::sync::oneshot::channel();
    let mut lease = runtime.reserve_fixture_support().expect("support");
    let payload = lease
        .activate_fixture(
            || (),
            |_| async move {
                started.await.expect("external authority published");
                let mut authority = original_shared.lock().await;
                let refused = authority
                    .as_mut()
                    .expect("same original lease")
                    .join_original()
                    .await;
                result.send(refused).expect("self-join result");
            },
        )
        .expect("registration");
    *shared.lock().await = Some(lease);
    start.send(()).expect("start original");
    assert_eq!(
        observed.await.expect("actual original self-join"),
        Err(AdmissionRefusal::InvalidTransition)
    );
    let mut lease = shared
        .lock()
        .await
        .take()
        .expect("external original authority");
    lease
        .join_original()
        .await
        .expect("external positive original join");
    drop(payload);
    lease
        .try_close()
        .expect("self-join refusal did not poison custody");
    assert_eq!(used(&runtime), 0);
}
