//! Owned FEL helper -> timestamped NUT -> existing encoder, finite or streaming.
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
#[cfg(any(target_os = "linux", test))]
const WINDOW_TRACE_LIMIT: u64 = 128 * 1024;
const SCRATCH_LIMIT: u64 = 512 * 1024 * 1024;
const SOURCE_LIMIT: u64 = 64 * 1024 * 1024;

pub(crate) struct SegmentShape {
    pub(crate) video_index: u8,
    pub(crate) max_frames: u8,
    pub(crate) bl: (u32, u32),
    pub(crate) el: (u32, u32),
}
impl SegmentShape {
    fn frame_bytes(&self) -> Result<u64, String> {
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
        Ok(u64::from(w) * u64::from(h) * 6)
    }
    fn validate(&self) -> Result<u64, String> {
        let bytes = self.frame_bytes()? * u64::from(self.max_frames);
        if bytes > SCRATCH_LIMIT {
            return Err("segment exceeds RGB scratch budget".into());
        }
        Ok(bytes)
    }
    fn renderer_args(&self) -> Vec<String> {
        [
            fd(3),
            if cfg!(target_os = "linux") {
                fd(4)
            } else {
                ".".into()
            },
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
    /// Bounds renderer/mux preparation up to encoder handoff. The existing
    /// producer controller owns encoder cancellation, suspension and retirement.
    pub(crate) preparation_deadline: Instant,
    pub(crate) cancel: CancellationToken,
}

/// Private bounded observations from the actual renderer. Retains artifact
/// lifetime after reap without retaining GPU/CPU admission or source borrowing.
#[cfg(target_os = "linux")]
pub(crate) struct SegmentEvidence {
    directory: Arc<tempfile::TempDir>,
    held: Arc<std::fs::File>,
}
#[cfg(target_os = "linux")]
impl SegmentEvidence {
    fn open(
        directory: &std::fs::File,
        name: &std::ffi::CStr,
        flags: i32,
    ) -> Result<std::fs::File, String> {
        use std::os::fd::{AsRawFd, FromRawFd};
        // SAFETY: the held directory owns its descriptor; name is terminated,
        // and a successful openat returns a fresh descriptor owned below.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }
    fn read(
        directory: &std::fs::File,
        name: &std::ffi::CStr,
        limit: u64,
    ) -> Result<Vec<u8>, String> {
        let file = Self::open(directory, name, libc::O_RDONLY)?;
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("renderer evidence is not a regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > limit {
            return Err("renderer evidence bound exceeded".into());
        }
        Ok(bytes)
    }
    pub(crate) fn timing(&self) -> Result<Vec<u8>, String> {
        Self::read(&self.held, c"timing.tsv", DIAGNOSTIC_LIMIT)
    }
    pub(crate) fn renderer_events(&self) -> Result<Vec<u8>, String> {
        Self::read(&self.held, c"renderer.jsonl", WINDOW_TRACE_LIMIT)
    }
    pub(crate) fn rpu(&self, frame: u8) -> Result<Vec<u8>, String> {
        if frame >= 64 {
            return Err("renderer RPU index exceeds picture bound".into());
        }
        let directory = Self::open(&self.held, c"rpus", libc::O_RDONLY | libc::O_DIRECTORY)?;
        let name =
            std::ffi::CString::new(format!("frame-{frame:03}.nal")).expect("bounded RPU name");
        Self::read(&directory, &name, 16 * 1024 * 1024)
    }
}

/// Same registered pipes/writer barrier the existing producer consumes. On
/// unsuccessful handoff the detached owner retires this exact generation.
pub(crate) struct SegmentProducer {
    registration: ProducerRegistration,
    writers: Option<ProducerWriters>,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    unaccepted_cancel: Option<CancellationToken>,
    helper_completion: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
    #[cfg(target_os = "linux")]
    evidence: Option<SegmentEvidence>,
}
impl SegmentProducer {
    #[cfg(target_os = "linux")]
    pub(crate) fn take_evidence(&mut self) -> Option<SegmentEvidence> {
        self.evidence.take()
    }

    /// Reports actual helper exit, source fencing and bounded observation capture.
    /// The consumer must validate those observations and final encoder bytes
    /// before publishing an interval or effective-processing report.
    pub(crate) fn take_helper_completion(
        &mut self,
    ) -> Option<tokio::sync::oneshot::Receiver<Result<(), String>>> {
        self.helper_completion.take()
    }

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
    directory: Arc<tempfile::TempDir>,
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
fn one_thread_cap(scope: &[String], flags: &[&str]) -> Result<usize, String> {
    let caps: Vec<_> = scope
        .windows(2)
        .filter(|p| flags.contains(&p[0].as_str()))
        .collect();
    if caps.len() != 1 {
        return Err("encoder recipe needs one unambiguous scoped thread cap".into());
    }
    caps[0][1]
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| "encoder recipe has an automatic or invalid thread cap".into())
}

fn check_encoder_budget(args: &[String], cpu: usize) -> Result<(), String> {
    // Only the concrete finite x265 recipe is supported. Input decoder options
    // precede their own -i; output codec caps follow the last input.
    let inputs: Vec<_> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "-i")
        .map(|(i, _)| i)
        .collect();
    if !(1..=2).contains(&inputs.len())
        || args.get(inputs[0] + 1) != Some(&fd(3))
        || (inputs.len() == 2 && args.get(inputs[1] + 1) != Some(&fd(4)))
        || args
            .iter()
            .any(|a| a.starts_with("-threads:") && a != "-threads:v" && a != "-threads:a")
    {
        return Err("unsupported segment encoder input or thread scope".into());
    }
    let mut threads = 0usize;
    let mut start = 0;
    for &input in &inputs {
        let scope = &args[start..input];
        if scope.iter().any(|a| a.starts_with("-threads:")) {
            return Err("segment inputs require one generic decoder thread cap".into());
        }
        threads = threads
            .checked_add(one_thread_cap(scope, &["-threads"])?)
            .ok_or("thread budget overflow")?;
        start = input + 2;
    }
    let output = &args[start..];
    threads = threads
        .checked_add(one_thread_cap(output, &["-threads", "-threads:v"])?)
        .ok_or("thread budget overflow")?;
    let codecs: Vec<_> = output.windows(2).filter(|p| p[0] == "-c:v").collect();
    if codecs.len() != 1
        || codecs[0][1] != "libx265"
        || args.iter().any(|a| {
            a == "-codec"
                || a == "-c"
                || a == "-vcodec"
                || a == "-acodec"
                || a.starts_with("-codec:")
                || (a.starts_with("-c:") && a != "-c:v" && a != "-c:a")
                || (a.starts_with("-x265-params") && a != "-x265-params")
        })
    {
        return Err("finite segment adapter requires its bounded x265 video recipe".into());
    }
    let audio: Vec<_> = output.windows(2).filter(|p| p[0] == "-c:a").collect();
    if audio.len() > 1 || (inputs.len() == 2 && audio.is_empty()) {
        return Err("ambiguous audio encoder".into());
    }
    if audio.first().is_some_and(|p| p[1] != "copy") {
        if audio[0][1] != "aac" || inputs.len() != 2 {
            return Err("unsupported bounded audio recipe".into());
        }
        threads = threads
            .checked_add(one_thread_cap(output, &["-threads:a"])?)
            .ok_or("thread budget overflow")?;
    } else if output.iter().any(|a| a == "-threads:a") {
        return Err("unexpected audio thread cap".into());
    }
    threads = threads
        .checked_add(one_thread_cap(args, &["-filter_threads"])?)
        .ok_or("thread budget overflow")?;
    if args
        .iter()
        .any(|a| a == "-filter_complex" || a == "-filter_complex_threads")
    {
        threads = threads
            .checked_add(one_thread_cap(args, &["-filter_complex_threads"])?)
            .ok_or("thread budget overflow")?;
    }
    if threads > cpu {
        return Err("encoder scopes exceed concrete CPU admission".into());
    }
    let params: Vec<_> = output
        .windows(2)
        .filter(|p| p[0] == "-x265-params")
        .collect();
    if params.len() != 1 {
        return Err("x265 needs one explicit pool/frame-thread bound".into());
    }
    let values: Vec<_> = params[0][1].split(':').collect();
    if !values.contains(&"pools=none")
        || !values.contains(&"frame-threads=1")
        || values.iter().filter(|v| v.starts_with("pools=")).count() != 1
        || values
            .iter()
            .filter(|v| v.starts_with("frame-threads="))
            .count()
            != 1
    {
        return Err("x265 must disable pools and cap frame threads to one".into());
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
    let reservation = request
        .producer
        .try_reserve_empty()
        .map_err(|e| e.to_string())?;
    let lane = tokio::select! {
        _ = cancel.cancelled() => return Err("segment cancelled".into()),
        _ = tokio::time::sleep_until(request.preparation_deadline) => return Err("segment deadline expired".into()),
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
        directory: Arc::new(directory),
    });
    // /dev/fd on macOS shares the description's offset. A fresh demuxer
    // starts at the segment beginning while this graph owns the lane.
    (&resources.source.handle)
        .seek(SeekFrom::Start(0))
        .map_err(|e| e.to_string())?;
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
        request.preparation_deadline,
        &cancel,
        "renderer",
        DIAGNOSTIC_LIMIT,
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
    stage(
        mux,
        resources.clone(),
        request.preparation_deadline,
        &cancel,
        "muxer",
        DIAGNOSTIC_LIMIT,
    )
    .await?;
    let nut_bytes = nut.metadata().map_err(|e| e.to_string())?.len();
    if nut_bytes == 0 || nut_bytes > raw_limit + 1024 * 1024 {
        return Err("NUT intermediate exceeds bounded output envelope".into());
    }
    (&nut).seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    (&resources.source.handle)
        .seek(SeekFrom::Start(0))
        .map_err(|e| e.to_string())?;
    check_source(&resources.source)?;
    if cancel.is_cancelled() || Instant::now() >= request.preparation_deadline {
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
    let (registration, writers) = reservation.attach_registered_job_owned(
        encoded.child,
        encoded.child_job,
        request.at,
        Box::new((resources, nut)),
    );
    let watched = registration.clone();
    // The observer must not keep the controlling slot alive after its caller
    // disappears: ProducerSlot::drop owns actual retirement in that case.
    let slot = Arc::downgrade(&request.producer);
    let handoff_cancel = cancel.clone();
    tokio::spawn(async move {
        tokio::select! { _ = cancel.cancelled() => {
            if let Some(slot) = slot.upgrade() {
                let _ = slot.request_registered_retirement(&watched).await;
            }
        }, _ = watched.wait_confirmed_reap() => {} }
    });
    Ok(SegmentProducer {
        registration,
        writers: Some(writers),
        stdout: Some(encoded.stdout),
        stderr: Some(encoded.stderr),
        unaccepted_cancel: Some(handoff_cancel),
        helper_completion: None,
        #[cfg(target_os = "linux")]
        evidence: None,
    })
}

/// Exact original-source presentation interval. This is a processing window,
/// never a replacement source identity or backend qualification.
#[cfg(target_os = "linux")]
pub(crate) struct SegmentWindow {
    pub(crate) start: (i64, u32),
    pub(crate) end: (i64, u32),
    pub(crate) max_preroll: u16,
}
#[cfg(target_os = "linux")]
impl SegmentWindow {
    fn args(&self) -> Result<[String; 3], String> {
        let (start, sd) = self.start;
        let (end, ed) = self.end;
        if start < 0
            || end < 0
            || sd == 0
            || ed == 0
            || sd > i32::MAX as u32
            || ed > i32::MAX as u32
            || i128::from(start) * i128::from(ed) >= i128::from(end) * i128::from(sd)
            || i128::from(end) * i128::from(sd) - i128::from(start) * i128::from(ed)
                > 3 * i128::from(sd) * i128::from(ed)
            || self.max_preroll > 512
        {
            return Err("invalid bounded original-source interval".into());
        }
        Ok([
            format!("{start}/{sd}"),
            format!("{end}/{ed}"),
            self.max_preroll.to_string(),
        ])
    }
}

/// Direct timestamped NUT transport. The renderer and encoder run concurrently
/// under one GPU slot and the sum of their concrete CPU budgets. This still
/// requires a finite fenced segment; it does not seek or qualify a movie.
#[cfg(target_os = "linux")]
pub(crate) struct StreamingSegmentRequest {
    pub(crate) request: SegmentRequest,
    /// Renderer completion must be bounded even after encoder handoff.
    pub(crate) renderer_deadline: Instant,
    pub(crate) window: Option<SegmentWindow>,
}

#[cfg(target_os = "linux")]
pub(crate) async fn spawn_streaming(
    request: StreamingSegmentRequest,
) -> Result<SegmentProducer, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let cancel = request.request.cancel.child_token();
    let owner_cancel = cancel.clone();
    tokio::spawn(async move {
        let _ = sender.send(run_streaming(request, owner_cancel).await);
    });
    struct CancelOnDrop(Option<CancellationToken>);
    impl Drop for CancelOnDrop {
        fn drop(&mut self) {
            if let Some(cancel) = &self.0 {
                cancel.cancel();
            }
        }
    }
    let mut guard = CancelOnDrop(Some(cancel));
    let result = receiver
        .await
        .map_err(|_| "streaming segment owner ended without handoff".to_owned())?;
    guard.0.take();
    result
}

#[cfg(target_os = "linux")]
async fn run_streaming(
    stream: StreamingSegmentRequest,
    cancel: CancellationToken,
) -> Result<SegmentProducer, String> {
    let window_args = stream
        .window
        .as_ref()
        .map(SegmentWindow::args)
        .transpose()?;
    let windowed = window_args.is_some();
    let request = stream.request;
    request.shape.frame_bytes()?;
    let (hardware, cpu) = request.admission.into_parts();
    let hardware = hardware.ok_or("streaming segment requires concrete GPU admission")?;
    let cpu = cpu
        .filter(|permit| permit.threads() >= 5)
        .ok_or("concurrent renderer and encoder require at least five admitted CPU threads")?;
    check_encoder_budget(&request.encoder_args, cpu.threads() - 2)?;
    if !request.encoder_args.iter().any(|a| a == "-copyts")
        || !request
            .encoder_args
            .windows(2)
            .any(|p| p == ["-enc_time_base", "-1"])
    {
        return Err("streaming encoder must retain the actual NUT timestamp base".into());
    }
    let reservation = request
        .producer
        .try_reserve_empty()
        .map_err(|e| e.to_string())?;
    let lane = tokio::select! {
        _ = cancel.cancelled() => return Err("streaming segment cancelled".into()),
        _ = tokio::time::sleep_until(request.preparation_deadline) => return Err("streaming segment handoff deadline expired".into()),
        lane = request.source_offsets.acquire_owned() => lane.map_err(|_| "source offset lane closed")?,
    };
    check_source(&request.source)?;
    let bytes = request
        .source
        .handle
        .metadata()
        .map_err(|e| e.to_string())?
        .len();
    // Window mode keeps the original movie descriptor. The helper bounds all
    // header/probe/preroll/window reads together at 128MiB; seeks never reset it.
    if bytes == 0 || (!windowed && bytes > SOURCE_LIMIT) {
        return Err("finite streaming source exceeds byte envelope".into());
    }
    let original_offset = (&request.source.handle)
        .stream_position()
        .map_err(|e| e.to_string())?;
    let directory = tempfile::Builder::new()
        .prefix("dv-stream-")
        .tempdir_in(&request.runtime_cache)
        .map_err(|e| e.to_string())?;
    let resources = Arc::new(Resources {
        source: request.source,
        original_offset,
        _offset_lane: Some(lane),
        _hardware: hardware,
        _cpu: cpu,
        directory: Arc::new(directory),
    });
    (&resources.source.handle)
        .seek(SeekFrom::Start(0))
        .map_err(|e| e.to_string())?;
    let directory =
        plurx_core::fs_secure::open_directory_nofollow_blocking(resources.directory.path())
            .map_err(|e| e.to_string())?;
    let evidence = SegmentEvidence {
        directory: resources.directory.clone(),
        held: Arc::new(directory.try_clone().map_err(|e| e.to_string())?),
    };
    let evidence_hold = evidence.held.clone();
    let evidence_lease = evidence.directory.clone();
    let (read, write) = std::io::pipe().map_err(|e| e.to_string())?;
    let read: std::fs::File = std::os::fd::OwnedFd::from(read).into();
    let write: std::fs::File = std::os::fd::OwnedFd::from(write).into();
    let mut args = request.shape.renderer_args();
    args.push(fd(5));
    if let Some(window_args) = window_args {
        args.extend(window_args);
    }
    let render = producer_spawn::spawn(
        &request.renderer,
        &args,
        SpawnOptions {
            runtime_cache: &request.runtime_cache,
            progress: Progress::None,
            descriptors: Descriptors::from_files(
                Some(&resources.source.handle),
                Some(&directory),
                Some(&write),
                true,
            ),
            env: &[],
            work: crate::process_control::ChildWork::realtime("DV streaming reconstruction"),
        },
    )?;
    drop(write); // Only the real renderer owns the writer; its exit delivers EOF.
    let helper_cancel = cancel.child_token();
    let stage_cancel = helper_cancel.clone();
    let stage_resources = resources.clone();
    let helper = tokio::spawn(async move {
        stage(
            render,
            stage_resources,
            stream.renderer_deadline,
            &stage_cancel,
            "streaming renderer",
            WINDOW_TRACE_LIMIT,
        )
        .await
    });
    let encoded = check_source(&resources.source).and_then(|()| {
        if cancel.is_cancelled() || Instant::now() >= request.preparation_deadline {
            return Err("streaming segment cancelled before encoder".into());
        }
        producer_spawn::spawn(
            &request.encoder,
            &request.encoder_args,
            SpawnOptions {
                runtime_cache: &request.runtime_cache,
                progress: Progress::Stderr,
                descriptors: Descriptors::from_files(
                    Some(&read),
                    Some(&resources.source.handle),
                    None,
                    false,
                ),
                env: &[],
                work: crate::process_control::ChildWork::realtime("DV streaming encoder"),
            },
        )
    });
    drop(read); // No parent reader can prevent SIGPIPE after encoder failure.
    let encoded = match encoded {
        Ok(encoded) => encoded,
        Err(error) => {
            helper_cancel.cancel();
            let _ = helper.await;
            return Err(error);
        }
    };
    let watched_source = resources.source.clone();
    let (registration, writers) = reservation.attach_registered_job_owned(
        encoded.child,
        encoded.child_job,
        request.at,
        Box::new(resources),
    );
    let watched = registration.clone();
    let slot = Arc::downgrade(&request.producer);
    let handoff_cancel = cancel.clone();
    let (completed, completion) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut helper = helper;
        let helper_result = tokio::select! {
            result = &mut helper => result.unwrap_or_else(|e| Err(format!("streaming renderer owner failed: {e}"))),
            _ = cancel.cancelled() => { helper_cancel.cancel(); if let Some(slot) = slot.upgrade() { let _ = slot.request_registered_retirement(&watched).await; } helper.await.unwrap_or_else(|e| Err(format!("streaming renderer owner failed: {e}"))) },
            _ = watched.wait_confirmed_reap() => { helper_cancel.cancel(); helper.await.unwrap_or_else(|e| Err(format!("streaming renderer owner failed: {e}"))) },
        };
        let helper_result = helper_result.and_then(|observations| {
            check_source(&watched_source)?;
            use std::io::Write;
            let _lease = (&evidence_lease, &evidence_hold);
            let mut file = SegmentEvidence::open(
                &evidence_hold,
                c"renderer.jsonl",
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            )?;
            file.write_all(&observations).map_err(|e| e.to_string())
        });
        let failed = helper_result.is_err();
        let _ = completed.send(helper_result);
        if failed || cancel.is_cancelled() {
            if let Some(slot) = slot.upgrade() {
                let _ = slot.request_registered_retirement(&watched).await;
            }
        } else {
            tokio::select! {
                _ = cancel.cancelled() => { if let Some(slot) = slot.upgrade() { let _ = slot.request_registered_retirement(&watched).await; } },
                _ = watched.wait_confirmed_reap() => {},
            }
        }
    });
    Ok(SegmentProducer {
        registration,
        writers: Some(writers),
        stdout: Some(encoded.stdout),
        stderr: Some(encoded.stderr),
        unaccepted_cancel: Some(handoff_cancel),
        helper_completion: Some(completion),
        evidence: Some(evidence),
    })
}

