//! Actual one-closure partial P completion; authored/compiled only.
use super::resumption::{finish_metadata, original_ids};
use super::stages::Gate;
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_one_probe_closure_keeps_p_and_m_charged_until_same_pending_engine_task_finishes() {
    let root = tempfile::tempdir().expect("durable native fixture");
    let (runtime, mut owner, selection, native_deadline) =
        super::joined::actual_owner(root.path()).await;
    owner
        .prepare_inputs(native_deadline)
        .await
        .expect("original source File");
    let ids = original_ids(&selection).await;
    let gate = Gate::install(ids.engine.member_id.uuid());
    let charge = runtime.accounting().expect("original P+M charge").used;
    {
        let collect = owner.collect_and_refine(native_deadline);
        tokio::pin!(collect);
        tokio::select! {
            _ = gate.probe_joined() => {},
            _result = &mut collect => panic!("real probe must join while original engine remains gated"),
        }
        // Dropping this borrowed waiter leaves its SAME original engine
        // JoinHandle and captured plan/native deadline in the charged book.
    }
    gate.entered().await;
    assert_eq!(gate.visits(), 1);
    assert_eq!(original_ids(&selection).await, ids);
    let (sequence, original_engine_task) = {
        let bundle = selection.state.lock().await;
        let p = &bundle.roles[0];
        let tasks = p.planning_tasks.as_ref().expect("original P tasks");
        assert!(tasks.native_state() == PlanningNativeState::Pending);
        assert!(tasks.entered && tasks.probe.is_none());
        let probe = tasks
            .probe_closed
            .as_ref()
            .expect("one ACTUAL native closure");
        assert_ne!(probe.version_identity().pid, 0);
        assert_ne!(probe.document_identity().pid, 0);
        assert_ne!(probe.document_identity().start_marker, 0);
        assert_eq!(
            probe.document_close_operation_id(),
            ids.probe.document_close_operation_id.uuid()
        );
        assert!(tasks.engine_closed.is_none() && tasks.engine_observed.is_none());
        assert!(tasks.probe_failure.is_none() && tasks.engine_failure.is_none() && !tasks.unknown);
        let task = tasks
            .engine
            .as_ref()
            .expect("same original engine remains registered");
        assert!(!task.is_finished());
        assert!(bundle.roles.iter().all(|role| !role.refunded));
        (p.sequence, task.id())
    };
    for _ in 0..2 {
        let error = owner
            .finish_native(deadline())
            .await
            .expect_err("one closure cannot settle seven members");
        assert!(matches!(error.0, OperationError::State));
        let bundle = selection.state.lock().await;
        let p = &bundle.roles[0];
        assert_eq!(
            p.sequence, sequence,
            "partial completion cannot append Hold/Settle"
        );
        assert_ne!(p.phase, Phase::UnknownHeld);
        assert!(p.transition.is_none() && bundle.roles.iter().all(|role| !role.refunded));
        assert_eq!(
            p.planning_tasks
                .as_ref()
                .unwrap()
                .engine
                .as_ref()
                .unwrap()
                .id(),
            original_engine_task
        );
        assert_eq!(
            runtime.accounting().expect("full charge retained").used,
            charge
        );
    }
    assert_eq!(original_ids(&selection).await, ids);
    assert_eq!(
        runtime
            .read_planning_ownership_diagnostics()
            .expect("partial diagnostics")
            .native_roles_held,
        1
    );
    gate.release();
    let report = owner
        .collect_and_refine(native_deadline)
        .await
        .expect("join SAME released real FFmpeg/dependency job");
    assert_eq!(
        gate.visits(),
        1,
        "the original task was never replaced or re-entered"
    );
    assert_eq!(original_ids(&selection).await, ids);
    {
        let bundle = selection.state.lock().await;
        let tasks = bundle.roles[0]
            .planning_tasks
            .as_ref()
            .expect("both original closures");
        assert!(tasks.native_state() == PlanningNativeState::Joined);
        assert!(tasks.probe.is_none() && tasks.engine.is_none());
        let engine = tasks
            .engine_closed
            .as_ref()
            .expect("actual remaining original native closure");
        assert_eq!(engine.task(), &ids.engine);
        assert_ne!(engine.version_identity().pid, 0);
        assert_ne!(engine.dependency_identity().pid, 0);
        assert!(engine.used_file_slots() > 0);
        assert!(bundle.roles.iter().all(|role| !role.refunded));
    }
    assert_eq!(
        runtime.accounting().expect("joined but not settled").used,
        charge
    );
    owner
        .finish_native(deadline())
        .await
        .expect("one seven-member original settlement");
    assert_only_m_survives(&runtime, &selection).await;
    let settled = selection.state.lock().await.roles[0].sequence;
    let metadata_charge = runtime.accounting().expect("M survives").used;
    owner
        .finish_native(deadline())
        .await
        .expect("same P closes once");
    assert_eq!(selection.state.lock().await.roles[0].sequence, settled);
    assert_eq!(
        runtime.accounting().expect("unchanged M").used,
        metadata_charge
    );
    drop(gate);
    finish_metadata(runtime, owner, selection, report).await;
}
