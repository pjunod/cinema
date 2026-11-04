//! Negative original-constructor boundaries; no probe or engine is entered.
use super::*;
use crate::process::fixed_probe::{DecodeFactCache, ProbeStreamSelection};
use crate::resource_custody::InstallationContext;
use std::time::Duration;

fn fixture_runtime(root: &std::path::Path) -> Arc<NodeResourceRuntime> {
    let owner = super::super::super::daemon_owner("fixture-node".into(), "fixture-cluster".into());
    let runtime = Arc::new(
        NodeResourceRuntime::open(super::super::super::tests::options(
            root,
            owner,
            InstallationContext::FirstInstallation,
        ))
        .expect("original journal/runtime"),
    );
    runtime
        .update_capacity(
            1,
            Metric::Known(ResourceVector {
                cpu_ns_per_second: u64::MAX / 4,
                peak_memory_bytes: u64::MAX / 4,
                handles: u64::MAX / 4,
                processes: u64::MAX / 4,
                ..ResourceVector::default()
            }),
        )
        .expect("fixture observation");
    runtime
}
fn request(root: &std::path::Path) -> FixedPlanningProbeRequest {
    FixedPlanningProbeRequest {
        source: FixedPlanningSourceRequest {
            path: root.join("unopened.mp4"),
            media_file_id: "fixture-media".into(),
            scanner_size: 0,
            scanner_mtime: 0,
        },
        recipe_source_binding: "fixture-recipe".into(),
        probe_program_locator: "/never-entered/ffprobe".into(),
        ffmpeg_program: "/never-entered/ffmpeg".into(),
        cache: Arc::new(DecodeFactCache::new()),
        catalog: crate::transcode::DecodeCatalogMetadata::new(
            None,
            None,
            crate::domain::DolbyVisionFacts::default(),
        )
        .expect("catalog"),
        selected_stream: ProbeStreamSelection::FirstPlayable,
        work: crate::process::ChildWork::background("negative planning constructor fixture"),
        application_metadata_bytes: 4096,
    }
}
#[tokio::test]
async fn expired_invalid_and_overflow_planning_requests_are_before_reservation() {
    let root = tempfile::tempdir().expect("isolated runtime");
    let runtime = fixture_runtime(root.path());
    let before = runtime.accounting().expect("before requests").used;
    let error = prepare(&runtime, request(root.path()), Instant::now())
        .err()
        .expect("expired");
    assert!(matches!(error, OperationError::BeforeReservation(_)));
    let mut invalid_request = request(root.path());
    invalid_request.source.path = "relative.mp4".into();
    let error = prepare(
        &runtime,
        invalid_request,
        Instant::now() + Duration::from_secs(30),
    )
    .err()
    .expect("invalid path");
    assert!(matches!(error, OperationError::BeforeReservation(_)));
    let mut overflow = request(root.path());
    overflow.application_metadata_bytes = usize::MAX;
    let error = prepare(&runtime, overflow, Instant::now() + Duration::from_secs(30))
        .err()
        .expect("layout overflow");
    assert!(matches!(error, OperationError::BeforeReservation(_)));
    assert!(matches!(
        add([u64::MAX, 1]),
        Err(OperationError::BeforeReservation(_))
    ));
    assert!(matches!(
        multiply(u64::MAX, 2),
        Err(OperationError::BeforeReservation(_))
    ));
    assert_eq!(
        runtime.accounting().expect("no P or M reserved").used,
        before
    );
    let counts = runtime
        .planning_ownership_diagnostics()
        .expect("actual book inventory");
    assert_eq!(counts.native_roles_held, 0);
    assert_eq!(counts.metadata_owners, 0);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn expired_continuous_original_p_closes_without_source_and_waits_for_actual_m_alias() {
    let root = tempfile::tempdir().expect("original durable runtime");
    let runtime = fixture_runtime(root.path());
    let mut owner = runtime
        .prepare_fixed_planning_probe(
            request(root.path()),
            Instant::now() + Duration::from_secs(30),
        )
        .expect("same actual P/M owner");
    let application = owner
        .take_application_lease(64)
        .expect("actual original application alias");
    let clock = Arc::new(crate::cluster::clock::ClusterClockGuard::new(false));
    let original = clock
        .capture_continuous()
        .expect("actual Linux clock")
        .deadline_after(Duration::from_millis(250))
        .expect("original live cutoff");
    owner
        .retain_continuous_entry_deadline(original)
        .expect("before any source/native entry");
    let charged = runtime.accounting().expect("original P/M charge").used;
    assert!(charged.processes > 0 && charged.peak_memory_bytes > 0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(owner
        .prepare_inputs(Instant::now() + Duration::from_secs(30))
        .await
        .is_err());
    assert_eq!(
        runtime.accounting().expect("failure cannot refund").used,
        charged
    );
    owner
        .finish_native(Instant::now() + Duration::from_secs(30))
        .await
        .expect("same actual no-task graph and unopened source close");
    let retained = runtime.accounting().expect("M retains original alias").used;
    assert_eq!(retained.processes, 0);
    assert!(retained.peak_memory_bytes > 0);
    let mut cleanup = owner.into_cleanup();
    let blocked = tokio::time::timeout(
        Duration::from_millis(20),
        cleanup.finish_retirement(Instant::now() + Duration::from_secs(30)),
    )
    .await;
    assert!(
        !matches!(blocked, Ok(Ok(()))),
        "actual application alias still owns M"
    );
    assert_eq!(runtime.accounting().expect("alias-gated M").used, retained);
    drop(application);
    cleanup
        .finish_retirement(Instant::now() + Duration::from_secs(30))
        .await
        .expect("original M closes only after actual alias drop");
    assert!(cleanup.known_closed());
    assert_eq!(
        runtime.accounting().expect("exact original refund").used,
        ResourceVector::default()
    );
    drop(cleanup);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("no original runtime alias")
        .drain()
        .expect("positive journal drain");
}

#[derive(Clone, Copy)]
enum ApplicationSurvivor {
    Entry,
    Frame,
    ControllerState,
}
#[tokio::test]
async fn registered_m_waits_for_actual_entry_alias_and_original_join() {
    registered_m_survivor(ApplicationSurvivor::Entry).await;
}
#[tokio::test]
async fn registered_m_waits_for_completed_original_inline_frame_destruction() {
    registered_m_survivor(ApplicationSurvivor::Frame).await;
}
#[tokio::test]
async fn registered_m_waits_for_actual_shared_controller_state_destruction() {
    registered_m_survivor(ApplicationSurvivor::ControllerState).await;
}
async fn registered_m_survivor(survivor: ApplicationSurvivor) {
    use crate::adaptive_resources::runtime::application::PendingApplicationGroup;
    let root = tempfile::tempdir().expect("original durable runtime");
    let runtime = fixture_runtime(root.path());
    let mut owner = runtime
        .prepare_fixed_planning_probe(
            request(root.path()),
            Instant::now() + Duration::from_secs(30),
        )
        .expect("same actual P/M");
    let group = PendingApplicationGroup::new();
    let (release, wait) = tokio::sync::oneshot::channel();
    let task = group
        .spawn_original(async {
            let _ = wait.await;
        })
        .expect("original heartbeat");
    let inner = group
        .wrap(vec![3; 64])
        .expect("actual shared controller state");
    let controller = group.wrap(vec![1; 64]).expect("actual rich controller");
    let entry = Arc::new(group.wrap(vec![2; 64]).expect("actual rich entry"));
    let alias = Arc::clone(&entry);
    let mut frame = Box::pin(
        group
            .frame(std::future::ready(()))
            .expect("original inline pass"),
    );
    let registered = owner
        .take_registered_application(4096, &group)
        .expect("same original M reference");
    assert!(
        group.wrap(()).is_err(),
        "one-shot binding seals registration"
    );
    assert!(runtime
        .affected_owner_inventory()
        .snapshot()
        .expect("census")
        .records()
        .any(|record| record.identity
            == crate::adaptive_resources::owner_inventory::InventoryIdentity::Original(
                group.identity()
            )));
    owner
        .finish_native(Instant::now() + Duration::from_secs(30))
        .await
        .expect("original never-entered P closes");
    let mut cleanup = owner.into_cleanup();
    cleanup
        .seal_for_logical_close(Instant::now() + Duration::from_secs(30))
        .await
        .expect("P and report closure are separate");
    let retained = runtime
        .accounting()
        .expect("M retains actual application")
        .used;
    drop(controller);
    drop(entry);
    frame.as_mut().await;
    assert!(
        registered.finish().is_err(),
        "Ready leaves actual pinned F and escaped entry counted"
    );
    assert!(tokio::time::timeout(Duration::from_millis(10), task.wait())
        .await
        .is_err());
    release.send(()).expect("original task remains owned");
    task.wait().await.expect("same original task joined");
    drop(task);
    match survivor {
        ApplicationSurvivor::Entry => {
            drop(inner);
            drop(frame);
            assert!(
                registered.finish().is_err(),
                "entry-only survivor keeps original M"
            );
            drop(alias);
        }
        ApplicationSurvivor::Frame => {
            drop(inner);
            drop(alias);
            assert!(
                registered.finish().is_err(),
                "completed-F-only survivor keeps original M"
            );
            drop(frame);
        }
        ApplicationSurvivor::ControllerState => {
            drop(alias);
            drop(frame);
            assert!(
                registered.finish().is_err(),
                "shared-controller-only survivor keeps original M"
            );
            drop(inner);
        }
    }
    assert_eq!(
        runtime
            .accounting()
            .expect("original M still held before explicit finalization")
            .used,
        retained
    );
    registered
        .finish()
        .expect("sealed actual payloads destroyed and original task joined");
    drop(registered);
    assert!(!runtime
        .affected_owner_inventory()
        .snapshot()
        .expect("census")
        .records()
        .any(|record| record.identity
            == crate::adaptive_resources::owner_inventory::InventoryIdentity::Original(
                group.identity()
            )));
    cleanup
        .finish_retirement(Instant::now() + Duration::from_secs(30))
        .await
        .expect("same original M settlement");
    assert!(cleanup.known_closed());
    assert_eq!(
        runtime.accounting().expect("same original refund").used,
        ResourceVector::default()
    );
    drop((cleanup, group));
    Arc::try_unwrap(runtime)
        .ok()
        .expect("no surviving runtime aliases")
        .drain()
        .expect("journal drain");
}

#[tokio::test]
async fn deterministic_group_bind_refusals_never_orphan_original_m_references() {
    use crate::adaptive_resources::owner_inventory::OwnerKind;
    use crate::adaptive_resources::runtime::application::PendingApplicationGroup;
    let root_a = tempfile::tempdir().expect("original A runtime");
    let root_b = tempfile::tempdir().expect("original B runtime");
    let runtime_a = fixture_runtime(root_a.path());
    let runtime_b = fixture_runtime(root_b.path());
    let mut owner_a = runtime_a
        .prepare_fixed_planning_probe(
            request(root_a.path()),
            Instant::now() + Duration::from_secs(30),
        )
        .expect("original A P/M");
    let mut owner_b = runtime_b
        .prepare_fixed_planning_probe(
            request(root_b.path()),
            Instant::now() + Duration::from_secs(30),
        )
        .expect("original B P/M");
    let group_a = PendingApplicationGroup::new();
    let group_b = PendingApplicationGroup::new();
    let task_a = group_a.spawn_original(async {}).expect("original A task");
    let task_b = group_b.spawn_original(async {}).expect("original B task");
    let payloads_a: Vec<_> = (0..4)
        .map(|_| group_a.wrap(vec![1; 64]).expect("original A payload"))
        .collect();
    let payloads_b: Vec<_> = (0..4)
        .map(|_| group_b.wrap(vec![2; 64]).expect("original B payload"))
        .collect();
    let references = |runtime: &NodeResourceRuntime| {
        runtime
            .affected_owner_inventory()
            .snapshot()
            .expect("actual original census")
            .records()
            .filter(|record| matches!(record.kind, OwnerKind::Reference { .. }))
            .count()
    };
    assert!(owner_a.take_registered_application(0, &group_a).is_err());
    assert!(owner_a.take_registered_application(4097, &group_a).is_err());
    assert_eq!(
        references(&runtime_a),
        0,
        "deterministic byte refusal precedes census insertion"
    );
    let registered_a = owner_a
        .take_registered_application(4096, &group_a)
        .expect("same group retries valid original capacity");
    assert_eq!(references(&runtime_a), 1);
    assert!(owner_a.take_registered_application(4096, &group_b).is_err());
    assert_eq!(
        references(&runtime_a),
        1,
        "already-taken M never inserts a second group"
    );
    assert!(owner_b.take_registered_application(4096, &group_a).is_err());
    assert_eq!(
        references(&runtime_b),
        0,
        "sealed original group cannot orphan a cross-runtime reference"
    );
    let registered_b = owner_b
        .take_registered_application(4096, &group_b)
        .expect("original B slot and group remain available");
    owner_a
        .finish_native(Instant::now() + Duration::from_secs(30))
        .await
        .expect("original A native closure");
    owner_b
        .finish_native(Instant::now() + Duration::from_secs(30))
        .await
        .expect("original B native closure");
    let mut cleanup_a = owner_a.into_cleanup();
    let mut cleanup_b = owner_b.into_cleanup();
    drop((payloads_a, payloads_b));
    task_a.wait().await.expect("same original A join");
    task_b.wait().await.expect("same original B join");
    registered_a.finish().expect("original A application close");
    registered_b.finish().expect("original B application close");
    cleanup_a
        .finish_retirement(Instant::now() + Duration::from_secs(30))
        .await
        .expect("no orphan blocks original A M");
    cleanup_b
        .finish_retirement(Instant::now() + Duration::from_secs(30))
        .await
        .expect("no orphan blocks original B M");
    assert_eq!(references(&runtime_a), 0);
    assert_eq!(references(&runtime_b), 0);
    assert_eq!(
        runtime_a.accounting().expect("original A refund").used,
        ResourceVector::default()
    );
    assert_eq!(
        runtime_b.accounting().expect("original B refund").used,
        ResourceVector::default()
    );
    drop((
        registered_a,
        registered_b,
        cleanup_a,
        cleanup_b,
        task_a,
        task_b,
        group_a,
        group_b,
    ));
    Arc::try_unwrap(runtime_a)
        .ok()
        .expect("original A aliases gone")
        .drain()
        .expect("original A drain");
    Arc::try_unwrap(runtime_b)
        .ok()
        .expect("original B aliases gone")
        .drain()
        .expect("original B drain");
}
