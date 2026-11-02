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
