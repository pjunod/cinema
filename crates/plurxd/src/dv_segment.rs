//! Physical, finite FEL helper -> timestamped NUT -> existing encoder ownership.
//! This adapter mints no playback authority or qualification. The backend registry
//! remains closed; a future qualified caller supplies the fenced segment and its
//! existing recipe, concrete admission and producer slot.
#![cfg_attr(not(test), allow(dead_code))]

use crate::{
    admission::{HwSlot, SwPermit, TranscodePermit},
    fragment_index_cluster::SourceFence,
    prodrun::{ProducerRegistration, ProducerSlot, ProducerWriters},
    producer_spawn::{self, Descriptors, Progress, SpawnOptions, Spawned},
};
use std::{
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

const DIAGNOSTIC_LIMIT: u64 = 64 * 1024;
const SCRATCH_LIMIT: u64 = 512 * 1024 * 1024;
const SOURCE_LIMIT: u64 = 64 * 1024 * 1024;

pub(crate) struct SegmentShape {
    pub(crate) video_index: u8,
    pub(crate) max_frames: u8,
    pub(crate) bl: (u32, u32),
    pub(crate) el: (u32, u32),
}
impl SegmentShape {
    fn validate(&self) -> Result<u64, String> {
        let (w, h) = self.bl;
        let (ew, eh) = self.el;
        if !(1..=64).contains(&self.max_frames)
            || !(64..=3840).contains(&w)
            || !(64..=2160).contains(&h)
            || !(32..=3840).contains(&ew)
            || !(32..=2160).contains(&eh)
            || [w, h, ew, eh].iter().any(|v| v % 2 != 0)
            || !((w == ew && h == eh) || (w == 2 * ew && h == 2 * eh))
        {
            return Err("unsupported bounded segment geometry or cadence".into());
        }
        let bytes = u64::from(w) * u64::from(h) * 6 * u64::from(self.max_frames);
        if bytes > SCRATCH_LIMIT {
            return Err("segment exceeds RGB scratch budget".into());
        }
        Ok(bytes)
    }
    fn renderer_args(&self) -> Vec<String> {
        [
            fd(3),
            ".".into(),
            self.video_index.to_string(),
            self.max_frames.to_string(),
            self.bl.0.to_string(),
            self.bl.1.to_string(),
            self.el.0.to_string(),
            self.el.1.to_string(),
            "0".into(),
            "bt2020-pq-master-clip".into(),
        ]
        .into()
    }
    fn mux_args(&self) -> Vec<String> {
        [
            fd(3),
            fd(5),
            fd(4),
            self.bl.0.to_string(),
            self.bl.1.to_string(),
            self.max_frames.to_string(),
        ]
        .into()
    }
}

/// Executable paths are worker configuration, never request-controlled. Recipe
/// argv already selects encoder/output and keeps source time with -copyts. Its
/// NUT video is fd3; original source audio remains fd4 (independent demux input).
pub(crate) struct SegmentRequest {
    pub(crate) renderer: PathBuf,
    pub(crate) muxer: PathBuf,
    pub(crate) encoder: PathBuf,
    pub(crate) encoder_args: Vec<String>,
    pub(crate) runtime_cache: PathBuf,
    pub(crate) shape: SegmentShape,
    pub(crate) source: Arc<SourceFence>,
    pub(crate) source_offsets: Arc<Semaphore>,
    pub(crate) admission: TranscodePermit,
    pub(crate) producer: Arc<ProducerSlot>,
    pub(crate) at: u32,
    pub(crate) deadline: Instant,
    pub(crate) cancel: CancellationToken,
}

/// Same registered pipes/writer barrier the existing producer consumes. On
/// unsuccessful handoff the detached owner retires this exact generation.
pub(crate) struct SegmentProducer {
    registration: ProducerRegistration,
    writers: Option<ProducerWriters>,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    unaccepted_cancel: Option<CancellationToken>,
}
impl SegmentProducer {
    /// Transfer into the existing producer's pipe/writer owner. Until accepted,
    /// dropping the handoff requests retirement of the actual encoder child.
    pub(crate) fn into_parts(
        mut self,
    ) -> (
        ProducerRegistration,
        ProducerWriters,
        tokio::process::ChildStdout,
        tokio::process::ChildStderr,
    ) {
        self.unaccepted_cancel.take();
        (
            self.registration.clone(),
            self.writers.take().expect("segment writers"),
            self.stdout.take().expect("segment stdout"),
            self.stderr.take().expect("segment stderr"),
        )
    }
}
impl Drop for SegmentProducer {
    fn drop(&mut self) {
        if let Some(cancel) = &self.unaccepted_cancel {
            cancel.cancel();
        }
    }
}

struct Resources {
    source: Arc<SourceFence>,
    original_offset: u64,
    _offset_lane: Option<OwnedSemaphorePermit>,
    _hardware: HwSlot,
    _cpu: SwPermit,
    directory: tempfile::TempDir,
}
impl Drop for Resources {
    fn drop(&mut self) {
        // macOS /dev/fd duplicates the held file description. The offset lane
        // remains held until after this restoration, following confirmed reap.
        if (&self.source.handle)
            .seek(SeekFrom::Start(self.original_offset))
            .is_err()
        {
            // A failed restore cannot give another demuxer authority to borrow
            // an unknown shared offset. Keep this one lane closed.
            if let Some(lane) = self._offset_lane.take() {
                lane.forget();
            }
        }
    }
}
fn fd(number: u8) -> String {
    #[cfg(target_os = "linux")]
    {
        format!("/proc/self/fd/{number}")
    }
    #[cfg(not(target_os = "linux"))]
    {
        format!("/dev/fd/{number}")
    }
}
fn check_encoder_budget(args: &[String], cpu: usize) -> Result<(), String> {
    let mut threads = 0usize;
    let mut codec_caps = 0;
    let mut filter_cap = false;
    for pair in args.windows(2) {
        if pair[0] == "-threads"
            || pair[0].starts_with("-threads:")
            || pair[0] == "-filter_threads"
            || pair[0] == "-filter_complex_threads"
        {
            let count = pair[1]
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or("encoder recipe contains an automatic or invalid thread cap")?;
            threads = threads
                .checked_add(count)
                .ok_or("encoder thread budget overflow")?;
            if pair[0].starts_with("-threads") {
                codec_caps += 1;
            }
            if pair[0] == "-filter_threads" {
                filter_cap = true;
            }
        }
    }
    if codec_caps < 2 || !filter_cap || threads > cpu {
        return Err("encoder decoder/filter/codec caps exceed concrete CPU admission".into());
    }
    if args.iter().any(|arg| arg == "libx265") {
        let parameters = args
            .windows(2)
            .find(|pair| pair[0] == "-x265-params")
            .ok_or("x265 recipe needs explicit pool and frame-thread bounds")?;
        let values: Vec<_> = parameters[1].split(':').collect();
        if !values.contains(&"pools=none")
            || !values.contains(&"frame-threads=1")
            || values
                .iter()
                .filter(|value| value.starts_with("pools="))
                .count()
                != 1
            || values
                .iter()
                .filter(|value| value.starts_with("frame-threads="))
                .count()
                != 1
        {
            return Err("x265 recipe must disable pools and cap frame threads to one".into());
        }
    }
    Ok(())
}

fn check_source(source: &SourceFence) -> Result<(), String> {
    source.drift().map_or(Ok(()), |reason| {
        Err(format!("segment source fence: {reason}"))
    })
}

/// Caller cancellation cannot abandon a spawned helper: one detached owner
/// carries every stage through exact-child reap before releasing the graph.
pub(crate) async fn spawn(request: SegmentRequest) -> Result<SegmentProducer, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let cancel = request.cancel.child_token();
    let work_cancel = cancel.clone();
    let receiver_cancel = cancel.clone();
    tokio::spawn(async move {
        let result = run(request, work_cancel).await;
        // A refused oneshot handoff drops SegmentProducer: its cancellation
        // token retires the registered encoder and its abandoned pipes settle.
        let _ = sender.send(result);
    });
    // Dropping this future cancels the owner, without aborting its reap work.
    struct CancelOnDrop(Option<CancellationToken>);
    impl Drop for CancelOnDrop {
        fn drop(&mut self) {
            if let Some(token) = &self.0 {
                token.cancel();
            }
        }
    }
    let mut guard = CancelOnDrop(Some(receiver_cancel));
    let result = receiver
        .await
        .map_err(|_| "segment owner stopped before handoff".to_owned())?;
    if result.is_ok() {
        guard.0.take();
    }
    result
}

