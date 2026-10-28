//! Real copy owners and original native/journal lifecycle boundaries.
//! Authored only: no behavioral or mutation execution before candidate review.
use super::*;
use crate::adaptive_resources::runtime::copy_vod::{CopyOutput, CopyVodOwner};
use std::path::Path;
use std::time::Duration;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
async fn original(root: &Path) -> (Arc<NodeResourceRuntime>, CopyVodOwner, OwnedPreparation) {
    let (runtime, request) = NodeResourceRuntime::fixture_actual_copy_request(root, false).await;
    let owner = runtime
        .prepare_copy_vod(request, deadline())
        .await
        .expect("actual original copy R");
    let preparation = NodeResourceRuntime::fixture_copy_original_preparation(&owner);
    (runtime, owner, preparation)
}
async fn cancel_actual_start(
    runtime: &NodeResourceRuntime,
    owner: &mut CopyVodOwner,
    stage: CopyStartFixtureStage,
) {
    let gate = runtime.fixture_pause_copy_start(stage);
    let mut start = Box::pin(owner.start(deadline()));
    tokio::select! {
        result=&mut start=>panic!("actual start returned before fixture stage {stage:?}: {result:?}"),
        ()=gate.reached()=>{},
        ()=tokio::time::sleep(Duration::from_secs(15))=>panic!("actual copy start stage watchdog; no native closure inferred"),
    }
    drop(start); // only the borrowed waiter leaves; original fields remain
    drop(gate);
}
async fn original_prefix(
    runtime: &NodeResourceRuntime,
    owner: &mut CopyVodOwner,
    preparation: &OwnedPreparation,
) {
    owner
        .prepare_inputs(deadline())
        .await
        .expect("actual original source opens");
    let member = output_member(preparation).await;
    runtime
        .arm_scratch_bootstrap(preparation, 0, member, deadline())
        .await
        .expect("actual prefix arm");
    runtime
        .open_scratch_bootstrap_root(preparation, 0, member, deadline())
        .await
        .expect("actual configured root open");
    runtime
        .open_scratch_bootstrap_sink(preparation, 0, member, deadline())
        .await
        .expect("actual original sink prefix");
}
async fn output_member(preparation: &OwnedPreparation) -> Uuid {
    let bundle = preparation.state.lock().await;
    bundle.roles[0]
        .typed_members
        .as_ref()
        .expect("actual typed copy")
        .iter()
        .find_map(|p| match p {
            v2::MemberPlan::ScratchAssetsV3 { slot, .. } => Some(slot.member_id.uuid()),
            _ => None,
        })
        .expect("original output member")
}
async fn pending_command(preparation: &OwnedPreparation, task_acquired: bool) -> Uuid {
    let bundle = preparation.state.lock().await;
    let role = &bundle.roles[0];
    assert!(
        !role.refunded && role.copy_vod.is_none(),
        "full R survives original native closure"
    );
    let pending = role
        .pending_finite_close
        .as_ref()
        .expect("same original close DTO retained");
    let Mutation::SettleTyped(closed) = &pending.mutation else {
        panic!("original typed copy settlement")
    };
    assert_eq!(
        closed.settlement.operation_id.uuid(),
        pending.key.operation_id
    );
    let facts = &closed.settlement.members;
    assert_eq!(facts.len(), 5);
    assert_eq!(
        facts
            .iter()
            .filter(|d| matches!(
                d,
                v2::MemberDisposition::Closed {
                    fact: v2::ClosedFact::SourceV1 { .. },
                    ..
                }
            ))
            .count(),
        2
    );
    assert_eq!(
        facts
            .iter()
            .filter(|d| matches!(
                d,
                v2::MemberDisposition::Closed {
                    fact: v2::ClosedFact::ScratchAssetsV3 { .. },
                    ..
                }
            ))
            .count(),
        1,
        "acquired prefix cannot become NeverAcquired"
    );
    assert_eq!(
        facts
            .iter()
            .filter(|d| matches!(
                d,
                v2::MemberDisposition::Closed {
                    fact: v2::ClosedFact::VodOutputTaskV1 { .. },
                    ..
                }
            ))
            .count(),
        usize::from(task_acquired)
    );
    assert_eq!(
        facts
            .iter()
            .filter(|d| matches!(
                d,
                v2::MemberDisposition::Closed {
                    fact: v2::ClosedFact::VodOutputTaskNeverAcquiredV1 { .. },
                    ..
                }
            ))
            .count(),
        usize::from(!task_acquired)
    );
    pending.key.operation_id
}
async fn retire_with_actual_alias_barriers(
    runtime: Arc<NodeResourceRuntime>,
    owner: CopyVodOwner,
    preparation: OwnedPreparation,
    task_acquired: bool,
) {
    assert_ne!(
        runtime.accounting().expect("same actual R charge").used,
        ResourceVector::default()
    );
    let output: CopyOutput = owner.output();
    let application = owner
        .take_application_lease(1024)
        .expect("original app allocation lease");
    retire_with_retained_aliases(
        runtime,
        owner,
        preparation,
        task_acquired,
        output,
        application,
    )
    .await;
}
async fn retire_with_retained_aliases<L>(
    runtime: Arc<NodeResourceRuntime>,
    mut owner: CopyVodOwner,
    preparation: OwnedPreparation,
    task_acquired: bool,
    output: CopyOutput,
    application: L,
) {
    let charge = runtime
        .accounting()
        .expect("same original charge at retry")
        .used;
    let mut ticket = owner
        .prepare_retirement(true, deadline())
        .await
        .expect("original per-state physical closure");
    let command = pending_command(&preparation, task_acquired).await;
    drop(owner);
    assert!(
        ticket
            .finish_retirement(Instant::now() + Duration::from_millis(1))
            .await
            .is_err(),
        "live output/application aliases refunded original R"
    );
    assert_eq!(
        pending_command(&preparation, task_acquired).await,
        command,
        "blocked waiter replaced original close operation"
    );
    assert_eq!(runtime.accounting().expect("aliases charged").used, charge);
    drop(output);
    assert!(
        ticket
            .finish_retirement(Instant::now() + Duration::from_millis(1))
            .await
            .is_err(),
        "last actual app lease did not gate release"
    );
    assert_eq!(pending_command(&preparation, task_acquired).await, command);
    assert_eq!(
        runtime.accounting().expect("last alias charged").used,
        charge
    );
    drop(preparation);
    drop(application);
    ticket
        .finish_retirement(deadline())
        .await
        .expect("known original close after actual aliases and rich bundle drop");
    assert_eq!(
        runtime.accounting().expect("one matching refund").used,
        ResourceVector::default()
    );
    drop(ticket);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole original runtime")
        .drain()
        .expect("original journal drain");
}

