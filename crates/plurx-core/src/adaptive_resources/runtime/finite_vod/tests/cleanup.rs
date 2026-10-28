//! Actual queued journal and fixed producer owners. Execution and
//! qualification are recorded separately in source-bound receipts.
use super::*;
use crate::domain::MediaFile;
use crate::resource_custody::InstallationContext;
use crate::transcode::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{Notify, Semaphore};

struct Barrier {
    entered: Notify,
    release: Semaphore,
}
fn gates() -> &'static Mutex<BTreeMap<Uuid, Arc<Barrier>>> {
    static GATES: OnceLock<Mutex<BTreeMap<Uuid, Arc<Barrier>>>> = OnceLock::new();
    GATES.get_or_init(|| Mutex::new(BTreeMap::new()))
}
struct Gate {
    member: Uuid,
    barrier: Arc<Barrier>,
}
impl Gate {
    fn install(member: Uuid) -> Self {
        let barrier = Arc::new(Barrier {
            entered: Notify::new(),
            release: Semaphore::new(0),
        });
        assert!(gates()
            .lock()
            .expect("fixture gates")
            .insert(member, Arc::clone(&barrier))
            .is_none());
        Self { member, barrier }
    }
    async fn entered(&self) {
        self.barrier.entered.notified().await;
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        gates().lock().expect("fixture gates").remove(&self.member);
        self.barrier.release.add_permits(1);
    }
}
/// Scheduling only: the original SourceRoot command is already queued. This
/// hook cannot manufacture a journal error, native result or close witness.
pub(in crate::adaptive_resources::runtime) async fn before_source_root_ack(member: Uuid) {
    let barrier = gates().lock().expect("fixture gates").get(&member).cloned();
    if let Some(barrier) = barrier {
        barrier.entered.notify_one();
        barrier
            .release
            .acquire()
            .await
            .expect("fixture release")
            .forget();
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
fn executable_path(name: &str) -> PathBuf {
    let path = PathBuf::from(name);
    let selected = if path.is_absolute() {
        path
    } else {
        std::env::split_paths(&std::env::var_os("PATH").expect("fixture PATH"))
            .map(|entry| entry.join(&path))
            .find(|candidate| candidate.is_file())
            .expect("install the actual selected FFmpeg executable")
    };
    std::fs::canonicalize(selected).expect("canonical actual executable")
}

fn original_owner(
    root: &Path,
    reject_prepare: bool,
) -> (Arc<NodeResourceRuntime>, SoftwareVodOwner) {
    let (runtime, owner) = original_owner_at_generation(
        root,
        reject_prepare,
        SoftwareVodRequest::INITIAL_GENERATION_EPOCH,
    );
    (
        runtime,
        owner.expect("same original accepted queue command"),
    )
}

fn original_owner_at_generation(
    root: &Path,
    reject_prepare: bool,
    generation_epoch: u64,
) -> (
    Arc<NodeResourceRuntime>,
    Result<SoftwareVodOwner, FiniteVodError>,
) {
    crate::testfixtures::require_ffmpeg();
    let source = root.join("source.mkv");
    crate::testfixtures::run(
        std::process::Command::new(crate::testfixtures::ffmpeg())
            .args(["-v", "error", "-y", "-i"])
            .arg(crate::testfixtures::source("h264"))
            .args(["-t", "1", "-an", "-c:v", "copy"])
            .arg(&source),
    );
    let source = std::fs::canonicalize(source).expect("actual source path");
    let metadata = std::fs::metadata(&source).expect("actual source metadata");
    let identity = crate::source_identity::SourceIdentity::from_metadata(&metadata);
    let probe_bytes = crate::testfixtures::run(
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
        serde_json::from_slice(&probe_bytes).expect("actual probe JSON");
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
    let executable_path = executable_path(&crate::testfixtures::ffmpeg());
    let executable_metadata =
        std::fs::metadata(&executable_path).expect("actual executable metadata");
    let executable_identity =
        crate::source_identity::SourceIdentity::from_metadata(&executable_metadata);
    let executable_digest = hex::encode(Sha256::digest(
        std::fs::read(&executable_path).expect("actual executable bytes"),
    ));
    let facts = DecodeFacts::from_ffprobe_json(
        &document,
        DecodeSourceIdentity::from_sha256(hex::encode(Sha256::digest(
            std::fs::read(&source).expect("actual source bytes"),
        )))
        .expect("bound source digest"),
    )
    .expect("actual source decode facts");
    let capabilities = DecodeCapabilities::new(
        DecodeCapabilitySnapshotIdentity::new(
            executable_digest.clone(),
            "finite-cleanup-fixture".into(),
            None,
        )
        .expect("actual executable version"),
        vec![],
        vec![SoftwareDecoder {
            codec: file.video_codec.clone().expect("probed video codec"),
            implementation: None,
        }],
    )
    .expect("fixed software decoder inventory");
    let options = TranscodeOptions {
        pipeline: Pipeline::Cpu,
        target_height: 360,
        video_bitrate_kbps: 800,
        software_threads: Some(1),
        ..Default::default()
    };
    let plan = resolve_transcode(
        &TranscodeRequest::new(
            Encoder::Software,
            TranscodeMediaOptions::from_options_with_facts(&file, &options, &facts),
        ),
        &facts,
        &capabilities,
        &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        &AttemptRestrictions::none(),
    )
    .expect("actual software recipe");
    let execution = TranscodeExecution::from_options(&file, &options, Pacing::unpaced(), ".")
        .expect("actual execution");
    let grid = VodFrameGrid::new(24, 1).expect("original pump grid");
    let command = SoftwareAvcCommand::compile(
        &file,
        &plan,
        &execution,
        grid,
        1.0,
        false,
        VodSourceClock::ProducerDefaultZero,
        SoftwareAvcExecutable::new(
            executable_path.clone(),
            executable_identity,
            &executable_digest,
        )
        .expect("actual executable plan"),
        // Engine path versions use their existing seven-field metadata
        // format; SourceIdentity's s1 wire prefix belongs to source custody.
        FiniteEngineSnapshot::from_captured(
            &[(
                executable_path,
                format!(
                    "{}:{}:{}:{}:{}:{}:{}",
                    executable_metadata.dev(),
                    executable_metadata.ino(),
                    executable_metadata.size(),
                    executable_metadata.mtime(),
                    executable_metadata.mtime_nsec(),
                    executable_metadata.ctime(),
                    executable_metadata.ctime_nsec(),
                ),
            )],
            None,
        )
        .expect("actual engine metadata fence"),
        Vec::new(),
        root.to_owned(),
    )
    .expect("fixed software command");
    let mut options = super::super::tests::options(
        root,
        super::super::daemon_owner(
            "finite-cleanup-node".into(),
            "finite-cleanup-cluster".into(),
        ),
        InstallationContext::FirstInstallation,
    );
    let renditions = root.join("renditions");
    std::fs::create_dir(&renditions).expect("actual configured output root");
    options.queue_capacity = 4;
    let runtime = Arc::new(
        NodeResourceRuntime::open_with_rendition_root(options, renditions)
            .expect("original runtime"),
    );
    let vector = ResourceVector {
        cpu_ns_per_second: 1_000_000_000,
        peak_memory_bytes: 256 * 1024 * 1024,
        handles: 64,
        processes: 1,
        hardware_slots: 0,
        tuner_slots: 0,
        scratch_bytes: 64 * 1024 * 1024,
    };
    runtime
        .update_capacity(1, Metric::Known(vector))
        .expect("known fixture capacity");
    // The original public constructor queues these immutable bounds. The
    // actual journal validator rejects zero file objects; no fixture returns
    // an injected error or replaces the original accepted command/result.
    let owner = runtime.prepare_software_vod(
        SoftwareVodRequest {
            rendition_id: "original-finite".into(),
            generation_epoch,
            recipe_source_binding: "actual-original-source".into(),
            command,
            producer: super::tests::pump_plan(),
            sources: vec![FiniteSourceRequest {
                path: source,
                media_file_id: "1".into(),
                object_version: identity.render_s1(),
                scanner_size: file.size,
                scanner_mtime: file.mtime,
            }],
            segment_count: 1,
            encoded_process_marker: true,
            demand: WorkloadDemand {
                resources: Metric::Known(vector),
                limit_epoch: 1,
                trial: false,
            },
            application_owner_bytes: 0,
            output: FiniteOutputLimits {
                response_memory_bytes: 1024 * 1024,
                retained_metadata_bytes: 1024 * 1024,
                client_state_bytes: 1024 * 1024,
                resident_file_objects: if reject_prepare { 0 } else { 4 },
                resident_logical_bytes: 4 * 1024 * 1024,
                reserved_storage_bytes: 64 * 1024 * 1024,
                one_file_logical_bytes: 1024 * 1024,
                one_io_payload_bytes: 1024 * 1024,
                live_handles: 8,
                reader_leases: 4,
                // Bootstrap, maximum payload, and bounded sync/publication workspace.
                resident_memory_bytes: 3 * 1024 * 1024,
                allocation_unit_bytes: 4096,
                per_object_reserve_bytes: 4096,
                child_directory_reserve_bytes: 4096,
                parent_entry_reserve_bytes: 4096,
                bootstrap_memory_bytes: 1024 * 1024,
            },
        },
        deadline(),
    );
    (runtime, owner)
}

#[tokio::test]
async fn zero_typed_software_generation_refuses_before_any_reservation() {
    let root = tempfile::tempdir().expect("actual software generation fixture");
    let (runtime, result) = original_owner_at_generation(root.path(), false, 0);
    let Err(error) = result else {
        panic!("zero typed generation must not allocate an owner");
    };
    assert!(error.definitely_before_reservation());
    assert_eq!(
        runtime.accounting().expect("same empty ledger").used,
        ResourceVector::default()
    );
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole original runtime")
        .drain()
        .expect("original journal drain");
}

async fn finish_same_ticket(
    runtime: Arc<NodeResourceRuntime>,
    owner: SoftwareVodOwner,
    mut ticket: FiniteCloseTicket,
) {
    let charge = runtime.accounting().expect("held original charge").used;
    assert_ne!(charge, ResourceVector::default());
    let output_alias = owner.output();
    let control_alias = owner.control();
    assert!(
        ticket
            .finish_retirement(Instant::now() + Duration::from_millis(1))
            .await
            .is_err(),
        "live original owner blocks final release"
    );
    drop(owner);
    assert!(
        ticket
            .finish_retirement(Instant::now() + Duration::from_millis(1))
            .await
            .is_err(),
        "original output/control aliases still block"
    );
    assert_eq!(
        runtime
            .accounting()
            .expect("same charge while aliases live")
            .used,
        charge
    );
    drop(output_alias);
    drop(control_alias);
    ticket
        .finish_retirement(deadline())
        .await
        .expect("same original receipt/ticket closes after aliases drop");
    assert_eq!(
        runtime.accounting().expect("one final refund").used,
        ResourceVector::default()
    );
    drop(ticket);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("no original app/native selector remains")
        .drain()
        .expect("join original journal");
}

#[tokio::test]
async fn accepted_finite_cleanup_ignores_new_admission_epoch_but_waits_for_original_app_aliases() {
    let root = tempfile::tempdir().expect("durable finite fixture");
    let (runtime, owner) = original_owner(root.path(), false);
    assert!(runtime
        .acknowledge_finite_preparation_for_cleanup(&owner.preparation)
        .await
        .expect("original accepted preparation"));
    runtime
        .arm_scratch_bootstrap(&owner.preparation, 0, owner.scratch_member, deadline())
        .await
        .expect("original committed bootstrap arm, no root entry");
    runtime
        .update_capacity(2, Metric::Known(ResourceVector::default()))
        .expect("new admission fenced to zero");
    let ticket = owner
        .prepare_cancel(deadline())
        .await
        .expect("same accepted original cleanup, no readmission");
    finish_same_ticket(runtime, owner, ticket).await;
}

#[tokio::test]
async fn definitely_unaccepted_finite_prepare_keeps_charge_until_same_no_root_ticket_drops_app_aliases(
) {
    let root = tempfile::tempdir().expect("durable finite fixture");
    let (runtime, owner) = original_owner(root.path(), true);
    assert!(
        owner
            .runtime
            .resolve_preparation(&owner.preparation, deadline())
            .await
            .is_err(),
        "real known prepare rejection"
    );
    assert_ne!(
        runtime
            .accounting()
            .expect("rejected prepare still owns app allocations")
            .used,
        ResourceVector::default()
    );
    let ticket = owner
        .prepare_cancel(deadline())
        .await
        .expect("original known-unaccepted/no-root receipt");
    finish_same_ticket(runtime, owner, ticket).await;
}

#[tokio::test]
async fn cancelled_original_source_root_observation_resumes_before_parent_file_close_without_relaunch(
) {
    let root = tempfile::tempdir().expect("durable actual native fixture");
    let (runtime, mut owner) = original_owner(root.path(), false);
    owner
        .prepare_inputs(deadline())
        .await
        .expect("same actual source/executable Files");
    // start() owns the original output bootstrap before native root entry.
    // Pre-opening it here would re-enter arm_scratch_bootstrap after SinkReady,
    // whose original pending root job has already been acknowledged.
    let gate = Gate::install(owner.source_members[0]);
    {
        let start = owner.start(deadline());
        tokio::pin!(start);
        tokio::select! { _ = gate.entered() => {}, result = &mut start => panic!("must reach original SourceRoot wait: {result:?}") }
    }
    // Dropped borrowed start retains the same queued SourceRoot receiver,
    // original Child/Job and original source Files. Cleanup must acknowledge
    // that receiver before it consumes a parent's actual close inputs.
    drop(gate);
    let charge = runtime.accounting().expect("whole original R held").used;
    let ticket = owner
        .prepare_cancel(deadline())
        .await
        .expect("resume original observation before native close");
    assert_eq!(
        runtime
            .accounting()
            .expect("native close alone does not refund app")
            .used,
        charge
    );
    finish_same_ticket(runtime, owner, ticket).await;
}
