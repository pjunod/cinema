//! One DV processing helper for a rendition's contiguous play.
//!
//! A processed rendition produces one window per VOD generation. Started
//! fresh for each window, the helper re-opened and re-probed the source,
//! re-created its VA-API and Vulkan/libplacebo state and decoded the keyframe
//! preroll again before every window's first picture; on a long-GOP title
//! that made a 2 s window cost 1.6 s by hand and 2.6-3.1 s live.
//!
//! A [`HelperSession`] keeps one helper (`segment_decode_render --session`)
//! for consecutive windows. The helper keeps its demuxer, decoders and GPU
//! state, and decodes preroll only for the session's first window. Each
//! window is still its own unit of proof: it has its own encoder process,
//! private spool, evidence directory, deadline, observations, verification
//! and publication, exactly as a single window had. The session owns what
//! the whole graph held: the source read custody, the source-offset lane and
//! the admission (with its exclusive worker claim), so the rendition's
//! retained credit is used by this one helper and no second process.
//!
//! The session is node-local, lives no longer than its rendition, and is
//! visible in Activity as the "DV streaming reconstruction" process, which
//! the existing process stop ends (the next window then fails and falls back,
//! as any window failure does).
//!
//! It ends, and the next window starts cold, when play moves elsewhere
//! (seek, discontinuity), production pauses or yields (flow stop, idle,
//! capacity hold), the rendition closes, its source read authority ends, or a
//! window fails. Only a window failure is a backend failure.
#![cfg(target_os = "linux")]
use super::*;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// Why a session ended. Every cause but [`SessionEnd::WindowFailed`] is
/// ordinary lifecycle: the next window opens a new session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionEnd {
    /// The next window is not the one this session is positioned for.
    Discontinuity,
    /// Production paused or gave its capacity back.
    FlowStop,
    /// The rendition closed, failed or was replaced.
    Closed,
    /// The session's source read authority ended (for example its custody
    /// lease); the next window opens new custody.
    SourceAuthority,
    /// The next window needs a differently configured helper.
    Changed,
    /// A window failed or was cancelled; nothing the helper holds is trusted.
    WindowFailed,
    /// A single prepared window (the processed trial) is the whole session.
    Single,
}

/// The daemon's end of the helper's SOCK_SEQPACKET control socket.
struct ControlSocket(tokio::io::unix::AsyncFd<OwnedFd>);

