//! Real copy owners and original native/journal lifecycle boundaries.
//! Authored only: no behavioral or mutation execution before candidate review.
use super::*;
use crate::adaptive_resources::runtime::copy_vod::{CopyCloseTicket, CopyOutput, CopyVodOwner};
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
async fn retained_native_close_operation(
    preparation: &OwnedPreparation,
    task_acquired: bool,
) -> Uuid {
    let bundle = preparation.state.lock().await;
    let role = &bundle.roles[0];
    assert!(!role.refunded, "the same full R remains held");
    if let Some(copy) = role.copy_vod.as_ref() {
        assert!(role.pending_finite_close.is_none() && role.transition.is_none());
        assert!(role.graph.is_none() && role.sealed.is_some());
        assert!(matches!(role.phase, Phase::Sealed | Phase::UnknownHeld));
        assert_eq!(
            usize::from(copy.never_root_closure.is_some())
                + usize::from(copy.root_only_closure.is_some())
                + usize::from(copy.process_closure.is_some()),
            1,
            "same original native witness retained, never a typed close DTO"
        );
        assert_eq!(copy.task.is_some(), task_acquired);
        assert!(role.sources.values().all(|source| source.parent_closed()));
        for (member, bootstrap) in &role.bootstrap {
            bootstrap
                .producer_close_fact(role.binding, *member)
                .expect("same prefix native close remains unconsumed");
        }
        return copy.settlement_operation_id;
    }
    if let Some(transition) = role.transition.as_ref() {
        assert!(transition.effect == Effect::Settle);
        return transition.operation.id;
    }
    drop(bundle);
    pending_command(preparation, task_acquired).await
}

// Inspect the unchanged FULL five-fact DTO only after real application and
// response release. The gate delays delivery, never substitutes a result.
pub(super) async fn finish_validated_ticket_with<F>(
    runtime: &NodeResourceRuntime,
    ticket: &mut CopyCloseTicket,
    preparation: OwnedPreparation,
    task_acquired: bool,
    expected: Uuid,
    inspect: F,
) where
    F: for<'a> FnOnce(
        &'a OwnedPreparation,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>,
{
    let gate = runtime.fixture_pause_copy_start(CopyStartFixtureStage::CleanupSettlementPrepared);
    let mut finish = Box::pin(ticket.finish_retirement(deadline()));
    tokio::select! {
        result=&mut finish=>panic!("finish skipped real pending DTO gate: {result:?}"),
        ()=gate.reached()=>{},
        ()=tokio::time::sleep(Duration::from_secs(15))=>panic!("real full DTO gate watchdog"),
    }
    assert_eq!(pending_command(&preparation, task_acquired).await, expected);
    inspect(&preparation).await;
    drop(preparation);
    drop(gate);
    finish
        .await
        .expect("same original validated close and matching refund");
}

async fn finish_validated_ticket(
    runtime: &NodeResourceRuntime,
    ticket: &mut CopyCloseTicket,
    preparation: OwnedPreparation,
    task_acquired: bool,
    expected: Uuid,
) {
    finish_validated_ticket_with(
        runtime,
        ticket,
        preparation,
        task_acquired,
        expected,
        |_| Box::pin(async {}),
    )
    .await;
}

async fn finish_existing_ticket_with_aliases<L, F>(
    runtime: Arc<NodeResourceRuntime>,
    owner: CopyVodOwner,
    preparation: OwnedPreparation,
    task_acquired: bool,
    aliases: (CopyOutput, L),
    mut ticket: CopyCloseTicket,
    inspect: F,
) where
    F: for<'a> FnOnce(
        &'a OwnedPreparation,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>,
{
    let (output, application) = aliases;
    let charge = runtime.accounting().expect("actual native ticket R").used;
    let operation = retained_native_close_operation(&preparation, task_acquired).await;
    drop(owner);
    assert!(
        ticket
            .finish_retirement(Instant::now() + Duration::from_millis(1))
            .await
            .is_err(),
        "original output/application aliases still retain full R"
    );
    assert_eq!(
        runtime.accounting().expect("actual aliases charged").used,
        charge
    );
    drop(output);
    assert!(
        ticket
            .finish_retirement(Instant::now() + Duration::from_millis(1))
            .await
            .is_err(),
        "last original application lease still gates final preflight"
    );
    assert_eq!(
        retained_native_close_operation(&preparation, task_acquired).await,
        operation
    );
    assert_eq!(
        runtime.accounting().expect("last alias charged").used,
        charge
    );
    drop(application);
    finish_validated_ticket_with(
        &runtime,
        &mut ticket,
        preparation,
        task_acquired,
        operation,
        inspect,
    )
    .await;
    assert_eq!(
        runtime
            .accounting()
            .expect("one matching original refund")
            .used,
        ResourceVector::default()
    );
    drop(ticket);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole original runtime")
        .drain()
        .expect("same original journal drained");
}

