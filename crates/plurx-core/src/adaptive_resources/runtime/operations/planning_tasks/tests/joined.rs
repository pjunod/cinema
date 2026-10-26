//! Actual installed-tool positive P closure. Authored/compiled only; execution
//! belongs to the complete-candidate batch after independent final review.
use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn native_tool(variable: &str, default: &str) -> PathBuf {
    let path = std::env::var_os(variable).map_or_else(|| PathBuf::from(default), PathBuf::from);
    assert!(
        path.is_absolute(),
        "{variable} must name an absolute installed executable: {}",
        path.display()
    );
    let metadata = std::fs::metadata(&path).unwrap_or_else(|error| {
        panic!(
            "required native planning test tool unavailable: {} ({variable}): {error}",
            path.display()
        )
    });
    assert!(
        metadata.is_file(),
        "required native tool is not a file: {} ({variable})",
        path.display()
    );
    assert!(
        metadata.permissions().mode() & 0o111 != 0,
        "required native tool is not executable: {} ({variable})",
        path.display()
    );
    path
}

pub(super) async fn actual_owner(
    root: &std::path::Path,
) -> (
    Arc<NodeResourceRuntime>,
    crate::adaptive_resources::runtime::planning_probe::FixedPlanningProbeOwner,
    OwnedPreparation,
    Instant,
) {
    let ffmpeg = native_tool("PLURX_TEST_FFMPEG", "/usr/bin/ffmpeg");
    let ffprobe = native_tool("PLURX_TEST_FFPROBE", "/usr/bin/ffprobe");
    let source = root.join("never-opened.mp4");
    // Fixture creation precedes the admitted operation. Every planning source,
    // probe root and engine/dependency root below uses its real original owner.
    let fixture = tokio::process::Command::new(&ffmpeg)
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:r=2",
            "-frames:v",
            "2",
            "-an",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-y",
        ])
        .arg(&source)
        .kill_on_drop(true)
        .output()
        .await
        .expect("execute the required installed FFmpeg for fixture creation");
    assert!(
        fixture.status.success(),
        "required FFmpeg could not create the short AVC fixture: {}",
        String::from_utf8_lossy(&fixture.stderr)
    );
    drop(fixture);
    let runtime = fixture_runtime(root);
    let request = fixture_request(
        root,
        ffprobe.to_string_lossy().into_owned(),
        ffmpeg.to_string_lossy().into_owned(),
    );
    let native_deadline = Instant::now() + Duration::from_secs(120);
    let owner = runtime
        .prepare_fixed_planning_probe(request, native_deadline)
        .expect("prepare the original P/M graph");
    let mut selections = runtime
        .retained_preparations()
        .expect("original owner book");
    assert_eq!(selections.len(), 1);
    let selection = selections.pop().expect("one original selection");
    (runtime, owner, selection, native_deadline)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn joined_planning_native_settles_seven_members_once_and_keeps_m_until_last_alias() {
    let root = tempfile::tempdir().expect("durable test scratch");
    let (runtime, mut owner, selection, native_deadline) = actual_owner(root.path()).await;
    let application = owner
        .take_application_lease(4096)
        .expect("original coordinator application lease");
    let settlement_operation = {
        let bundle = selection.state.lock().await;
        assert_eq!(bundle.roles[0].typed_members.as_ref().unwrap().len(), 7);
        bundle.roles[0]
            .planning_tasks
            .as_ref()
            .unwrap()
            .settlement_operation_id
    };
    owner
        .prepare_inputs(native_deadline)
        .await
        .expect("acquire the same original source File");
    let report = owner.collect_and_refine(native_deadline).await.expect(
        "join actual original FFprobe and FFmpeg/dependency tasks and refine the fresh document",
    );
    assert!(report.document().contains("streams"));
    let alias = report.clone();
    owner
        .finish_native(native_deadline)
        .await
        .expect("positive seven-member joined P settlement");
    assert_only_m_survives(&runtime, &selection).await;
    let metadata_charge = runtime.accounting().unwrap().used;
    let sequence = {
        let bundle = selection.state.lock().await;
        let p = &bundle.roles[0];
        assert_eq!(p.phase, Phase::Settled);
        assert!(p.transition.is_none() && p.pending_finite_close.is_none());
        assert!(p.members.is_empty() && p.members.capacity() == 0);
        assert!(p.typed_members.is_none() && p.planning_tasks.is_none());
        assert!(p.sources.is_empty() && p.roots.is_empty() && p.drains.is_empty());
        assert!(p.closed_drains.is_empty() && p.bootstrap.is_empty() && p.scratch.is_empty());
        assert!(p.armed.is_empty() && p.invoked.is_empty());
        assert!(p.graph.is_none() && p.sealed.is_none());
        assert!(p.finite_vod.is_none() && p.copy_vod.is_none() && p.retirement.is_none());
        p.sequence
    };
    assert_ne!(settlement_operation, Uuid::nil());
    let diagnostics = runtime
        .read_planning_ownership_diagnostics()
        .expect("complete owner diagnostics");
    assert_eq!(diagnostics.native_roles_held, 0);
    assert_eq!(diagnostics.metadata_owners, 1);
    owner
        .finish_native(native_deadline)
        .await
        .expect("same P settles once");
    assert_eq!(selection.state.lock().await.roles[0].sequence, sequence);
    assert_eq!(runtime.accounting().unwrap().used, metadata_charge);
    assert!(
        alias.document().contains("streams"),
        "M still owns the actual report"
    );
    drop(selection); // no extra rich-book selector may survive final M destruction
    let mut cleanup = owner.into_cleanup();
    assert!(cleanup.finish_retirement(native_deadline).await.is_err());
    assert!(!cleanup.known_closed());
    assert_eq!(runtime.accounting().unwrap().used, metadata_charge);
    drop(report);
    assert!(
        cleanup.finish_retirement(native_deadline).await.is_err(),
        "original report alias retains M"
    );
    assert_eq!(runtime.accounting().unwrap().used, metadata_charge);
    drop(alias);
    assert!(
        cleanup.finish_retirement(native_deadline).await.is_err(),
        "original application frame still retains M"
    );
    assert_eq!(runtime.accounting().unwrap().used, metadata_charge);
    drop(application);
    cleanup
        .finish_retirement(native_deadline)
        .await
        .expect("the original M finalizer completes after every report/application alias drops");
    assert!(cleanup.known_closed());
    assert_eq!(
        runtime.accounting().unwrap().used,
        ResourceVector::default()
    );
    assert!(runtime.retained_preparations().unwrap().is_empty());
    let diagnostics = runtime.read_planning_ownership_diagnostics().unwrap();
    assert_eq!(diagnostics.native_roles_held, 0);
    assert_eq!(diagnostics.metadata_owners, 0);
    drop(cleanup);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("no original task/selector survives")
        .drain()
        .unwrap();
}