impl ControlSocket {
    fn pair() -> std::io::Result<(Self, std::fs::File)> {
        let mut fds = [0; 2];
        // SAFETY: socketpair writes two fresh descriptors on success only.
        if unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: both descriptors were just created and are owned here.
        let (parent, child) =
            unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        // SAFETY: F_GETFL/F_SETFL on an owned descriptor.
        unsafe {
            let flags = libc::fcntl(parent.as_raw_fd(), libc::F_GETFL);
            if flags < 0
                || libc::fcntl(parent.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok((
            Self(tokio::io::unix::AsyncFd::new(parent)?),
            std::fs::File::from(child),
        ))
    }

    async fn send(&self, text: &[u8], fds: &[RawFd]) -> std::io::Result<()> {
        loop {
            let mut ready = self.0.writable().await?;
            match ready.try_io(|socket| send_with_fds(socket.get_ref().as_raw_fd(), text, fds)) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }

    /// One reply; empty when the helper closed its end (it exited).
    async fn recv(&self) -> std::io::Result<Vec<u8>> {
        loop {
            let mut ready = self.0.readable().await?;
            match ready.try_io(|socket| {
                let mut buffer = [0u8; 64];
                // SAFETY: recv writes at most buffer.len() bytes into buffer.
                let received = unsafe {
                    libc::recv(
                        socket.get_ref().as_raw_fd(),
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                        0,
                    )
                };
                if received < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(buffer[..received as usize].to_vec())
                }
            }) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }
}

fn send_with_fds(socket: RawFd, text: &[u8], fds: &[RawFd]) -> std::io::Result<()> {
    let payload = std::mem::size_of_val(fds);
    // SAFETY: CMSG_SPACE/CMSG_LEN are pure size computations.
    let (space, length) = unsafe {
        (
            libc::CMSG_SPACE(payload as u32) as usize,
            libc::CMSG_LEN(payload as u32) as usize,
        )
    };
    // u64 storage keeps the control buffer aligned for cmsghdr.
    let mut control = vec![0u64; space.div_ceil(8)];
    let mut iov = libc::iovec {
        iov_base: text.as_ptr().cast_mut().cast(),
        iov_len: text.len(),
    };
    // SAFETY: a zeroed msghdr is valid; every pointer set below outlives the
    // sendmsg call, and the control buffer holds exactly one SCM_RIGHTS
    // header of CMSG_SPACE(payload) bytes.
    let sent = unsafe {
        let mut message: libc::msghdr = std::mem::zeroed();
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = space as _;
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = length as _;
        std::ptr::copy_nonoverlapping(fds.as_ptr().cast::<u8>(), libc::CMSG_DATA(header), payload);
        libc::sendmsg(socket, &message, libc::MSG_NOSIGNAL)
    };
    if sent < 0 {
        Err(std::io::Error::last_os_error())
    } else if sent as usize != text.len() {
        Err(std::io::Error::other("short DV helper session command"))
    } else {
        Ok(())
    }
}

/// Everything a session's helper process is configured with. Two windows can
/// share a session only when this is identical for both.
#[derive(Clone)]
pub(crate) struct SessionHelper {
    pub(crate) renderer: PathBuf,
    pub(crate) renderer_env: Vec<(String, std::ffi::OsString)>,
    pub(crate) runtime_cache: PathBuf,
    pub(crate) video_index: u8,
    pub(crate) bl: (u32, u32),
    pub(crate) mode: plurx_core::transcode::dv_processing::DvProcessingMode,
    pub(crate) decode: RendererDecode,
    pub(crate) export_raster: (u32, u32),
    pub(crate) display_target: Option<plurx_core::transcode::dv_processing::DvDisplayTarget>,
    pub(crate) source_origin: plurx_core::transcode::dv_processing::DvSourceOrigin,
    /// The encoder's soundtrack ordinal (`-map 0:a:K`), when it has one.
    pub(crate) audio_ordinal: Option<u32>,
}

impl SessionHelper {
    /// The helper argv: the single-shot window argv with its per-window
    /// fields as '-', followed by the session options. Argument 0 is the
    /// session's source URL.
    fn args(&self, source_url: &str) -> Result<Vec<String>, String> {
        let base_only =
            self.mode == plurx_core::transcode::dv_processing::DvProcessingMode::BaseRpu;
        plurx_core::transcode::dv_processing::DvExportPolicy::PqBilinearYuv420p10V1 {
            raster: self.export_raster,
        }
        .raster(self.bl)
        .map_err(|error| error.to_string())?;
        let mut args = SegmentShape {
            video_index: self.video_index,
            max_frames: 64,
            bl: self.bl,
            el: (0, 0),
        }
        .renderer_args();
        args[0] = source_url.to_owned();
        args[1] = "-".into();
        args.extend(["-".into(), "-".into(), "-".into(), "512".into()]);
        args.push(if base_only { "base-rpu" } else { "fel" }.into());
        args.extend([
            self.export_raster.0.to_string(),
            self.export_raster.1.to_string(),
            "pq-bilinear-yuv420p10-v1".into(),
        ]);
        if let Some(target) = self.display_target {
            args[9] = target.output_policy();
            args.extend(target.endpoint_args());
        }
        let (numerator, denominator) = self.source_origin.exact();
        args.extend([
            "--held-source-origin".into(),
            format!("{numerator}/{denominator}"),
            // The control socket, inherited as descriptor 5.
            "--session".into(),
            "5".into(),
        ]);
        if let Some(ordinal) = self.audio_ordinal {
            args.extend(["--audio-ordinal".into(), ordinal.to_string()]);
        }
        Ok(args)
    }

    fn identity(&self) -> Result<Vec<String>, String> {
        let mut identity = self.args("source")?;
        identity.insert(0, self.renderer.display().to_string());
        identity.extend(
            self.renderer_env
                .iter()
                .map(|(key, value)| format!("{key}={}", value.to_string_lossy())),
        );
        Ok(identity)
    }
}

/// `[from, end)` of a window's carried soundtrack, as exact rationals on the
/// source clock.
pub(crate) type SoundtrackInterval = ((i64, u32), (i64, u32));

/// What one window of a session needs: its encoder, its bounds and its
/// soundtrack interval, its deadline and the rendition's producer slot.
pub(crate) struct SessionWindowRequest {
    pub(crate) encoder: PathBuf,
    pub(crate) encoder_args: Vec<String>,
    pub(crate) producer: Arc<ProducerSlot>,
    pub(crate) at: u32,
    pub(crate) window: SegmentWindow,
    pub(crate) frames: u8,
    /// `[from, end)` of the carried soundtrack, on the source clock.
    pub(crate) audio: Option<SoundtrackInterval>,
    pub(crate) deadline: Instant,
    pub(crate) cancel: CancellationToken,
    pub(crate) stages: Arc<WindowStageClock>,
}

/// The physical owners every process of a session's graph holds: the source
/// read use, the source-offset lane and the admission. The helper and each
/// window's encoder hold one reference; the last reap releases them.
struct SessionResources {
    _source_input: SourceInputUse,
    source: Arc<SourceFence>,
    original_offset: u64,
    offset_lane: Option<OwnedSemaphorePermit>,
    admission: SegmentAdmission,
}
impl Drop for SessionResources {
    fn drop(&mut self) {
        // As Resources: the lane stays closed if the shared offset cannot be
        // restored.
        if (&self.source.handle)
            .seek(SeekFrom::Start(self.original_offset))
            .is_err()
        {
            if let Some(lane) = self.offset_lane.take() {
                lane.forget();
            }
        }
    }
}

struct SessionProcess {
    control: Arc<ControlSocket>,
    slot: Arc<ProducerSlot>,
    registration: ProducerRegistration,
    exit: tokio::sync::watch::Receiver<Option<HelperOutcome>>,
    resources: Arc<SessionResources>,
    source_input: Arc<SourceInputLease>,
}

/// One live helper and the windows it has produced. Dropping it without
/// [`HelperSession::retire`] still retires the helper, without waiting.
pub(crate) struct HelperSession {
    process: Option<SessionProcess>,
    identity: Vec<String>,
    mode: plurx_core::transcode::dv_processing::DvProcessingMode,
    decode: RendererDecode,
    cpu_threads: usize,
    runtime_cache: PathBuf,
    /// The window index and exact start the next window must have.
    next: Option<(u32, (i64, u32))>,
    windows: u32,
    observer: Option<crate::source_custody::SourceObserver>,
}

impl HelperSession {
    /// Start a session's helper. It takes the window's custody, lane and
    /// admission, which then belong to the session until it ends.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn spawn(
        helper: SessionHelper,
        source_input: Arc<SourceInputLease>,
        observer: Option<crate::source_custody::SourceObserver>,
        source: Arc<SourceFence>,
        source_offsets: Arc<Semaphore>,
        admission: SegmentAdmission,
        deadline: Instant,
        cancel: &CancellationToken,
        stages: &WindowStageClock,
    ) -> Result<Self, String> {
        let identity = helper.identity()?;
        let held = admission.resources();
        held.hardware
            .ok_or("streaming segment requires concrete GPU admission")?;
        let cpu_threads = held
            .software
            .ok_or("concurrent helper and encoder lack their frozen CPU debit")?
            .threads();
        let lane = acquire_offset_lane(source_offsets, deadline, cancel, stages).await?;
        check_source(&source)?;
        if source.handle.metadata().map_err(|e| e.to_string())?.len() == 0 {
            return Err("finite streaming source exceeds byte envelope".into());
        }
        let original_offset = (&source.handle)
            .stream_position()
            .map_err(|e| e.to_string())?;
        let args = helper.args(&source_input.url)?;
        let resources = Arc::new(SessionResources {
            _source_input: source_input.physical_owner(),
            source,
            original_offset,
            offset_lane: Some(lane),
            admission,
        });
        (&resources.source.handle)
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        let (control, child) = ControlSocket::pair().map_err(|e| e.to_string())?;
        let renderer_env: Vec<_> = helper
            .renderer_env
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_os_str()))
            .collect();
        let spawned = producer_spawn::spawn(
            &helper.renderer,
            &args,
            SpawnOptions {
                runtime_cache: &helper.runtime_cache,
                progress: Progress::None,
                descriptors: Descriptors::from_files(
                    Some(&resources.source.handle),
                    None,
                    Some(&child),
                    false,
                ),
                env: &renderer_env,
                work: crate::process_control::ChildWork::realtime("DV streaming reconstruction"),
            },
        )?;
        drop(child);
        stages.mark(WindowStage::HelperSpawned);
        let slot = Arc::new(ProducerSlot::new());
        let (registration, writers) = slot
            .attach_registered_job_owned(
                spawned.child,
                spawned.child_job,
                0,
                Some(Box::new(Arc::clone(&resources))),
            )
            .await;
        // The helper exits by itself when its control socket closes; its
        // reaper waits for that, and retirement still kills it at once.
        slot.request_registered_completion(&registration)
            .await
            .map_err(|e| e.to_string())?;
        let (exited, exit) = tokio::sync::watch::channel(None);
        let watched = registration.clone();
        let (stdout, stderr) = (spawned.stdout, spawned.stderr);
        tokio::spawn(async move {
            // The helper writes each window's observations into that window's
            // evidence; its own stdout carries nothing. Keep the newest
            // diagnostics for the failure that ends the session.
            let tail = async {
                let mut stderr = stderr;
                let mut kept = std::collections::VecDeque::with_capacity(DIAGNOSTIC_LIMIT as usize);
                let mut buffer = [0u8; 8192];
                while let Ok(read) = stderr.read(&mut buffer).await {
                    if read == 0 {
                        break;
                    }
                    kept.extend(&buffer[..read]);
                    while kept.len() > DIAGNOSTIC_LIMIT as usize {
                        kept.pop_front();
                    }
                }
                Vec::from(kept)
            };
            let discard = async {
                let _ = tokio::io::copy(
                    &mut tokio::io::BufReader::new(stdout),
                    &mut tokio::io::sink(),
                )
                .await;
            };
            let (tail, ()) = tokio::join!(tail, discard);
            writers.settled();
            let status = watched.wait_child_exit().await;
            let outcome = if status.success() {
                HelperOutcome::Completed
            } else {
                HelperOutcome::Failed(format!(
                    "streaming renderer {}: {}",
                    if status.code() == Some(1) {
                        "refused segment"
                    } else {
                        "failed"
                    },
                    crate::scratch_put::redact(&String::from_utf8_lossy(&tail))
                ))
            };
            let _ = exited.send_replace(Some(outcome));
        });
        Ok(Self {
            process: Some(SessionProcess {
                control: Arc::new(control),
                slot,
                registration,
                exit,
                resources,
                source_input,
            }),
            identity,
            mode: helper.mode,
            decode: helper.decode,
            cpu_threads,
            runtime_cache: helper.runtime_cache,
            next: None,
            windows: 0,
            observer,
        })
    }