async fn finish_validated_maintenance(
    runtime: &NodeResourceRuntime,
    preparation: OwnedPreparation,
    task_acquired: bool,
    expected: Uuid,
) {
    let gate = runtime.fixture_pause_copy_start(CopyStartFixtureStage::CleanupSettlementPrepared);
    let mut finish = Box::pin(runtime.maintain_failed_copy_admissions(deadline()));
    tokio::select! {
        result=&mut finish=>panic!("repair skipped same full DTO gate: {result:?}"),
        ()=gate.reached()=>{},
        ()=tokio::time::sleep(Duration::from_secs(15))=>panic!("original repair DTO watchdog"),
    }
    assert_eq!(pending_command(&preparation, task_acquired).await, expected);
    drop(preparation);
    drop(gate);
    assert_eq!(finish.await.expect("same original maintenance close"), 1);
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
    let mut ticket = match owner.prepare_retirement(true, deadline()).await {
        Ok(ticket) => ticket,
        Err(error) => {
            let bundle = preparation.state.lock().await;
            let role = &bundle.roles[0];
            let copy = role
                .copy_vod
                .as_ref()
                .expect("original failed close selector");
            let prefix_phases: Vec<_> = role
                .bootstrap
                .values()
                .map(|bootstrap| {
                    bootstrap.fold().map(|fold| {
                        format!(
                            "phase={:?}, lifecycle={:?}, pending_jobs={}",
                            fold.state().bootstrap.phase,
                            fold.state().asset.lifecycle,
                            fold.state().asset.totals.pending_jobs
                        )
                    })
                })
                .collect();
            let source_closed: Vec<_> = role
                .sources
                .values()
                .map(|source| source.parent_closed())
                .collect();
            panic!(
                "original per-state physical closure: {error:?}; phase={:?}, transition={}, sealed={}, closing={}, never_root={}, root_only={}, joined={}, source_closed={source_closed:?}, bootstrap={prefix_phases:?}, pending_close={}, permanent={:?}, retryable={:?}",
                role.phase,
                role.transition.is_some(),
                role.sealed.is_some(),
                copy.closing,
                copy.never_root_closure.is_some(),
                copy.root_only_closure.is_some(),
                copy.process_closure.is_some(),
                role.pending_finite_close.is_some(),
                copy.permanent_hold,
                copy.retryable_hold
            );
        }
    };
    let command = retained_native_close_operation(&preparation, task_acquired).await;
    drop(owner);
    assert!(
        ticket
            .finish_retirement(Instant::now() + Duration::from_millis(1))
            .await
            .is_err(),
        "live output/application aliases refunded original R"
    );
    assert_eq!(
        retained_native_close_operation(&preparation, task_acquired).await,
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
    assert_eq!(
        retained_native_close_operation(&preparation, task_acquired).await,
        command
    );
    assert_eq!(
        runtime.accounting().expect("last alias charged").used,
        charge
    );
    drop(application);
    finish_validated_ticket(&runtime, &mut ticket, preparation, task_acquired, command).await;
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
// is exposed. Original native witnesses and the preplanned logical ID remain
// retained; no app/response-close DTO or commit-unknown result is invented.
#[tokio::test]
async fn copy_cancelled_close_before_ticket_exposure_retains_same_receipt_and_last_alias_barrier() {
    let root = tempfile::tempdir().expect("actual close ticket handoff fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::TaskArmed).await;
    let output = owner.output();
    let application = owner
        .take_application_lease(1024)
        .expect("original app lease before sealing");
    let gate = runtime.fixture_pause_copy_start(CopyStartFixtureStage::CleanupTicketPrepared);
    let mut cleanup = Box::pin(owner.prepare_retirement(true, deadline()));
    tokio::select! { result=&mut cleanup=>panic!("original cleanup returned before pending receipt gate: {}",result.is_ok()), ()=gate.reached()=>{}, ()=tokio::time::sleep(Duration::from_secs(15))=>panic!("original pending receipt watchdog") }
    drop(cleanup);
    drop(gate);
    let original = retained_native_close_operation(&preparation, false).await;
    assert!(!original.is_nil());
    retire_with_retained_aliases(runtime, owner, preparation, false, output, application).await;
}

// The native ticket is valid while real actor metadata/client allocations
// remain. Neither those top leases nor their last original slot alias may be
// treated as gone after the actual acquired sources, child and output prefix close.
#[tokio::test]
async fn copy_native_ticket_precedes_original_metadata_client_and_last_slot_destruction() {
    let root = tempfile::tempdir().expect("actual copy actor partition fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    let charge = runtime.accounting().expect("actual full R").used;
    let output = owner.output();
    let application = owner
        .take_application_lease(1024)
        .expect("actual app lease");
    let (metadata, client) = output
        .reserve_metadata_partitions(4096, 4096)
        .await
        .expect("same actual metadata/client budget partitions");
    let slot = client.reserve(128).expect("original real client slot");
    let last_slot = Arc::clone(&slot);
    cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::TaskArmed).await;
    let mut ticket = owner
        .prepare_retirement(true, deadline())
        .await
        .expect("native ticket must not require actor allocations to be destroyed");
    let operation = retained_native_close_operation(&preparation, false).await;
    assert!(
        client.reserve(128).is_err(),
        "native close fences new client allocations"
    );
    assert!(
        slot.reserve_related(128).is_err(),
        "same retained slot cannot extend closing budget"
    );
    assert!(
        output
            .allocate_response_buffer(128, deadline())
            .await
            .is_err(),
        "actual closed prefix cannot allocate a new body"
    );
    assert!(ticket
        .finish_retirement(Instant::now() + Duration::from_millis(1))
        .await
        .is_err());
    assert_eq!(
        runtime.accounting().expect("actual aliases retain R").used,
        charge
    );
    drop(owner);
    drop(output);
    drop(application);
    assert_eq!(ticket.fixture_application_released(), Some(true));
    assert!(
        ticket
            .finish_retirement(Instant::now() + Duration::from_millis(1))
            .await
            .is_err(),
        "real metadata/client owners block final preflight"
    );
    let mut borrowed = Box::pin(ticket.finish_retirement(deadline()));
    std::future::poll_fn(|cx| {
        assert!(
            std::future::Future::poll(borrowed.as_mut(), cx).is_pending(),
            "real response owners keep finish pending"
        );
        std::task::Poll::Ready(())
    })
    .await;
    drop(borrowed); // Only the waiter; original ticket/claim/budget remain.
    assert_eq!(
        retained_native_close_operation(&preparation, false).await,
        operation
    );
    assert_eq!(
        runtime
            .accounting()
            .expect("cancelled wait retains full R")
            .used,
        charge
    );
    drop(metadata);
    assert!(ticket
        .finish_retirement(Instant::now() + Duration::from_millis(1))
        .await
        .is_err());
    drop(client);
    drop(slot);
    assert!(
        ticket
            .finish_retirement(Instant::now() + Duration::from_millis(1))
            .await
            .is_err(),
        "last real client-slot alias still retains the original response budget"
    );
    assert_eq!(
        retained_native_close_operation(&preparation, false).await,
        operation
    );
    assert_eq!(
        runtime.accounting().expect("last slot retains full R").used,
        charge
    );
    assert!(
        !runtime.has_failed_copy_admission(),
        "same live ticket retains exclusive custody"
    );
    drop(last_slot);
    finish_validated_ticket(&runtime, &mut ticket, preparation, false, operation).await;
    ticket
        .finish_retirement(deadline())
        .await
        .expect("same completed ticket idempotent");
    assert_eq!(
        runtime.accounting().expect("one matching R refund").used,
        ResourceVector::default()
    );
    drop(ticket);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole original runtime")
        .drain()
        .expect("original journal drained");
}

// A real CopySink constructor refusal occurs after R and the original empty
// CopyLaunch queue, before the atomic work/response-budget installation.
// Its generic error provenance alone cannot prove that cleanup returned R.
#[tokio::test]
async fn copy_constructor_refusal_before_budget_install_closes_same_charged_bundle() {
    let root = tempfile::tempdir().expect("actual empty-copy constructor rollback");
    let (runtime, mut request) =
        NodeResourceRuntime::fixture_actual_copy_request(root.path(), false).await;
    let baseline = runtime
        .accounting()
        .expect("original pre-constructor ledger")
        .used;
    assert_eq!(baseline, ResourceVector::default());
    request.operation_timeout = Duration::from_secs(121); // actual CopySink refusal
    let native_gate =
        runtime.fixture_pause_copy_start(CopyStartFixtureStage::CleanupTicketPrepared);
    let mut prepare = Box::pin(runtime.prepare_copy_vod(request, deadline()));
    tokio::select! {
        result = &mut prepare => panic!("constructor returned before original empty-prefix cleanup: {}", result.is_ok()),
        () = native_gate.reached() => {},
        () = tokio::time::sleep(Duration::from_secs(15)) => panic!("actual constructor native-close watchdog"),
    }
    let charge = runtime
        .accounting()
        .expect("same queued constructor R")
        .used;
    assert_ne!(charge, baseline);
    let mut retained = runtime
        .retained_preparations()
        .expect("original charged book entry");
    assert_eq!(retained.len(), 1);
    let preparation = retained
        .pop()
        .expect("same original constructor preparation");
    drop(retained);
    let (binding, operation, absence_close) = {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role
            .copy_vod
            .as_ref()
            .expect("same original empty CopyLaunch");
        assert!(
            copy.work.is_none() && role.response_budget.is_none(),
            "refusal must precede the actual atomic work/budget installation"
        );
        assert!(!copy.entered && copy.task.is_none() && !role.refunded);
        assert!(role.sources.is_empty() && role.bootstrap.is_empty() && role.roots.is_empty());
        let absence = copy
            .never_root_closure
            .as_ref()
            .expect("original graph absence witness");
        assert!(absence.matches(role.sealed.as_ref().expect("same sealed original graph")));
        (
            role.binding,
            copy.settlement_operation_id,
            absence.close_operation_id(),
        )
    };
    let dto_gate =
        runtime.fixture_pause_copy_start(CopyStartFixtureStage::CleanupSettlementPrepared);
    drop(native_gate);
    tokio::select! {
        result = &mut prepare => panic!("constructor skipped unchanged final DTO preflight: {}", result.is_ok()),
        () = dto_gate.reached() => {},
        () = tokio::time::sleep(Duration::from_secs(15)) => panic!("actual constructor settlement watchdog"),
    }
    {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        assert_eq!(role.binding, binding);
        assert!(!role.refunded && role.copy_vod.is_none() && role.response_budget.is_none());
        let pending = role
            .pending_finite_close
            .as_ref()
            .expect("same original deferred settlement");
        assert_eq!(pending.key.operation_id, operation);
        let Mutation::SettleTyped(closed) = &pending.mutation else {
            panic!("original typed constructor close")
        };
        assert_eq!(closed.settlement.members.len(), 5);
        assert!(closed.settlement.members.iter().any(|member| matches!(member,
            v2::MemberDisposition::Closed { fact: v2::ClosedFact::LegacyNeverAcquiredV1 { operation_id, .. }, .. }
            if operation_id.uuid() == absence_close)), "same native absence witness must back the DTO");
    }
    assert_eq!(
        runtime
            .accounting()
            .expect("real preflight is not ledger release")
            .used,
        charge
    );
    drop(preparation); // actual last inspection alias, before book destruction
    drop(dto_gate);
    let error = prepare
        .await
        .err()
        .expect("original actual CopySink refusal");
    assert!(!error.definitely_before_reservation());
    assert_eq!(
        runtime
            .accounting()
            .expect("one matching original R refund")
            .used,
        baseline
    );
    assert!(!runtime.has_failed_copy_admission());
    assert!(runtime
        .retained_preparations()
        .expect("original book after actual destruction")
        .is_empty());
    Arc::try_unwrap(runtime)
        .ok()
        .expect("no original constructor owner remains")
        .drain()
        .expect("positive original journal drain after rollback");
}

// The actual rejected immutable Scratch limits make the original journal
// prepare definitively RolledBack. Cancellation below happens only AFTER the
// original graph absence witness became the retained physical receipt.
#[tokio::test]
async fn copy_unaccepted_cached_receipt_replays_after_cancelled_finalizer_clears_budget() {
    let root = tempfile::tempdir().expect("actual rejected-copy receipt replay");
    let (runtime, request) =
        NodeResourceRuntime::fixture_actual_copy_request(root.path(), true).await;
    let mut owner = runtime
        .prepare_copy_vod(request, deadline())
        .await
        .expect("original queued rejected copy");
    let preparation = NodeResourceRuntime::fixture_copy_original_preparation(&owner);
    let output = owner.output();
    let charge = runtime
        .accounting()
        .expect("original rejected prepare remains charged")
        .used;
    assert_ne!(charge, ResourceVector::default());
    let mut ticket = owner
        .prepare_retirement(true, deadline())
        .await
        .expect("actual known rollback native ticket");
    let binding = {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        assert!(finite_prepare_definitely_unaccepted(&bundle));
        assert!(role.response_budget.is_some() && role.receipt.is_none());
        let copy = role.copy_vod.as_ref().expect("same retained absent graph");
        assert!(copy
            .never_root_closure
            .as_ref()
            .expect("original absence proof")
            .matches(role.sealed.as_ref().expect("same original sealed graph")));
        role.binding
    };
    drop(owner);
    drop(output);
    let gate = runtime.fixture_pause_copy_start(CopyStartFixtureStage::CleanupSettlementPrepared);
    let mut finish = Box::pin(ticket.finish_retirement(deadline()));
    tokio::select! {
        result = &mut finish => panic!("finalizer skipped original receipt boundary: {result:?}"),
        () = gate.reached() => {},
        () = tokio::time::sleep(Duration::from_secs(15)) => panic!("real unaccepted receipt watchdog"),
    }
    {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        assert_eq!(role.binding, binding);
        assert_eq!(
            role.receipt
                .as_ref()
                .expect("actual retained absence receipt")
                .custody_binding(),
            binding
        );
        assert!(!role.refunded && role.copy_vod.is_none() && role.response_budget.is_none());
        assert!(
            role.sealed.is_none()
                && role.typed_members.is_none()
                && role.pending_finite_close.is_none()
        );
    }
    assert_eq!(
        runtime
            .accounting()
            .expect("receipt alone is not refund")
            .used,
        charge
    );
    drop(finish); // only borrowed waiting; the original receipt stays in role
    drop(gate);
    drop(ticket); // existing claim Drop requests repair of THIS same role
    assert!(runtime.has_failed_copy_admission());
    let replay_gate =
        runtime.fixture_pause_copy_start(CopyStartFixtureStage::CleanupSettlementPrepared);
    let mut repair = Box::pin(runtime.maintain_failed_copy_admissions(deadline()));
    tokio::select! {
        result = &mut repair => panic!("repair failed to replay retained budget-less receipt: {result:?}"),
        () = replay_gate.reached() => {},
        () = tokio::time::sleep(Duration::from_secs(15)) => panic!("same original receipt replay watchdog"),
    }
    {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        assert_eq!(role.binding, binding);
        assert_eq!(
            role.receipt
                .as_ref()
                .expect("same original receipt remains")
                .custody_binding(),
            binding
        );
        assert!(role.copy_vod.is_none() && role.response_budget.is_none() && role.sealed.is_none());
        assert!(!role.refunded && finite_prepare_definitely_unaccepted(&bundle));
    }
    assert_eq!(
        runtime
            .accounting()
            .expect("replay still holds original R")
            .used,
        charge
    );
    drop(preparation);
    drop(replay_gate);
    assert_eq!(
        repair
            .await
            .expect("positive same-receipt maintenance close"),
        1
    );
    assert_eq!(
        runtime
            .accounting()
            .expect("one exact original refund")
            .used,
        ResourceVector::default()
    );
    assert!(!runtime.has_failed_copy_admission());
    assert!(runtime
        .retained_preparations()
        .expect("actual destroyed original book entry")
        .is_empty());
    Arc::try_unwrap(runtime)
        .ok()
        .expect("no original rejected-copy owner remains")
        .drain()
        .expect("positive original journal drain after receipt replay");
}

// C5: scheduling custody never replaces the actual native/journal facts.
async fn retained_copy_close_operation(preparation: &OwnedPreparation) -> Uuid {
    let bundle = preparation.state.lock().await;
    let role = &bundle.roles[0];
    assert!(!role.refunded);
    if let Some(copy) = role.copy_vod.as_ref() {
        assert!(role.pending_finite_close.is_none() && role.transition.is_none());
        assert!(
            copy.never_root_closure.is_some()
                || copy.root_only_closure.is_some()
                || copy.process_closure.is_some()
        );
        return copy.settlement_operation_id;
    }
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
    let operation = retained_native_close_operation(&preparation, false).await;
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
    assert_eq!(
        retained_native_close_operation(&preparation, false).await,
        operation
    );
    assert_eq!(runtime.accounting().expect("same full R").used, charge);
    finish_validated_ticket(&runtime, &mut ticket, preparation, false, operation).await;
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
    let operation = retained_native_close_operation(&preparation, false).await;
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
    finish_validated_maintenance(&runtime, preparation, false, operation).await;
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
    let gate = runtime.fixture_pause_copy_start(CopyStartFixtureStage::CleanupTicketPrepared);
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
    let operation = retained_native_close_operation(&preparation, false).await;
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
    finish_validated_maintenance(&runtime, preparation, false, operation).await;
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
    let operation = retained_native_close_operation(&preparation, true).await;
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
    let dto_gate =
        runtime.fixture_pause_copy_start(CopyStartFixtureStage::CleanupSettlementPrepared);
    let mut finish = Box::pin(ticket.finish_retirement(deadline()));
    tokio::select! {
        result=&mut finish=>panic!("finish skipped original DTO gate: {result:?}"),
        ()=dto_gate.reached()=>{},
        ()=tokio::time::sleep(Duration::from_secs(15))=>panic!("original DTO gate watchdog"),
    }
    assert_eq!(pending_command(&preparation, true).await, operation);
    drop(dto_gate);
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
    finish_existing_ticket_with_aliases(
        runtime,
        owner,
        preparation,
        task_acquired,
        (output, application),
        ticket,
        move |preparation| {
            Box::pin(async move {
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
                                fact:
                                    v2::ClosedFact::VodOutputTaskNeverAcquiredV1 {
                                        operation_id, ..
                                    },
                                ..
                            } if !task_acquired => operation_id.uuid() == close,
                            _ => false,
                        }),
                        "retry replaced the original process/task close operation"
                    );
                }
            })
        },
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
    drop(graph);
    finish_existing_ticket_with_aliases(
        runtime, owner, preparation, false, (output, application), ticket,
        move |preparation| Box::pin(async move {
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
        }),
    ).await;
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
    let mut ticket = owner
        .prepare_retirement(true, deadline())
        .await
        .expect("native ticket precedes application disposal and full DTO preflight");
    assert_eq!(
        retained_native_close_operation(&preparation, true).await,
        settlement
    );
    drop(owner);
    drop(output);
    drop(application);
    assert!(
        ticket.finish_retirement(deadline()).await.is_err(),
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
        !runtime.has_failed_copy_admission(),
        "the same live ticket retains its claim across a borrowed finish refusal"
    );
    assert_eq!(
        runtime.copy_cleanup_holds().expect("actual metadata hold")[0].reason,
        CopyCleanupHoldReason::ValidationRejected
    );

    let gate = runtime.fixture_pause_copy_start(CopyStartFixtureStage::CleanupSettlementPrepared);
    let mut retry = Box::pin(ticket.finish_retirement(deadline()));
    tokio::select! {
        result=&mut retry=>panic!("retry skipped full original preflight gate: {result:?}"),
        ()=gate.reached()=>{},
        ()=tokio::time::sleep(Duration::from_secs(15))=>panic!("original preflight retry watchdog"),
    }
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
    drop(preparation);
    drop(gate);
    retry
        .await
        .expect("same original validated witnesses settle after metadata retry");
    assert_eq!(
        runtime.accounting().expect("one matching refund").used,
        ResourceVector::default()
    );
    drop(ticket);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole original runtime")
        .drain()
        .expect("same original journal drained");
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
    // The real stream needs more than the negative fixture's two fragments
    // before a cut. Reserve the existing production eight-slot shape before R;
    // keep the original 256MiB demand and the immutable five-file window.
    request.max_pending_fragments = 8;
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
    if tokio::time::timeout(Duration::from_secs(30), gate.reached())
        .await
        .is_err()
    {
        // Read only the same completion slot. This never starts, seals, joins,
        // replaces or fabricates a parser or grants any native closure proof.
        let terminal = runtime
            .wait_copy_parser_result(&preparation, 0, Instant::now() + Duration::from_millis(50))
            .await;
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role
            .copy_vod
            .as_ref()
            .expect("original failed window owner");
        let prefix = role
            .bootstrap
            .values()
            .next()
            .and_then(|entry| entry.fold())
            .map(|fold| {
                let state = fold.state();
                (
                    state.bootstrap.phase.clone(),
                    state.asset.lifecycle.clone(),
                    state.asset.totals.clone(),
                )
            });
        panic!("FAILURE watchdog: original sink never reached real full window; actual terminal={terminal:?}, prefix={prefix:?}, phase={:?}, transition={}, closing={}, custody_closing={}",
            role.phase, role.transition.is_some(), copy.closing,
            role.copy_cleanup.as_ref().is_none_or(|custody| custody.closing()));
    }
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
    let operation = retained_native_close_operation(&preparation, true).await;
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
    finish_validated_ticket_with(&runtime, &mut ticket, preparation, true, operation,
        move |preparation| Box::pin(async move {
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
        })).await;
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
    if root_only {
        // The deliberately retained CopyWork still owns its original sink,
        // which owns this runtime. That permanent hold cannot be drained as
        // though the rich payload were gone. Test-process teardown is not a
        // native-close or ledger-refund witness.
        assert!(Arc::strong_count(&runtime) > 1);
        drop(runtime);
    } else {
        Arc::try_unwrap(runtime)
            .ok()
            .expect("sole joined held runtime")
            .drain()
            .expect("durable held journal, no refund");
    }
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

