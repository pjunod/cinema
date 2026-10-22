//! The concrete writer must wait for its retained start decision before it
//! reads the original root pipes. These fixtures cover that latch only; they
//! are not journal or native-output qualification.
use super::*;
use crate::adaptive_resources::physical_graph::OwnedPhysicalGraph;
use crate::adaptive_resources::runtime::{daemon_owner, DurableArm, NodeResourceRuntime};
use crate::adaptive_resources::{
    FamilyDemand, FamilyRole, FamilyRoleDemand, Metric, ResourceLedger, ResourceVector,
    WorkloadDemand,
};
use crate::fmp4::{CutPolicy, FiniteSegmenterLimits, FragmentReaderBounds, MergeMemoryLimits};
use crate::process::ChildWork;
use crate::resource_custody::{
    Adapter, DaemonLockGuard, InstallationContext, MemberKind, OpenOptions, PlannedMember,
    RecoveryMethod,
};
use crate::segplan::{PlanCut, PlanEntry, PlanEntryKind, SegmentPlan, SEGPLAN_VERSION};
use crate::vod_generation::GenerationMemoryLimits;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

fn resources() -> ResourceVector {
    ResourceVector {
        cpu_ns_per_second: 1_000_000,
        peak_memory_bytes: 64 * 1024 * 1024,
        handles: 16,
        processes: 1,
        hardware_slots: 0,
        tuner_slots: 0,
        scratch_bytes: 64 * 1024 * 1024,
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn runtime(root: &std::path::Path) -> Arc<NodeResourceRuntime> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("finite-vod-fixture.lock"))
        .expect("fixture lock");
    let options = OpenOptions {
        data_dir: std::fs::canonicalize(root).expect("canonical fixture"),
        excluded_roots: Vec::new(),
        installation: InstallationContext::FirstInstallation,
        owner: daemon_owner(
            "finite-vod-test-node".into(),
            "finite-vod-test-cluster".into(),
        ),
        daemon_lock: DaemonLockGuard::from_held_lock(lock),
        queue_capacity: 4,
    };
    let runtime = Arc::new(NodeResourceRuntime::open(options).expect("actual runtime open"));
    runtime
        .update_capacity(1, Metric::Known(resources()))
        .expect("known fixture capacity");
    runtime
}

fn preparation(runtime: &NodeResourceRuntime) -> super::super::operations::OwnedPreparation {
    let demand = WorkloadDemand {
        resources: Metric::Known(resources()),
        limit_epoch: 1,
        trial: false,
    };
    let roles = [FamilyRoleDemand {
        role: FamilyRole::Video,
        demand,
        retained: None,
    }];
    let plan = crate::adaptive_resources::runtime::operations::GraphPlan {
        attempt_id: None,
        members: vec![PlannedMember {
            member_id: Uuid::new_v4(),
            kind: MemberKind::Root,
            adapter: Adapter::ProcessRoot,
            adapter_version: 1,
            domain: "finite-vod-start-latch-fixture".into(),
            locator: "unexecuted-fixture-root".into(),
            recovery: RecoveryMethod::LiveOnly {
                reason: "start-latch fixture only".into(),
            },
            owned_by: None,
            depends_on: Vec::new(),
            maximum_cardinality: 1,
            protected_paths: Vec::new(),
        }],
    };
    runtime
        .prepare_families(
            &[FamilyDemand {
                identity: runtime.new_family().expect("family identity"),
                roles: &roles,
            }],
            vec![plan],
            deadline(),
        )
        .expect("actual preparation owner")
}