    /// The plan index of the window this session can produce next.
    pub(crate) fn next_index(&self) -> Option<u32> {
        self.next.map(|(index, _)| index)
    }

    /// The source observer of the session's custody, for the generation that
    /// uses it.
    pub(crate) fn observer(&self) -> Option<crate::source_custody::SourceObserver> {
        self.observer.clone()
    }

    /// The retained reservation the session's admission belongs to.
    pub(crate) fn reservation(&self) -> Option<crate::vodencode::EncodePermit> {
        match &self.process.as_ref()?.resources.admission {
            SegmentAdmission::Retained(worker) => Some(worker.reservation()),
            SegmentAdmission::Owned(_) => None,
        }
    }

    /// Whether the session can serve a window configured like `helper`.
    pub(crate) fn serves(&self, helper: &SessionHelper) -> bool {
        helper
            .identity()
            .is_ok_and(|identity| identity == self.identity)
    }

    /// What ended a helper that exited outside a window.
    fn exit_cause(&self) -> String {
        match self
            .process
            .as_ref()
            .and_then(|process| process.exit.borrow().clone())
        {
            Some(HelperOutcome::Failed(cause)) => cause,
            _ => "DV helper session ended between windows".into(),
        }
    }

    /// Why this live session cannot produce another window, if it cannot.
    pub(crate) fn unusable(&self) -> Option<SessionEnd> {
        let process = self.process.as_ref()?;
        if process.exit.borrow().is_some() {
            return Some(SessionEnd::WindowFailed);
        }
        if self
            .observer
            .as_ref()
            .is_some_and(|observer| !observer.has_authority())
            || process.source_input.read_failed()
        {
            return Some(SessionEnd::SourceAuthority);
        }
        None
    }