// Native filesystem/journal lifecycle coverage, not media-decoder qualification.
// Both operations enter through the actual runtime APIs and retain their real
// acknowledged owners; the fixture never fabricates readiness or a close fact.
async fn missing_registered_native_io_keeps_original_charge(publication: bool) {
    let root = tempfile::tempdir().expect("actual original native IO custody fixture");
    let (runtime, mut owner, preparation) = original(root.path()).await;
    original_prefix(&runtime, &mut owner, &preparation).await;
    let member = output_member(&preparation).await;
    let charge = runtime.accounting().expect("same original R").used;
    let output = owner.output();
    let application = owner
        .take_application_lease(1024)
        .expect("original lease before closing");
    // A real ftyp box is enough for this native Init publication boundary;
    // no parser or FFmpeg outcome is inferred from its bytes.
    let mut payload = b"\0\0\0\x18ftypisom\0\0\0\0isomiso6".to_vec();
    let publication_id = runtime
        .queue_scratch_publication(
            &preparation,
            0,
            member,
            v2::scratch::OutputKey::Init,
            &mut payload,
        )
        .expect("queue original actual init publication");
    assert!(
        payload.is_empty(),
        "original runtime adopted the same payload"
    );
    runtime
        .publish_scratch_output(&preparation, 0, member, publication_id, deadline())
        .await
        .expect("actual original publication and durable observation");
    let sync_id = runtime
        .queue_scratch_sync(&preparation, 0, member, &[v2::scratch::OutputKey::Init])
        .expect("queue original actual init sync");
    runtime
        .sync_scratch_outputs(&preparation, 0, member, sync_id, deadline())
        .await
        .expect("actual original sync and durable observation");
    let retained_original = {
        let mut bundle = preparation.state.lock().await;
        bundle.roles[0]
            .bootstrap
            .get_mut(&member)
            .expect("same original native prefix")
            .fixture_take_original_native_io(publication)
    };
    let Err(error) = owner.prepare_retirement(true, deadline()).await else {
        panic!("a missing registered original IO owner must refuse closure");
    };
    assert!(!error.definitely_before_reservation());
    assert_eq!(
        runtime.accounting().expect("original full R held").used,
        charge
    );
    {
        let mut bundle = preparation.state.lock().await;
        let role = &mut bundle.roles[0];
        assert!(!role.refunded && role.pending_finite_close.is_none());
        assert!(role.copy_vod.is_some(), "same close selector retained");
        role.bootstrap
            .get_mut(&member)
            .expect("original prefix survives refusal")
            .fixture_restore_original_native_io(retained_original);
    }
    // Existing helper proves pending command stability, both original alias
    // barriers, one refund, rich bundle destruction and original journal drain.
    retire_with_retained_aliases(runtime, owner, preparation, false, output, application).await;
}