fn pump_plan() -> SoftwareAvcPumpPlan {
    let grid = crate::transcode::VodFrameGrid::new(24_000, 1).expect("canonical grid");
    let plan = SegmentPlan {
        version: SEGPLAN_VERSION,
        timescale: grid.numerator,
        entries: vec![PlanEntry {
            index: 0,
            kind: PlanEntryKind::Video,
            start_ticks: 0,
            duration_ticks: 24_000,
            est_bytes: 1_000_000,
            cut: PlanCut::EndOfStream,
            fragments: 1,
        }],
        target_duration: 1,
    };
    let memory = GenerationMemoryLimits {
        reader: FragmentReaderBounds {
            buffer_bytes: 64 * 1024,
            unit_bytes: 64 * 1024,
            max_tracks: 8,
            max_runs: 64,
            max_samples: 4096,
        },
        read_chunk_bytes: 4096,
        retained_fragment_memory_bytes: 1024 * 1024,
        retained_fragments: 1,
        init_memory_bytes: 1024 * 1024,
        segmenter: FiniteSegmenterLimits {
            merge: MergeMemoryLimits {
                workspace_bytes: 1024 * 1024,
                output_bytes: 1024 * 1024,
                max_tracks: 8,
                max_samples: 4096,
            },
            prefix_memory_bytes: 1024 * 1024,
            split_memory_bytes: 1024 * 1024,
            published_batch_bytes: 1024 * 1024,
            retained_fragments: 1,
        },
    };
    SoftwareAvcPumpPlan::new(
        plan,
        0,
        grid,
        CutPolicy::new(1, 1, 1024 * 1024, 2, grid.numerator),
        memory,
        4096,
        1024,
        "start-latch-test".into(),
    )
    .expect("bounded pump plan")
}

fn graph() -> (
    ResourceLedger,
    crate::adaptive_resources::ReservationBinding,
    OwnedPhysicalGraph,
    crate::adaptive_resources::CleanupAuthority,
) {
    let resources = resources();
    let ledger = ResourceLedger::new(1, Metric::Known(resources)).expect("graph ledger");
    let (reservation, cleanup) = ledger
        .reserve(WorkloadDemand {
            resources: Metric::Known(resources),
            limit_epoch: 1,
            trial: false,
        })
        .expect("graph reservation");
    let binding = reservation.custody_binding();
    let graph = reservation.into_physical_graph().expect("physical graph");
    (ledger, binding, graph, cleanup)
}

#[tokio::test]
async fn finite_software_writer_waits_for_original_start_sender_before_reading() {
    let runtime_dir = tempfile::tempdir().expect("runtime fixture");
    let output_path = runtime_dir.path().join("not-opened-before-start");
    let runtime = runtime(runtime_dir.path());
    let preparation = preparation(&runtime);
    let output = FiniteOutput {
        runtime: Arc::clone(&runtime),
        preparation,
        member: Uuid::new_v4(),
        directory_path: output_path,
        encoded_process_marker: true,
    };
    let (pump, mut events) = pump::SoftwarePump::new(output, pump_plan()).expect("real pump");

    let (ledger, binding, mut graph, cleanup) = graph();
    let mut child = tokio::process::Command::new("/bin/true");
    child
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let root = graph
        .spawn_root(
            &mut child,
            ChildWork::background("finite VOD start-latch fixture"),
        )
        .await
        .expect("actual child root");
    let arm = DurableArm::adapter_fixture(binding, graph.identity().generation);
    let mut retained_pump = Some(pump);
    let task = graph
        .spawn_durable_finite_vod(root, Uuid::new_v4(), arm, &mut retained_pump)
        .await
        .expect("retain concrete finite pump task");
    assert!(retained_pump.is_none());
    let start = graph
        .take_finite_vod_start(task)
        .await
        .expect("original start sender");
    let sealed = graph.seal();
    assert!(sealed
        .wait_root(root)
        .await
        .expect("wait exited child")
        .success());

    assert!(tokio::time::timeout(
        Duration::from_millis(20),
        sealed.join_finite_vod(task, false),
    )
    .await
    .is_err());
    assert!(matches!(
        events.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    start.send(()).expect("release same original task");
    let terminal = sealed
        .join_finite_vod(task, false)
        .await
        .expect("join original pump")
        .expect("pump terminal result");
    assert!(matches!(
        terminal.outcome,
        crate::vod_generation::Outcome::Failed(_)
    ));
    let _ = sealed.close().await;
    drop(cleanup);
    drop(ledger);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("pump released runtime owner")
        .drain()
        .expect("drain actual journal worker");
}