async fn drain(pipe: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>, String> {
    drain_bounded(pipe, DIAGNOSTIC_LIMIT).await
}
async fn drain_bounded(
    mut pipe: impl tokio::io::AsyncRead + Unpin,
    limit: u64,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    (&mut pipe)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
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
    trace_limit: u64,
) -> Result<Vec<u8>, String> {
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
        let pipes = async {
            tokio::try_join!(
                drain_bounded(spawned.stdout, trace_limit),
                drain(spawned.stderr)
            )
        };
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
            result = completed => result.and_then(|(status,(stdout,stderr))| {
                if status.success() { Ok(stdout) } else {
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

#[cfg(any(test, plurx_dv_segment_probe))]
async fn probe_spawn(request: SegmentRequest, streaming: bool) -> Result<SegmentProducer, String> {
    if streaming {
        #[cfg(target_os = "linux")]
        return spawn_streaming(StreamingSegmentRequest {
            request,
            renderer_deadline: Instant::now() + Duration::from_secs(30),
            window: std::env::var("PLURX_DV_SEGMENT_WINDOW_START")
                .ok()
                .map(|start| {
                    let time = |value: &str| {
                        let (n, d) = value.split_once('/').expect("exact rational window");
                        (
                            n.parse().expect("window numerator"),
                            d.parse().expect("window denominator"),
                        )
                    };
                    SegmentWindow {
                        start: time(&start),
                        end: time(
                            &std::env::var("PLURX_DV_SEGMENT_WINDOW_END").expect("window end"),
                        ),
                        max_preroll: 512,
                    }
                }),
        })
        .await;
        #[cfg(not(target_os = "linux"))]
        return Err("streaming physical probe requires Linux".into());
    }
    spawn(request).await
}

#[cfg(all(target_os = "linux", any(test, plurx_dv_segment_probe)))]
fn window_argument_regression() {
    let mut window = SegmentWindow {
        start: (42, 1000),
        end: (167, 1000),
        max_preroll: 512,
    };
    assert_eq!(
        window.args().expect("bounded original-source window"),
        ["42/1000", "167/1000", "512"]
    );
    let film_entry = SegmentWindow {
        start: (0, 1),
        end: (48 * 1001, 24000),
        max_preroll: 512,
    };
    assert!(
        film_entry.args().is_ok(),
        "48 film frames span exactly 2.002s"
    );
    window.end = (3043, 1000);
    assert!(
        window.args().is_err(),
        "more than three seconds refused exactly"
    );
    window.end = (42, 1000);
    assert!(window.args().is_err());
    window.end = (167, 1000);
    window.start = (-1, 1000);
    assert!(window.args().is_err());
    window.start = (42, 0);
    assert!(window.args().is_err());
    window.start = (42, 1000);
    window.max_preroll = 513;
    assert!(window.args().is_err());
    window.max_preroll = 0;
    assert!(
        window.args().is_ok(),
        "zero preroll is a meaningful strict budget"
    );
}

/// Explicit compile-time physical probe, never a daemon route or receipt.
#[cfg(any(test, plurx_dv_segment_probe))]
pub(crate) async fn physical_probe() {
    #[cfg(target_os = "linux")]
    window_argument_regression();
    use crate::admission::{Admissions, Priority, TranscodeResourceEstimate};
    use plurx_core::domain::MediaFile;
    let streaming = std::env::var("PLURX_DV_SEGMENT_STREAMING").is_ok_and(|v| v == "1");
    let cpu_threads = if streaming { 5 } else { 3 };
    let max_frames = std::env::var("PLURX_DV_SEGMENT_MAX_FRAMES")
        .map(|v| v.parse::<u8>().expect("bounded picture count"))
        .unwrap_or(6);
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
    (&source.handle)
        .seek(SeekFrom::Start(7))
        .expect("nonzero borrowed offset");
    let original = (&source.handle).stream_position().expect("original offset");
    let offsets = Arc::new(Semaphore::new(1));
    let admissions = Admissions::new();
    let estimate = TranscodeResourceEstimate {
        hardware_slot: true,
        cpu_threads,
        decoder_threads: Some(2),
    };
    let admission = admissions
        .try_admit_bundle(1, cpu_threads, &estimate, Priority::Live)
        .expect("concrete graph admission");
    let cache = tempfile::tempdir().expect("private graph cache");
    let slot = Arc::new(ProducerSlot::new());
    let args: Vec<String>=["-nostdin","-v","error","-threads","1","-copyts","-i",&fd(3),"-filter_threads","1",
    "-vf","zscale=matrixin=gbr:transferin=smpte2084:primariesin=2020:rangein=full:matrix=2020_ncl:transfer=smpte2084:primaries=2020:range=limited:chromal=center:filter=point,format=yuv420p10le",
    "-c:v","libx265","-threads","1","-profile:v","main10","-x265-params",
    "pools=none:frame-threads=1:qp=0:bframes=0:repeat-headers=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:range=limited:chromaloc=1",
    "-fps_mode","passthrough","-enc_time_base","-1","-an","-f","matroska","pipe:1"].into_iter().map(str::to_owned).collect();
    // A real retired predecessor with held writers must refuse a cancelled
    // successor before source/helper/encoder ownership starts.
    let predecessor = tokio::process::Command::new("/bin/sleep")
        .arg("60")
        .spawn()
        .expect("real predecessor");
    let predecessor_job =
        crate::process_control::ChildJob::attach(&predecessor).expect("owned predecessor");
    let (old, old_writers) = slot
        .attach_registered_job_owned(predecessor, predecessor_job, 0, None)
        .await;
    slot.request_registered_retirement(&old)
        .await
        .expect("retire predecessor");
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let refused = tokio::time::timeout(
        Duration::from_millis(500),
        probe_spawn(
            SegmentRequest {
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
                    max_frames,
                    bl: (64, 64),
                    el: (64, 64),
                },
                source: source.clone(),
                source_offsets: offsets.clone(),
                admission,
                producer: slot.clone(),
                at: 0,
                preparation_deadline: Instant::now() + Duration::from_secs(30),
                cancel: cancelled,
            },
            streaming,
        ),
    )
    .await
    .expect("held predecessor never parks successor");
    match refused {
        Err(error) => assert!(error.contains("producer slot is not ready"), "{error}"),
        Ok(_) => panic!("busy predecessor admitted successor"),
    }
    assert_eq!(offsets.available_permits(), 1);
    assert_eq!(
        (&source.handle)
            .stream_position()
            .expect("unborrowed offset"),
        original
    );
    let admission = admissions
        .try_admit_bundle(1, cpu_threads, &estimate, Priority::Live)
        .expect("refused successor returns real capacity");
    old_writers.settled();
    old.wait_confirmed_reap().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if slot.try_reserve_empty().is_ok() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("predecessor slot fully settled");
    let mut producer = probe_spawn(
        SegmentRequest {
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
                max_frames,
                bl: (64, 64),
                el: (64, 64),
            },
            source: source.clone(),
            source_offsets: offsets.clone(),
            admission,
            producer: slot.clone(),
            at: 0,
            preparation_deadline: Instant::now() + Duration::from_secs(30),
            cancel: CancellationToken::new(),
        },
        streaming,
    )
    .await
    .expect("physical graph handoff");
    #[cfg(target_os = "linux")]
    let evidence = producer.take_evidence();
    let completion = producer.take_helper_completion();
    let (registration, writers, stdout, stderr) = producer.into_parts();
    assert_eq!(
        offsets.available_permits(),
        0,
        "source offset custody follows encoder"
    );
    assert!(
        admissions
            .try_admit_bundle(1, cpu_threads, &estimate, Priority::Live)
            .is_none(),
        "actual graph still owns GPU and CPU"
    );
    let (encoded, diagnostic) =
        tokio::try_join!(drain_bounded(stdout, SOURCE_LIMIT), drain(stderr))
            .expect("encoder pipes");
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
    if let Some(completion) = completion {
        completion
            .await
            .expect("streaming renderer completion")
            .expect("streaming renderer success");
    }
    assert!(encoded.len() > 100, "actual encoded Matroska output");
    assert!(source.unchanged(), "same source revision retained");
    assert!(
        admissions
            .try_admit_bundle(1, cpu_threads, &estimate, Priority::Live)
            .is_none(),
        "exited child alone does not release writer admission"
    );
    // Losing the actual controller must retire its child even though
    // the cancellation observer still retains the registration receipt.
    drop(slot);
    writers.settled();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), registration.wait_confirmed_reap())
            .await
            .expect("dropped actual encoder controller reaps")
            .matches(&registration)
    );

    assert_eq!(offsets.available_permits(), 1);
    assert_eq!(
        (&source.handle).stream_position().expect("restored offset"),
        original
    );
    assert!(
        admissions
            .try_admit_bundle(1, cpu_threads, &estimate, Priority::Live)
            .is_some(),
        "confirmed graph releases concrete admission"
    );
    #[cfg(target_os = "linux")]
    if let Some(evidence) = evidence {
        let timing = evidence
            .timing()
            .expect("retained actual timing after encoder reap");
        let events = evidence
            .renderer_events()
            .expect("retained bounded renderer observations");
        assert!(!timing.is_empty() && !events.is_empty());
        if let Ok(prefix) = std::env::var("PLURX_DV_SEGMENT_EVIDENCE_PREFIX") {
            std::fs::write(format!("{prefix}.timing.tsv"), &timing).expect("save actual timing");
            std::fs::write(format!("{prefix}.renderer.jsonl"), &events)
                .expect("save actual observations");
        }
        assert!(!evidence.rpu(0).expect("retained raw source RPU").is_empty());
        assert!(
            evidence.rpu(64).is_err(),
            "RPU reads remain picture bounded"
        );
        assert!(
            admissions
                .try_admit_bundle(1, cpu_threads, &estimate, Priority::Live)
                .is_some(),
            "artifact lease does not retain admission"
        );
    }
    let slot = Arc::new(ProducerSlot::new());
    let mut unaccepted = probe_spawn(
        SegmentRequest {
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
                max_frames,
                bl: (64, 64),
                el: (64, 64),
            },
            source: source.clone(),
            source_offsets: offsets.clone(),
            admission: admissions
                .try_admit_bundle(1, cpu_threads, &estimate, Priority::Live)
                .expect("second actual graph admission"),
            producer: slot.clone(),
            at: 1,
            preparation_deadline: Instant::now() + Duration::from_secs(30),
            cancel: CancellationToken::new(),
        },
        streaming,
    )
    .await
    .expect("actual unaccepted encoder handoff");
    let completion = unaccepted.take_helper_completion();
    let abandoned = unaccepted.registration.clone();
    assert_eq!(offsets.available_permits(), 0);
    drop(unaccepted);
    let receipt = tokio::time::timeout(Duration::from_secs(5), abandoned.wait_confirmed_reap())
        .await
        .expect("dropped actual handoff retires encoder");
    assert!(receipt.matches(&abandoned));
    if let Some(completion) = completion {
        let _ = completion.await.expect("cancelled helper owned completion");
    }
    assert_eq!(
        receipt.writers(),
        crate::prodrun::WriterSettlement::Abandoned
    );
    assert_eq!(offsets.available_permits(), 1);
    assert!(admissions
        .try_admit_bundle(1, cpu_threads, &estimate, Priority::Live)
        .is_some());
    assert!(source.unchanged());
    if streaming {
        let slot = Arc::new(ProducerSlot::new());
        let mut refused = probe_spawn(
            SegmentRequest {
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
                    max_frames,
                    bl: (64, 64),
                    el: (32, 32),
                },
                source: source.clone(),
                source_offsets: offsets.clone(),
                admission: admissions
                    .try_admit_bundle(1, cpu_threads, &estimate, Priority::Live)
                    .expect("refusal graph admission"),
                producer: slot.clone(),
                at: 2,
                preparation_deadline: Instant::now() + Duration::from_secs(30),
                cancel: CancellationToken::new(),
            },
            true,
        )
        .await
        .expect("owned graph starts before actual raster refusal");
        let completion = refused
            .take_helper_completion()
            .expect("actual streaming completion");
        assert!(tokio::time::timeout(Duration::from_secs(5), completion)
            .await
            .expect("refusal completion bounded")
            .expect("refusal owner completes")
            .is_err());
        let registration = refused.registration.clone();
        drop(refused);
        tokio::time::timeout(Duration::from_secs(5), registration.wait_confirmed_reap())
            .await
            .expect("renderer refusal retires encoder");
        assert_eq!(offsets.available_permits(), 1);
        assert!(admissions
            .try_admit_bundle(1, cpu_threads, &estimate, Priority::Live)
            .is_some());
        assert_eq!(
            (&source.handle)
                .stream_position()
                .expect("refusal source offset"),
            original
        );
    }
    if let Ok(path) = std::env::var("PLURX_DV_SEGMENT_ENCODED_OUTPUT") {
        std::fs::write(path, encoded).expect("save actual graph observation");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn window_observations_allow_exact_bound_and_refuse_overflow() {
        let observations = vec![b'x'; WINDOW_TRACE_LIMIT as usize];
        assert_eq!(
            drain_bounded(observations.as_slice(), WINDOW_TRACE_LIMIT)
                .await
                .expect("exact bounded trace")
                .len(),
            WINDOW_TRACE_LIMIT as usize
        );
        let overflow = vec![b'x'; WINDOW_TRACE_LIMIT as usize + 1];
        assert!(drain_bounded(overflow.as_slice(), WINDOW_TRACE_LIMIT)
            .await
            .is_err());
    }

    #[test]
    fn shape_and_actual_helper_cli_are_bounded() {
        let recipe = [
            "-threads",
            "1",
            "-i",
            &fd(3),
            "-filter_threads",
            "1",
            "-c:v",
            "libx265",
            "-threads:v",
            "1",
            "-x265-params",
            "pools=none:frame-threads=1",
        ]
        .map(str::to_owned);
        assert!(check_encoder_budget(&recipe, 3).is_ok());
        assert!(check_encoder_budget(&recipe, 2).is_err());
        let duplicate_input = [
            "-threads",
            "1",
            "-threads",
            "1",
            "-filter_threads",
            "1",
            "-i",
            &fd(3),
            "-c:v",
            "libx265",
            "-x265-params",
            "pools=none:frame-threads=1",
        ]
        .map(str::to_owned);
        assert!(check_encoder_budget(&duplicate_input, 8).is_err());
        let mut uncapped_audio = recipe.to_vec();
        uncapped_audio.splice(4..4, ["-i".into(), fd(4)]);
        assert!(check_encoder_budget(&uncapped_audio, 8).is_err());
        let mut duplicate_filter = recipe.to_vec();
        duplicate_filter.extend(["-filter_threads".into(), "0".into()]);
        assert!(check_encoder_budget(&duplicate_filter, 8).is_err());
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
                if cfg!(target_os = "linux") {
                    fd(4)
                } else {
                    ".".into()
                },
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
        let full_interval = SegmentShape {
            video_index: 0,
            max_frames: 64,
            bl: (3840, 2160),
            el: (1920, 1080),
        };
        assert_eq!(
            full_interval.frame_bytes().expect("streaming raster"),
            3840 * 2160 * 6
        );
        assert!(
            full_interval.validate().is_err(),
            "finite RGB scratch cap stays intact"
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

    #[cfg(target_os = "linux")]
    #[test]
    fn original_movie_window_uses_exact_bounded_rationals() {
        window_argument_regression();
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
            DIAGNOSTIC_LIMIT,
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
                DIAGNOSTIC_LIMIT,
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
    #[tokio::test]
    #[ignore = "requires reviewed Linux FEL helper and bounded timestamped source"]
    async fn actual_segment_helper_nut_encoder_preserves_owned_custody() {
        physical_probe().await;
    }
}