async fn run(
    request: SegmentRequest,
    cancel: CancellationToken,
) -> Result<SegmentProducer, String> {
    let raw_limit = request.shape.validate()?;
    let (hardware, cpu) = request.admission.into_parts();
    let hardware = hardware.ok_or("segment requires concrete GPU admission")?;
    let cpu = cpu
        .filter(|permit| permit.threads() >= 2)
        .ok_or("segment requires two admitted decoder threads")?;
    check_encoder_budget(&request.encoder_args, cpu.threads())?;
    if !request.encoder_args.iter().any(|arg| arg == "-copyts")
        || !request
            .encoder_args
            .windows(2)
            .any(|pair| pair == ["-i", fd(3).as_str()])
    {
        return Err("segment encoder recipe must retain NUT source timestamps on fd3".into());
    }
    let lane = tokio::select! {
        _ = cancel.cancelled() => return Err("segment cancelled".into()),
        _ = tokio::time::sleep_until(request.deadline) => return Err("segment deadline expired".into()),
        lane = request.source_offsets.acquire_owned() => lane.map_err(|_| "source offset lane closed")?,
    };
    check_source(&request.source)?;
    let source_size = request
        .source
        .handle
        .metadata()
        .map_err(|e| e.to_string())?
        .len();
    if source_size == 0 || source_size > SOURCE_LIMIT {
        return Err("finite source segment exceeds byte envelope".into());
    }
    let original_offset = (&request.source.handle)
        .stream_position()
        .map_err(|e| e.to_string())?;
    let directory = tempfile::Builder::new()
        .prefix("dv-segment-")
        .tempdir_in(&request.runtime_cache)
        .map_err(|e| e.to_string())?;
    let resources = Arc::new(Resources {
        source: request.source,
        original_offset,
        _offset_lane: Some(lane),
        _hardware: hardware,
        _cpu: cpu,
        directory,
    });
    let held_directory =
        plurx_core::fs_secure::open_directory_nofollow_blocking(resources.directory.path())
            .map_err(|e| e.to_string())?;
    let render = producer_spawn::spawn(
        &request.renderer,
        &request.shape.renderer_args(),
        SpawnOptions {
            runtime_cache: &request.runtime_cache,
            progress: Progress::None,
            descriptors: Descriptors::from_files(
                Some(&resources.source.handle),
                Some(&held_directory),
                None,
                true,
            ),
            env: &[],
            work: crate::process_control::ChildWork::realtime("DV segment reconstruction"),
        },
    )?;
    stage(
        render,
        resources.clone(),
        request.deadline,
        &cancel,
        "renderer",
    )
    .await?;
    check_source(&resources.source)?;
    let rgb = plurx_core::fs_secure::open_read_nofollow_blocking(
        &resources.directory.path().join("reconstructed.rgb48le"),
    )
    .map_err(|e| e.to_string())?;
    let mut timing = plurx_core::fs_secure::open_read_nofollow_blocking(
        &resources.directory.path().join("timing.tsv"),
    )
    .map_err(|e| e.to_string())?;
    let bytes = rgb.metadata().map_err(|e| e.to_string())?.len();
    let frame_bytes = u64::from(request.shape.bl.0) * u64::from(request.shape.bl.1) * 6;
    if bytes == 0 || bytes > raw_limit || bytes % frame_bytes != 0 {
        return Err("renderer RGB shape or frame bound mismatch".into());
    }
    let mut rows = String::new();
    (&mut timing)
        .take(DIAGNOSTIC_LIMIT + 1)
        .read_to_string(&mut rows)
        .map_err(|e| e.to_string())?;
    if rows.len() as u64 > DIAGNOSTIC_LIMIT || rows.lines().count() as u64 != bytes / frame_bytes {
        return Err("renderer timing/frame count mismatch".into());
    }
    timing.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let nut = tempfile::tempfile_in(resources.directory.path()).map_err(|e| e.to_string())?;
    let mux = producer_spawn::spawn(
        &request.muxer,
        &request.shape.mux_args(),
        SpawnOptions {
            runtime_cache: &request.runtime_cache,
            progress: Progress::None,
            descriptors: Descriptors::from_files(Some(&rgb), Some(&nut), Some(&timing), false),
            env: &[],
            work: crate::process_control::ChildWork::realtime("DV timestamped NUT mux"),
        },
    )?;
    stage(mux, resources.clone(), request.deadline, &cancel, "muxer").await?;
    let nut_bytes = nut.metadata().map_err(|e| e.to_string())?.len();
    if nut_bytes == 0 || nut_bytes > raw_limit + 1024 * 1024 {
        return Err("NUT intermediate exceeds bounded output envelope".into());
    }
    (&nut).seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    (&resources.source.handle)
        .seek(SeekFrom::Start(original_offset))
        .map_err(|e| e.to_string())?;
    check_source(&resources.source)?;
    if cancel.is_cancelled() || Instant::now() >= request.deadline {
        return Err("segment cancelled before encoder".into());
    }
    let encoded = producer_spawn::spawn(
        &request.encoder,
        &request.encoder_args,
        SpawnOptions {
            runtime_cache: &request.runtime_cache,
            progress: Progress::Stderr,
            descriptors: Descriptors::from_files(
                Some(&nut),
                Some(&resources.source.handle),
                None,
                false,
            ),
            env: &[],
            work: crate::process_control::ChildWork::realtime("DV segment encoder"),
        },
    )?;
    let (registration, writers) = request
        .producer
        .attach_registered_job_owned(
            encoded.child,
            encoded.child_job,
            request.at,
            Some(Box::new((resources, nut))),
        )
        .await;
    let watched = registration.clone();
    let slot = request.producer;
    let handoff_cancel = cancel.clone();
    tokio::spawn(async move {
        tokio::select! { _ = cancel.cancelled() => {
            let _ = slot.request_registered_retirement(&watched).await;
        }, _ = watched.wait_confirmed_reap() => {} }
    });
    Ok(SegmentProducer {
        registration,
        writers: Some(writers),
        stdout: Some(encoded.stdout),
        stderr: Some(encoded.stderr),
        unaccepted_cancel: Some(handoff_cancel),
    })
}