// R1 + R22 S0: real acquired V3 prefix, original root never entered.
#[tokio::test]
async fn copy_partial_prefix_before_root_closes_exact_head_and_waits_last_original_alias() {
    let root = tempfile::tempdir().expect("actual prefix fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    original_prefix(&runtime, &mut owner, &preparation).await;
    {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role.copy_vod.as_ref().expect("same copy");
        assert!(!copy.entered && copy.task.is_none() && role.roots.is_empty());
        assert_eq!(role.bootstrap.len(), 1);
    }
    retire_with_actual_alias_barriers(runtime, owner, preparation, false).await;
}

// R6 + R22 S2: cancel at the real source-acquisition observation boundary.
#[tokio::test]
async fn copy_root_only_cancellation_reconciles_original_source_observations_before_close() {
    let root = tempfile::tempdir().expect("actual root-only fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::RootRegistered).await;
    {
        let mut bundle = preparation.state.lock().await;
        let role = &mut bundle.roles[0];
        let copy = role.copy_vod.as_ref().expect("same copy");
        assert!(copy.entered && copy.task.is_none());
        assert_eq!(role.roots.len(), 1);
        for member in copy.source_members {
            assert!(
                !role
                    .sources
                    .get_mut(&member)
                    .expect("original source")
                    .inherited_source(copy.root_member)
                    .expect("same inherited copy")
                    .acknowledged
            );
        }
    }
    retire_with_actual_alias_barriers(runtime, owner, preparation, false).await;
}

// R7/U3 + R22 S2: the actual journal Arm is committed but no task registered.
#[tokio::test]
async fn copy_armed_uninvoked_task_uses_actual_root_only_witness_and_typed_never_acquired() {
    let root = tempfile::tempdir().expect("actual armed copy task fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::TaskArmed).await;
    {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role.copy_vod.as_ref().expect("same copy");
        assert!(role.armed.contains(&copy.task_member));
        assert!(!role.invoked.contains(&copy.task_member));
        assert!(copy.task.is_none());
    }
    retire_with_actual_alias_barriers(runtime, owner, preparation, false).await;
}

// R8: grow the SAME retained fixture payload, so the real parser adapter
// rejects its charged memory budget after root entry, before writer acquisition.
// This deliberate negative fault is not a valid profile or native-peak claim.
#[tokio::test]
async fn copy_post_root_actual_parser_budget_refusal_preserves_root_and_refunds_after_close() {
    let root = tempfile::tempdir().expect("actual post-root parser budget fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::TaskArmed).await;
    let charge = runtime.accounting().expect("original full R").used;
    let original_root = {
        let mut bundle = preparation.state.lock().await;
        let role = &mut bundle.roles[0];
        let copy = role.copy_vod.as_mut().expect("same original copy");
        assert!(copy.entered && copy.task.is_none() && copy.fixed_image.is_some());
        let payload = &mut copy
            .work
            .as_mut()
            .expect("unentered original parser work")
            .diagnostic_id;
        let excess = usize::try_from(charge.peak_memory_bytes)
            .expect("fixture memory fits usize")
            .checked_add(1)
            .expect("fixture capacity increment");
        payload
            .try_reserve_exact(excess)
            .expect("negative payload-growth fixture");
        assert!(
            payload.capacity() > usize::try_from(charge.peak_memory_bytes).expect("same bound")
        );
        *role
            .roots
            .get(&copy.root_member)
            .expect("original entered root")
    };
    let error = runtime
        .start_copy_vod(&preparation, 0, deadline())
        .await
        .expect_err("actual adapter budget must refuse before registration");
    assert!(matches!(&error, OperationError::Physical(error)
        if error.to_string() == "physical graph adapters exceed charged budget"));
    {
        let mut bundle = preparation.state.lock().await;
        let role = &mut bundle.roles[0];
        let copy = role.copy_vod.as_ref().expect("same refused original copy");
        assert_eq!(role.roots.get(&copy.root_member), Some(&original_root));
        assert!(copy.task.is_none() && copy.work.is_some() && copy.fixed_image.is_some());
        assert!(
            role.armed.contains(&copy.task_member) && !role.invoked.contains(&copy.task_member)
        );
        // Seal only now, after the actual refusal, as ordinary cleanup does.
        role.sealed = Some(role.graph.take().expect("same unsealed root graph").seal());
        let snapshot = role.sealed.as_ref().expect("same graph").snapshot().await;
        assert_eq!((snapshot.roots, snapshot.writers), (1, 0));
    }
    assert_eq!(
        runtime.accounting().expect("refusal retains same R").used,
        charge
    );
    retire_with_actual_alias_barriers(runtime, owner, preparation, false).await;
}

// R10: cancel at the literal take_copy_start membership-mutex await, without
// sealing first or taking the one-shot sender on behalf of its real driver.
#[tokio::test]
async fn copy_cancelled_start_at_original_members_lock_joins_same_never_entered_task() {
    use std::future::Future;
    use std::task::Poll;
    let root = tempfile::tempdir().expect("actual start-latch mutex fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::TaskObserved).await;
    let charge = runtime.accounting().expect("original full R").used;
    let (holder, original_task) = {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role
            .copy_vod
            .as_ref()
            .expect("same registered original copy");
        assert!(copy.task_observed_committed && !copy.start_released);
        (
            role.graph
                .as_ref()
                .expect("unsealed original graph")
                .fixture_copy_members(),
            (copy.task, copy.task_observation),
        )
    };
    let entered = tokio::sync::Notify::new();
    let release = tokio::sync::Notify::new();
    let mut members = Box::pin(holder.hold(&entered, &release));
    tokio::select! {
        ()=&mut members=>panic!("original members lock unexpectedly released"),
        ()=entered.notified()=>{},
        ()=tokio::time::sleep(Duration::from_secs(15))=>panic!("original mutex acquisition watchdog"),
    }
    let boundary = runtime.fixture_pause_copy_start(CopyStartFixtureStage::BeforeTaskStartLock);
    let mut start = Box::pin(runtime.start_copy_vod(&preparation, 0, deadline()));
    tokio::select! {
        result=&mut start=>panic!("start returned before original mutex await: {result:?}"),
        ()=boundary.reached()=>{},
        ()=tokio::time::sleep(Duration::from_secs(15))=>panic!("original start-lock boundary watchdog"),
    }
    drop(boundary);
    // There is no suspension between this released boundary and the actual
    // take_copy_start mutex await. One poll establishes real contention.
    std::future::poll_fn(|cx| {
        assert!(
            start.as_mut().poll(cx).is_pending(),
            "start must await the held original mutex"
        );
        Poll::Ready(())
    })
    .await;
    drop(start);
    release.notify_one();
    members.await;
    drop(holder); // No fixture alias survives native closure or ledger release.
    assert_eq!(
        runtime
            .accounting()
            .expect("cancelled waiter retains R")
            .used,
        charge
    );
    {
        let bundle = preparation.state.lock().await;
        let copy = bundle.roles[0]
            .copy_vod
            .as_ref()
            .expect("same retained registered task");
        assert_eq!((copy.task, copy.task_observation), original_task);
        assert!(!copy.start_released && copy.terminal.is_none());
    }
    let result = tokio::time::timeout_at(deadline().into(), owner.stop_producer(false))
        .await
        .expect("same original task join watchdog; timeout is not a witness")
        .expect("same parked task joined after sender destruction");
    assert!(!result.completed);
    {
        let bundle = preparation.state.lock().await;
        let copy = bundle.roles[0]
            .copy_vod
            .as_ref()
            .expect("same naturally joined task");
        assert_eq!((copy.task, copy.task_observation), original_task);
        assert_eq!(copy.terminal, Some(CopyTaskTerminal::NeverEntered));
    }
    retire_with_actual_alias_barriers(runtime, owner, preparation, true).await;
}

// R12/R13 + R22 S3: pending or not-yet-issued ORIGINAL named observation.
async fn registered_unreleased(stage: CopyStartFixtureStage) {
    let root = tempfile::tempdir().expect("actual registered unreleased copy fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, stage).await;
    {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role.copy_vod.as_ref().expect("same copy");
        assert!(copy.task.is_some() && !copy.task_observed_committed && !copy.start_released);
        assert!(!copy.task_observation.is_nil());
        if stage == CopyStartFixtureStage::TaskObservationQueued {
            assert_eq!(
                role.transition
                    .as_ref()
                    .expect("original pending observation")
                    .operation
                    .id,
                copy.task_observation
            );
        } else {
            assert!(role.transition.is_none());
        }
    }
    retire_with_actual_alias_barriers(runtime, owner, preparation, true).await;
}
#[tokio::test]
async fn copy_registered_parser_reissues_definitely_unissued_same_named_observation_before_closed_fact(
) {
    registered_unreleased(CopyStartFixtureStage::TaskRegistered).await;
}
#[tokio::test]
async fn copy_registered_parser_replays_pending_same_named_observation_before_closed_fact() {
    registered_unreleased(CopyStartFixtureStage::TaskObservationQueued).await;
}

// R10/R11 + R22 S3: positive NeverEntered comes from the real parked task.
#[tokio::test]
async fn copy_unsent_observed_parser_naturally_joins_as_never_entered_without_default_summary() {
    let root = tempfile::tempdir().expect("actual unsent original parser fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::TaskObserved).await;
    let original = {
        let bundle = preparation.state.lock().await;
        let copy = bundle.roles[0].copy_vod.as_ref().expect("same copy");
        assert!(copy.task_observed_committed && !copy.start_released);
        (copy.task, copy.task_observation)
    };
    let result = tokio::time::timeout_at(deadline().into(), owner.stop_producer(false))
        .await
        .expect("original latch drop/join watchdog; timeout proves no closure")
        .expect("same parked original task joined");
    assert!(!result.completed);
    assert_eq!(result.segments, 0);
    {
        let bundle = preparation.state.lock().await;
        let copy = bundle.roles[0].copy_vod.as_ref().expect("same joined copy");
        assert_eq!((copy.task, copy.task_observation), original);
        assert_eq!(copy.terminal, Some(CopyTaskTerminal::NeverEntered));
    }
    retire_with_actual_alias_barriers(runtime, owner, preparation, true).await;
}

// R14: corrupt only the retained delivery bit; never construct a terminal or witness.
#[tokio::test]
async fn copy_original_never_entered_with_inconsistent_delivery_bit_is_sticky_held_not_refunded() {
    let root = tempfile::tempdir().expect("actual terminal mismatch fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::TaskObserved).await;
    let charge = runtime.accounting().expect("original held charge").used;
    {
        let mut bundle = preparation.state.lock().await;
        bundle.roles[0]
            .copy_vod
            .as_mut()
            .expect("original copy")
            .start_released = true;
    }
    assert!(
        tokio::time::timeout_at(deadline().into(), owner.stop_producer(true))
            .await
            .expect("same native wait watchdog")
            .is_err()
    );
    let (graph, root_handle) = {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role.copy_vod.as_ref().expect("same held copy");
        assert_eq!(
            copy.permanent_hold,
            Some(CopyCleanupHoldReason::TerminalIdentityMismatch)
        );
        (
            role.sealed
                .as_ref()
                .expect("same sealed graph")
                .select_same(),
            *role.roots.get(&copy.root_member).expect("same child"),
        )
    };
    // Explicitly reap the SAME killed child for fixture hygiene. This cannot
    // mint a settlement for the inconsistent task-delivery history.
    tokio::time::timeout_at(deadline().into(), graph.wait_root(root_handle))
        .await
        .expect("same child wait watchdog")
        .expect("same actual root reaped");
    let diagnostics = runtime
        .copy_cleanup_holds()
        .expect("actual observable hold");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].reason,
        CopyCleanupHoldReason::TerminalIdentityMismatch
    );
    assert_eq!(diagnostics[0].held_resources, charge);
    assert!(
        !runtime.has_failed_copy_admission(),
        "sticky hold must not spin ordinary cleanup maintenance"
    );
    assert_eq!(
        runtime
            .maintain_failed_copy_admissions(deadline())
            .await
            .expect("same maintenance"),
        0
    );
    assert_eq!(
        runtime
            .accounting()
            .expect("inconsistent original remains charged")
            .used,
        charge
    );
    drop(graph);
    drop(preparation);
    drop(owner);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole original runtime")
        .drain()
        .expect("durable held journal, no refund");
}

// R19: cancellation after original acquisition replay leaves the exact IDs,
// then another waiter resumes the same owners and settlement.
#[tokio::test]
async fn copy_cancelled_cleanup_after_acquisition_replay_keeps_same_original_task_and_observation()
{
    let root = tempfile::tempdir().expect("actual cleanup replay fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(
        &runtime,
        &mut owner,
        CopyStartFixtureStage::TaskObservationQueued,
    )
    .await;
    let original = {
        let bundle = preparation.state.lock().await;
        let copy = bundle.roles[0]
            .copy_vod
            .as_ref()
            .expect("original queued parser");
        (copy.task, copy.task_observation)
    };
    let output = owner.output();
    let application = owner
        .take_application_lease(1024)
        .expect("original app lease before sealing");
    let gate = runtime.fixture_pause_copy_start(CopyStartFixtureStage::CleanupAcquisitionsResolved);
    let mut cleanup = Box::pin(owner.prepare_retirement(true, deadline()));
    tokio::select! { result=&mut cleanup=>panic!("original cleanup returned before gate: {}",result.is_ok()), ()=gate.reached()=>{}, ()=tokio::time::sleep(Duration::from_secs(15))=>panic!("cleanup replay fixture watchdog") }
    drop(cleanup);
    drop(gate);
    {
        let bundle = preparation.state.lock().await;
        let copy = bundle.roles[0]
            .copy_vod
            .as_ref()
            .expect("same registered parser");
        assert!(copy.task_observed_committed);
        assert_eq!((copy.task, copy.task_observation), original);
        assert!(!copy.start_released);
    }
    retire_with_retained_aliases(runtime, owner, preparation, true, output, application).await;
}

// R19/R22: lose the waiter after positive native closure but before a ticket
// is exposed. This is pending-command retention, not a commit-unknown claim.
#[tokio::test]
async fn copy_cancelled_close_before_ticket_exposure_retains_same_receipt_and_last_alias_barrier() {
    let root = tempfile::tempdir().expect("actual close ticket handoff fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::TaskArmed).await;
    let output = owner.output();
    let application = owner
        .take_application_lease(1024)
        .expect("original app lease before sealing");
    let gate = runtime.fixture_pause_copy_start(CopyStartFixtureStage::CleanupSettlementPrepared);
    let mut cleanup = Box::pin(owner.prepare_retirement(true, deadline()));
    tokio::select! { result=&mut cleanup=>panic!("original cleanup returned before pending receipt gate: {}",result.is_ok()), ()=gate.reached()=>{}, ()=tokio::time::sleep(Duration::from_secs(15))=>panic!("original pending receipt watchdog") }
    drop(cleanup);
    drop(gate);
    let original = pending_command(&preparation, false).await;
    assert!(!original.is_nil());
    retire_with_retained_aliases(runtime, owner, preparation, false, output, application).await;
}

// C5: scheduling custody never replaces the actual native/journal facts.
async fn retained_copy_close_operation(preparation: &OwnedPreparation) -> Uuid {
    let bundle = preparation.state.lock().await;
    let role = &bundle.roles[0];
    assert!(!role.refunded && role.copy_vod.is_none());
    if let Some(pending) = role.pending_finite_close.as_ref() {
        pending.key.operation_id
    } else {
        let transition = role.transition.as_ref().expect("same original settlement");
        assert!(transition.effect == Effect::Settle);
        transition.operation.id
    }
}

#[tokio::test]
async fn copy_live_close_ticket_excludes_maintenance_and_repeated_preparation() {
    let root = tempfile::tempdir().expect("actual live close custody fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::TaskArmed).await;
    let charge = runtime.accounting().expect("actual original R").used;
    let output = owner.output();
    let application = owner
        .take_application_lease(1024)
        .expect("actual app lease");
    let mut ticket = owner
        .prepare_retirement(true, deadline())
        .await
        .expect("actual native close");
    let operation = pending_command(&preparation, false).await;
    assert!(
        !runtime.has_failed_copy_admission(),
        "live ticket owns close custody"
    );
    assert!(
        owner.prepare_retirement(true, deadline()).await.is_err(),
        "no duplicate live ticket"
    );
    assert!(
        !runtime.has_failed_copy_admission(),
        "busy refusal cannot revoke the first claim"
    );
    assert!(
        ticket
            .finish_retirement(Instant::now() + Duration::from_millis(1))
            .await
            .is_err(),
        "real aliases block release"
    );
    assert!(
        !runtime.has_failed_copy_admission(),
        "a borrowed finish timeout keeps the actual ticket"
    );
    drop(owner);
    drop(output);
    drop(application);
    assert_eq!(ticket.fixture_application_released(), Some(true));
    assert_eq!(
        runtime
            .maintain_failed_copy_admissions(deadline())
            .await
            .expect("same maintenance"),
        0
    );
    // Old maintenance could move the pending command even while the ticket's
    // preparation alias prevented refund. Check the untouched command itself.
    assert_eq!(pending_command(&preparation, false).await, operation);
    assert_eq!(runtime.accounting().expect("same full R").used, charge);
    drop(preparation); // inspection must not obstruct actual rich-bundle destruction
    ticket
        .finish_retirement(deadline())
        .await
        .expect("same ticket finalizes once");
    ticket
        .finish_retirement(deadline())
        .await
        .expect("same completed ticket is idempotent");
    drop(ticket);
    assert!(!runtime.has_failed_copy_admission());
    assert_eq!(
        runtime
            .maintain_failed_copy_admissions(deadline())
            .await
            .expect("no resurrected repair"),
        0
    );
    assert_eq!(
        runtime.accounting().expect("one original refund").used,
        ResourceVector::default()
    );
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole runtime")
        .drain()
        .expect("same journal drained");
}

#[tokio::test]
async fn copy_lost_close_ticket_requests_original_repair_while_owner_remains_alive() {
    let root = tempfile::tempdir().expect("actual lost ticket live owner fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::TaskArmed).await;
    let charge = runtime.accounting().expect("actual original R").used;
    let ticket = owner
        .prepare_retirement(true, deadline())
        .await
        .expect("actual original ticket");
    let operation = pending_command(&preparation, false).await;
    drop(ticket); // owner is still alive: its Drop cannot be the repair signal
    assert!(runtime.has_failed_copy_admission());
    assert_eq!(
        runtime
            .maintain_failed_copy_admissions(Instant::now() + Duration::from_millis(100))
            .await
            .expect("repair keeps live application owner"),
        0
    );
    assert_eq!(retained_copy_close_operation(&preparation).await, operation);
    assert_eq!(
        runtime
            .accounting()
            .expect("same owner still retains R")
            .used,
        charge
    );
    drop(owner);
    drop(preparation);
    assert_eq!(
        runtime
            .maintain_failed_copy_admissions(deadline())
            .await
            .expect("original repair after last actual owner"),
        1
    );
    assert!(!runtime.has_failed_copy_admission());
    assert_eq!(
        runtime.accounting().expect("one original refund").used,
        ResourceVector::default()
    );
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole runtime")
        .drain()
        .expect("same journal drained");
}

#[tokio::test]
async fn copy_cancelled_close_before_ticket_exposure_hands_original_custody_to_maintenance() {
    let root = tempfile::tempdir().expect("actual pre-exposure repair fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::TaskArmed).await;
    let charge = runtime.accounting().expect("actual original R").used;
    let gate = runtime.fixture_pause_copy_start(CopyStartFixtureStage::CleanupSettlementPrepared);
    let mut close = Box::pin(owner.prepare_retirement(true, deadline()));
    tokio::select! {
        result=&mut close=>panic!("close returned before original handoff gate: {}",result.is_ok()),
        ()=gate.reached()=>{},
        ()=tokio::time::sleep(Duration::from_secs(15))=>panic!("actual close handoff watchdog"),
    }
    assert!(
        !runtime.has_failed_copy_admission(),
        "the preparation future owns custody"
    );
    drop(close);
    drop(gate);
    let operation = pending_command(&preparation, false).await;
    assert!(
        runtime.has_failed_copy_admission(),
        "cancellation hands back original custody before owner Drop"
    );
    assert_eq!(
        runtime
            .maintain_failed_copy_admissions(Instant::now() + Duration::from_millis(100))
            .await
            .expect("repair retains actual owner"),
        0
    );
    assert_eq!(retained_copy_close_operation(&preparation).await, operation);
    assert_eq!(
        runtime.accounting().expect("full R while owner lives").used,
        charge
    );
    drop(owner);
    drop(preparation);
    assert_eq!(
        runtime
            .maintain_failed_copy_admissions(deadline())
            .await
            .expect("same original repair"),
        1
    );
    assert_eq!(
        runtime.accounting().expect("one original refund").used,
        ResourceVector::default()
    );
    assert!(!runtime.has_failed_copy_admission());
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole runtime")
        .drain()
        .expect("same journal drained");
}

// R2: the real clock expires after the SAME root Arm commits. The actual
// launcher queues a Hold, which cleanup must retry under that captured ID.
#[tokio::test]
async fn copy_root_arm_then_real_deadline_replays_original_hold_and_closes_acquired_prefix() {
    let root = tempfile::tempdir().expect("actual armed root deadline fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    original_prefix(&runtime, &mut owner, &preparation).await;
    let until = Instant::now() + Duration::from_secs(2);
    let gate = runtime.fixture_pause_copy_start(CopyStartFixtureStage::RootArmed);
    let mut start = Box::pin(runtime.start_copy_vod(&preparation, 0, until));
    tokio::select! {result=&mut start=>panic!("original launch returned before committed Arm delay: {result:?}"),()=gate.reached()=>{},()=tokio::time::sleep(Duration::from_secs(15))=>panic!("actual root Arm watchdog")}
    tokio::time::sleep_until((until + Duration::from_millis(10)).into()).await;
    drop(gate);
    assert!(
        matches!(start.await, Err(OperationError::Deadline)),
        "real post-Arm clock must produce the existing deadline path"
    );
    {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role
            .copy_vod
            .as_ref()
            .expect("same never-entered original copy");
        assert!(!copy.entered && role.roots.is_empty());
        assert!(role.armed.contains(&copy.root_member));
        let hold = role.transition.as_ref().expect("actual captured Hold");
        assert!(hold.effect == Effect::Hold);
        assert!(!hold.operation.id.is_nil());
    }
    retire_with_actual_alias_barriers(runtime, owner, preparation, false).await;
}

// R4/R5: generic launcher errors are not no-child authority, including a
// genuine std Child created before Tokio registration was intentionally lost.
async fn original_launcher_uncertainty(
    point: crate::process::spawn_faults::SpawnFaultPoint,
    child_exists: bool,
) {
    use crate::adaptive_resources::runtime::copy_vod::CopyPipeCommand;
    use crate::adaptive_resources::runtime::finite_vod::SoftwareAvcExecutable;
    use sha2::{Digest, Sha256};
    let root = tempfile::tempdir().expect("actual uncertain original launcher fixture");
    let (runtime, mut request) =
        NodeResourceRuntime::fixture_actual_copy_request(root.path(), false).await;
    let binary = super::physical_tests::epipe_ignoring_executable(root.path());
    let identity = crate::source_identity::SourceIdentity::from_metadata(
        &std::fs::metadata(&binary).expect("actual ELF metadata"),
    );
    let digest = hex::encode(Sha256::digest(
        std::fs::read(&binary).expect("actual original image"),
    ));
    request.command = CopyPipeCommand::compile(
        request.command.source().clone(),
        request.command.video(),
        SoftwareAvcExecutable::new(binary, identity, &digest)
            .expect("actual verified original image"),
        root.path().to_owned(),
        None,
        false,
        crate::transcode::Pacing::unpaced(),
        None,
    )
    .expect("same original recipe before reservation");
    let mut owner = runtime
        .prepare_copy_vod(request, deadline())
        .await
        .expect("same admitted original R");
    let preparation = NodeResourceRuntime::fixture_copy_original_preparation(&owner);
    let charge = runtime.accounting().expect("same pre-entry R charge").used;
    let output = owner.output();
    let application = owner
        .take_application_lease(1024)
        .expect("actual original app allocation");
    let fault = crate::process::spawn_faults::SpawnFaultScope::new(point);
    assert!(fault.run(owner.start(deadline())).await.is_err());
    assert_eq!(
        fault.attempts(),
        1,
        "only the original native launch was attempted"
    );
    if child_exists {
        assert!(
            fault.pid().is_some(),
            "genuine original stdChild existed after fork"
        );
        assert!(
            fault
                .try_wait()
                .expect("same original stdChild diagnostic")
                .is_none(),
            "actual helper must still exist, never infer absence from generic error"
        );
    } else {
        assert!(fault.pid().is_none());
        assert_eq!(
            fault
                .try_wait()
                .expect_err("pre-dispatch has no child guard")
                .kind(),
            std::io::ErrorKind::NotFound
        );
    }
    assert!(
        owner.prepare_retirement(true, deadline()).await.is_err(),
        "S1 cannot mint any close/refund authority"
    );
    let (graph, root_member, task_member, acquisition) = {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role.copy_vod.as_ref().expect("same original unknown copy");
        assert!(copy.entered && role.roots.is_empty() && copy.task.is_none());
        assert_eq!(
            copy.permanent_hold,
            Some(CopyCleanupHoldReason::UncertainLaunch)
        );
        assert!(
            copy.never_root_closure.is_none()
                && copy.root_only_closure.is_none()
                && copy.process_closure.is_none()
        );
        (
            role.sealed
                .as_ref()
                .expect("same sealed uncertain graph")
                .select_same(),
            copy.root_member,
            copy.task_member,
            copy.acquisition_id,
        )
    };
    assert!(
        graph
            .try_finite_vod_never_root_closure(root_member, task_member, acquisition)
            .await
            .is_err(),
        "unknown launch must reject actual absence witness even when runtime root map is empty"
    );
    let diagnostics = runtime
        .copy_cleanup_holds()
        .expect("actual original hold diagnostic");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].reason,
        CopyCleanupHoldReason::UncertainLaunch
    );
    assert_eq!(diagnostics[0].held_resources, charge);
    assert!(
        !runtime.has_failed_copy_admission(),
        "permanent unknown must not spin maintenance"
    );
    assert_eq!(
        runtime
            .maintain_failed_copy_admissions(deadline())
            .await
            .expect("same retained maintenance"),
        0
    );
    assert!(fault.run(owner.start(deadline())).await.is_err());
    assert_eq!(
        fault.attempts(),
        1,
        "retry must not launch a replacement child under held original R"
    );
    drop(fault); // fixture's SAME stdChild kill/wait, never a runtime close receipt
    assert_eq!(
        runtime
            .accounting()
            .expect("fixture native hygiene cannot refund R")
            .used,
        charge
    );
    drop(graph);
    drop(output);
    drop(application);
    drop(preparation);
    drop(owner);
    assert_eq!(
        runtime
            .accounting()
            .expect("unknown original remains fully charged after app aliases drop")
            .used,
        charge
    );
    // No drain/refund is claimed for this deliberately held unknown lineage.
    // Original custody remains in the runtime; restart policy U1 is unchanged.
}
#[tokio::test]
async fn copy_original_owner_dispatch_error_without_child_is_permanent_unknown_not_never_root() {
    original_launcher_uncertainty(
        crate::process::spawn_faults::SpawnFaultPoint::BeforeOwnerDispatch,
        false,
    )
    .await;
}
#[tokio::test]
async fn copy_original_post_std_spawn_registration_error_keeps_genuine_child_and_full_r_unknown() {
    original_launcher_uncertainty(
        crate::process::spawn_faults::SpawnFaultPoint::AfterStdSpawnBeforeTokioRegistration,
        true,
    )
    .await;
}

// R21: actual SQLite COMMIT and original receipt are retained in the custody
// worker while only the SAME reply is delayed. No error/result is injected.
#[tokio::test]
async fn copy_cancelled_settlement_reply_after_real_commit_reuses_same_ticket_operation_and_receipt(
) {
    let root = tempfile::tempdir().expect("actual copy committed-reply fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::TaskObserved).await;
    let charge = runtime.accounting().expect("same actual R charge").used;
    let output = owner.output();
    let application = owner
        .take_application_lease(1024)
        .expect("original app allocation before sealing");
    let mut ticket = owner
        .prepare_retirement(true, deadline())
        .await
        .expect("same original copy native closure and typed DTO");
    let operation = pending_command(&preparation, true).await;
    let gate = runtime
        .journal
        .fixture_settlement_reply_gate(operation)
        .expect("gate this exact owner and not-yet-enqueued original settlement");
    drop(owner);
    drop(output);
    drop(application);
    assert_eq!(
        ticket.fixture_application_released(),
        Some(true),
        "only real alias destruction permits final close"
    );
    let mut finish = Box::pin(ticket.finish_retirement(deadline()));
    let committed = tokio::select! {
        result=&mut finish=>panic!("original ticket returned before committed reply gate: {result:?}"),
        diagnostic=gate.committed()=>diagnostic,
        ()=tokio::time::sleep(Duration::from_secs(15))=>panic!("actual COMMIT gate watchdog; no settlement inferred"),
    };
    assert_eq!(committed.owner_id, runtime.owner.owner_id);
    assert_eq!(committed.operation_id, operation);
    assert_eq!(committed.phase, Phase::Settled);
    assert_eq!(
        runtime
            .accounting()
            .expect("actual COMMIT alone cannot refund rich bundle")
            .used,
        charge
    );
    drop(finish); // borrowed waiter only; original receiver stays in runtime
    assert!(
        !runtime.has_failed_copy_admission(),
        "cancelled borrowed finish retains the live ticket claim"
    );
    assert_eq!(
        runtime
            .maintain_failed_copy_admissions(deadline())
            .await
            .expect("maintenance skips claimed original reply"),
        0
    );
    {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        assert_eq!(
            role.handle
                .as_ref()
                .expect("same original obligation")
                .obligation_id(),
            committed.obligation_id
        );
        assert_eq!(
            role.handle
                .as_ref()
                .expect("same original graph")
                .graph_id(),
            committed.graph_id
        );
        let transition = role
            .transition
            .as_ref()
            .expect("same original pending settlement");
        assert_eq!(transition.operation.id, operation);
        assert!(transition.effect == Effect::Settle);
        assert!(transition.operation.pending.is_some() && transition.operation.result.is_none());
        assert!(
            role.pending_finite_close.is_none(),
            "the SAME command was transferred once, never reminted"
        );
        assert!(
            !role.refunded && role.phase != Phase::Settled,
            "runtime has not observed the actual worker COMMIT"
        );
    }
    gate.release();
    drop(gate); // no new worker command while the reply was gated
    drop(preparation);
    ticket
        .finish_retirement(deadline())
        .await
        .expect("SAME ticket, original receiver/operation/receipt, then actual bundle destruction");
    assert_eq!(
        runtime
            .accounting()
            .expect("one matching refund after known original ack")
            .used,
        ResourceVector::default()
    );
    drop(ticket);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole original runtime")
        .drain()
        .expect("same journal drained");
}

// R19: checked source parents and original process proof already exist; retry
// must read committed metadata, never fstat/reopen a closed original File.
async fn cancellation_after_native_close(stage: CopyStartFixtureStage, task_acquired: bool) {
    let root = tempfile::tempdir().expect("actual post-native cleanup retry fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, stage).await;
    let original = {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role.copy_vod.as_ref().expect("same original copy");
        (
            *role
                .roots
                .get(&copy.root_member)
                .expect("same original child"),
            copy.task,
            copy.task_observation,
            copy.acquisition_id,
        )
    };
    let output = owner.output();
    let application = owner
        .take_application_lease(1024)
        .expect("actual original application allocation");
    let gate = runtime.fixture_pause_copy_start(CopyStartFixtureStage::CleanupNativeClosed);
    let mut cleanup = Box::pin(owner.prepare_retirement(true, deadline()));
    tokio::select! {result=&mut cleanup=>panic!("cleanup returned before actual native closure gate: {}",result.is_ok()),()=gate.reached()=>{},()=tokio::time::sleep(Duration::from_secs(15))=>panic!("actual source-close retry watchdog")}
    let bundle = preparation.state.lock().await;
    let role = &bundle.roles[0];
    let copy = role.copy_vod.as_ref().expect("same native-closed copy");
    assert!(
        role.sources.values().all(|source| source.parent_closed()),
        "actual original parent close must precede retry"
    );
    for source in role.sources.values() {
        assert!(source
            .copy_root_observation_committed(copy.root_member, copy.acquisition_id)
            .expect("same metadata acknowledgment without File/fstat"));
    }
    assert_eq!(
        (
            *role
                .roots
                .get(&copy.root_member)
                .expect("retained original root selector"),
            copy.task,
            copy.task_observation,
            copy.acquisition_id
        ),
        original
    );
    let close = if task_acquired {
        copy.process_closure
            .as_ref()
            .expect("original joined proof")
            .close_operation_id()
    } else {
        copy.root_only_closure
            .as_ref()
            .expect("original root-only proof")
            .close_operation_id()
    };
    assert!(!close.is_nil());
    drop(cleanup); // real contention: original bundle guard remains held here
    drop(bundle);
    drop(gate);
    let ticket = owner
        .prepare_retirement(true, deadline())
        .await
        .expect("retry uses metadata and SAME stored native proof");
    {
        let bundle = preparation.state.lock().await;
        let pending = bundle.roles[0]
            .pending_finite_close
            .as_ref()
            .expect("same pending close");
        let Mutation::SettleTyped(closed) = &pending.mutation else {
            panic!("original typed close")
        };
        assert!(
            closed.settlement.members.iter().any(|d| match d {
                v2::MemberDisposition::Closed {
                    fact:
                        v2::ClosedFact::VodOutputTaskV1 {
                            join_operation_id, ..
                        },
                    ..
                } if task_acquired => join_operation_id.uuid() == close,
                v2::MemberDisposition::Closed {
                    fact: v2::ClosedFact::VodOutputTaskNeverAcquiredV1 { operation_id, .. },
                    ..
                } if !task_acquired => operation_id.uuid() == close,
                _ => false,
            }),
            "retry replaced the original process/task close operation"
        );
    }
    drop(ticket); // losing a returned selector still cannot replace its receipt
    retire_with_retained_aliases(
        runtime,
        owner,
        preparation,
        task_acquired,
        output,
        application,
    )
    .await;
}
#[tokio::test]
async fn copy_root_only_cleanup_cancelled_after_parent_close_reuses_original_acknowledgments_and_proof(
) {
    cancellation_after_native_close(CopyStartFixtureStage::TaskArmed, false).await;
}
#[tokio::test]
async fn copy_joined_cleanup_cancelled_after_parent_close_reuses_original_acknowledgments_and_proof(
) {
    cancellation_after_native_close(CopyStartFixtureStage::TaskObserved, true).await;
}

// R19: force original members/bundle lock contention at the no-root proof
// handoff. Poll only borrowed fixture/cleanup futures; spawn no native task.
#[tokio::test]
async fn copy_never_root_witness_is_stored_before_cancellable_contended_bundle_handoff() {
    use std::future::Future;
    use std::task::Poll;
    let root = tempfile::tempdir().expect("actual one-shot no-root handoff fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    original_prefix(&runtime, &mut owner, &preparation).await;
    let graph = {
        let mut bundle = preparation.state.lock().await;
        let role = &mut bundle.roles[0];
        if let Some(graph) = role.graph.take() {
            role.sealed = Some(graph.seal());
        }
        role.sealed
            .as_ref()
            .expect("same original sealed graph")
            .select_same()
    };
    let entered = tokio::sync::Notify::new();
    let release = tokio::sync::Notify::new();
    let mut members = Box::pin(graph.fixture_hold_copy_members(&entered, &release));
    tokio::select! {()=&mut members=>panic!("members guard unexpectedly released"),()=entered.notified()=>{},()=tokio::time::sleep(Duration::from_secs(15))=>panic!("actual members lock watchdog")}
    let before = runtime.fixture_pause_copy_start(CopyStartFixtureStage::NeverRootProofReadyToMint);
    let after = runtime.fixture_pause_copy_start(CopyStartFixtureStage::NeverRootProofStored);
    let output = owner.output();
    let application = owner
        .take_application_lease(1024)
        .expect("actual app allocation before sealing");
    let mut cleanup = Box::pin(owner.prepare_retirement(true, deadline()));
    tokio::select! {result=&mut cleanup=>panic!("cleanup returned before physical proof handoff: {}",result.is_ok()),()=before.reached()=>{},()=tokio::time::sleep(Duration::from_secs(15))=>panic!("actual no-root constructor watchdog")}
    // If minting left the bundle unlocked, this SAME contender takes it and
    // forces a minted local proof to face a cancellable post-mint lock await.
    let contender = preparation.state.try_lock().ok();
    drop(before);
    release.notify_one();
    members.await; // release ONLY the original members lock
    let mut stored = Box::pin(after.reached());
    let stored_before_cancellation = std::future::poll_fn(|cx| {
        assert!(
            cleanup.as_mut().poll(cx).is_pending(),
            "fixture expected a borrowed cleanup wait"
        );
        Poll::Ready(stored.as_mut().poll(cx).is_ready())
    })
    .await;
    drop(stored);
    drop(cleanup);
    drop(contender);
    drop(after);
    assert!(
        stored_before_cancellation,
        "minted original no-root proof was stranded before guarded storage"
    );
    let close = {
        let bundle = preparation.state.lock().await;
        let copy = bundle.roles[0]
            .copy_vod
            .as_ref()
            .expect("same original copy");
        let proof = copy
            .never_root_closure
            .as_ref()
            .expect("original one-shot proof retained despite cancellation");
        assert!(proof.matches(&graph));
        proof.close_operation_id()
    };
    let ticket = owner
        .prepare_retirement(true, deadline())
        .await
        .expect("same one-shot absence proof resumed");
    {
        let bundle = preparation.state.lock().await;
        let Mutation::SettleTyped(closed) = &bundle.roles[0]
            .pending_finite_close
            .as_ref()
            .expect("same typed close")
            .mutation
        else {
            panic!("original typed close")
        };
        assert!(closed.settlement.members.iter().any(|d|matches!(d,v2::MemberDisposition::Closed{fact:v2::ClosedFact::LegacyNeverAcquiredV1{operation_id,..},..} if operation_id.uuid()==close)),"retry minted a replacement no-root proof");
    }
    drop(ticket);
    drop(graph);
    retire_with_retained_aliases(runtime, owner, preparation, false, output, application).await;
}

// R3/R6 acknowledgment-unknown shape: the real original source observation
// is queued; its worker may already have committed, but this waiter never
// observed the reply. Explicit storage-Unknown is a separate unforced case.
#[tokio::test]
async fn copy_pending_original_source_observation_replays_before_any_parent_or_root_close() {
    let root = tempfile::tempdir().expect("actual pending source-root fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(
        &runtime,
        &mut owner,
        CopyStartFixtureStage::SourceRootObservationQueued,
    )
    .await;
    {
        let mut bundle = preparation.state.lock().await;
        let role = &mut bundle.roles[0];
        let copy = role.copy_vod.as_ref().expect("same original copy");
        assert!(copy.entered && copy.task.is_none());
        let root_member = copy.root_member;
        let transition = role
            .transition
            .as_ref()
            .expect("original source acquisition pending");
        let Effect::SourceRoot(member) = transition.effect else {
            panic!("same source-root command")
        };
        let operation = transition.operation.id;
        let source = role
            .sources
            .get_mut(&member)
            .expect("same original source cell");
        let inherited = source
            .inherited_source(root_member)
            .expect("same still-open acknowledgment inputs");
        assert_eq!(inherited.observation_id, operation);
        assert!(!inherited.acknowledged && !source.parent_closed());
    }
    retire_with_actual_alias_barriers(runtime, owner, preparation, false).await;
}

// The FULL validator must reject a malformed local DTO before consuming ANY
// original one-shot process/prefix witness. Restore only the fixture flag and
// retry the SAME owner/settlement ID; no native handle or command is replaced.
#[tokio::test]
async fn copy_full_preflight_rejection_retains_original_witnesses_and_same_settlement_for_retry() {
    let root = tempfile::tempdir().expect("actual copy preflight rejection fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::TaskObserved).await;
    let charge = runtime.accounting().expect("same original R").used;
    let output = owner.output();
    let application = owner
        .take_application_lease(1024)
        .expect("original live app allocation");
    let settlement = {
        let mut bundle = preparation.state.lock().await;
        let copy = bundle.roles[0]
            .copy_vod
            .as_mut()
            .expect("same original copy");
        copy.preflight_omit_owner_once = true;
        copy.settlement_operation_id
    };
    assert!(!settlement.is_nil());
    assert!(
        owner.prepare_retirement(true, deadline()).await.is_err(),
        "malformed constructed DTO bypassed full preflight before witness consumption"
    );
    let close = {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role
            .copy_vod
            .as_ref()
            .expect("same unconsumed original witnesses");
        assert_eq!(copy.settlement_operation_id, settlement);
        assert!(!copy.preflight_omit_owner_once);
        assert!(copy.permanent_hold.is_none());
        assert_eq!(
            copy.retryable_hold,
            Some(CopyCleanupHoldReason::ValidationRejected)
        );
        assert!(role.pending_finite_close.is_none() && role.transition.is_none() && !role.refunded);
        let member = role
            .bootstrap
            .keys()
            .next()
            .copied()
            .expect("same native-closed original prefix");
        role.bootstrap
            .get(&member)
            .expect("original prefix")
            .producer_close_fact(role.binding, member)
            .expect("prefix proof still unissued after failed preflight");
        assert!(role.sources.values().all(|source| source.parent_closed()));
        copy.process_closure
            .as_ref()
            .expect("original process proof was not consumed by rejected DTO")
            .close_operation_id()
    };
    assert_eq!(
        runtime
            .accounting()
            .expect("full R after rejected DTO")
            .used,
        charge
    );
    assert!(
        runtime.has_failed_copy_admission(),
        "metadata rejection must keep the same original retry eligible"
    );
    assert_eq!(
        runtime.copy_cleanup_holds().expect("actual metadata hold")[0].reason,
        CopyCleanupHoldReason::ValidationRejected
    );

    let ticket = owner
        .prepare_retirement(true, deadline())
        .await
        .expect("same original metadata restored, full validation then one-shot custody");
    assert_eq!(
        pending_command(&preparation, true).await,
        settlement,
        "preflight retry minted another logical settlement"
    );
    {
        let bundle = preparation.state.lock().await;
        let Mutation::SettleTyped(closed) = &bundle.roles[0]
            .pending_finite_close
            .as_ref()
            .expect("same pending command")
            .mutation
        else {
            panic!("original typed copy close")
        };
        assert!(closed.settlement.members.iter().any(|m|matches!(m,v2::MemberDisposition::Closed{fact:v2::ClosedFact::VodOutputTaskV1{join_operation_id,..},..} if join_operation_id.uuid()==close)),"preflight retry replaced original native close proof");
    }
    drop(ticket);
    retire_with_retained_aliases(runtime, owner, preparation, true, output, application).await;
}

// C1: unlike the deadline case, these actual refusals occur inside the
// selected native launcher AFTER committed Arm and the invoked bookkeeping.
async fn copy_post_arm_native_admission_refusal(epoch_change: bool) {
    let root = tempfile::tempdir().expect("actual post-Arm refusal fixture");
    let (runtime, request) =
        NodeResourceRuntime::fixture_actual_copy_request(root.path(), false).await;
    let capacity = request.demand.resources;
    let epoch = request.demand.limit_epoch;
    let mut owner = runtime
        .prepare_copy_vod(request, deadline())
        .await
        .expect("original R");
    let preparation = NodeResourceRuntime::fixture_copy_original_preparation(&owner);
    original_prefix(&runtime, &mut owner, &preparation).await;
    let gate = runtime.fixture_pause_copy_start(CopyStartFixtureStage::RootArmed);
    let mut start = Box::pin(runtime.start_copy_vod(&preparation, 0, deadline()));
    tokio::select! {
        result=&mut start=>panic!("original launch skipped committed Arm boundary: {result:?}"),
        ()=gate.reached()=>{},
        ()=tokio::time::sleep(Duration::from_secs(15))=>panic!("FAILURE watchdog: committed original Arm missing"),
    }
    if epoch_change {
        runtime
            .update_capacity(epoch.checked_add(1).expect("next real epoch"), capacity)
            .expect("actual same-ledger capacity epoch change");
    } else {
        runtime
            .begin_drain()
            .expect("actual admission drain after committed Arm");
    }
    drop(gate);
    assert!(
        matches!(start.await, Err(OperationError::Physical(_))),
        "must reach real native acquisition refusal, not the earlier deadline branch"
    );
    {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role.copy_vod.as_ref().expect("same never-entered original");
        assert!(!copy.entered && role.roots.is_empty() && copy.task.is_none());
        assert!(role.armed.contains(&copy.root_member));
        assert!(
            role.invoked.contains(&copy.root_member),
            "exercise the former closer rejection"
        );
        assert!(
            role.transition
                .as_ref()
                .expect("original refusal Hold")
                .effect
                == Effect::Hold
        );
        let snapshot = role
            .sealed
            .as_ref()
            .expect("same sealed graph")
            .snapshot()
            .await;
        assert!(!snapshot.acquired && snapshot.roots == 0 && snapshot.writers == 0);
    }
    retire_with_actual_alias_barriers(runtime, owner, preparation, false).await;
}

#[tokio::test]
async fn copy_post_arm_actual_drain_refusal_closes_same_never_root_and_refunds() {
    copy_post_arm_native_admission_refusal(false).await;
}

#[tokio::test]
async fn copy_post_arm_actual_epoch_refusal_closes_same_never_root_and_refunds() {
    copy_post_arm_native_admission_refusal(true).await;
}

// R15 is required: actual entered parser, actual immutable five-file window,
// no reader acknowledgment, and natural original task join after cancellation.
#[tokio::test]
async fn copy_entered_parser_full_window_retirement_wakes_and_joins_before_deadline() {
    use crate::adaptive_resources::runtime::copy_vod::CopyPipeCommand;
    use crate::adaptive_resources::runtime::finite_vod::SoftwareAvcExecutable;
    let root = tempfile::tempdir().expect("actual full-window fixture");
    let (runtime, mut request) =
        NodeResourceRuntime::fixture_actual_copy_request(root.path(), false).await;
    // Eight genuine source iterations exceed the valid minimum five-file
    // rolling window even if one complete source GOP makes each segment.
    // This is fixture input creation, not a manufactured fold or native proof.
    let source = root.path().join("full-window-source.mp4");
    crate::testfixtures::run(
        std::process::Command::new(crate::testfixtures::ffmpeg())
            .args(["-y", "-v", "error", "-stream_loop", "7", "-i"])
            .arg(&request.source.path)
            .args(["-map", "0", "-c", "copy"])
            .arg(&source),
    );
    let source = std::fs::canonicalize(source).expect("canonical genuine extended source");
    let metadata = std::fs::metadata(&source).expect("actual extended source metadata");
    let identity = crate::source_identity::SourceIdentity::from_metadata(&metadata);
    let probe = crate::testfixtures::run(
        std::process::Command::new(crate::testfixtures::ffprobe())
            .args([
                "-v",
                "error",
                "-show_streams",
                "-show_format",
                "-of",
                "json",
            ])
            .arg(&source),
    );
    let probe: serde_json::Value = serde_json::from_slice(&probe).expect("actual extended probe");
    let probe = crate::scan::probe::parse_probe_json(&probe);
    let mut media = request.command.source().clone();
    media.path = source.clone();
    media.size = i64::try_from(metadata.len()).expect("actual source size");
    media.mtime = identity.mtime;
    media.duration_ms = probe.duration_ms;
    request.command = CopyPipeCommand::compile(
        media,
        request.command.video(),
        SoftwareAvcExecutable::new(
            request.command.executable().path().to_owned(),
            request.command.executable().source_identity(),
            &hex::encode(request.command.executable().sha256()),
        )
        .expect("same cached executable plan; original SourceCell must verify it"),
        root.path().to_owned(),
        None,
        false,
        crate::transcode::Pacing::unpaced(),
        None,
    )
    .expect("same fixed copy launcher for actual source");
    request.source.path = source;
    request.source.object_version = identity.render_s1();
    request.source.scanner_size = i64::try_from(metadata.len()).expect("actual scanner size");
    request.source.scanner_mtime = identity.mtime;
    // These are valid immutable pre-R payload limits, as in the existing real
    // positive-publication fixture. Do not widen the five-file window itself.
    request.parser_limits.max_bytes = 4 * 1024 * 1024;
    request.reader_bounds.unit_bytes = 4 * 1024 * 1024;
    request.reader_bounds.buffer_bytes = 4 * 1024 * 1024;
    request.output.one_io_payload_bytes = 4 * 1024 * 1024;
    request.output.one_file_logical_bytes = 4 * 1024 * 1024;
    request.output.resident_logical_bytes = 48 * 1024 * 1024;
    request.output.resident_memory_bytes = 8 * 1024 * 1024;
    assert_eq!(request.output.resident_file_objects, 5);
    let window = request.output.resident_file_objects;
    let mut owner = runtime
        .prepare_copy_vod(request, deadline())
        .await
        .expect("actual tight-window R");
    let preparation = NodeResourceRuntime::fixture_copy_original_preparation(&owner);
    let charge = runtime.accounting().expect("full original R").used;
    let output = owner.output();
    let application = owner
        .take_application_lease(1024)
        .expect("real application alias");
    let gate = runtime.fixture_pause_copy_start(CopyStartFixtureStage::OutputWindowFull);
    owner
        .start(deadline())
        .await
        .expect("actual original native parser entry");
    tokio::time::timeout(Duration::from_secs(30), gate.reached())
        .await
        .expect("FAILURE watchdog: original sink never reached its real full window");
    let (observation, root_member) = {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role.copy_vod.as_ref().expect("same actual entered parser");
        assert!(copy.entered && copy.start_released && copy.task_observed_committed);
        let member = output_member_from_role(role);
        let state = role
            .bootstrap
            .get(&member)
            .expect("same actual fold")
            .fold()
            .expect("same acknowledged output state")
            .state();
        assert_eq!(state.asset.totals.resident_file_objects, window);
        assert!(
            copy.task.is_some(),
            "same original task retained at full window"
        );
        (copy.task_observation, copy.root_member)
    };
    drop(gate); // only the fixture delay leaves; no real window credit is freed.
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            owner.wait_producer_result(deadline())
        )
        .await
        .is_err(),
        "full original window must keep the actual parser pending without acknowledgment"
    );
    let mut ticket = tokio::time::timeout(
        Duration::from_secs(15),
        owner.prepare_retirement(true, deadline()),
    )
    .await
    .expect("R15 FAILURE: original full-window parser did not naturally join")
    .expect("same original native/scratch closure");
    {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        assert!(role.copy_vod.is_none() && role.pending_finite_close.is_some());
        let Mutation::SettleTyped(closed) = &role
            .pending_finite_close
            .as_ref()
            .expect("same pending close")
            .mutation
        else {
            panic!("original typed close")
        };
        assert!(closed.settlement.members.iter().any(|m| matches!(m,
            v2::MemberDisposition::Closed { fact: v2::ClosedFact::VodOutputTaskV1 { task: actual, .. }, .. }
                if actual.root_member_id.uuid() == root_member)), "original registered task inventory survives closure");
    }
    assert!(!observation.is_nil());
    assert_eq!(
        runtime
            .accounting()
            .expect("native closure is not refund")
            .used,
        charge
    );
    drop(owner);
    assert!(
        ticket
            .finish_retirement(Instant::now() + Duration::from_millis(1))
            .await
            .is_err(),
        "actual output/application/preparation aliases still retain R"
    );
    drop(output);
    drop(application);
    drop(preparation);
    ticket
        .finish_retirement(deadline())
        .await
        .expect("same original joined ticket and alias release");
    assert_eq!(
        runtime.accounting().expect("known original refund").used,
        ResourceVector::default()
    );
    drop(ticket);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole original runtime")
        .drain()
        .expect("actual journal drain");
}

fn output_member_from_role(role: &OwnedRole) -> Uuid {
    role.typed_members
        .as_ref()
        .expect("same original typed plan")
        .iter()
        .find_map(|plan| match plan {
            v2::MemberPlan::ScratchAssetsV3 { slot, .. } => Some(slot.member_id.uuid()),
            _ => None,
        })
        .expect("same original output member")
}

async fn copy_post_mint_source_fact_collision(root_only: bool) {
    let root = tempfile::tempdir().expect("actual post-mint collision fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(
        &runtime,
        &mut owner,
        if root_only {
            CopyStartFixtureStage::RootRegistered
        } else {
            CopyStartFixtureStage::TaskObserved
        },
    )
    .await;
    let charge = runtime.accounting().expect("original held R").used;
    preparation.state.lock().await.roles[0]
        .copy_vod
        .as_mut()
        .expect("same original copy")
        .post_mint_source_collision = true;
    assert!(owner.prepare_retirement(true, deadline()).await.is_err());
    let (graph, close) = {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role
            .copy_vod
            .as_ref()
            .expect("retained same original witness");
        assert_eq!(
            copy.permanent_hold,
            Some(CopyCleanupHoldReason::TerminalIdentityMismatch)
        );
        assert!(!copy.post_mint_source_collision && role.pending_finite_close.is_none());
        let close = if root_only {
            copy.root_only_closure
                .as_ref()
                .expect("genuine root-only proof retained")
                .close_operation_id()
        } else {
            copy.process_closure
                .as_ref()
                .expect("genuine joined proof retained")
                .close_operation_id()
        };
        assert!(role.sources.values().all(|s| s.parent_closed()));
        (
            role.sealed.as_ref().expect("same graph").select_same(),
            close,
        )
    };
    assert_eq!(
        graph.snapshot().await.roots,
        0,
        "actual constructor already consumed its native inventory"
    );
    assert!(owner.prepare_retirement(true, deadline()).await.is_err());
    {
        let bundle = preparation.state.lock().await;
        let copy = bundle.roles[0]
            .copy_vod
            .as_ref()
            .expect("same permanently held original");
        let retry_close = if root_only {
            copy.root_only_closure
                .as_ref()
                .expect("same proof")
                .close_operation_id()
        } else {
            copy.process_closure
                .as_ref()
                .expect("same proof")
                .close_operation_id()
        };
        assert_eq!(retry_close, close);
    }
    assert!(
        !runtime.has_failed_copy_admission(),
        "permanent contradiction must not spin maintenance"
    );
    assert_eq!(
        runtime
            .maintain_failed_copy_admissions(deadline())
            .await
            .expect("same maintenance"),
        0
    );
    assert_eq!(
        runtime.accounting().expect("no contradictory refund").used,
        charge
    );
    drop(graph);
    drop(preparation);
    drop(owner);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole held runtime")
        .drain()
        .expect("durable held journal, no refund");
}

#[tokio::test]
async fn copy_post_mint_root_only_fact_identity_error_is_permanent_and_retains_proof() {
    copy_post_mint_source_fact_collision(true).await;
}

#[tokio::test]
async fn copy_post_mint_joined_fact_identity_error_is_permanent_and_retains_proof() {
    copy_post_mint_source_fact_collision(false).await;
}

// C4/R22: drive the actual runtime closer with its task selector lost while
// the SAME physical graph still owns the actual registered writer/JoinHandle.
#[tokio::test]
async fn copy_runtime_missing_task_with_actual_registered_writer_is_sticky_held() {
    let root = tempfile::tempdir().expect("actual registered writer selector-loss fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::TaskRegistered).await;
    let charge = runtime.accounting().expect("same original R").used;
    let (graph, task, child) = {
        let mut bundle = preparation.state.lock().await;
        let role = &mut bundle.roles[0];
        let copy = role
            .copy_vod
            .as_mut()
            .expect("actual original registered writer");
        let task = copy
            .task
            .take()
            .expect("lose only retained runtime selector");
        assert!(role.invoked.contains(&copy.task_member));
        let child = *role
            .roots
            .get(&copy.root_member)
            .expect("same original root");
        let graph = role.graph.take().expect("same original graph").seal();
        role.sealed = Some(graph);
        (
            role.sealed
                .as_ref()
                .expect("same original sealed graph")
                .select_same(),
            task,
            child,
        )
    };
    assert_eq!(graph.snapshot().await.writers, 1);
    assert!(owner.prepare_retirement(true, deadline()).await.is_err());
    {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role.copy_vod.as_ref().expect("same held original");
        assert_eq!(
            copy.permanent_hold,
            Some(CopyCleanupHoldReason::TerminalIdentityMismatch)
        );
        assert!(
            copy.task.is_none()
                && copy.root_only_closure.is_none()
                && copy.process_closure.is_none()
                && copy.never_root_closure.is_none()
        );
        assert!(role.pending_finite_close.is_none());
    }
    assert!(!runtime.has_failed_copy_admission());
    assert_eq!(
        runtime
            .maintain_failed_copy_admissions(deadline())
            .await
            .expect("same maintenance"),
        0
    );
    assert_eq!(
        runtime
            .accounting()
            .expect("selector loss does not refund")
            .used,
        charge
    );
    // Fixture hygiene uses ONLY captured genuine original selectors. Do not
    // restore the runtime field, create a replacement task or mint a witness.
    let _ = graph.start_kill_root(child).await;
    graph
        .wait_root(child)
        .await
        .expect("same original native child wait");
    graph
        .join_copy_vod(task)
        .await
        .expect("same original parked task naturally joins");
    {
        let mut bundle = preparation.state.lock().await;
        for source in bundle.roles[0].sources.values_mut() {
            source
                .close_parent()
                .await
                .expect("same actual original parent close");
        }
    }
    assert_eq!(
        runtime
            .accounting()
            .expect("native teardown alone is not refund")
            .used,
        charge
    );
    drop(graph);
    drop(preparation);
    drop(owner);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole held runtime")
        .drain()
        .expect("durable held journal, no refund");
}
