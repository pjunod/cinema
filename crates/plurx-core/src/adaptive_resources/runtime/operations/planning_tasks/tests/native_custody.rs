//! Real original command custody and lost-result regressions. Compile only
//! until complete-candidate review; no receipt is supplied by these gates.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_original_engine_custody_wait_resumes_the_same_registered_native_task() {
    let root = tempfile::tempdir().expect("actual native planning scratch");
    let (runtime, mut owner, selection, native_deadline) =
        super::joined::actual_owner(root.path()).await;
    let gate = owner
        .fixture_native_gate(false)
        .await
        .expect("same original gate");
    assert!(
        owner.fixture_native_gate(false).await.is_err(),
        "no duplicate gate"
    );
    owner
        .prepare_inputs(native_deadline)
        .await
        .expect("original source File");
    let ids = super::resumption::original_ids(&selection).await;
    let charge = runtime.accounting().expect("original P+M").used;
    let started = {
        let collect = owner.collect_and_refine(native_deadline);
        tokio::pin!(collect);
        tokio::select! {
            started = gate.started() => started,
            result = &mut collect => panic!(
                "must observe actual installed native custody; original collect error: {:?}",
                result.err().map(|error| error.0)
            ),
        }
        // Dropping only this borrowed collector leaves the SAME task handle
        // and native custody installed. This PID is diagnostic, not liveness.
    };
    assert!(started.pid > 0);
    assert_eq!(
        started.close_operation_id,
        ids.engine.version_close_operation_id.uuid()
    );
    assert_eq!(started.member_id, ids.engine.member_id.uuid());
    assert_eq!(super::resumption::original_ids(&selection).await, ids);
    let original_task = {
        let bundle = selection.state.lock().await;
        let p = &bundle.roles[0];
        assert!(!p.refunded);
        assert_ne!(p.phase, Phase::UnknownHeld);
        p.planning_tasks
            .as_ref()
            .unwrap()
            .engine
            .as_ref()
            .unwrap()
            .id()
    };
    assert_eq!(
        runtime.accounting().expect("retained original charge").used,
        charge
    );
    {
        let collect = owner.collect_and_refine(Instant::now() + Duration::from_millis(10));
        let error = collect
            .await
            .err()
            .expect("API deadline while the same native custody is paused");
        assert!(
            matches!(&error.0, OperationError::Deadline),
            "expected only the API deadline while original engine custody remains paused: {:?}",
            error.0
        );
    }
    {
        let bundle = selection.state.lock().await;
        assert_eq!(
            bundle.roles[0]
                .planning_tasks
                .as_ref()
                .unwrap()
                .engine
                .as_ref()
                .unwrap()
                .id(),
            original_task
        );
        assert_ne!(bundle.roles[0].phase, Phase::UnknownHeld);
    }
    assert_eq!(super::resumption::original_ids(&selection).await, ids);
    gate.release();
    gate.release(); // idempotent release creates no second native task
    let report = owner
        .collect_and_refine(native_deadline)
        .await
        .expect("same actual native joins");
    assert_eq!(super::resumption::original_ids(&selection).await, ids);
    owner
        .finish_native(native_deadline)
        .await
        .expect("known original P close");
    super::assert_only_m_survives(&runtime, &selection).await;
    drop(gate);
    super::resumption::finish_metadata(runtime, owner, selection, report).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_original_engine_task_panic_after_native_return_keeps_p_and_m_unknown_held() {
    let root = tempfile::tempdir().expect("actual native planning scratch");
    let (runtime, mut owner, selection, native_deadline) =
        super::joined::actual_owner(root.path()).await;
    let gate = owner
        .fixture_native_gate(true)
        .await
        .expect("same original task panic gate");
    owner
        .prepare_inputs(native_deadline)
        .await
        .expect("original source File");
    let ids = super::resumption::original_ids(&selection).await;
    let charge = runtime.accounting().expect("original P+M").used;
    {
        let collect = owner.collect_and_refine(native_deadline);
        tokio::pin!(collect);
        let started = tokio::select! {
            started = gate.started() => started,
            result = &mut collect => panic!(
                "must observe actual installed native custody; original collect error: {:?}",
                result.err().map(|error| error.0)
            ),
        };
        assert_eq!(
            started.close_operation_id,
            ids.engine.version_close_operation_id.uuid()
        );
        assert_eq!(started.member_id, ids.engine.member_id.uuid());
        gate.release();
        let error = collect
            .await
            .err()
            .expect("genuine original task panic loses its native summary");
        assert!(matches!(error.0, OperationError::State));
    }
    assert_eq!(super::resumption::original_ids(&selection).await, ids);
    // The real finish path acknowledges the SAME durable Hold; collection's
    // immediate ledger uncertainty alone is not a committed role transition.
    assert!(owner.finish_native(native_deadline).await.is_err());
    let sequence = {
        let bundle = selection.state.lock().await;
        let p = &bundle.roles[0];
        let tasks = p.planning_tasks.as_ref().expect("same original P");
        // Existing JoinError handling sets unknown without manufacturing a
        // native failure/closure. The successful collector's result was lost.
        assert!(tasks.unknown && tasks.engine.is_none());
        assert!(tasks.engine_failure.is_none() && tasks.engine_closed.is_none());
        assert!(tasks.probe_failure.is_none() && tasks.probe_closed.is_some());
        assert_eq!(p.phase, Phase::UnknownHeld);
        assert!(bundle.roles.iter().all(|role| !role.refunded));
        p.sequence
    };
    for _ in 0..2 {
        assert!(owner.finish_native(native_deadline).await.is_err());
        assert_eq!(selection.state.lock().await.roles[0].sequence, sequence);
        assert_eq!(
            runtime.accounting().expect("unknown original charge").used,
            charge
        );
    }
    drop(gate);
    drop(owner);
    drop(selection);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole original runtime")
        .drain()
        .expect("durable Unknown drain");
}
