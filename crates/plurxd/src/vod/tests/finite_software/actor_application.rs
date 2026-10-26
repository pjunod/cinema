//! Authentic application lifetime regression. The original selected Store and
//! core role come from the parent fixture; the R producer is never started.
//! Only that original owner's no-entry cancel ticket may close/refund it.
use super::*;

#[tokio::test]
async fn original_application_lease_waits_for_last_actor_snapshot_before_same_role_close() {
    use crate::playback_control::{RollingActorAccounting, RollingControlHandle};

    let fixture = finite_cancellation_fixture().await;
    let runtime = &fixture.runtime;
    let baseline = runtime.accounting().expect("original baseline accounting");
    let deadline = Instant::now() + Duration::from_secs(90);
    let epoch = runtime
        .refresh_admission_capacity(deadline)
        .await
        .expect("actual admission capacity");
    let capacity = runtime.accounting().expect("observed capacity").capacity;
    let mut request = finite_profile::compile(
        &fixture.serve.shared,
        &"e".repeat(64),
        &fixture.recipe,
        None,
        &fixture.plan,
        epoch,
        capacity,
        crate::admission::software_budget() as u64,
    )
    .expect("production request from the authentic fixture");
    let application_bytes = RollingControlHandle::original_actor_frame_bytes()
        .expect("actual actor/application requested layout");
    request
        .charge_application_owner_bytes(application_bytes)
        .expect("charge before the original R is admitted");
    let client_partition = usize::try_from(request.output.client_state_bytes)
        .expect("actual original client partition");
    let owner = runtime
        .prepare_software_vod(request, deadline)
        .expect("actual original never-entered R owner");
    let held = runtime.accounting().expect("accepted original role").used;
    assert_ne!(held, baseline.used, "fixture failed to hold an actual R");
    let application = owner
        .take_application_lease(application_bytes)
        .expect("SAME admitted role's original application lease");
    let output = owner.output();
    let budget_owner_bytes = RollingActorAccounting::owner_bytes().expect("actual budget Arc");
    let client_limit = client_partition
        .checked_sub(budget_owner_bytes)
        .expect("budget owner fits original partition");
    let (metadata, clients) = tokio::time::timeout_at(
        deadline.into(),
        output.reserve_metadata_partitions(budget_owner_bytes, client_limit),
    )
    .await
    .expect("borrowed metadata wait deadline")
    .expect("original role's partition slices");
    let accounting = RollingActorAccounting::new(metadata, clients)
        .expect("initial actor state reserved before construction");
    let (control, registration) =
        RollingControlHandle::spawn_prepublication_producer_with_application(
            "actual-never-entered-application-fixture",
            application,
            accounting,
        );
    let snapshot = control
        .snapshot()
        .await
        .expect("actual original actor reply");
    let last_alias = snapshot.clone();
    let mut selector = control.original_actor_join();
    // prepare_cancel generates the original positive no-entry proof. Neither
    // the snapshot, timeout nor this fixture can manufacture that receipt.
    let mut original_ticket = owner
        .prepare_cancel(deadline)
        .await
        .expect("original no-entry native cancellation receipt");
    drop(output);
    drop(owner);
    drop(registration);
    drop(control);
    selector
        .join(deadline)
        .await
        .expect("original run frame finishes despite escaped snapshot aliases");
    drop(selector);
    drop(snapshot);
    assert_eq!(
        runtime.accounting().expect("post-join accounting").used,
        held
    );
    assert!(
        original_ticket
            .finish_retirement(Instant::now() + Duration::from_millis(20))
            .await
            .is_err(),
        "a live original application alias permitted role settlement"
    );
    assert_eq!(
        runtime.accounting().expect("alias still charged").used,
        held
    );
    drop(last_alias);
    original_ticket
        .finish_retirement(deadline)
        .await
        .expect("SAME original ticket closes after the final actual alias drops");
    assert_eq!(
        runtime.accounting().expect("known original closure").used,
        baseline.used
    );
}