    /// Produce one window: its own encoder, pipes and evidence directory,
    /// handed to the helper with the window's command. The result is the
    /// same producer a single window had; the helper's part completes when
    /// it acknowledges the window.
    pub(crate) async fn window(
        &mut self,
        request: SessionWindowRequest,
    ) -> Result<SegmentProducer, String> {
        let process = self.process.as_ref().ok_or("DV helper session ended")?;
        if let Some(HelperOutcome::Failed(cause)) = process.exit.borrow().clone() {
            return Err(cause);
        }
        if process.exit.borrow().is_some() {
            return Err("DV helper session ended".into());
        }
        let [start, end, preroll] = request.window.args()?;
        if self
            .next
            .is_some_and(|(index, start)| index != request.at || start != request.window.start)
        {
            return Err("DV helper session window is not contiguous".into());
        }
        if request.window.source_origin.is_none() {
            return Err("DV helper session windows carry the held source origin".into());
        }
        if !(1..=64).contains(&request.frames) {
            return Err("unsupported bounded segment geometry or cadence".into());
        }
        check_streaming_budget(
            self.mode,
            self.decode,
            &request.encoder_args,
            self.cpu_threads,
        )?;
        check_streaming_clock(&request.encoder_args, true)?;
        let reservation = request
            .producer
            .try_reserve_empty()
            .map_err(|e| e.to_string())?;
        check_source(&process.resources.source)?;
        let directory = tempfile::Builder::new()
            .prefix("dv-stream-")
            .tempdir_in(&self.runtime_cache)
            .map_err(|e| e.to_string())?;
        let held = plurx_core::fs_secure::open_directory_nofollow_blocking(directory.path())
            .map_err(|e| e.to_string())?;
        let evidence = SegmentEvidence {
            directory: Arc::new(directory),
            held: Arc::new(held.try_clone().map_err(|e| e.to_string())?),
        };
        let pipe = || -> Result<(std::fs::File, std::fs::File), String> {
            let (read, write) = std::io::pipe().map_err(|e| e.to_string())?;
            Ok((OwnedFd::from(read).into(), OwnedFd::from(write).into()))
        };
        let (nut_read, nut_write) = pipe()?;
        let audio = request.audio.map(|_| pipe()).transpose()?;
        let encoded = producer_spawn::spawn(
            &request.encoder,
            &request.encoder_args,
            SpawnOptions {
                runtime_cache: &self.runtime_cache,
                progress: Progress::Stderr,
                descriptors: Descriptors::from_files(
                    Some(&nut_read),
                    audio.as_ref().map(|(read, _)| read),
                    None,
                    false,
                ),
                env: &[],
                work: crate::process_control::ChildWork::realtime("DV streaming encoder"),
            },
        )?;
        // Only the encoder reads; a helper writing to a retired encoder gets
        // EPIPE, never a parent reader that keeps the pipe alive.
        drop(nut_read);
        let audio_write = audio.map(|(_, write)| write);
        request.stages.mark(WindowStage::EncoderSpawned);
        let mut command = format!("window {start} {end} {} {preroll}", request.frames);
        let mut fds = vec![nut_write.as_raw_fd(), held.as_raw_fd()];
        if let (Some(((from_n, from_d), (end_n, end_d))), Some(write)) =
            (request.audio, audio_write.as_ref())
        {
            if from_n < 0 || end_n < 0 || from_d == 0 || end_d == 0 {
                return Err("invalid carried soundtrack interval".into());
            }
            command.push_str(&format!(" {from_n}/{from_d} {end_n}/{end_d}"));
            fds.push(write.as_raw_fd());
        }
        let (registration, writers) = reservation.attach_registered_job_owned(
            encoded.child,
            encoded.child_job,
            request.at,
            Box::new((
                Arc::clone(&process.resources),
                Arc::clone(&evidence.directory),
            )),
        );
        let sent = process.control.send(command.as_bytes(), &fds).await;
        // The helper owns the window's write ends now; its close is the
        // encoder's end of input.
        drop((nut_write, audio_write, held));
        if let Err(error) = sent {
            writers.settled();
            let _ = request
                .producer
                .request_registered_retirement(&registration)
                .await;
            return Err(format!("DV helper session command: {error}"));
        }
        request.stages.mark(WindowStage::HelperSpawned);
        let completion = supervise_session_window(
            Arc::clone(&process.control),
            process.exit.clone(),
            Arc::clone(&process.slot),
            process.registration.clone(),
            request.cancel.clone(),
            request.deadline,
            Arc::downgrade(&request.producer),
            registration.clone(),
        );
        self.next = Some((
            request.at.checked_add(1).ok_or("window index overflow")?,
            request.window.end,
        ));
        self.windows = self.windows.saturating_add(1);
        Ok(SegmentProducer {
            registration,
            writers: Some(writers),
            stdout: Some(encoded.stdout),
            stderr: Some(encoded.stderr),
            unaccepted_cancel: Some(request.cancel),
            helper_completion: Some(completion),
            evidence: Some(evidence),
        })
    }

