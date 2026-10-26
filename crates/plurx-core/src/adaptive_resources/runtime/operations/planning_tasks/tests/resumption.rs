//! Decision 497 runtime regressions using actual installed FFprobe/FFmpeg.
//! Authored/compiled only; execution waits for complete-candidate Fable review.
use super::stages::Gate;
use super::*;

#[derive(Debug, PartialEq)]
pub(super) struct OriginalIds {
    bundle: Uuid,
    prepare: Uuid,
    graph: Uuid,
    obligation: Uuid,
    pub(super) probe: v2::FixedProbeDiscoverTaskV1,
    pub(super) engine: v2::FixedPlanningEngineTaskV1,
    source_close: Uuid,
    enrichments: [Uuid; 6],
    settlement: Uuid,
    manifest: Vec<v2::MemberPlan>,
}
pub(super) async fn original_ids(selection: &OwnedPreparation) -> OriginalIds {
    let bundle = selection.state.lock().await;
    let p = &bundle.roles[0];
    let tasks = p.planning_tasks.as_ref().expect("original P tasks");
    let handle = p.handle.as_ref().expect("original accepted obligation");
    OriginalIds {
        bundle: bundle.id,
        prepare: bundle.prepare.id,
        graph: handle.graph_id(),
        obligation: handle.obligation_id(),
        probe: tasks.probe_plan.clone(),
        engine: tasks.engine_plan.clone(),
        source_close: p.sources[&tasks.source_member].parent_close_operation_id(),
        enrichments: [
            tasks.root_observations[0].operation_id,
            tasks.root_observations[1].operation_id,
            tasks.root_observations[2].operation_id,
            tasks.root_observations[3].operation_id,
            tasks.probe_task_observation.operation_id,
            tasks.engine_task_observation.operation_id,
        ],
        settlement: tasks.settlement_operation_id,
        manifest: p.typed_members.clone().expect("seven original members"),
    }
}

pub(super) async fn finish_metadata(
    runtime: Arc<NodeResourceRuntime>,
    owner: crate::adaptive_resources::runtime::planning_probe::FixedPlanningProbeOwner,
    selection: OwnedPreparation,
    report: crate::adaptive_resources::runtime::planning_probe::FixedProbeMetadataLease,
) {
    drop(report);
    drop(selection);
    let mut cleanup = owner.into_cleanup();
    cleanup
        .finish_retirement(Instant::now() + Duration::from_secs(120))
        .await
        .expect("original M cleanup after report drop");
    assert!(cleanup.known_closed());
    assert_eq!(
        runtime.accounting().expect("ledger").used,
        ResourceVector::default()
    );
    drop(cleanup);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("no original selector/task remains")
        .drain()
        .expect("drain original journal");
}

#[tokio::test]
async fn actual_enrichment_waiter_deadline_resumes_same_queued_operation_after_four_root_observations(
) {
    let root = tempfile::tempdir().expect("durable native fixture");
    let (runtime, mut owner, selection, native_deadline) =
        super::joined::actual_owner(root.path()).await;
    owner
        .prepare_inputs(native_deadline)
        .await
        .expect("original source File");
    let report = owner
        .collect_and_refine(native_deadline)
        .await
        .expect("actual original native joins");
    assert!(report.document().contains("streams"));
    let ids = original_ids(&selection).await;
    let charge = runtime.accounting().expect("original P+M charge").used;
    let before = selection.state.lock().await.roles[0].sequence;
    let operation = ids.enrichments[4]; // first task enrichment, after f's four roots
    let gate = Gate::install(operation);
    let enrichment_deadline = Instant::now() + Duration::from_secs(30);
    {
        let finish = owner.finish_native(enrichment_deadline);
        tokio::pin!(finish);
        tokio::select! {
            _ = gate.entered() => {},
            _result = &mut finish => panic!("must reach the queued f-to-g acknowledgment boundary"),
        }
        // Only the retained acknowledgment waiter is gated. The real journal
        // worker still owns/executes the exact accepted command. Advance Tokio
        // time AFTER the stage is reached; this is not a book-lock deadline.
        // The hook returns no error/outcome: the API's real journal-wait
        // timeout cancels only the borrow; the original operation survives.
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        // This outer guard is a FAILURE bound only. Its expiry must panic;
        // only the inner API's own Deadline result satisfies the regression.
        let error = tokio::time::timeout(Duration::from_secs(1), finish)
            .await
            .expect("outer failure guard expired: API must enforce its own journal deadline")
            .expect_err("actual API-internal journal deadline");
        tokio::time::resume();
        assert!(matches!(error.0, OperationError::Deadline));
    }
    assert_eq!(gate.visits(), 1);
    assert_eq!(original_ids(&selection).await, ids);
    {
        let bundle = selection.state.lock().await;
        assert!(bundle.prepare.acknowledged);
        assert!(bundle.roles.iter().all(|role| !role.refunded));
        let p = &bundle.roles[0];
        assert_ne!(
            p.phase,
            Phase::UnknownHeld,
            "a deadline alone cannot write Hold"
        );
        assert_eq!(
            p.sequence,
            before + 5,
            "source child and four real root observations acknowledged"
        );
        let tasks = p
            .planning_tasks
            .as_ref()
            .expect("same native closures retained");
        assert!(tasks
            .root_observations
            .iter()
            .all(|observation| observation.committed));
        assert!(
            !tasks.probe_task_observation.committed && !tasks.engine_task_observation.committed
        );
        assert!(tasks.probe_closed.is_some() && tasks.engine_closed.is_some());
        let transition = p
            .transition
            .as_ref()
            .expect("original in-flight enrichment");
        assert!(matches!(transition.effect, Effect::Enrich));
        assert_eq!(transition.operation.id, operation);
        assert_eq!(
            transition
                .operation
                .pending
                .as_ref()
                .expect("same receiver retained")
                .operation_id(),
            operation
        );
        assert!(transition.operation.result.is_none());
        assert!(transition.operation.rejected.is_none());
        assert!(!transition.operation.acknowledged);
    }
    assert_eq!(runtime.accounting().expect("held ledger").used, charge);
    assert_eq!(
        runtime
            .read_planning_ownership_diagnostics()
            .expect("held diagnostics")
            .native_roles_held,
        1
    );
    gate.release();
    let resume_deadline = Instant::now() + Duration::from_secs(120);
    owner
        .finish_native(resume_deadline)
        .await
        .expect("resume exact queued enrichment and settlement");
    assert_eq!(
        gate.visits(),
        2,
        "same pre-minted operation re-entered once; no replacement submission"
    );
    assert_only_m_survives(&runtime, &selection).await;
    let sequence = selection.state.lock().await.roles[0].sequence;
    assert_eq!(
        sequence,
        before + 9,
        "one child, four roots, two tasks, Seal and one Settle; no duplicate enrichment/Hold"
    );
    let metadata_charge = runtime.accounting().expect("M charge").used;
    owner
        .finish_native(resume_deadline)
        .await
        .expect("idempotent original P finish");
    assert_eq!(selection.state.lock().await.roles[0].sequence, sequence);
    assert_eq!(
        runtime.accounting().expect("same M charge").used,
        metadata_charge
    );
    drop(gate);
    finish_metadata(runtime, owner, selection, report).await;
}
