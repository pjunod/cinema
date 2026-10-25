//! Original planning-task refusal/hold regressions. Native programs are never
//! entered by these fixtures; behavioral execution waits for final review.
use super::*;
use crate::adaptive_resources::runtime::planning_probe::plan;
use crate::process::fixed_probe::{DecodeFactCache, ProbeStreamSelection};
use crate::resource_custody::InstallationContext;
use std::time::Duration;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

async fn prepared(root: &std::path::Path) -> (Arc<NodeResourceRuntime>, OwnedPreparation) {
    let owner = super::super::super::daemon_owner("fixture-node".into(), "fixture-cluster".into());
    let runtime = Arc::new(
        NodeResourceRuntime::open(super::super::super::tests::options(
            root,
            owner,
            InstallationContext::FirstInstallation,
        ))
        .unwrap(),
    );
    let bound = u64::MAX / 4;
    runtime
        .update_capacity(
            1,
            Metric::Known(ResourceVector {
                cpu_ns_per_second: bound,
                peak_memory_bytes: bound,
                handles: bound,
                processes: bound,
                hardware_slots: bound,
                tuner_slots: bound,
                scratch_bytes: bound,
            }),
        )
        .unwrap();
    let request = FixedPlanningProbeRequest {
        source: FixedPlanningSourceRequest {
            path: root.join("never-opened.mp4"),
            media_file_id: "fixture-media".into(),
            scanner_size: 0,
            scanner_mtime: 0,
        },
        recipe_source_binding: "fixture-recipe".into(),
        probe_program_locator: "/never-entered/ffprobe".into(),
        cache: Arc::new(DecodeFactCache::new()),
        catalog: crate::transcode::DecodeCatalogMetadata::new(
            None,
            None,
            crate::domain::DolbyVisionFacts::default(),
        )
        .unwrap(),
        selected_stream: ProbeStreamSelection::FirstPlayable,
        work: crate::process::ChildWork::background("unexecuted planning fixture"),
        ffmpeg_program: "/never-entered/ffmpeg".into(),
        application_metadata_bytes: 4096,
    };
    let mut planned = plan::prepare(&runtime, &request, deadline()).unwrap();
    let preparation = planned.preparation.clone();
    runtime
        .register_planning_tasks(&preparation, &mut planned, request)
        .unwrap();
    runtime
        .resolve_preparation(&preparation, deadline())
        .await
        .unwrap();
    (runtime, preparation)
}

#[tokio::test]
async fn uncertain_planning_repeats_one_durable_hold_and_keeps_p_and_m_charged() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner) = prepared(root.path()).await;
    let charge = runtime.accounting().unwrap().used;
    {
        let mut bundle = owner.state.lock().await;
        bundle.roles[0].planning_tasks.as_mut().unwrap().unknown = true;
        runtime
            .retain_planning_uncertainty(bundle.roles[0].binding)
            .unwrap();
    }
    assert!(matches!(
        runtime.finish_planning_native(&owner, deadline()).await,
        Err(OperationError::State)
    ));
    let sequence = {
        let bundle = owner.state.lock().await;
        assert_eq!(bundle.roles[0].phase, Phase::UnknownHeld);
        assert!(bundle.roles.iter().all(|role| !role.refunded));
        bundle.roles[0].sequence
    };
    for _ in 0..3 {
        assert!(matches!(
            runtime.finish_planning_native(&owner, deadline()).await,
            Err(OperationError::State)
        ));
        let bundle = owner.state.lock().await;
        assert_eq!(
            bundle.roles[0].sequence, sequence,
            "no second Hold operation"
        );
        assert!(bundle.roles[0].transition.is_none());
    }
    assert_eq!(runtime.accounting().unwrap().used, charge);
    assert_eq!(
        runtime
            .read_planning_ownership_diagnostics()
            .unwrap()
            .native_roles_held,
        1
    );
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole runtime owner")
        .drain()
        .unwrap();
}

#[tokio::test]
async fn incomplete_planning_cannot_refund_one_missing_native_closure() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner) = prepared(root.path()).await;
    let charge = runtime.accounting().unwrap().used;
    let sequence = {
        let mut bundle = owner.state.lock().await;
        bundle.roles[0].planning_tasks.as_mut().unwrap().entered = true;
        bundle.roles[0].sequence
    };
    assert!(matches!(
        runtime.finish_planning_native(&owner, deadline()).await,
        Err(OperationError::State)
    ));
    let bundle = owner.state.lock().await;
    assert_eq!(bundle.roles[0].sequence, sequence);
    assert_eq!(bundle.roles[0].phase, Phase::Prepared);
    assert!(bundle.roles.iter().all(|role| !role.refunded));
    assert_eq!(runtime.accounting().unwrap().used, charge);
    drop(bundle);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole runtime owner")
        .drain()
        .unwrap();
}

#[tokio::test]
async fn expired_planning_lock_preserves_original_observation_and_settlement_ids() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner) = prepared(root.path()).await;
    let charge = runtime.accounting().unwrap().used;
    let mut bundle = owner.state.lock().await;
    let tasks = bundle.roles[0].planning_tasks.as_mut().unwrap();
    tasks.unknown = true;
    let operations: Vec<_> = tasks
        .root_observations
        .iter()
        .map(|o| o.operation_id)
        .chain([
            tasks.probe_task_observation.operation_id,
            tasks.engine_task_observation.operation_id,
            tasks.settlement_operation_id,
        ])
        .collect();
    runtime
        .retain_planning_uncertainty(bundle.roles[0].binding)
        .unwrap();
    assert!(matches!(
        runtime.finish_planning_native(&owner, Instant::now()).await,
        Err(OperationError::Deadline)
    ));
    drop(bundle);
    assert!(matches!(
        runtime.finish_planning_native(&owner, deadline()).await,
        Err(OperationError::State)
    ));
    let bundle = owner.state.lock().await;
    let tasks = bundle.roles[0].planning_tasks.as_ref().unwrap();
    let resumed: Vec<_> = tasks
        .root_observations
        .iter()
        .map(|o| o.operation_id)
        .chain([
            tasks.probe_task_observation.operation_id,
            tasks.engine_task_observation.operation_id,
            tasks.settlement_operation_id,
        ])
        .collect();
    assert_eq!(resumed, operations);
    assert_eq!(bundle.roles[0].phase, Phase::UnknownHeld);
    assert_eq!(runtime.accounting().unwrap().used, charge);
    drop(bundle);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole runtime owner")
        .drain()
        .unwrap();
}