    /// End the session: retire and reap the helper, then close the source
    /// custody once every process of the graph is reaped.
    pub(crate) async fn retire(mut self, cause: SessionEnd) {
        if let Some(process) = self.process.take() {
            retire_process(process, cause, self.windows).await;
        }
    }
}

impl Drop for HelperSession {
    fn drop(&mut self) {
        if let Some(process) = self.process.take() {
            let windows = self.windows;
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(retire_process(process, SessionEnd::Closed, windows));
            }
        }
    }
}

async fn retire_process(process: SessionProcess, cause: SessionEnd, windows: u32) {
    let SessionProcess {
        control,
        slot,
        registration,
        resources,
        source_input,
        ..
    } = process;
    drop(control);
    let _ = slot.request_registered_retirement(&registration).await;
    registration.wait_confirmed_reap().await;
    drop(resources);
    let closed = source_input.close_and_wait().await;
    tracing::info!(target: "plurxd::dv", windows, cause = ?cause,
        source_closed = closed.is_ok(), "DV helper session ended");
}

/// The helper's part of one window: its acknowledgement, or why there is
/// none. Anything but an acknowledgement retires the encoder and the helper:
/// a helper interrupted mid-window is not trusted with another.
#[allow(clippy::too_many_arguments)]
fn supervise_session_window(
    control: Arc<ControlSocket>,
    mut exit: tokio::sync::watch::Receiver<Option<HelperOutcome>>,
    helper_slot: Arc<ProducerSlot>,
    helper: ProducerRegistration,
    cancel: CancellationToken,
    deadline: Instant,
    slot: std::sync::Weak<ProducerSlot>,
    encoder: ProducerRegistration,
) -> HelperCompletion {
    let (completed, completion) = tokio::sync::watch::channel(None);
    tokio::spawn(async move {
        let retirement = encoder.retirement();
        let outcome = tokio::select! {
            reply = control.recv() => match reply {
                Ok(reply) if reply == b"done" => HelperOutcome::Completed,
                Ok(reply) if reply.is_empty() => loop {
                    // The helper ended: its exit says why.
                    if let Some(outcome) = exit.borrow().clone() {
                        break match outcome {
                            HelperOutcome::Completed => HelperOutcome::Failed(
                                "DV helper session ended inside its window".into(),
                            ),
                            outcome => outcome,
                        };
                    }
                    if exit.changed().await.is_err() {
                        break HelperOutcome::Failed("DV helper session owner lost".into());
                    }
                },
                Ok(_) => HelperOutcome::Failed("DV helper session sent an unexpected reply".into()),
                Err(error) => HelperOutcome::Failed(format!("DV helper session control: {error}")),
            },
            _ = cancel.cancelled() => HelperOutcome::Cancelled,
            _ = retirement.cancelled() => HelperOutcome::Cancelled,
            _ = tokio::time::sleep_until(deadline) => {
                HelperOutcome::Failed("streaming renderer deadline expired".into())
            }
        };
        let finished = matches!(outcome, HelperOutcome::Completed);
        if !finished {
            // Retire the encoder before the helper that may be blocked
            // writing into it; the rendition's slot still owns its reap.
            if let Some(slot) = slot.upgrade() {
                let _ = slot.request_registered_retirement(&encoder).await;
            }
            let _ = helper_slot.request_registered_retirement(&helper).await;
        }
        let _ = completed.send_replace(Some(outcome));
        if finished {
            tokio::select! {
                _ = cancel.cancelled() => {},
                _ = retirement.cancelled() => {},
            }
        }
        if let Some(slot) = slot.upgrade() {
            let _ = slot.request_registered_retirement(&encoder).await;
        }
    });
    HelperCompletion(completion)
}