async fn drain(mut pipe: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    (&mut pipe)
        .take(DIAGNOSTIC_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > DIAGNOSTIC_LIMIT {
        Err("helper diagnostic bound exceeded".into())
    } else {
        Ok(bytes)
    }
}
async fn stage<R: Send + Sync + 'static>(
    spawned: Spawned,
    resources: Arc<R>,
    deadline: Instant,
    cancel: &CancellationToken,
    name: &str,
) -> Result<(), String> {
    let slot = ProducerSlot::new();
    let (registration, writers) = slot
        .attach_registered_job_owned(
            spawned.child,
            spawned.child_job,
            0,
            Some(Box::new(resources)),
        )
        .await;
    let result = {
        let pipes = async { tokio::try_join!(drain(spawned.stdout), drain(spawned.stderr)) };
        tokio::pin!(pipes);
        let exited = async {
            loop {
                if let Some(status) = slot
                    .try_wait_registered(&registration)
                    .await
                    .map_err(|e| e.to_string())?
                {
                    return Ok(status);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let completed = async { tokio::try_join!(exited, &mut pipes) };
        tokio::select! {
            result = completed => result.and_then(|(status,(_,stderr))| {
                if status.success() { Ok(()) } else {
                    Err(format!("{name} {}: {}",if status.code()==Some(1) {"refused segment"} else {"failed"},String::from_utf8_lossy(&stderr)))
                }
            }),
            _ = cancel.cancelled() => Err(format!("{name} cancelled")),
            _ = tokio::time::sleep_until(deadline) => Err(format!("{name} deadline expired")),
        }
        // End pipe futures before releasing the actual writer barrier, including
        // overflow/error/cancel. The registered reaper, not this waiter, owns wait.
    };
    writers.settled();
    slot.request_registered_retirement(&registration)
        .await
        .map_err(|e| e.to_string())?;
    registration.wait_confirmed_reap().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn shape_and_actual_helper_cli_are_bounded() {
        let recipe =
            ["-threads", "1", "-filter_threads", "1", "-threads:v", "1"].map(str::to_owned);
        assert!(check_encoder_budget(&recipe, 3).is_ok());
        assert!(check_encoder_budget(&recipe, 2).is_err());
        assert!(check_encoder_budget(&["-threads".into(), "0".into()], 3).is_err());
        let mut shape = SegmentShape {
            video_index: 2,
            max_frames: 6,
            bl: (64, 64),
            el: (32, 32),
        };
        assert_eq!(shape.validate().expect("bounded raster"), 6 * 64 * 64 * 6);
        assert_eq!(
            shape.renderer_args(),
            [
                fd(3),
                ".".into(),
                "2".into(),
                "6".into(),
                "64".into(),
                "64".into(),
                "32".into(),
                "32".into(),
                "0".into(),
                "bt2020-pq-master-clip".into()
            ]
        );
        assert_eq!(
            shape.mux_args(),
            [fd(3), fd(5), fd(4), "64".into(), "64".into(), "6".into()]
        );
        shape.max_frames = 0;
        assert!(shape.validate().is_err());
        shape.max_frames = 65;
        assert!(shape.validate().is_err());
        shape.max_frames = 64;
        shape.bl = (3840, 2160);
        shape.el = shape.bl;
        assert!(
            shape.validate().is_err(),
            "scratch bounded independently of pictures"
        );
        shape.max_frames = 1;
        shape.el = (1920, 540);
        assert!(
            shape.validate().is_err(),
            "independent layer declarations require supported ratio"
        );
    }

    #[test]
    fn helper_child() {
        match std::env::var("PLURX_DV_SEGMENT_TEST_CHILD").as_deref() {
            Ok("sleep") => std::thread::sleep(Duration::from_secs(60)),
            Ok("refuse") => {
                eprintln!("actual helper refusal");
                std::process::exit(1);
            }
            Ok("overflow") => {
                print!("{}", "x".repeat(DIAGNOSTIC_LIMIT as usize + 1024));
                std::thread::sleep(Duration::from_secs(60));
            }
            Ok("success") => {}
            _ => {}
        }
    }
    fn child(mode: &str, cache: &std::path::Path) -> Spawned {
        producer_spawn::spawn(
            &std::env::current_exe().expect("test executable"),
            &[
                "--exact".into(),
                "dv_segment::tests::helper_child".into(),
                "--nocapture".into(),
            ],
            SpawnOptions {
                runtime_cache: cache,
                progress: Progress::None,
                descriptors: Descriptors::default(),
                env: &[("PLURX_DV_SEGMENT_TEST_CHILD", std::ffi::OsStr::new(mode))],
                work: crate::process_control::ChildWork::realtime("DV lifecycle regression"),
            },
        )
        .expect("real child")
    }
    struct Released(Arc<AtomicBool>);
    impl Drop for Released {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn cancelled_helper_is_reaped_before_resources_release() {
        let cache = tempfile::tempdir().expect("runtime cache");
        let released = Arc::new(AtomicBool::new(false));
        let cancel = CancellationToken::new();
        let run = stage(
            child("sleep", cache.path()),
            Arc::new(Released(released.clone())),
            Instant::now() + Duration::from_secs(5),
            &cancel,
            "renderer",
        );
        tokio::pin!(run);
        tokio::select! { result=&mut run => panic!("helper unexpectedly ended: {result:?}"),
        _=tokio::time::sleep(Duration::from_millis(50))=>{} }
        assert!(!released.load(Ordering::SeqCst));
        cancel.cancel();
        assert!(run
            .await
            .expect_err("cancelled helper")
            .contains("cancelled"));
        assert!(
            released.load(Ordering::SeqCst),
            "returns only after registered child reap and writers ended"
        );
    }
    #[tokio::test]
    async fn refusal_deadline_and_diagnostic_overflow_retire_owned_children() {
        let cache = tempfile::tempdir().expect("runtime cache");
        for (mode, timeout, expected) in [
            ("refuse", 5000, "refused segment"),
            ("sleep", 50, "deadline expired"),
            ("overflow", 5000, "diagnostic bound exceeded"),
        ] {
            let released = Arc::new(AtomicBool::new(false));
            let error = stage(
                child(mode, cache.path()),
                Arc::new(Released(released.clone())),
                Instant::now() + Duration::from_millis(timeout),
                &CancellationToken::new(),
                "renderer",
            )
            .await
            .expect_err("bounded helper failure");
            assert!(error.contains(expected), "{mode}: {error}");
            assert!(
                released.load(Ordering::SeqCst),
                "{mode}: confirmed resource release"
            );
        }
    }
    // Runs the physical renderer/muxer/encoder graph only when the reviewed
    // Linux helper runtime is explicitly supplied. It grants no route receipt.
    #[tokio::test]
    #[ignore = "requires reviewed Linux FEL helper and bounded timestamped source"]
    async fn actual_segment_helper_nut_encoder_preserves_owned_custody() {
        use crate::admission::{Admissions, Priority, TranscodeResourceEstimate};
        use plurx_core::domain::MediaFile;
        let source_path =
            PathBuf::from(std::env::var("PLURX_DV_SEGMENT_SOURCE").expect("actual finite source"));
        let metadata = std::fs::metadata(&source_path).expect("source metadata");
        let file = MediaFile {
            id: 1,
            item_id: 1,
            path: source_path,
            size: metadata.len() as i64,
            mtime: metadata
                .modified()
                .expect("mtime")
                .duration_since(std::time::UNIX_EPOCH)
                .expect("epoch")
                .as_secs() as i64,
            duration_ms: Some(250),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: None,
            width: Some(64),
            height: Some(64),
            bit_depth: Some(10),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            dolby_vision: Default::default(),
            bitrate: None,
            audio_streams: vec![],
            subtitle_streams: vec![],
            downloaded_subtitles: vec![],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
        };
        let source = Arc::new(
            crate::fragment_index_cluster::open_source_fence(&file, None)
                .await
                .expect("held actual source"),
        );
        let original = (&source.handle).stream_position().expect("original offset");
        let offsets = Arc::new(Semaphore::new(1));
        let admissions = Admissions::new();
        let estimate = TranscodeResourceEstimate {
            hardware_slot: true,
            cpu_threads: 3,
            decoder_threads: Some(2),
        };
        let admission = admissions
            .try_admit_bundle(1, 3, &estimate, Priority::Live)
            .expect("concrete graph admission");
        let cache = tempfile::tempdir().expect("private graph cache");
        let slot = Arc::new(ProducerSlot::new());
        let args: Vec<String>=["-nostdin","-v","error","-threads","1","-copyts","-i",&fd(3),"-filter_threads","1",
            "-vf","zscale=matrixin=gbr:transferin=smpte2084:primariesin=2020:rangein=full:matrix=2020_ncl:transfer=smpte2084:primaries=2020:range=limited:chromal=center:filter=point,format=yuv420p10le",
            "-c:v","libx265","-threads","1","-profile:v","main10","-x265-params",
            "pools=none:frame-threads=1:qp=0:bframes=0:repeat-headers=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:range=limited:chromaloc=1",
            "-fps_mode","passthrough","-enc_time_base","1/1000","-an","-f","matroska","pipe:1"].into_iter().map(str::to_owned).collect();
        let producer = spawn(SegmentRequest {
            renderer: std::env::var("PLURX_DV_SEGMENT_RENDERER")
                .expect("renderer")
                .into(),
            muxer: std::env::var("PLURX_DV_SEGMENT_MUXER")
                .expect("muxer")
                .into(),
            encoder: "/usr/bin/ffmpeg".into(),
            encoder_args: args.clone(),
            runtime_cache: cache.path().to_owned(),
            shape: SegmentShape {
                video_index: 0,
                max_frames: 6,
                bl: (64, 64),
                el: (64, 64),
            },
            source: source.clone(),
            source_offsets: offsets.clone(),
            admission,
            producer: slot.clone(),
            at: 0,
            deadline: Instant::now() + Duration::from_secs(30),
            cancel: CancellationToken::new(),
        })
        .await
        .expect("physical graph handoff");
        let (registration, writers, stdout, stderr) = producer.into_parts();
        assert_eq!(
            offsets.available_permits(),
            0,
            "source offset custody follows encoder"
        );
        assert!(
            admissions
                .try_admit_bundle(1, 3, &estimate, Priority::Live)
                .is_none(),
            "actual graph still owns GPU and CPU"
        );
        let (encoded, diagnostic) =
            tokio::try_join!(drain(stdout), drain(stderr)).expect("encoder pipes");
        let status = loop {
            if let Some(status) = slot
                .try_wait_registered(&registration)
                .await
                .expect("exact encoder wait")
            {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert!(
            status.success(),
            "encoder: {}",
            String::from_utf8_lossy(&diagnostic)
        );
        assert!(encoded.len() > 100, "actual encoded Matroska output");
        assert!(source.unchanged(), "same source revision retained");
        assert!(
            admissions
                .try_admit_bundle(1, 3, &estimate, Priority::Live)
                .is_none(),
            "exited child alone does not release writer admission"
        );
        writers.settled();
        slot.request_registered_retirement(&registration)
            .await
            .expect("retire actual encoder");
        assert!(registration
            .wait_confirmed_reap()
            .await
            .matches(&registration));
        assert_eq!(offsets.available_permits(), 1);
        assert_eq!(
            (&source.handle).stream_position().expect("restored offset"),
            original
        );
        assert!(
            admissions
                .try_admit_bundle(1, 3, &estimate, Priority::Live)
                .is_some(),
            "confirmed graph releases concrete admission"
        );
        let unaccepted = spawn(SegmentRequest {
            renderer: std::env::var("PLURX_DV_SEGMENT_RENDERER")
                .expect("renderer")
                .into(),
            muxer: std::env::var("PLURX_DV_SEGMENT_MUXER")
                .expect("muxer")
                .into(),
            encoder: "/usr/bin/ffmpeg".into(),
            encoder_args: args,
            runtime_cache: cache.path().to_owned(),
            shape: SegmentShape {
                video_index: 0,
                max_frames: 6,
                bl: (64, 64),
                el: (64, 64),
            },
            source: source.clone(),
            source_offsets: offsets.clone(),
            admission: admissions
                .try_admit_bundle(1, 3, &estimate, Priority::Live)
                .expect("second actual graph admission"),
            producer: slot.clone(),
            at: 1,
            deadline: Instant::now() + Duration::from_secs(30),
            cancel: CancellationToken::new(),
        })
        .await
        .expect("actual unaccepted encoder handoff");
        let abandoned = unaccepted.registration.clone();
        assert_eq!(offsets.available_permits(), 0);
        drop(unaccepted);
        let receipt = tokio::time::timeout(Duration::from_secs(5), abandoned.wait_confirmed_reap())
            .await
            .expect("dropped actual handoff retires encoder");
        assert!(receipt.matches(&abandoned));
        assert_eq!(
            receipt.writers(),
            crate::prodrun::WriterSettlement::Abandoned
        );
        assert_eq!(offsets.available_permits(), 1);
        assert!(admissions
            .try_admit_bundle(1, 3, &estimate, Priority::Live)
            .is_some());
        assert!(source.unchanged());
        if let Ok(path) = std::env::var("PLURX_DV_SEGMENT_ENCODED_OUTPUT") {
            std::fs::write(path, encoded).expect("save actual graph observation");
        }
    }
}
