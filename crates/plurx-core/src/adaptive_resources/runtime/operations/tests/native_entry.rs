//! Negative tests for the synchronous native-entry fence.
//! These prove only that refused latches do not enter native work or refund
//! reservations; they are not physical-close or cancellation receipts.
use super::*;
use tokio::sync::oneshot;

#[cfg(unix)]
#[tokio::test]
async fn overage_fence_closes_second_role_latch_and_keeps_both_charges() {
    let root = tempfile::tempdir().expect("durable TMPDIR");
    let runtime = runtime(root.path());
    let owner = prepare(&runtime, vec![plan(), plan()], deadline()).expect("two role prepare");
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .expect("durable role preparation");
    let (first, second) = {
        let bundle = owner.state.lock().await;
        (bundle.roles[0].binding, bundle.roles[1].binding)
    };
    runtime
        .ledger
        .record_native_overage(
            first,
            Uuid::new_v4(),
            ResourceVector {
                scratch_bytes: 1,
                ..ResourceVector::default()
            },
        )
        .expect("same-ledger overage observation");

    let (enter, entered) = oneshot::channel();
    assert!(matches!(
        runtime.release_native_entry(second, enter),
        Err(OperationError::State)
    ));
    assert!(entered.await.is_err(), "refusal drops the unopened latch");
    {
        let state = runtime.ledger.inner.lock().expect("ledger state");
        assert_eq!(
            state.entries.get(&second.reservation).unwrap().state,
            ReservationState::Reserved,
            "the refused role remains reserved"
        );
    }
    assert_eq!(
        runtime.accounting().expect("charge remains held").used,
        ResourceVector {
            scratch_bytes: 5,
            ..vector(4)
        }
    );
    runtime.drain().expect("drain journal owner");
}

#[cfg(unix)]
#[tokio::test]
async fn native_entry_refuses_old_binding_after_capacity_epoch_refresh() {
    let root = tempfile::tempdir().expect("durable TMPDIR");
    let runtime = runtime(root.path());
    let owner = prepare(&runtime, vec![plan()], deadline()).expect("role prepare");
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .expect("durable role preparation");
    let binding = owner.state.lock().await.roles[0].binding;
    runtime
        .update_capacity(2, Metric::Known(vector(100)))
        .expect("advance capacity epoch");

    let (enter, entered) = oneshot::channel();
    assert!(matches!(
        runtime.release_native_entry(binding, enter),
        Err(OperationError::State)
    ));
    assert!(
        entered.await.is_err(),
        "stale binding never enters native work"
    );
    assert_eq!(
        runtime.accounting().expect("charge remains held").used,
        vector(2)
    );
    assert_eq!(runtime.accounting().expect("held role").held, 1);
    assert_eq!(
        runtime
            .ledger
            .inner
            .lock()
            .expect("ledger state")
            .entries
            .get(&binding.reservation)
            .unwrap()
            .state,
        ReservationState::Reserved
    );
    runtime.drain().expect("drain journal owner");
}

#[cfg(unix)]
#[tokio::test]
async fn native_entry_refuses_after_drain_without_canceling_or_refunding_role() {
    let root = tempfile::tempdir().expect("durable TMPDIR");
    let runtime = runtime(root.path());
    let owner = prepare(&runtime, vec![plan()], deadline()).expect("role prepare");
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .expect("durable role preparation");
    let binding = owner.state.lock().await.roles[0].binding;
    runtime.begin_drain().expect("linearize drain");

    let (enter, entered) = oneshot::channel();
    assert!(matches!(
        runtime.release_native_entry(binding, enter),
        Err(OperationError::State)
    ));
    assert!(entered.await.is_err(), "drain closes the unopened latch");
    assert_eq!(
        runtime.accounting().expect("charge remains held").used,
        vector(2)
    );
    assert_eq!(runtime.accounting().expect("held role").held, 1);
    assert_eq!(
        runtime
            .ledger
            .inner
            .lock()
            .expect("ledger state")
            .entries
            .get(&binding.reservation)
            .unwrap()
            .state,
        ReservationState::Reserved
    );
    runtime.drain().expect("drain journal owner");
}