/// The rendition's one live session, between its windows.
#[derive(Default)]
pub(crate) struct HelperSessionSlot {
    state: std::sync::Mutex<SlotState>,
}

#[derive(Default)]
struct SlotState {
    session: Option<HelperSession>,
    retiring: Vec<tokio::task::JoinHandle<()>>,
    /// Counts ends, so a window that took the session before an end cannot
    /// put it back after.
    ended: u64,
}

/// A session taken for one window. Whatever is still held when this drops
/// is retired, and the rendition's next window waits for its reap.
pub(crate) struct TakenSession<'a> {
    slot: &'a HelperSessionSlot,
    session: Option<HelperSession>,
    stamp: u64,
    failure: Option<String>,
}

impl TakenSession<'_> {
    pub(crate) fn get(&self) -> Option<&HelperSession> {
        self.session.as_ref()
    }
    pub(crate) fn take(&mut self) -> Option<HelperSession> {
        self.session.take()
    }
    /// The helper this window was positioned on exited between windows: it
    /// was stopped (Activity) or crashed. That is a window failure, with
    /// the same fallback.
    pub(crate) fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }
    /// Give a completed window's session back for the next window.
    pub(crate) async fn put(&mut self, session: HelperSession) {
        self.slot.put(session, self.stamp).await;
    }
}

