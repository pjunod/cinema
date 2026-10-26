//! Actual queued copy owners and original copy receipts. Authored/compiled
//! only; behavioral/native execution requires complete-candidate Fable review.
use super::*;
use crate::domain::MediaFile;
use crate::resource_custody::InstallationContext;
use crate::transcode::{CopyVideoOptions, Pacing};
use sha2::{Digest, Sha256};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
fn executable_path() -> PathBuf {
    let selected = PathBuf::from(crate::testfixtures::ffmpeg());
    let selected = if selected.is_absolute() {
        selected
    } else {
        std::env::split_paths(&std::env::var_os("PATH").expect("fixture PATH"))
            .map(|directory| directory.join(&selected))
            .find(|path| path.is_file())
            .expect("install the actual FFmpeg executable")
    };
    std::fs::canonicalize(selected).expect("canonical actual executable")
}

async fn actual_request(root: &Path, rejected: bool) -> (Arc<NodeResourceRuntime>, CopyVodRequest) {
    crate::testfixtures::require_ffmpeg();
    let source =
        std::fs::canonicalize(crate::testfixtures::source("h264")).expect("actual media source");
    let metadata = std::fs::metadata(&source).expect("actual source metadata");
    let identity = crate::source_identity::SourceIdentity::from_metadata(&metadata);
    let bytes = crate::testfixtures::run(
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
    let document: serde_json::Value =
        serde_json::from_slice(&bytes).expect("actual probe document");
    let probe = crate::scan::probe::parse_probe_json(&document);
    let file = MediaFile {
        id: 1,
        item_id: 1,
        path: source.clone(),
        size: i64::try_from(metadata.len()).expect("source size"),
        mtime: identity.mtime,
        duration_ms: probe.duration_ms,
        container: probe.container,
        video_codec: probe.video_codec,
        video_codec_tag: probe.video_codec_tag,
        field_order: probe.field_order,
        video_profile: probe.video_profile,
        width: probe.width,
        height: probe.height,
        bit_depth: probe.bit_depth,
        hdr: probe.hdr,
        hdr_format: probe.hdr_format,
        max_cll: probe.max_cll,
        max_fall: probe.max_fall,
        mastering_max_luminance: probe.mastering_max_luminance,
        luminance_source: probe.luminance_source,
        dolby_vision: probe.dolby_vision,
        bitrate: probe.bitrate,
        audio_streams: probe.audio_streams,
        subtitle_streams: probe.subtitle_streams,
        downloaded_subtitles: Vec::new(),
        scanned_at: 1,
        audio_offset_ms: 0,
        probed: true,
    };
    let binary = executable_path();
    let binary_identity = crate::source_identity::SourceIdentity::from_metadata(
        &std::fs::metadata(&binary).expect("actual executable metadata"),
    );
    let digest = hex::encode(Sha256::digest(
        std::fs::read(&binary).expect("actual executable bytes"),
    ));
    let command = CopyPipeCommand::compile(
        file.clone(),
        CopyVideoOptions::new(false, false),
        super::super::super::finite_vod::SoftwareAvcExecutable::new(
            binary,
            binary_identity,
            &digest,
        )
        .expect("actual executable plan"),
        root.to_owned(),
        None,
        false,
        Pacing::unpaced(),
        None,
    )
    .expect("original fixed one-input copy recipe");
    let rendition_root = root.join("renditions");
    std::fs::create_dir(&rendition_root).expect("configured copy root");
    let runtime = Arc::new(
        NodeResourceRuntime::open_with_rendition_root(
            super::super::super::tests::options(
                root,
                super::super::super::daemon_owner(
                    "copy-cleanup-node".into(),
                    "copy-cleanup-cluster".into(),
                ),
                InstallationContext::FirstInstallation,
            ),
            rendition_root,
        )
        .expect("actual original runtime"),
    );
    let capacity = ResourceVector {
        cpu_ns_per_second: 1_000_000_000,
        peak_memory_bytes: 256 * 1024 * 1024,
        handles: 64,
        processes: 1,
        hardware_slots: 0,
        tuner_slots: 0,
        scratch_bytes: 64 * 1024 * 1024,
    };
    runtime
        .update_capacity(1, Metric::Known(capacity))
        .expect("original known fixture capacity");
    let request = CopyVodRequest {
        rendition_id: "original-copy".into(),
        generation_epoch: 0,
        recipe_source_binding: "actual-original-copy-source".into(),
        source: FiniteSourceRequest {
            path: source,
            media_file_id: file.id.to_string(),
            object_version: identity.render_s1(),
            scanner_size: file.size,
            scanner_mtime: file.mtime,
        },
        command,
        max_segments: 8,
        parser_limits: crate::copy_segmenter::Limits {
            max_bytes: 64 * 1024,
            ..Default::default()
        },
        reader_bounds: crate::fmp4::FragmentReaderBounds {
            buffer_bytes: 64 * 1024,
            unit_bytes: 64 * 1024,
            max_tracks: 8,
            max_runs: 64,
            max_samples: 4096,
        },
        max_pending_fragments: 2,
        operation_timeout: Duration::from_secs(30),
        application_owner_bytes: 1024 * 1024,
        demand: WorkloadDemand {
            resources: Metric::Known(capacity),
            limit_epoch: 1,
            trial: false,
        },
        output: FiniteOutputLimits {
            response_memory_bytes: 1024 * 1024,
            retained_metadata_bytes: 1024 * 1024,
            client_state_bytes: 1024 * 1024,
            resident_file_objects: 5,
            resident_logical_bytes: 5 * 1024 * 1024,
            reserved_storage_bytes: 64 * 1024 * 1024,
            one_file_logical_bytes: 1024 * 1024,
            one_io_payload_bytes: 1024 * 1024,
            // Queued immutable limits are validated by the actual DB worker.
            // No hook injects its known rollback or manufactures a close fact.
            live_handles: if rejected { 1 } else { 8 },
            reader_leases: 4,
            resident_memory_bytes: 1024 * 1024,
            allocation_unit_bytes: 4096,
            per_object_reserve_bytes: 4096,
            child_directory_reserve_bytes: 4096,
            parent_entry_reserve_bytes: 4096,
            bootstrap_memory_bytes: 1024 * 1024,
        },
    };
    (runtime, request)
}

async fn actual_owner(root: &Path, rejected: bool) -> (Arc<NodeResourceRuntime>, CopyVodOwner) {
    let (runtime, request) = actual_request(root, rejected).await;
    let owner = runtime
        .prepare_copy_vod(request, deadline())
        .await
        .expect("actual original queued copy owner");
    (runtime, owner)
}

async fn lost_ticket_still_uses_original_maintenance(
    runtime: Arc<NodeResourceRuntime>,
    mut owner: CopyVodOwner,
) {
    let charge = runtime
        .accounting()
        .expect("same original copy charge")
        .used;
    assert_ne!(charge, ResourceVector::default());
    let output = owner.output();
    let application = owner
        .take_application_lease(1024 * 1024)
        .expect("original rich worker lease");
    let mut ticket = owner
        .prepare_retirement(true, deadline())
        .await
        .expect("original copy-specific native/no-entry receipt");
    assert!(
        ticket
            .finish_retirement(Instant::now() + Duration::from_millis(1))
            .await
            .is_err(),
        "live app blocks direct refund"
    );
    drop(ticket);
    drop(owner);
    assert_eq!(
        runtime
            .maintain_failed_copy_admissions(deadline())
            .await
            .expect("same selected maintenance"),
        0
    );
    assert!(
        runtime.has_failed_copy_admission(),
        "selection survives CopyLaunch and ticket destruction"
    );
    assert_eq!(
        runtime
            .accounting()
            .expect("held until final actual aliases disappear")
            .used,
        charge
    );
    drop(output);
    assert_eq!(
        runtime
            .maintain_failed_copy_admissions(deadline())
            .await
            .expect("same maintenance while worker lease lives"),
        0
    );
    assert_eq!(
        runtime.accounting().expect("worker still charged").used,
        charge
    );
    drop(application);
    assert_eq!(
        runtime
            .maintain_failed_copy_admissions(deadline())
            .await
            .expect("original app and rich bundle final destruction"),
        1
    );
    assert_eq!(
        runtime
            .accounting()
            .expect("one matching original release")
            .used,
        ResourceVector::default()
    );
    assert!(!runtime.has_failed_copy_admission());
    Arc::try_unwrap(runtime)
        .ok()
        .expect("no original owner remains")
        .drain()
        .expect("join actual journal");
}

#[tokio::test]
async fn accepted_copy_cleanup_ignores_new_admission_epoch_and_survives_lost_ticket_until_app_drop()
{
    let root = tempfile::tempdir().expect("durable real copy fixture");
    let (runtime, owner) = actual_owner(root.path(), false).await;
    runtime
        .update_capacity(2, Metric::Known(ResourceVector::default()))
        .expect("new admission fenced to zero");
    lost_ticket_still_uses_original_maintenance(runtime, owner).await;
}

#[tokio::test]
async fn known_unaccepted_copy_uses_its_original_absence_receipt_and_never_refunds_live_app() {
    let root = tempfile::tempdir().expect("durable real copy fixture");
    let (runtime, owner) = actual_owner(root.path(), true).await;
    assert!(
        matches!(runtime.resolve_preparation(&owner.preparation, deadline()).await,
        Err(super::super::super::operations::OperationError::Custody(error))
            if error.certainty == crate::resource_custody::CommitCertainty::RolledBack)
    );
    assert_ne!(
        runtime
            .accounting()
            .expect("actual rejected original still charged")
            .used,
        ResourceVector::default()
    );
    lost_ticket_still_uses_original_maintenance(runtime, owner).await;
}

#[tokio::test]
async fn invalid_copy_profile_has_exact_before_reservation_provenance_and_no_family() {
    let root = tempfile::tempdir().expect("actual copy invalid-profile fixture");
    let (runtime, mut request) = actual_request(root.path(), false).await;
    let before = runtime.accounting().expect("actual baseline accounting");
    let family_counter = runtime
        .ledger
        .inner
        .lock()
        .expect("actual family allocator")
        .next_family;
    request.parser_limits.max_bytes = COPY_VOD_MAX_SEGMENT_BYTES + 1;
    let error = runtime
        .prepare_copy_vod(request, deadline())
        .await
        .err()
        .expect("actual pure pre-R rejection");
    assert!(error.definitely_before_reservation());
    assert_eq!(
        runtime
            .ledger
            .inner
            .lock()
            .expect("same family allocator")
            .next_family,
        family_counter,
        "pure rejection occurs before the original family is minted"
    );
    let after = runtime.accounting().expect("actual unchanged accounting");
    assert_eq!(after.used, before.used);
    assert_eq!(after.held, before.held);
    assert_eq!(after.unknown_peak_producers, before.unknown_peak_producers);
}

#[tokio::test]
async fn copy_constructor_error_after_r_never_becomes_before_reservation_provenance() {
    let root = tempfile::tempdir().expect("actual copy post-R fixture");
    let (runtime, mut request) = actual_request(root.path(), false).await;
    let family_counter = runtime
        .ledger
        .inner
        .lock()
        .expect("actual family allocator")
        .next_family;
    // The actual bounded CopySink constructor refuses this after R was queued.
    // No hook injects an error or manufactures an original close receipt.
    request.operation_timeout = Duration::from_secs(121);
    let error = runtime
        .prepare_copy_vod(request, deadline())
        .await
        .err()
        .expect("actual post-R sink refusal");
    assert!(
        !error.definitely_before_reservation(),
        "generic constructor errors never release caller quota"
    );
    assert!(
        runtime
            .ledger
            .inner
            .lock()
            .expect("same family allocator")
            .next_family
            > family_counter
    );
}