#[tokio::test]
async fn copy_missing_registered_publication_owner_holds_r_until_same_owner_restored() {
    missing_registered_native_io_keeps_original_charge(true).await;
}

#[tokio::test]
async fn copy_missing_registered_sync_owner_holds_r_until_same_owner_restored() {
    missing_registered_native_io_keeps_original_charge(false).await;
}

// Each irreversible errno filter runs in its own harness subprocess and on
// its current Tokio thread. These cases require actual kernel syscall errors;
// absence of the expected syscall/errno fails rather than supplying coverage.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod syscall_failures {
    use super::*;
    use crate::adaptive_resources::runtime::copy_vod::{CopyCloseTicket, CopyPipeCommand};
    use crate::adaptive_resources::runtime::finite_vod::SoftwareAvcExecutable;
    use sha2::{Digest, Sha256};

    fn subprocess_root(env: &str, test: &str) -> Option<std::path::PathBuf> {
        if let Some(root) = std::env::var_os(env) {
            return Some(root.into());
        }
        let root = tempfile::tempdir().expect("isolated original root errno fixture");
        let output =
            std::process::Command::new(std::env::current_exe().expect("same test harness"))
                .args(["--exact", test, "--nocapture"])
                .env(env, root.path())
                .output()
                .expect("isolated errno subprocess");
        assert!(
            output.status.success(),
            "original errno fixture failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("original-root-errno-and-alias-closure-verified"),
            "subprocess did not reach actual original wait/alias assertions"
        );
        None
    }

    async fn pausing_root(
        root: &Path,
        enter_parser: bool,
    ) -> (
        Arc<NodeResourceRuntime>,
        CopyVodOwner,
        OwnedPreparation,
        SealedPhysicalGraph,
        GraphRoot,
    ) {
        let (runtime, mut request) =
            NodeResourceRuntime::fixture_actual_copy_request(root, false).await;
        let source = root.join("original-pausing-child.c");
        let binary = root.join("original-pausing-child");
        // This is the original pre-R executable, not replacement work or an
        // output hook. The alarm is only a bounded fixture death backstop;
        // expiration before the asserted alive/error boundary fails the test.
        std::fs::write(
            &source,
            br#"#include <unistd.h>
int main(void) { alarm(20); for (;;) pause(); }
"#,
        )
        .expect("real ELF fixture source");
        let compiler = std::env::var_os("PLURX_TEST_CC").unwrap_or_else(|| "/usr/bin/cc".into());
        let built = std::process::Command::new(compiler)
            .args(["-O0", "-o"])
            .arg(&binary)
            .arg(&source)
            .output()
            .expect("actual C compiler required");
        assert!(
            built.status.success(),
            "real child compilation: {}",
            String::from_utf8_lossy(&built.stderr)
        );
        let binary = std::fs::canonicalize(binary).expect("original ELF path");
        let identity = crate::source_identity::SourceIdentity::from_metadata(
            &std::fs::metadata(&binary).expect("original ELF metadata"),
        );
        let digest = hex::encode(Sha256::digest(
            std::fs::read(&binary).expect("original ELF bytes"),
        ));
        request.command = CopyPipeCommand::compile(
            request.command.source().clone(),
            request.command.video(),
            SoftwareAvcExecutable::new(binary, identity, &digest)
                .expect("original immutable ELF binding"),
            root.to_owned(),
            None,
            false,
            crate::transcode::Pacing::unpaced(),
            None,
        )
        .expect("original fixed recipe selected before R");
        let mut owner = runtime
            .prepare_copy_vod(request, deadline())
            .await
            .expect("same original R");
        let preparation = NodeResourceRuntime::fixture_copy_original_preparation(&owner);
        if enter_parser {
            // Complete original registration and send its one-shot start latch
            // before sealing. No borrowed sender or non-entry adapter is used.
            owner
                .start(deadline())
                .await
                .expect("actual original parser entry");
        } else {
            cancel_actual_start(&runtime, &mut owner, CopyStartFixtureStage::RootRegistered).await;
        }
        let (graph, child) = {
            let mut bundle = preparation.state.lock().await;
            let role = &mut bundle.roles[0];
            let copy = role.copy_vod.as_ref().expect("original copy");
            if enter_parser {
                assert!(copy.task.is_some() && copy.start_released && copy.task_observed_committed);
            } else {
                assert!(copy.task.is_none() && copy.fixed_image.is_some());
            }
            let child = role.roots[&copy.root_member];
            role.sealed = Some(role.graph.take().expect("same original graph").seal());
            (
                role.sealed.as_ref().expect("same graph").select_same(),
                child,
            )
        };
        assert!(graph
            .try_wait_root(child)
            .await
            .expect("actual original alive check")
            .is_none());
        (runtime, owner, preparation, graph, child)
    }

    async fn original_ids(preparation: &OwnedPreparation) -> (Uuid, Uuid, Uuid, Uuid) {
        let bundle = preparation.state.lock().await;
        let copy = bundle.roles[0]
            .copy_vod
            .as_ref()
            .expect("same original identity");
        (
            copy.root_member,
            copy.task_member,
            copy.acquisition_id,
            copy.settlement_operation_id,
        )
    }

    async fn assert_held(preparation: &OwnedPreparation, ids: (Uuid, Uuid, Uuid, Uuid)) {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let copy = role.copy_vod.as_ref().expect("original copy remains owned");
        assert_eq!(
            (
                copy.root_member,
                copy.task_member,
                copy.acquisition_id,
                copy.settlement_operation_id
            ),
            ids
        );
        assert!(!role.refunded && role.pending_finite_close.is_none());
        assert!(
            copy.never_root_closure.is_none()
                && copy.root_only_closure.is_none()
                && copy.process_closure.is_none()
        );
        assert!(copy.fixed_image.is_some() && copy.task.is_none());
    }

    async fn finish_aliases<L>(
        runtime: Arc<NodeResourceRuntime>,
        owner: CopyVodOwner,
        preparation: OwnedPreparation,
        output: CopyOutput,
        application: L,
        mut ticket: CopyCloseTicket,
        expected: (ResourceVector, Uuid),
    ) {
        let (charge, settlement) = expected;
        assert_eq!(
            retained_native_close_operation(&preparation, false).await,
            settlement
        );
        assert_eq!(runtime.accounting().unwrap().used, charge);
        drop(owner);
        assert!(ticket
            .finish_retirement(Instant::now() + Duration::from_millis(1))
            .await
            .is_err());
        assert_eq!(
            retained_native_close_operation(&preparation, false).await,
            settlement
        );
        assert_eq!(runtime.accounting().unwrap().used, charge);
        drop(output);
        assert!(ticket
            .finish_retirement(Instant::now() + Duration::from_millis(1))
            .await
            .is_err());
        assert_eq!(runtime.accounting().unwrap().used, charge);
        drop(application);
        finish_validated_ticket(&runtime, &mut ticket, preparation, false, settlement).await;
        assert_eq!(
            runtime.accounting().unwrap().used,
            ResourceVector::default()
        );
        drop(ticket);
        Arc::try_unwrap(runtime)
            .ok()
            .expect("sole original runtime")
            .drain()
            .expect("actual journal drain");
    }

    fn complete() -> ! {
        use std::io::Write;
        println!("original-root-errno-and-alias-closure-verified");
        std::io::stdout().flush().unwrap();
        // R reached zero only through actual wait/native close/journal/alias
        // finalization above. Teardown of this filtered harness is no witness.
        std::process::exit(0)
    }

    #[test]
    fn copy_real_original_kill_eperm_waits_same_child_and_preserves_alias_charge() {
        const ENV: &str = "PLURX_COPY_KILL_ERRNO_CHILD_ROOT";
        const TEST: &str = "adaptive_resources::runtime::operations::copy_vod::runtime_tests::syscall_failures::copy_real_original_kill_eperm_waits_same_child_and_preserves_alias_charge";
        let Some(root) = subprocess_root(ENV, TEST) else {
            return;
        };
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
            let (runtime, mut owner, preparation, graph, child) = pausing_root(&root, false).await;
            let charge = runtime.accounting().unwrap().used;
            let ids = original_ids(&preparation).await;
            let output = owner.output();
            let application = owner.take_application_lease(1024).expect("original pre-close app alias");
            let control = child.registered_control().expect("same original registered pidfd control");
            graph.fixture_deny_original_root_kill(child).await.expect("actual owned kill filter");
            let error = graph.start_kill_root(child).await.expect_err("actual original kill must reach EPERM");
            assert_eq!(error.raw_os_error(), Some(libc::EPERM));
            assert!(graph.try_wait_root(child).await.expect("same alive child after EPERM").is_none());
            assert_held(&preparation, ids).await;
            assert_eq!(runtime.accounting().unwrap().used, charge);
            let mut close = Box::pin(owner.prepare_retirement(true, deadline()));
            let hold = async {
                loop {
                    {
                        let bundle = preparation.state.lock().await;
                        if bundle.roles[0].copy_vod.as_ref().is_some_and(|copy|
                            copy.retryable_hold == Some(CopyCleanupHoldReason::FailedParserKill)) {
                            break;
                        }
                    }
                    tokio::task::yield_now().await;
                }
            };
            tokio::select! {
                result = &mut close => panic!("original cleanup completed before actual wait recovery: {}", result.is_ok()),
                result = tokio::time::timeout(Duration::from_secs(5), hold) => result.expect("real EPERM hold must be retained while original wait is pending"),
            }
            assert_held(&preparation, ids).await;
            assert_eq!(runtime.accounting().unwrap().used, charge);
            // Cancel only this borrowed waiter. Its original Child, claim
            // repair selection and all R/native owners stay in runtime custody.
            drop(close);
            // This SAME registered control uses its original pidfd syscall,
            // which the kill(pid) filter intentionally does not intercept.
            control.start_kill().expect("genuine original pidfd SIGKILL recovery");
            let waited = tokio::time::timeout(Duration::from_secs(10), graph.wait_root(child)).await
                .expect("same original Child wait must finish").expect("actual original wait after EPERM");
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(waited.signal(), Some(libc::SIGKILL), "original pidfd recovery, not fixture alarm");
            assert_eq!(graph.snapshot().await.waited_roots, 1);
            assert_held(&preparation, ids).await;
            assert_eq!(runtime.accounting().unwrap().used, charge, "actual wait is not refund");
            let ticket = owner.prepare_retirement(true, deadline()).await
                .expect("same original owner resumes actual closure after EPERM");
            assert_eq!(graph.snapshot().await.roots, 0);
            drop(graph);
            finish_aliases(runtime, owner, preparation, output, application, ticket, (charge, ids.3)).await;
            complete();
        });
    }

    #[test]
    fn copy_real_original_wait_errno_retains_charge_until_same_child_wait_recovers() {
        const ENV: &str = "PLURX_COPY_WAIT_ERRNO_CHILD_ROOT";
        const TEST: &str = "adaptive_resources::runtime::operations::copy_vod::runtime_tests::syscall_failures::copy_real_original_wait_errno_retains_charge_until_same_child_wait_recovers";
        let Some(root) = subprocess_root(ENV, TEST) else {
            return;
        };
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let (runtime, mut owner, preparation, graph, child) =
                    pausing_root(&root, false).await;
                let charge = runtime.accounting().unwrap().used;
                let ids = original_ids(&preparation).await;
                let output = owner.output();
                let application = owner
                    .take_application_lease(1024)
                    .expect("original pre-close app alias");
                // Created BEFORE the thread-local filter. This waiter holds only a
                // selector for SAME GraphCustody and polls graph.wait_root, which
                // locks the original Child. No waitpid/reap occurs behind its owner.
                let same_graph = graph.select_same();
                let handle = tokio::runtime::Handle::current();
                let (recover, recovery) = std::sync::mpsc::channel();
                let recovery_thread = std::thread::spawn(move || {
                    recovery
                        .recv_timeout(Duration::from_secs(15))
                        .expect("bounded original-wait recovery request");
                    handle.block_on(same_graph.wait_root(child))
                });
                graph
                    .fixture_deny_original_root_wait(child)
                    .await
                    .expect("actual owned wait filter");
                let Err(error) = tokio::time::timeout(
                    Duration::from_secs(5),
                    owner.prepare_retirement(true, deadline()),
                )
                .await
                .expect("actual original wait syscall must fail promptly") else {
                    panic!("failed original wait must not mint a close ticket");
                };
                assert_eq!(
                    error.into_io_error().raw_os_error(),
                    Some(libc::ECHILD),
                    "actual wait4/waitid errno required"
                );
                assert_held(&preparation, ids).await;
                assert_eq!(runtime.accounting().unwrap().used, charge);
                let snapshot = graph.snapshot().await;
                assert_eq!((snapshot.roots, snapshot.waited_roots), (1, 0));
                // The first actual cleanup already requested SIGKILL. The original
                // graph and Child remain owned; only the polling thread changes.
                recover.send(()).expect("resume actual original graph wait");
                let waited = tokio::time::timeout(
                    Duration::from_secs(10),
                    tokio::task::spawn_blocking(move || recovery_thread.join()),
                )
                .await
                .expect("original wait recovery watchdog")
                .expect("fixture join worker")
                .expect("original waiter thread")
                .expect("genuine same-Child wait");
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(
                    waited.signal(),
                    Some(libc::SIGKILL),
                    "actual owned cleanup SIGKILL, not fixture alarm"
                );
                assert_eq!(graph.snapshot().await.waited_roots, 1);
                assert_held(&preparation, ids).await;
                assert_eq!(
                    runtime.accounting().unwrap().used,
                    charge,
                    "actual wait alone is not refund"
                );
                let ticket = owner
                    .prepare_retirement(true, deadline())
                    .await
                    .expect("same original cached wait and native closure");
                drop(graph);
                finish_aliases(
                    runtime,
                    owner,
                    preparation,
                    output,
                    application,
                    ticket,
                    (charge, ids.3),
                )
                .await;
                complete();
            });
    }

    // R19: original ENTERED parser JoinHandle is genuinely pending on stdout
    // with no bytes/EOF. Cancellation leaves that handle in original custody.
    #[tokio::test]
    async fn copy_entered_parser_join_cancellation_preserves_same_task_until_original_eof() {
        use crate::adaptive_resources::physical_graph::copy_vod::CopyStartDisposition;
        use std::os::unix::process::ExitStatusExt;
        let root = tempfile::tempdir().expect("actual entered-join cancellation fixture");
        let (runtime, owner, preparation, graph, child) = pausing_root(root.path(), true).await;
        let charge = runtime.accounting().expect("same original full R").used;
        let ids = original_ids(&preparation).await;
        let output = owner.output();
        let application = owner
            .take_application_lease(1024)
            .expect("original app alias before cleanup");
        let (task, observation) = {
            let bundle = preparation.state.lock().await;
            let copy = bundle.roles[0]
                .copy_vod
                .as_ref()
                .expect("same original registered parser");
            assert!(copy.start_released && copy.task_observed_committed);
            (
                copy.task.expect("same original task"),
                copy.task_observation,
            )
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            while !graph.fixture_copy_task_entered(task).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("original task must receive its actual start latch before join cancellation");
        let mut join = Box::pin(graph.join_copy_vod(task));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut join)
                .await
                .is_err(),
            "actual entered parser join must remain pending while original child owns open stdout"
        );
        drop(join); // ONLY the borrowed waiter; no abort, reset or replacement.
        let snapshot = graph.snapshot().await;
        assert_eq!(
            (
                snapshot.roots,
                snapshot.waited_roots,
                snapshot.writers,
                snapshot.joined_writers,
                snapshot.abandoned_writers
            ),
            (1, 0, 1, 0, 0)
        );
        assert!(!graph.fixture_copy_task_finished(task).await);
        assert_eq!(
            graph.fixture_copy_disposition(task).await,
            Some(CopyStartDisposition::TakenBeforeSeal)
        );
        assert!(graph
            .try_wait_root(child)
            .await
            .expect("same original live child")
            .is_none());
        {
            let bundle = preparation.state.lock().await;
            let role = &bundle.roles[0];
            let copy = role
                .copy_vod
                .as_ref()
                .expect("same original copy survives cancelled join");
            assert_eq!(copy.task, Some(task));
            assert_eq!(copy.task_observation, observation);
            assert_eq!(role.roots[&copy.root_member], child);
            assert!(copy.terminal.is_none() && copy.process_closure.is_none());
            assert!(copy.permanent_hold.is_none() && role.pending_finite_close.is_none());
            assert_eq!(
                (
                    copy.root_member,
                    copy.task_member,
                    copy.acquisition_id,
                    copy.settlement_operation_id
                ),
                ids
            );
        }
        assert_eq!(runtime.accounting().unwrap().used, charge);
        graph
            .start_kill_root(child)
            .await
            .expect("actual same-root SIGKILL");
        let waited = tokio::time::timeout(Duration::from_secs(10), graph.wait_root(child))
            .await
            .expect("original wait watchdog")
            .expect("actual same Child wait");
        assert_eq!(
            waited.signal(),
            Some(libc::SIGKILL),
            "fixture alarm is no substitute"
        );
        let terminal = tokio::time::timeout(Duration::from_secs(10), graph.join_copy_vod(task))
            .await
            .expect("same original JoinHandle watchdog")
            .expect("original entered task joins after real EOF");
        assert!(matches!(terminal, CopyTaskTerminal::Entered(summary) if !summary.completed),
            "empty real stream must report actual entered parser failure, not NeverEntered: {terminal:?}");
        let snapshot = graph.snapshot().await;
        assert_eq!(
            (
                snapshot.waited_roots,
                snapshot.joined_writers,
                snapshot.abandoned_writers
            ),
            (1, 1, 0)
        );
        assert_eq!(
            graph.fixture_copy_disposition(task).await,
            Some(CopyStartDisposition::TakenBeforeSeal)
        );
        assert_eq!(graph.fixture_copy_pipe_close_calls(task).await, [1, 1]);
        assert_eq!(
            runtime.accounting().unwrap().used,
            charge,
            "real wait/join alone cannot refund R"
        );
        {
            let bundle = preparation.state.lock().await;
            let copy = bundle.roles[0]
                .copy_vod
                .as_ref()
                .expect("same original task for runtime cleanup");
            assert_eq!(copy.task, Some(task));
            assert_eq!(copy.task_observation, observation);
            assert_eq!(
                (
                    copy.root_member,
                    copy.task_member,
                    copy.acquisition_id,
                    copy.settlement_operation_id
                ),
                ids
            );
        }
        drop(graph);
        // Normal owner cleanup reuses the actual original join and root wait,
        // then emits acquired-task owner facts and waits both original aliases.
        retire_with_retained_aliases(runtime, owner, preparation, true, output, application).await;
    }

    #[tokio::test]
    async fn copy_entered_source_close_worker_cancellation_keeps_original_native_result_and_r() {
        use super::super::super::native_result_fixture::{Gate, Kind};
        let root = tempfile::tempdir().expect("actual source-close result fixture");
        let (runtime, mut owner, preparation) = original(root.path()).await;
        original_prefix(&runtime, &mut owner, &preparation).await;
        let charge = runtime.accounting().unwrap().used;
        let ids = original_ids(&preparation).await;
        let output = owner.output();
        let application = owner
            .take_application_lease(1024)
            .expect("original pre-close app alias");
        let (member, operation) = {
            let bundle = preparation.state.lock().await;
            let role = &bundle.roles[0];
            let member = role.copy_vod.as_ref().unwrap().source_members[0];
            (member, role.sources[&member].parent_close_operation_id())
        };
        let gate = Gate::install(Kind::SourceParentClose, operation);
        let mut close = Box::pin(owner.prepare_retirement(true, deadline()));
        let job = tokio::select! {
            result = &mut close => panic!("original cleanup returned before post-native source result: {}", result.is_ok()),
            result = tokio::time::timeout(Duration::from_secs(10), gate.reached()) => result.expect("original close syscall/result watchdog"),
        };
        drop(close); // only the borrowed cleanup; real result stays in SAME worker.
        {
            let bundle = preparation.state.lock().await;
            let role = &bundle.roles[0];
            let source = &role.sources[&member];
            assert_eq!(source.fixture_parent_close_job(), Some(job));
            assert_eq!(source.parent_close_operation_id(), operation);
            assert!(
                !source.parent_closed(),
                "native success is not delivered until original join"
            );
            assert!(role.pending_finite_close.is_none() && !role.refunded);
        }
        assert_eq!(runtime.accounting().unwrap().used, charge);
        drop(gate);
        {
            let bundle = preparation.state.lock().await;
            assert_eq!(
                bundle.roles[0].sources[&member].fixture_parent_close_job(),
                Some(job)
            );
        }
        let ticket = owner
            .prepare_retirement(true, deadline())
            .await
            .expect("same original source worker resumes");
        assert_eq!(runtime.accounting().unwrap().used, charge);
        assert_eq!(original_ids(&preparation).await.3, ids.3);
        finish_existing_ticket_with_aliases(
            runtime, owner, preparation, false, (output, application), ticket,
            move |preparation| Box::pin(async move {
        {
            let bundle = preparation.state.lock().await;
            let Mutation::SettleTyped(closed) = &bundle.roles[0]
                .pending_finite_close
                .as_ref()
                .unwrap()
                .mutation
            else {
                panic!("same original typed close");
            };
            assert!(closed.settlement.members.iter().any(|member_fact| matches!(member_fact,
                v2::MemberDisposition::Closed { member_id, fact: v2::ClosedFact::SourceV1 { copies, .. } }
                if member_id.uuid() == member && copies.iter().any(|copy| matches!(copy,
                    v2::CopyClosed::ActualOwnedClose { operation_id, .. } if operation_id.uuid() == operation)))));
        }
            }),
        ).await;
    }

    #[tokio::test]
    async fn copy_entered_bootstrap_root_worker_cancellation_keeps_original_fd_result_and_r() {
        use super::super::super::native_result_fixture::{Gate, Kind};
        let root = tempfile::tempdir().expect("actual root-open result fixture");
        let (runtime, mut owner, preparation) = original(root.path()).await;
        owner
            .prepare_inputs(deadline())
            .await
            .expect("actual original sources");
        let member = output_member(&preparation).await;
        runtime
            .arm_scratch_bootstrap(&preparation, 0, member, deadline())
            .await
            .expect("same original root arm");
        let charge = runtime.accounting().unwrap().used;
        let ids = original_ids(&preparation).await;
        let output = owner.output();
        let application = owner
            .take_application_lease(1024)
            .expect("original pre-close app alias");
        let operation = preparation.state.lock().await.roles[0].bootstrap[&member]
            .fixture_root_arm_operation()
            .expect("same original root IO id");
        let gate = Gate::install(Kind::BootstrapRoot, operation);
        let mut open =
            Box::pin(runtime.open_scratch_bootstrap_root(&preparation, 0, member, deadline()));
        let job = tokio::select! {
            result = &mut open => panic!("original open returned before post-native root result: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(10), gate.reached()) => result.expect("original open syscall/result watchdog"),
        };
        drop(open); // result with its ACTUAL File stays in original blocking worker.
        let identity = {
            let bundle = preparation.state.lock().await;
            let role = &bundle.roles[0];
            let prefix = &role.bootstrap[&member];
            let identity = prefix
                .fixture_root_job_identity()
                .expect("same original native root cell");
            assert_eq!(identity.0, operation);
            assert_eq!(identity.3, Some(job));
            assert_eq!(prefix.fold().unwrap().state().asset.totals.pending_jobs, 1);
            assert!(role.pending_finite_close.is_none() && !role.refunded);
            identity
        };
        assert_eq!(runtime.accounting().unwrap().used, charge);
        drop(gate);
        {
            let bundle = preparation.state.lock().await;
            assert_eq!(
                bundle.roles[0].bootstrap[&member].fixture_root_job_identity(),
                Some(identity)
            );
        }
        runtime
            .open_scratch_bootstrap_root(&preparation, 0, member, deadline())
            .await
            .expect("join SAME original root result and observation");
        {
            let bundle = preparation.state.lock().await;
            let prefix = &bundle.roles[0].bootstrap[&member];
            let joined = prefix.fixture_root_job_identity().unwrap();
            assert_eq!(
                (joined.0, joined.1, joined.2),
                (identity.0, identity.1, identity.2)
            );
            assert!(joined.3.is_none());
            let state = prefix.fold().unwrap().state();
            assert_eq!(state.asset.totals.pending_jobs, 0);
            let actual = state
                .bootstrap
                .current
                .as_ref()
                .expect("actual acquired namespace prefix");
            assert_eq!(actual.observed.acquisition_io_id.uuid(), operation);
        }
        assert_eq!(
            runtime.accounting().unwrap().used,
            charge,
            "original native open/join is no refund"
        );
        let ticket = owner
            .prepare_retirement(true, deadline())
            .await
            .expect("actual same acquired prefix retirement");
        assert_eq!(runtime.accounting().unwrap().used, charge);
        assert_eq!(original_ids(&preparation).await.3, ids.3);
        finish_existing_ticket_with_aliases(
            runtime, owner, preparation, false, (output, application), ticket,
            move |preparation| Box::pin(async move {
        {
            let bundle = preparation.state.lock().await;
            let Mutation::SettleTyped(closed) = &bundle.roles[0]
                .pending_finite_close
                .as_ref()
                .unwrap()
                .mutation
            else {
                panic!("same original typed close");
            };
            assert!(closed.settlement.members.iter().any(|member_fact| matches!(member_fact,
                v2::MemberDisposition::Closed { member_id, fact: v2::ClosedFact::ScratchAssetsV3 { .. } } if member_id.uuid() == member)),
                "actual acquired prefix must close at exact head, never become NeverAcquired");
        }
            }),
        ).await;
    }

    // R19 native-result delivery boundaries. These gates retain real positive
    // native results in their original registered workers. No gate supplies a
    // result, acknowledgment, replacement task, descriptor or close witness.
    type NativeIoIdentity = (Uuid, Uuid, Uuid, Option<tokio::task::Id>, bool);

    async fn pending_native_io(
        preparation: &OwnedPreparation,
        member: Uuid,
        kind: super::super::super::native_result_fixture::Kind,
    ) -> NativeIoIdentity {
        use super::super::super::native_result_fixture::Kind;
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let prefix = &role.bootstrap[&member];
        assert!(!role.refunded && role.pending_finite_close.is_none());
        assert_eq!(prefix.fold().unwrap().state().asset.totals.pending_jobs, 1);
        let identity = match kind {
            Kind::Publication => prefix.fixture_publication_job_identity(),
            Kind::Sync => prefix.fixture_sync_job_identity(),
            Kind::CleanupRemoveFile
            | Kind::CleanupSyncChild
            | Kind::CleanupRemoveChild
            | Kind::CleanupSyncParent
            | Kind::CleanupCloseHandles => prefix.fixture_cleanup_job_identity(),
            _ => panic!("unexpected fixture owner"),
        }
        .expect("same original registered IO owner");
        assert!(identity.3.is_some() && !identity.4);
        identity
    }

    async fn actual_publication(
        runtime: &NodeResourceRuntime,
        preparation: &OwnedPreparation,
        member: Uuid,
        output: v2::scratch::OutputKey,
    ) -> Uuid {
        // Actual bytes are written/renamed/closed by PublicationIo. This native
        // custody fixture makes no decoder/parser claim about the media unit.
        let mut payload = b"\0\0\0\x18ftypisom\0\0\0\0isomiso6".to_vec();
        let operation = runtime
            .queue_scratch_publication(preparation, 0, member, output, &mut payload)
            .expect("same original payload adopted before native entry");
        assert!(payload.is_empty());
        runtime
            .publish_scratch_output(preparation, 0, member, operation, deadline())
            .await
            .expect("actual publication and durable original Observe");
        operation
    }

    async fn entered_bootstrap_step_cancellation(child: bool) {
        use super::super::super::native_result_fixture::{Gate, Kind};
        let root = tempfile::tempdir().expect("actual entered bootstrap step fixture");
        let (runtime, mut owner, preparation) = original(root.path()).await;
        owner
            .prepare_inputs(deadline())
            .await
            .expect("same original sources");
        let member = output_member(&preparation).await;
        runtime
            .arm_scratch_bootstrap(&preparation, 0, member, deadline())
            .await
            .unwrap();
        runtime
            .open_scratch_bootstrap_root(&preparation, 0, member, deadline())
            .await
            .expect("actual namespace root before the tested step");
        let charge = runtime.accounting().unwrap().used;
        let ids = original_ids(&preparation).await;
        let output = owner.output();
        let application = owner
            .take_application_lease(1024)
            .expect("original app alias");
        let kind = if child {
            Kind::BootstrapChild
        } else {
            Kind::BootstrapTraversal
        };
        let gate = Gate::install_next(kind, member);
        let mut open =
            Box::pin(runtime.open_scratch_bootstrap_sink(&preparation, 0, member, deadline()));
        let job = tokio::select! {
            result = &mut open => panic!("sink returned before actual entered step result: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(10), gate.reached()) =>
                result.expect("real step result watchdog"),
        };
        drop(open);
        let identity = {
            let bundle = preparation.state.lock().await;
            let prefix = &bundle.roles[0].bootstrap[&member];
            let identity = prefix.fixture_root_job_identity().unwrap();
            assert_eq!((identity.0, identity.3), (gate.identity().0, Some(job)));
            assert_eq!(prefix.fold().unwrap().state().asset.totals.pending_jobs, 1);
            assert_eq!(
                prefix.fold().unwrap().state().bootstrap.phase,
                if child {
                    v2::scratch::bootstrap::BootstrapPhase::ChildPossible
                } else {
                    v2::scratch::bootstrap::BootstrapPhase::Traversing
                }
            );
            identity
        };
        assert_eq!(runtime.accounting().unwrap().used, charge);
        assert_eq!(original_ids(&preparation).await, ids);
        // Cleanup must join the same pending step BEFORE applying the strict
        // prefix classifier. No original start/open waiter will be resumed.
        let mut cleanup = Box::pin(owner.prepare_retirement(true, deadline()));
        match tokio::time::timeout(Duration::from_millis(100), &mut cleanup).await {
            Err(_) => {}
            Ok(Err(error)) => panic!("copy cleanup refused its original pending step: {error:?}"),
            Ok(Ok(_)) => panic!("copy cleanup returned before the original step result"),
        }
        drop(cleanup);
        {
            let bundle = preparation.state.lock().await;
            let role = &bundle.roles[0];
            assert_eq!(
                role.bootstrap[&member].fixture_root_job_identity(),
                Some(identity)
            );
            assert!(role.pending_finite_close.is_none() && !role.refunded);
            assert!(
                role.copy_vod.as_ref().unwrap().never_root_closure.is_none(),
                "pending prefix is reconciled before the no-process-root proof/classifier"
            );
        }
        assert_eq!(runtime.accounting().unwrap().used, charge);
        // After release, ordinary cleanup ALONE must Observe this same step.
        // Stop at an actual later cleanup result so its folded acquired IO and
        // original RootCell identity remain inspectable before destruction.
        let close_kind = if child {
            Kind::CleanupRemoveChild
        } else {
            Kind::CleanupCloseHandles
        };
        let close_gate = Gate::install_next(close_kind, member);
        drop(gate);
        let mut cleanup = Box::pin(owner.prepare_retirement(true, deadline()));
        let job = tokio::select! {
            result = &mut cleanup => panic!("ordinary cleanup failed before actual acquired-prefix close: {}", result.is_ok()),
            result = tokio::time::timeout(Duration::from_secs(10), close_gate.reached()) =>
                result.expect("actual acquired-prefix cleanup watchdog"),
        };
        drop(cleanup);
        {
            let bundle = preparation.state.lock().await;
            let role = &bundle.roles[0];
            let prefix = &role.bootstrap[&member];
            let joined = prefix.fixture_root_job_identity().unwrap();
            assert_eq!(
                (joined.0, joined.1, joined.2),
                (identity.0, identity.1, identity.2)
            );
            assert!(joined.3.is_none());
            let state = prefix.fold().unwrap().state();
            let acquired = if child {
                state
                    .asset
                    .directory
                    .as_ref()
                    .expect("same actually created child")
            } else {
                &state
                    .bootstrap
                    .current
                    .as_ref()
                    .expect("same traversed prefix")
                    .observed
            };
            assert_eq!(
                acquired.acquisition_io_id.uuid(),
                identity.0,
                "ordinary cleanup Observed the original step, not a replacement open/mkdir"
            );
            assert!(role.sources.values().all(|source| source.parent_closed()));
            assert!(role.copy_vod.as_ref().unwrap().never_root_closure.is_some());
        }
        let close = pending_native_io(&preparation, member, close_kind).await;
        assert_eq!((close.0, close.3), (close_gate.identity().0, Some(job)));
        assert_ne!(close.0, identity.0);
        assert_eq!(runtime.accounting().unwrap().used, charge);
        drop(close_gate);
        assert_eq!(
            pending_native_io(&preparation, member, close_kind).await,
            close
        );
        let ticket = owner
            .prepare_retirement(true, deadline())
            .await
            .expect("same original step Observe and acquired-prefix native closure");
        finish_aliases(
            runtime,
            owner,
            preparation,
            output,
            application,
            ticket,
            (charge, ids.3),
        )
        .await;
    }

    #[tokio::test]
    async fn copy_entered_traversal_and_partial_close_cancellation_keep_same_original_jobs() {
        entered_bootstrap_step_cancellation(false).await;
    }

    #[tokio::test]
    async fn copy_entered_mkdir_cancellation_observes_same_created_child_and_closes_originals() {
        entered_bootstrap_step_cancellation(true).await;
    }

    async fn entered_publication_or_sync_cancellation(sync: bool) {
        use super::super::super::native_result_fixture::{Gate, Kind};
        let root = tempfile::tempdir().expect("actual entered native output fixture");
        // The original pre-R ELF actually enters as the owned root. Root-only
        // cleanup waits that same Child before joining the retained IO job;
        // never-root cleanup would only refuse the pending asset job.
        let (runtime, mut owner, preparation, graph, _) = pausing_root(root.path(), false).await;
        let member = output_member(&preparation).await;
        let charge = runtime.accounting().unwrap().used;
        let ids = original_ids(&preparation).await;
        let output = owner.output();
        let application = owner
            .take_application_lease(1024)
            .expect("original app alias");
        let operation = if sync {
            actual_publication(&runtime, &preparation, member, v2::scratch::OutputKey::Init).await;
            runtime
                .queue_scratch_sync(&preparation, 0, member, &[v2::scratch::OutputKey::Init])
                .expect("same original file sync request")
        } else {
            let mut payload = b"\0\0\0\x18ftypisom\0\0\0\0isomiso6".to_vec();
            let operation = runtime
                .queue_scratch_publication(
                    &preparation,
                    0,
                    member,
                    v2::scratch::OutputKey::Init,
                    &mut payload,
                )
                .expect("same original Init payload");
            assert!(payload.is_empty());
            operation
        };
        let kind = if sync { Kind::Sync } else { Kind::Publication };
        let gate = Gate::install(kind, operation);
        // Both branches await the real original worker, then its ordinary
        // Observe. The post-native gate stops delivery, not native execution.
        let mut work = Box::pin(async {
            if sync {
                runtime
                    .sync_scratch_outputs(&preparation, 0, member, operation, deadline())
                    .await
            } else {
                runtime
                    .publish_scratch_output(&preparation, 0, member, operation, deadline())
                    .await
            }
        });
        let job = tokio::select! {
            result = &mut work => panic!("IO returned before actual native result: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(10), gate.reached()) =>
                result.expect("actual output IO result watchdog"),
        };
        drop(work);
        let identity = pending_native_io(&preparation, member, kind).await;
        assert_eq!((identity.0, identity.3), (operation, Some(job)));
        let mut cleanup = Box::pin(owner.prepare_retirement(true, deadline()));
        let waited = async {
            loop {
                if graph.snapshot().await.waited_roots == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        tokio::select! {
            result = &mut cleanup => panic!("cleanup returned before original retained IO join: {}", result.is_ok()),
            result = tokio::time::timeout(Duration::from_secs(10), waited) =>
                result.expect("same original Child wait watchdog"),
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut cleanup)
                .await
                .is_err()
        );
        drop(cleanup);
        assert_eq!(
            graph.snapshot().await.waited_roots,
            1,
            "cleanup reached the entered native-job join after the SAME Child wait"
        );
        assert_eq!(
            pending_native_io(&preparation, member, kind).await,
            identity
        );
        assert_eq!(original_ids(&preparation).await, ids);
        assert_eq!(runtime.accounting().unwrap().used, charge);
        drop(gate);
        assert_eq!(
            pending_native_io(&preparation, member, kind).await,
            identity
        );
        // Joining alone is NOT durable Observe or a returned directory. Replay
        // the SAME original runtime API; it must not launch another worker.
        if sync {
            runtime
                .sync_scratch_outputs(&preparation, 0, member, operation, deadline())
                .await
                .expect("same original sync result/Observe/child FD return");
        } else {
            runtime
                .publish_scratch_output(&preparation, 0, member, operation, deadline())
                .await
                .expect("same original publication result/Observe/child FD return");
        }
        {
            let bundle = preparation.state.lock().await;
            let prefix = &bundle.roles[0].bootstrap[&member];
            let joined = if sync {
                prefix.fixture_sync_job_identity()
            } else {
                prefix.fixture_publication_job_identity()
            }
            .unwrap();
            assert_eq!(
                (joined.0, joined.1, joined.2),
                (identity.0, identity.1, identity.2)
            );
            assert!(joined.3.is_none() && joined.4);
            let state = prefix.fold().unwrap().state();
            assert_eq!(state.asset.totals.pending_jobs, 0);
            assert_eq!(
                state.asset.head.as_ref().unwrap().operation_id.uuid(),
                identity.1,
                "durable Observe uses the original named operation"
            );
        }
        assert_eq!(runtime.accounting().unwrap().used, charge);
        drop(graph);
        let ticket = owner
            .prepare_retirement(true, deadline())
            .await
            .expect("normal original root-only/native-output cleanup after acknowledged IO");
        finish_aliases(
            runtime,
            owner,
            preparation,
            output,
            application,
            ticket,
            (charge, ids.3),
        )
        .await;
    }

    #[tokio::test]
    async fn copy_entered_publication_cleanup_join_cancellation_replays_same_observe_and_fd() {
        entered_publication_or_sync_cancellation(false).await;
    }

    #[tokio::test]
    async fn copy_entered_sync_cleanup_join_cancellation_replays_same_observe_and_fd() {
        entered_publication_or_sync_cancellation(true).await;
    }

    #[tokio::test]
    async fn copy_entered_retirement_action_results_survive_each_cancelled_original_join() {
        use super::super::super::native_result_fixture::{Gate, Kind};
        let root = tempfile::tempdir().expect("actual retirement action sweep");
        let (runtime, mut owner, preparation) = original(root.path()).await;
        original_prefix(&runtime, &mut owner, &preparation).await;
        let member = output_member(&preparation).await;
        actual_publication(&runtime, &preparation, member, v2::scratch::OutputKey::Init).await;
        let sync = runtime
            .queue_scratch_sync(&preparation, 0, member, &[v2::scratch::OutputKey::Init])
            .unwrap();
        runtime
            .sync_scratch_outputs(&preparation, 0, member, sync, deadline())
            .await
            .unwrap();
        let charge = runtime.accounting().unwrap().used;
        let ids = original_ids(&preparation).await;
        let output = owner.output();
        let application = owner
            .take_application_lease(1024)
            .expect("original app alias");
        let gates = [
            Kind::CleanupRemoveFile,
            Kind::CleanupSyncChild,
            Kind::CleanupRemoveChild,
            Kind::CleanupSyncParent,
            Kind::CleanupCloseHandles,
        ]
        .map(|kind| (kind, Gate::install_next(kind, member)));
        let mut seen = Vec::<NativeIoIdentity>::new();
        for (kind, gate) in gates {
            let mut cleanup = Box::pin(owner.prepare_retirement(true, deadline()));
            let job = tokio::select! {
                result = &mut cleanup => panic!("retirement returned before actual {kind:?} result: {}", result.is_ok()),
                result = tokio::time::timeout(Duration::from_secs(10), gate.reached()) =>
                    result.expect("actual retirement native-result watchdog"),
            };
            drop(cleanup);
            let identity = pending_native_io(&preparation, member, kind).await;
            assert_eq!((identity.0, identity.3), (gate.identity().0, Some(job)));
            assert!(
                seen.iter()
                    .all(|prior| prior.0 != identity.0 && prior.3 != identity.3),
                "each distinct native action has its own original named IO and job"
            );
            assert_eq!(runtime.accounting().unwrap().used, charge);
            assert_eq!(original_ids(&preparation).await, ids);
            drop(gate);
            assert_eq!(
                pending_native_io(&preparation, member, kind).await,
                identity,
                "releasing delivery cannot replace/Observe the original worker by itself"
            );
            seen.push(identity);
        }
        assert_eq!(seen.len(), 5);
        let ticket = owner
            .prepare_retirement(true, deadline())
            .await
            .expect("final original checked-close Observe and exact-head native proof");
        finish_aliases(
            runtime,
            owner,
            preparation,
            output,
            application,
            ticket,
            (charge, ids.3),
        )
        .await;
    }

    #[tokio::test]
    async fn copy_entered_live_removal_join_cancellation_replays_same_original_unlink_and_observe()
    {
        use super::super::super::native_result_fixture::{Gate, Kind};
        let root = tempfile::tempdir().expect("actual live prefix unlink result");
        let (runtime, mut owner, preparation) = original(root.path()).await;
        original_prefix(&runtime, &mut owner, &preparation).await;
        let member = output_member(&preparation).await;
        actual_publication(
            &runtime,
            &preparation,
            member,
            v2::scratch::OutputKey::Segment { index: 0 },
        )
        .await;
        let sync = runtime
            .queue_scratch_sync(
                &preparation,
                0,
                member,
                &[v2::scratch::OutputKey::Segment { index: 0 }],
            )
            .unwrap();
        runtime
            .sync_scratch_outputs(&preparation, 0, member, sync, deadline())
            .await
            .unwrap();
        let charge = runtime.accounting().unwrap().used;
        let ids = original_ids(&preparation).await;
        let output = owner.output();
        let application = owner
            .take_application_lease(1024)
            .expect("original app alias");
        let gate = Gate::install_next(Kind::CleanupRemoveFile, member);
        let mut removal = Box::pin(output.retire_segment(0, deadline()));
        let job = tokio::select! {
            result = &mut removal => panic!("live unlink returned before real result: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(10), gate.reached()) =>
                result.expect("actual live unlink result watchdog"),
        };
        drop(removal);
        let identity = {
            let bundle = preparation.state.lock().await;
            let role = &bundle.roles[0];
            let prefix = &role.bootstrap[&member];
            let identity = prefix
                .fixture_live_removal_job_identity()
                .expect("same original LiveRemoval");
            assert_eq!((identity.0, identity.3), (gate.identity().0, Some(job)));
            assert!(!identity.4 && role.pending_finite_close.is_none() && !role.refunded);
            assert!(
                prefix.fold().unwrap().files().iter().any(
                    |file| file.published == Some(v2::scratch::OutputKey::Segment { index: 0 })
                ),
                "the actual native unlink is not durable Observe yet"
            );
            assert_eq!(prefix.fold().unwrap().state().asset.totals.pending_jobs, 1);
            identity
        };
        assert_eq!(runtime.accounting().unwrap().used, charge);
        assert_eq!(original_ids(&preparation).await, ids);
        drop(gate);
        assert_eq!(
            preparation.state.lock().await.roles[0].bootstrap[&member]
                .fixture_live_removal_job_identity(),
            Some(identity)
        );
        output
            .retire_segment(0, deadline())
            .await
            .expect("SAME original live unlink worker, Observe and child FD return");
        {
            let bundle = preparation.state.lock().await;
            let prefix = &bundle.roles[0].bootstrap[&member];
            assert!(prefix.fixture_live_removal_job_identity().is_none());
            let state = prefix.fold().unwrap().state();
            assert_eq!(state.asset.totals.pending_jobs, 0);
            assert_eq!(
                state.asset.head.as_ref().unwrap().operation_id.uuid(),
                identity.1,
                "same LiveRemoval Observe, not a new operation"
            );
            assert!(!prefix
                .fold()
                .unwrap()
                .files()
                .iter()
                .any(|file| file.published == Some(v2::scratch::OutputKey::Segment { index: 0 })));
        }
        assert_eq!(
            runtime.accounting().unwrap().used,
            charge,
            "actual segment unlink/Observe alone cannot refund this owner"
        );
        // Known previous removal is idempotent and cannot arm replacement work.
        output
            .retire_segment(0, deadline())
            .await
            .expect("same already-removed segment retry");
        assert!(preparation.state.lock().await.roles[0].bootstrap[&member]
            .fixture_live_removal_job_identity()
            .is_none());
        let ticket = owner
            .prepare_retirement(true, deadline())
            .await
            .expect("normal original prefix retirement after live removal");
        finish_aliases(
            runtime,
            owner,
            preparation,
            output,
            application,
            ticket,
            (charge, ids.3),
        )
        .await;
    }
}