impl Drop for TakenSession<'_> {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            self.slot.retire_tracked(session, SessionEnd::Discontinuity);
        }
    }
}

impl HelperSessionSlot {
    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn retire_tracked(&self, session: HelperSession, cause: SessionEnd) {
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let handle = runtime.spawn(session.retire(cause));
            self.lock().retiring.push(handle);
        }
    }

    /// End the session now. Never waits, so a caller may hold any lock; the
    /// next [`Self::take`] waits until the helper is reaped.
    pub(crate) fn end(&self, cause: SessionEnd) {
        let session = {
            let mut state = self.lock();
            state.ended = state.ended.wrapping_add(1);
            state.session.take()
        };
        if let Some(session) = session {
            self.retire_tracked(session, cause);
        }
    }

    async fn settle(&self) {
        loop {
            let handles = std::mem::take(&mut self.lock().retiring);
            if handles.is_empty() {
                return;
            }
            for handle in handles {
                let _ = handle.await;
            }
        }
    }

    /// The live session if it is positioned for window `at` and can still
    /// produce it. Any other is ended first, and every ended session is
    /// reaped before this returns, so a cold window never runs beside an old
    /// helper or against its worker claim.
    pub(crate) async fn take(&self, at: u32) -> TakenSession<'_> {
        let mut failure = None;
        let (taken, unusable) = {
            let mut state = self.lock();
            match state.session.as_ref() {
                Some(session) if session.next_index() == Some(at) => {
                    let session = state.session.take().expect("live session");
                    match session.unusable() {
                        Some(cause) => (None, Some((session, cause))),
                        None => (Some(session), None),
                    }
                }
                _ => (None, None),
            }
        };
        if let Some((session, cause)) = unusable {
            if cause == SessionEnd::WindowFailed {
                failure = Some(session.exit_cause());
            }
            self.retire_tracked(session, cause);
        }
        if taken.is_none() {
            self.end(SessionEnd::Discontinuity);
        }
        self.settle().await;
        let stamp = self.lock().ended;
        TakenSession {
            slot: self,
            session: taken,
            stamp,
            failure,
        }
    }

    /// Keep a session for the rendition's next window, unless it was ended
    /// while its window ran.
    async fn put(&self, session: HelperSession, taken: u64) {
        let rejected = {
            let mut state = self.lock();
            if state.ended != taken || state.session.is_some() {
                Some(session)
            } else {
                state.session = Some(session);
                None
            }
        };
        if let Some(session) = rejected {
            session.retire(SessionEnd::FlowStop).await;
        }
    }
}

impl Drop for HelperSessionSlot {
    fn drop(&mut self) {
        self.end(SessionEnd::Closed);
    }
}
