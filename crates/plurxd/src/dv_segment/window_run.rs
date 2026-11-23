//! A window run: one renderer helper renders up to [`RUN_MAX`] consecutive
//! processed VOD windows (`segment_decode_render --window-run`), so only the
//! first pays the source open, probe, keyframe seek, preroll decode and GPU
//! start-up.
//!
//! Every window keeps everything a single window has: its own private
//! `dv-stream-*` directory (where the helper writes its `renderer.jsonl`,
//! `timing.tsv` and `rpus/`), its own encoder, producer-slot registration,
//! deadline, cancellation, verification and publication. What the run owns
//! is what the helper process owns: the source-offset lane, the original
//! offset restore, the source reader and the graph family's reservation. Each
//! later window's NUT video is a FIFO in that window's directory whose read
//! end the daemon opens before the helper starts, and hands to the window's
//! encoder as fd3 only when the window is asked for. Encoders stay one at a
//! time; the helper blocks on the next window's FIFO in between.
//!
//! The helper finishes a window (observations, timing, RPUs, flushed) before
//! it closes the window's NUT output and only then creates the next window's
//! `renderer.jsonl`, so that file's existence is the daemon's proof that the
//! previous window is completely written. The last window ends with the
//! helper's exit, as a single window does.
use super::*;
use std::collections::VecDeque;
use tokio::sync::watch;

/// The helper's `RUN_MAX`: windows one process may render.
pub(crate) const RUN_MAX: usize = 8;
/// A later window's NUT FIFO, inside that window's private directory.
const RUN_OUTPUT: &str = "video.nut";

type Bounds = ((i64, u32), (i64, u32));
/// A pre-started window encoder's spawn result: the child the run owns until
/// a window attaches it, or why it could not start.
type StartedEncoder = Result<(tokio::process::Child, crate::process_control::ChildJob), String>;

/// What the recipe supplies to start a run beyond its first window's request.
pub(crate) struct RunLaunch {
    /// The windows after the request's, each starting where the previous ends.
    pub(crate) later: Vec<SegmentWindow>,
    /// The graph family reservation the run's helper keeps admitted.
    pub(crate) reservation: crate::vodencode::EncodePermit,
    /// Bound on the helper process itself; each window has its own deadline.
    pub(crate) deadline: Instant,
    /// The helper invocation every later window must match ([`helper_key`]).
    pub(crate) key: Vec<String>,
}

/// The argv a window would need from its helper, with what legitimately
/// differs between a run's windows blanked: the source reader URL, the
/// picture cap and the window's bounds. Two windows with equal keys can be
/// rendered by the same helper process.
pub(crate) fn helper_key(args: &[String]) -> Vec<String> {
    let mut key = args.to_vec();
    for index in [0, 3, 11, 12] {
        if let Some(arg) = key.get_mut(index) {
            arg.clear();
        }
    }
    key
}

/// The helper key of a window's request ([`helper_key`]), plus what else
/// selects the helper process: its executable, environment and source mode.
pub(crate) fn request_key(stream: &StreamingSegmentRequest) -> Result<Vec<String>, String> {
    let mut key = helper_key(&streaming_helper_args(
        &stream.request.shape,
        None,
        stream.window,
        stream.mode,
        stream.export_raster,
        stream.display_target,
        &[],
    )?);
    key.push(stream.request.renderer.to_string_lossy().into_owned());
    key.extend(
        stream
            .request
            .renderer_env
            .iter()
            .map(|(name, value)| format!("{name}={}", value.to_string_lossy())),
    );
    key.push(stream.background.to_string());
    Ok(key)
}

/// `--window-run N` then `END DIR NUT` for each window after the first.
pub(crate) fn run_tail(later: &[(String, String, String)]) -> Vec<String> {
    let mut tail = vec!["--window-run".to_owned(), (later.len() + 1).to_string()];
    for (end, directory, nut) in later {
        tail.extend([end.clone(), directory.clone(), nut.clone()]);
    }
    tail
}

/// May a live run render the requested window next?
pub(crate) fn run_continues(
    next: Option<Bounds>,
    ended: bool,
    requested: Bounds,
    frames: u8,
    frame_cap: u8,
    same_helper: bool,
    same_admission: bool,
) -> bool {
    !ended && same_helper && same_admission && frames <= frame_cap && next == Some(requested)
}

/// One later window's private directory and the read end of its NUT FIFO.
pub(crate) struct RunOutput {
    bounds: Bounds,
    directory: Arc<tempfile::TempDir>,
    held: Arc<std::fs::File>,
    fifo: std::fs::File,
}

impl RunOutput {
    /// The FIFO's read end is opened non-blocking (no writer exists yet), so
    /// the helper's blocking open of its writer succeeds at once.
    fn create(
        runtime_cache: &std::path::Path,
        window: &SegmentWindow,
    ) -> Result<(Self, [String; 2]), String> {
        use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
        let directory = tempfile::Builder::new()
            .prefix("dv-stream-")
            .tempdir_in(runtime_cache)
            .map_err(|e| e.to_string())?;
        // The helper opens these after entering earlier windows' directories,
        // and libavformat would read a ':' as a protocol prefix.
        let path = std::path::absolute(directory.path()).map_err(|e| e.to_string())?;
        let fifo_path = path.join(RUN_OUTPUT);
        let (Some(directory_arg), Some(fifo_arg)) = (path.to_str(), fifo_path.to_str()) else {
            return Err("DV window run directory is not UTF-8".into());
        };
        if directory_arg.contains(':') {
            return Err("DV window run directory would read as a protocol".into());
        }
        let name =
            std::ffi::CString::new(fifo_path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        // SAFETY: a terminated path in this process's fresh private directory.
        if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
            return Err(format!(
                "DV window run FIFO: {}",
                std::io::Error::last_os_error()
            ));
        }
        let fifo = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(&fifo_path)
            .map_err(|e| format!("DV window run FIFO read end: {e}"))?;
        let held = plurx_core::fs_secure::open_directory_nofollow_blocking(directory.path())
            .map_err(|e| e.to_string())?;
        let args = [directory_arg.to_owned(), fifo_arg.to_owned()];
        Ok((
            Self {
                bounds: (window.start, window.end),
                directory: Arc::new(directory),
                held: Arc::new(held),
                fifo,
            },
            args,
        ))
    }
    pub(crate) fn directory(&self) -> Arc<tempfile::TempDir> {
        Arc::clone(&self.directory)
    }
    pub(crate) fn held(&self) -> Arc<std::fs::File> {
        Arc::clone(&self.held)
    }
}

/// Create the later windows' outputs and the helper's `--window-run` tail.
pub(crate) fn outputs(
    runtime_cache: &std::path::Path,
    first: &SegmentWindow,
    later: &[SegmentWindow],
) -> Result<(Vec<RunOutput>, Vec<String>), String> {
    if later.is_empty() || later.len() >= RUN_MAX {
        return Err("DV window run needs 2 to 8 windows".into());
    }
    let mut previous = first.end;
    let mut outputs = Vec::with_capacity(later.len());
    let mut triples = Vec::with_capacity(later.len());
    for window in later {
        if window.start != previous {
            return Err("DV window run windows are not contiguous".into());
        }
        let [_, end, _] = window.args()?;
        let (output, [directory, nut]) = RunOutput::create(runtime_cache, window)?;
        triples.push((end, directory, nut));
        outputs.push(output);
        previous = window.end;
    }
    Ok((outputs, run_tail(&triples)))
}

/// The run's helper process as the daemon observes it.
pub(crate) struct RunHelper {
    /// Set once, when the reaped helper's result is known.
    exit: watch::Receiver<Option<Result<(), String>>>,
    /// Set once the helper is reaped and the run's source reader is closed.
    settled: watch::Receiver<bool>,
    /// When the run must be killed if no window has claimed it by then.
    idle: watch::Sender<Option<Instant>>,
}

impl RunHelper {
    pub(crate) fn exit(&self) -> watch::Receiver<Option<Result<(), String>>> {
        self.exit.clone()
    }
    pub(crate) async fn settled(&self) {
        let mut settled = self.settled.clone();
        let _ = settled.wait_for(|settled| *settled).await;
    }
}

/// Own the run's helper: wait for its reap (or kill it when the run sits idle
/// or its owner drops), publish the result, then close the run's source reader
/// (which waits for the current window encoder's use) and its directories.
pub(crate) fn spawn_helper(
    staged: impl std::future::Future<Output = Result<Vec<u8>, String>> + Send + 'static,
    cancel: CancellationToken,
    input: Arc<SourceInputLease>,
    directories: Vec<Arc<tempfile::TempDir>>,
) -> RunHelper {
    let (exit_sender, exit) = watch::channel(None);
    let (settled_sender, settled) = watch::channel(false);
    let (idle, idle_receiver) = watch::channel(None);
    tokio::spawn(async move {
        tokio::pin!(staged);
        let result = tokio::select! {
            result = &mut staged => result,
            cause = idle_expiry(idle_receiver) => {
                cancel.cancel();
                let _ = staged.await;
                Err(cause.to_owned())
            }
        };
        let result = result.and_then(|stdout| {
            if stdout.is_empty() {
                Ok(())
            } else {
                Err("DV window run helper wrote observations to stdout".to_owned())
            }
        });
        let _ = exit_sender.send_replace(Some(result));
        let _ = input.close_and_wait().await;
        drop(directories);
        let _ = settled_sender.send_replace(true);
    });
    RunHelper {
        exit,
        settled,
        idle,
    }
}

/// Resolves when the run has rested past its idle deadline or its owner is
/// gone. A claimed window clears the deadline.
pub(crate) async fn idle_expiry(mut idle: watch::Receiver<Option<Instant>>) -> &'static str {
    loop {
        let rest = *idle.borrow_and_update();
        tokio::select! {
            changed = idle.changed() => if changed.is_err() {
                return "DV window run owner dropped";
            },
            () = tokio::time::sleep_until(rest.unwrap_or_else(Instant::now)), if rest.is_some() => {
                return "DV window run idle expired";
            }
        }
    }
}

/// A run window's helper completion: the next window's `renderer.jsonl`
/// exists (the helper creates it only after this window's observations are
/// flushed and its NUT output is closed), or the helper exited successfully.
pub(crate) async fn window_complete(
    next: Option<Arc<std::fs::File>>,
    mut exit: watch::Receiver<Option<Result<(), String>>>,
    source: Arc<SourceFence>,
) -> Result<(), String> {
    let advanced = |next: &Option<Arc<std::fs::File>>| {
        next.as_deref().is_some_and(|directory| {
            SegmentEvidence::open(directory, c"renderer.jsonl", libc::O_RDONLY).is_ok()
        })
    };
    loop {
        if advanced(&next) {
            break;
        }
        let ended = exit.borrow_and_update().clone();
        match ended {
            Some(Ok(())) => break,
            // The helper may have advanced just before it failed.
            Some(Err(_)) if advanced(&next) => break,
            Some(Err(cause)) => return Err(cause),
            None => {}
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(20)) => {}
            changed = exit.changed() => if changed.is_err() && exit.borrow().is_none() {
                return Err("DV window run helper owner lost".into());
            }
        }
    }
    check_source(&source)
}

/// Wait until the helper has opened this window's NUT FIFO and written to it.
/// A FIFO reader opened before any writer reports neither data nor hang-up
/// until a writer has come, so readiness means the window has begun.
pub(crate) async fn wait_for_output(
    fifo: &std::fs::File,
    exit: &mut watch::Receiver<Option<Result<(), String>>>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    loop {
        let mut poll = libc::pollfd {
            fd: fifo.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for a descriptor this function borrows.
        let ready = unsafe { libc::poll(&mut poll, 1, 0) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(format!("DV window run FIFO poll: {error}"));
            }
        } else if ready > 0 {
            return Ok(());
        }
        if let Some(ended) = exit.borrow_and_update().clone() {
            return Err(match ended {
                Ok(()) => "DV window run helper ended before this window".into(),
                Err(cause) => cause,
            });
        }
        tokio::select! {
            _ = cancel.cancelled() => return Err("DV window run window cancelled".into()),
            () = tokio::time::sleep_until(deadline) => {
                return Err("DV window run window deadline expired".into());
            }
            () = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
    }
}

/// The encoder reads its fd3 blocking, like the single window's pipe.
pub(crate) fn set_blocking(file: &std::fs::File) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    // SAFETY: fcntl flag reads and writes on a descriptor this function borrows.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0
    {
        return Err(format!(
            "DV window run FIFO mode: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// What a pre-started encoder needs that the run does not already hold.
pub(crate) struct PreStartRequest {
    /// The plan entry of the window the encoder is for.
    pub(crate) at: u32,
    /// That window's encoder argv, computed with its own start and end.
    pub(crate) encoder_args: Vec<String>,
    pub(crate) encoder: std::path::PathBuf,
    pub(crate) runtime_cache: std::path::PathBuf,
    pub(crate) mode: plurx_core::transcode::dv_processing::DvProcessingMode,
    pub(crate) decode: RendererDecode,
    /// How long the encoder may wait for the helper to begin its window.
    pub(crate) wait: Duration,
}

/// The run's next window's encoder, started as soon as the previous window's
/// encoder was reaped instead of after that window's verification,
/// publication and the next generation's spawn. It reads the window's FIFO
/// and its output is read (to the capture bounds) into memory. It holds the
/// family's one worker claim, which the next generation shares rather than
/// claims; until a window attaches it, the run owns and, when killed, reaps it.
struct PreStart {
    at: u32,
    encoder_args: Vec<String>,
    bounds: Bounds,
    directory: Arc<tempfile::TempDir>,
    held: Arc<std::fs::File>,
    claim: Arc<crate::vodencode::EncodeWorkerPermit>,
    cancel: CancellationToken,
    /// Resolves once the encoder is spawned (or could not be).
    started: Option<tokio::sync::oneshot::Receiver<StartedEncoder>>,
    child: Option<(tokio::process::Child, crate::process_control::ChildJob)>,
    /// The task that waits for the window, spawns the encoder and reads its
    /// output; its result is the window's output and diagnostic.
    drained: Option<DrainedOutput>,
}

impl PreStart {
    /// Wait for the spawn result. `false` means there is no encoder.
    async fn started(&mut self) -> bool {
        if self.child.is_none() {
            let Some(started) = self.started.take() else {
                return false;
            };
            match started.await {
                Ok(Ok(child)) => self.child = Some(child),
                _ => return false,
            }
        }
        true
    }
    /// Kill and reap the encoder (if it was started) and join its reader.
    async fn kill(mut self) {
        self.cancel.cancel();
        let _ = self.started().await;
        if let Some((mut child, job)) = self.child.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
            drop(job);
        }
        if let Some(drained) = self.drained.take() {
            let _ = drained.await;
        }
    }
}

/// The pre-start task: wait until the helper has begun the window, spawn its
/// encoder on the FIFO, hand the child out, then read its output.
fn spawn_prestart(
    request: PreStartRequest,
    fifo: std::fs::File,
    url: String,
    mut exit: watch::Receiver<Option<Result<(), String>>>,
    cancel: CancellationToken,
) -> (
    tokio::sync::oneshot::Receiver<StartedEncoder>,
    DrainedOutput,
) {
    let (sender, started) = tokio::sync::oneshot::channel();
    let drained = tokio::spawn(async move {
        let deadline = Instant::now() + request.wait;
        let spawned = async {
            wait_for_output(&fifo, &mut exit, &cancel, deadline).await?;
            set_blocking(&fifo)?;
            spawn_streaming_encoder(
                &request.encoder,
                &request.encoder_args,
                &request.runtime_cache,
                false,
                &fifo,
                None,
                Some(url.as_str()),
            )
        }
        .await;
        drop(fifo); // The encoder is this window's only reader.
        let Spawned {
            child,
            child_job,
            stdout,
            stderr,
        } = match spawned {
            Ok(spawned) => spawned,
            Err(cause) => {
                let _ = sender.send(Err(cause.clone()));
                return Err(cause);
            }
        };
        if let Err(Ok((mut child, job))) = sender.send(Ok((child, child_job))) {
            // The run was dropped while the encoder started.
            let _ = child.start_kill();
            let _ = child.wait().await;
            drop(job);
            return Err("DV pre-started window encoder abandoned".into());
        }
        let output = async {
            let mut bytes = Vec::new();
            stdout
                .take(ENCODER_OUTPUT_LIMIT + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(bytes)
        };
        let diagnostic = async {
            let mut bytes = Vec::new();
            stderr
                .take(ENCODER_DIAGNOSTIC_LIMIT + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(bytes)
        };
        tokio::try_join!(output, diagnostic)
    });
    (started, drained)
}

/// A live run, owned by its recipe between windows. Dropping it kills the
/// helper; [`Self::kill`] also waits until its resources are released.
pub(crate) struct WindowRun {
    key: Vec<String>,
    frame_cap: u8,
    windows: u8,
    outputs: VecDeque<RunOutput>,
    helper: RunHelper,
    cancel: CancellationToken,
    input: Arc<SourceInputLease>,
    reservation: crate::vodencode::EncodePermit,
    source: Arc<SourceFence>,
    /// The helper process's own registration. Every window registers it with
    /// the rendition's source owners ahead of its encoder, so a detaching
    /// viewer settles only after the helper reading its source is reaped.
    registration: ProducerRegistration,
    /// The next window's encoder, when it was started ahead of its request.
    prestart: Option<PreStart>,
}

/// The recipe's slot for its window run. Ending is sticky: a request to end
/// the run while a window holds it (or while the slot is briefly locked) is
/// remembered, and that window's run is killed instead of stored.
#[derive(Default)]
pub(crate) struct WindowRunSlot {
    run: tokio::sync::Mutex<Option<(u64, WindowRun)>>,
    ends: std::sync::atomic::AtomicU64,
}

impl WindowRunSlot {
    /// End the run now if it is resting, and in any case before it can be
    /// continued or stored again.
    pub(crate) fn end(&self) {
        self.ends.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Ok(mut run) = self.run.try_lock() {
            run.take();
        }
    }
    /// The current end count; a run stored under an older count is ended.
    pub(crate) fn ends(&self) -> u64 {
        self.ends.load(std::sync::atomic::Ordering::Acquire)
    }
    /// Take the stored run for one window, discarding (killing) one whose end
    /// was requested after it was stored.
    pub(crate) async fn take(
        &self,
    ) -> (
        tokio::sync::MutexGuard<'_, Option<(u64, WindowRun)>>,
        Option<WindowRun>,
    ) {
        let mut held = self.run.lock().await;
        let run = match held.take() {
            Some((stored, run)) if stored == self.ends() => Some(run),
            Some((_, run)) => {
                run.kill().await;
                None
            }
            None => None,
        };
        (held, run)
    }
    /// Store a run that may continue, unless an end was requested since `ends`.
    pub(crate) async fn store(
        &self,
        held: &mut tokio::sync::MutexGuard<'_, Option<(u64, WindowRun)>>,
        run: WindowRun,
        ends: u64,
    ) {
        if ends == self.ends() {
            **held = Some((ends, run));
        } else {
            run.kill().await;
        }
    }
    /// The worker claim a generation for plan entry `at` shares with the
    /// stored run's pre-started encoder for exactly that window and
    /// reservation. Any other run is killed and settled first, which releases
    /// a claim it held, so the caller can claim the reservation itself.
    pub(crate) async fn claim_for(
        &self,
        at: u32,
        reservation: &crate::vodencode::EncodePermit,
    ) -> Option<Arc<crate::vodencode::EncodeWorkerPermit>> {
        let mut held = self.run.lock().await;
        let claim = held
            .as_ref()
            .filter(|(stored, _)| *stored == self.ends())
            .and_then(|(_, run)| run.prestarted_claim(at, reservation));
        if claim.is_none() {
            if let Some((_, run)) = held.take() {
                run.kill().await;
            }
        }
        claim
    }
    /// Is `input` the stored run's source reader?
    pub(crate) async fn owns_input(&self, input: &Arc<SourceInputLease>) -> bool {
        self.run
            .lock()
            .await
            .as_ref()
            .is_some_and(|(_, run)| run.owns_input(input))
    }
}

impl WindowRun {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        launch: RunLaunch,
        frame_cap: u8,
        outputs: Vec<RunOutput>,
        helper: RunHelper,
        cancel: CancellationToken,
        input: Arc<SourceInputLease>,
        source: Arc<SourceFence>,
        registration: ProducerRegistration,
    ) -> Self {
        Self {
            key: launch.key,
            frame_cap,
            windows: u8::try_from(outputs.len() + 1).unwrap_or(u8::MAX),
            outputs: outputs.into(),
            helper,
            cancel,
            input,
            reservation: launch.reservation,
            source,
            registration,
            prestart: None,
        }
    }
    pub(crate) fn windows(&self) -> u8 {
        self.windows
    }
    /// Whether the helper is gone or going: a run that ended renders nothing more.
    pub(crate) fn ended(&self) -> bool {
        self.cancel.is_cancelled() || self.helper.exit.borrow().is_some()
    }
    pub(crate) fn has_more(&self) -> bool {
        !self.outputs.is_empty() && !self.ended()
    }
    fn prestarted_claim(
        &self,
        at: u32,
        reservation: &crate::vodencode::EncodePermit,
    ) -> Option<Arc<crate::vodencode::EncodeWorkerPermit>> {
        self.prestart
            .as_ref()
            .filter(|prestart| {
                prestart.at == at
                    && prestart.claim.reservation().same_reservation(reservation)
                    && !self.cancel.is_cancelled()
            })
            .map(|prestart| Arc::clone(&prestart.claim))
    }
    /// Start the next window's encoder now. Called right after the previous
    /// window's encoder was reaped, which released its worker claim: the run
    /// takes the family's one claim for this encoder (never a second one) and
    /// does nothing when the claim, the budget or the recipe does not allow it.
    pub(crate) fn prestart(&mut self, request: PreStartRequest) {
        if self.prestart.is_some() || self.ended() {
            return;
        }
        let Some(claim) = self.reservation.clone().try_claim_worker() else {
            return;
        };
        let Some(cpu) = claim.retained_resources().software.map(|cpu| cpu.threads()) else {
            return;
        };
        if check_streaming_budget(request.mode, request.decode, &request.encoder_args, cpu)
            .and_then(|()| check_streaming_clock(&request.encoder_args, true))
            .is_err()
        {
            return;
        }
        let Some(output) = self.outputs.pop_front() else {
            return;
        };
        let cancel = self.cancel.child_token();
        let at = request.at;
        let encoder_args = request.encoder_args.clone();
        let (started, drained) = spawn_prestart(
            request,
            output.fifo,
            self.input.url.clone(),
            self.helper.exit(),
            cancel.clone(),
        );
        self.prestart = Some(PreStart {
            at,
            encoder_args,
            bounds: output.bounds,
            directory: output.directory,
            held: output.held,
            claim: Arc::new(claim),
            cancel,
            started: Some(started),
            child: None,
            drained: Some(drained),
        });
    }
    /// Is this the run's source reader? The run, not the window that opened
    /// it, closes it.
    pub(crate) fn owns_input(&self, input: &Arc<SourceInputLease>) -> bool {
        Arc::ptr_eq(&self.input, input)
    }
    /// May this run render `window` next for an otherwise identical helper?
    pub(crate) fn continues(
        &self,
        window: &SegmentWindow,
        frames: u8,
        key: &[String],
        admission: &SegmentAdmission,
        encoder_args: &[String],
    ) -> bool {
        let same_admission = admission
            .reservation()
            .is_some_and(|reservation| reservation.same_reservation(&self.reservation));
        // A pre-started window needs only its encoder, already reading the
        // helper's output: the helper may have finished the run since.
        let (next, ended) = match &self.prestart {
            Some(prestart) => (
                (prestart.encoder_args == encoder_args).then_some(prestart.bounds),
                self.cancel.is_cancelled(),
            ),
            None => (
                self.outputs.front().map(|output| output.bounds),
                self.ended(),
            ),
        };
        run_continues(
            next,
            ended,
            (window.start, window.end),
            frames,
            self.frame_cap,
            key == self.key,
            same_admission && self.source.unchanged(),
        )
    }
    /// Arm the idle bound: the run is killed unless a window claims it first.
    pub(crate) fn rest(&self, idle: Duration) {
        let _ = self.helper.idle.send_replace(Some(Instant::now() + idle));
    }
    /// Claim the run for the window just accepted by [`Self::continues`] and
    /// wait until the helper has begun that window. `Ok(false)` means the run
    /// ended first (idle expiry, the helper's exit): the caller renders the
    /// window some other way, it is not this window's failure. An error is
    /// this window's own cancellation or deadline.
    pub(crate) async fn ready(
        &mut self,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<bool, String> {
        let _ = self.helper.idle.send_replace(None);
        if let Some(prestart) = self.prestart.as_mut() {
            return tokio::select! {
                started = prestart.started() => Ok(started && !self.cancel.is_cancelled()),
                _ = cancel.cancelled() => Err("DV window run window cancelled".into()),
                () = tokio::time::sleep_until(deadline) => {
                    Err("DV window run window deadline expired".into())
                }
            };
        }
        let Some(output) = self.outputs.front().filter(|_| !self.ended()) else {
            return Ok(false);
        };
        let mut exit = self.helper.exit();
        match wait_for_output(&output.fifo, &mut exit, cancel, deadline).await {
            Ok(()) => Ok(true),
            Err(_) if self.ended() => Ok(false),
            Err(cause) => Err(cause),
        }
    }
    /// Kill the helper and wait until its lane, offset, reservation, source
    /// reader and directories are released.
    pub(crate) async fn kill(mut self) {
        self.cancel.cancel();
        if let Some(prestart) = self.prestart.take() {
            prestart.kill().await;
        }
        self.helper.settled().await;
    }

    /// The producer for the run's next window: the same graph a single window
    /// gets, with the encoder reading this window's FIFO instead of a fresh
    /// helper's pipe. The window's own source reader is closed unused; its
    /// encoder reads audio through the run's.
    pub(crate) async fn attach(
        &mut self,
        mut stream: StreamingSegmentRequest,
    ) -> Result<SegmentProducer, String> {
        let _ = self.helper.idle.send_replace(None);
        if let Some(own) = stream.request.source_input.take() {
            if !Arc::ptr_eq(&own, &self.input) {
                // The generation published its fresh reader's observer; the
                // run's reader, which does the reading, takes its place.
                self.input.adopt_observer_publication(&own).await?;
                own.close_and_wait()
                    .await
                    .map_err(|error| format!("DV unused window source close: {error}"))?;
            }
        }
        stream.request.source_input = Some(Arc::clone(&self.input));
        check_streaming_request(&stream)?;
        let window = stream
            .window
            .ok_or("a DV window run requires a bounded window")?;
        if let Some(prestart) = self.prestart.take() {
            return self.attach_prestarted(stream, window, prestart).await;
        }
        let output = self
            .outputs
            .pop_front()
            .ok_or("DV window run has no further window")?;
        if (window.start, window.end) != output.bounds {
            return Err("the requested window is not the run's next window".into());
        }
        let index = self.windows - 1 - u8::try_from(self.outputs.len()).unwrap_or(0);
        stream.stages.run_window(index, self.windows);
        let stages = Arc::clone(&stream.stages);
        let request = stream.request;
        let cancel = request.cancel.child_token();
        let reservation = request
            .producer
            .try_reserve_empty()
            .map_err(|e| e.to_string())?;
        let mut exit = self.helper.exit();
        wait_for_output(
            &output.fifo,
            &mut exit,
            &cancel,
            request.preparation_deadline,
        )
        .await?;
        set_blocking(&output.fifo)?;
        check_source(&request.source)?;
        if cancel.is_cancelled() || Instant::now() >= request.preparation_deadline {
            return Err("streaming segment cancelled before encoder".into());
        }
        let encoded = spawn_streaming_encoder(
            &request.encoder,
            &request.encoder_args,
            &request.runtime_cache,
            stream.background,
            &output.fifo,
            None,
            Some(self.input.url.as_str()),
        )?;
        drop(output.fifo); // The encoder is this window's only reader.
        stages.mark(WindowStage::EncoderSpawned);
        let (registration, writers) = reservation.attach_registered_job_owned(
            encoded.child,
            encoded.child_job,
            request.at,
            Box::new(WindowResources {
                _source_input: self.input.physical_owner(),
                _admission: request.admission,
                _run_claim: None,
                _directory: Arc::clone(&output.directory),
            }),
        );
        let completed = window_complete(
            self.outputs.front().map(RunOutput::held),
            self.helper.exit(),
            Arc::clone(&self.source),
        );
        let completion = supervise_streaming_helper(
            completed,
            self.cancel.clone(),
            cancel.clone(),
            Arc::downgrade(&request.producer),
            registration.clone(),
        );
        Ok(SegmentProducer {
            source_settlement: None,
            registration,
            writers: Some(writers),
            stdout: Some(encoded.stdout),
            stderr: Some(encoded.stderr),
            unaccepted_cancel: Some(cancel),
            helper_completion: Some(completion),
            evidence: Some(SegmentEvidence {
                directory: output.directory,
                held: output.held,
            }),
            run_helper: Some(self.registration.clone()),
            drained: None,
        })
    }
}

impl WindowRun {
    /// [`Self::attach`] for the window whose encoder already runs: the same
    /// registration, resources, completion and evidence, with the encoder's
    /// output taken from its reader instead of live pipes.
    async fn attach_prestarted(
        &mut self,
        stream: StreamingSegmentRequest,
        window: SegmentWindow,
        mut prestart: PreStart,
    ) -> Result<SegmentProducer, String> {
        if (window.start, window.end) != prestart.bounds
            || stream.request.encoder_args != prestart.encoder_args
        {
            prestart.kill().await;
            return Err("the requested window is not the run's pre-started window".into());
        }
        let index = self.windows - 1 - u8::try_from(self.outputs.len()).unwrap_or(0);
        stream.stages.run_window(index, self.windows);
        let stages = Arc::clone(&stream.stages);
        let request = stream.request;
        let cancel = request.cancel.child_token();
        let reservation = match request.producer.try_reserve_empty() {
            Ok(reservation) => reservation,
            Err(error) => {
                prestart.kill().await;
                return Err(error.to_string());
            }
        };
        if !prestart.started().await {
            prestart.kill().await;
            return Err("DV pre-started window encoder did not start".into());
        }
        if let Err(cause) = check_source(&request.source) {
            prestart.kill().await;
            return Err(cause);
        }
        let (child, child_job) = prestart.child.take().expect("started pre-start encoder");
        stages.mark(WindowStage::EncoderSpawned);
        let (registration, writers) = reservation.attach_registered_job_owned(
            child,
            child_job,
            request.at,
            Box::new(WindowResources {
                _source_input: self.input.physical_owner(),
                _admission: request.admission,
                _run_claim: Some(Arc::clone(&prestart.claim)),
                _directory: Arc::clone(&prestart.directory),
            }),
        );
        let completed = window_complete(
            self.outputs.front().map(RunOutput::held),
            self.helper.exit(),
            Arc::clone(&self.source),
        );
        let completion = supervise_streaming_helper(
            completed,
            self.cancel.clone(),
            cancel.clone(),
            Arc::downgrade(&request.producer),
            registration.clone(),
        );
        Ok(SegmentProducer {
            source_settlement: None,
            registration,
            writers: Some(writers),
            stdout: None,
            stderr: None,
            unaccepted_cancel: Some(cancel),
            helper_completion: Some(completion),
            evidence: Some(SegmentEvidence {
                directory: Arc::clone(&prestart.directory),
                held: Arc::clone(&prestart.held),
            }),
            run_helper: Some(self.registration.clone()),
            drained: prestart.drained.take(),
        })
    }
}

impl Drop for WindowRun {
    fn drop(&mut self) {
        self.cancel.cancel();
        // A pre-started encoder nobody attached is killed and reaped by its
        // own owner task; its claim and directory go with it.
        if let Some(prestart) = self.prestart.take() {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(prestart.kill());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(start: i64, end: i64) -> Bounds {
        ((start, 24_000), (end, 24_000))
    }

    #[test]
    fn the_run_tail_names_each_later_window_end_directory_and_fifo() {
        let tail = run_tail(&[
            ("98098/24000".into(), "/c/a".into(), "/c/a/video.nut".into()),
            (
                "146146/24000".into(),
                "/c/b".into(),
                "/c/b/video.nut".into(),
            ),
        ]);
        assert_eq!(
            tail,
            [
                "--window-run",
                "3",
                "98098/24000",
                "/c/a",
                "/c/a/video.nut",
                "146146/24000",
                "/c/b",
                "/c/b/video.nut",
            ]
        );
    }

    #[test]
    fn a_helper_key_ignores_only_the_source_url_cap_and_bounds() {
        let args: Vec<String> = (0..20).map(|value| value.to_string()).collect();
        let mut other = args.clone();
        for index in [0, 3, 11, 12] {
            other[index] = "different".into();
        }
        assert_eq!(helper_key(&args), helper_key(&other));
        for index in [1, 2, 4, 9, 10, 13, 14, 19] {
            let mut changed = args.clone();
            changed[index] = "different".into();
            assert_ne!(helper_key(&args), helper_key(&changed), "argv {index}");
        }
    }

    #[test]
    fn a_run_continues_only_with_its_next_window_and_the_same_helper() {
        let next = Some(bounds(48048, 96096));
        let ok = |next, ended, requested, frames, same_helper, same_admission| {
            run_continues(
                next,
                ended,
                requested,
                frames,
                48,
                same_helper,
                same_admission,
            )
        };
        assert!(ok(next, false, bounds(48048, 96096), 48, true, true));
        // A seek (or a skipped window) asks for something else.
        assert!(!ok(next, false, bounds(96096, 144144), 48, true, true));
        assert!(!ok(next, false, bounds(0, 48048), 48, true, true));
        // Ended, idle-expired, exhausted or differently shaped runs restart.
        assert!(!ok(next, true, bounds(48048, 96096), 48, true, true));
        assert!(!ok(None, false, bounds(48048, 96096), 48, true, true));
        assert!(!ok(next, false, bounds(48048, 96096), 49, true, true));
        assert!(!ok(next, false, bounds(48048, 96096), 48, false, true));
        assert!(!ok(next, false, bounds(48048, 96096), 48, true, false));
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_run_expires_and_a_claimed_one_waits() {
        let (idle, receiver) = watch::channel(None);
        let expiry = tokio::spawn(idle_expiry(receiver));
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(!expiry.is_finished(), "an unarmed run never expires");
        idle.send_replace(Some(Instant::now() + Duration::from_secs(8)));
        tokio::time::sleep(Duration::from_secs(4)).await;
        // A window claimed it before the bound.
        idle.send_replace(None);
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert!(!expiry.is_finished());
        idle.send_replace(Some(Instant::now() + Duration::from_secs(8)));
        assert_eq!(expiry.await.expect("expiry"), "DV window run idle expired");

        let (idle, receiver) = watch::channel(None);
        let expiry = tokio::spawn(idle_expiry(receiver));
        drop(idle);
        assert_eq!(expiry.await.expect("expiry"), "DV window run owner dropped");
    }

    fn window(start: i64, end: i64) -> SegmentWindow {
        SegmentWindow {
            start: (start, 24_000),
            end: (end, 24_000),
            max_preroll: 512,
            source_origin: None,
        }
    }

    #[test]
    fn run_outputs_are_private_fifos_whose_read_end_exists_first() {
        use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
        let cache = tempfile::tempdir().expect("cache");
        let (created, tail) = outputs(
            cache.path(),
            &window(0, 48048),
            &[window(48048, 96096), window(96096, 144144)],
        )
        .expect("outputs");
        assert_eq!(&tail[..3], ["--window-run", "3", "96096/24000"]);
        assert_eq!(tail[5], "144144/24000");
        for (output, directory) in created.iter().zip([&tail[3], &tail[6]]) {
            assert!(std::path::Path::new(directory).is_absolute());
            assert_eq!(output.directory.path(), std::path::Path::new(directory));
            let nut = std::path::Path::new(directory).join(RUN_OUTPUT);
            assert!(std::fs::metadata(&nut).expect("fifo").file_type().is_fifo());
            // The reader exists, so a writer's open does not block.
            let writer = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&nut);
            assert!(writer.is_ok(), "writer open: {writer:?}");
        }
        assert!(outputs(cache.path(), &window(0, 48048), &[window(48049, 96096)]).is_err());
        assert!(outputs(cache.path(), &window(0, 48048), &[]).is_err());
    }

    #[tokio::test]
    async fn a_fifo_reader_hands_off_only_once_its_window_begins() {
        use std::io::{Read, Write};
        let cache = tempfile::tempdir().expect("cache");
        let (output, [_, nut]) =
            RunOutput::create(cache.path(), &window(48048, 96096)).expect("output");
        let (exit_sender, mut exit) = watch::channel(None);
        let cancel = CancellationToken::new();
        let soon = Instant::now() + Duration::from_millis(120);
        assert_eq!(
            wait_for_output(&output.fifo, &mut exit, &cancel, soon).await,
            Err("DV window run window deadline expired".into()),
            "no writer yet: not ready, and never a premature end of input"
        );
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&nut)
            .expect("helper writer");
        writer.write_all(b"nut header").expect("write");
        let later = Instant::now() + Duration::from_secs(5);
        wait_for_output(&output.fifo, &mut exit, &cancel, later)
            .await
            .expect("ready");
        set_blocking(&output.fifo).expect("blocking");
        drop(writer);
        let mut bytes = Vec::new();
        (&output.fifo).read_to_end(&mut bytes).expect("read");
        assert_eq!(bytes, b"nut header");

        // A helper that ends first fails the window instead of hanging it.
        let (other, _) = RunOutput::create(cache.path(), &window(96096, 144144)).expect("output");
        exit_sender.send_replace(Some(Err("renderer refused segment".into())));
        assert_eq!(
            wait_for_output(&other.fifo, &mut exit, &cancel, later).await,
            Err("renderer refused segment".into())
        );
    }

    fn fence() -> Arc<SourceFence> {
        let file = tempfile::tempfile().expect("source");
        Arc::new(SourceFence::clone_current_held(&file).expect("fence"))
    }

    #[tokio::test]
    async fn a_run_window_completes_when_the_helper_begins_the_next() {
        let next = tempfile::tempdir().expect("next");
        let held = Arc::new(
            plurx_core::fs_secure::open_directory_nofollow_blocking(next.path()).expect("held"),
        );
        let (exit_sender, exit) = watch::channel(None);
        let waiting = tokio::spawn(window_complete(Some(held.clone()), exit.clone(), fence()));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(!waiting.is_finished(), "the window is still being written");
        std::fs::write(next.path().join("renderer.jsonl"), b"").expect("next window");
        waiting.await.expect("join").expect("complete");

        // The last window completes with the helper's successful exit only.
        let last = tokio::spawn(window_complete(None, exit.clone(), fence()));
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(!last.is_finished());
        exit_sender.send_replace(Some(Ok(())));
        last.await.expect("join").expect("complete");

        // A helper failure before the next window began fails this one.
        let other = tempfile::tempdir().expect("other");
        let held = Arc::new(
            plurx_core::fs_secure::open_directory_nofollow_blocking(other.path()).expect("held"),
        );
        let (exit_sender, exit) = watch::channel(None);
        exit_sender.send_replace(Some(Err("renderer failed: exit 9".into())));
        assert_eq!(
            window_complete(Some(held), exit, fence()).await,
            Err("renderer failed: exit 9".into())
        );
    }

    fn reservation() -> crate::vodencode::EncodePermit {
        crate::admission::Admissions::new()
            .try_admit_bundle(
                2,
                4,
                &crate::admission::TranscodeResourceEstimate {
                    hardware_slot: true,
                    cpu_threads: 2,
                    decoder_threads: None,
                },
                crate::admission::Priority::Live,
            )
            .expect("window graph reservation")
            .into()
    }

    /// The pre-started encoder and the generation that later asks for its
    /// window share one worker claim. The family's next claim is possible only
    /// once both have let it go, i.e. after the encoder's reap and the
    /// window's end, never while either still holds it.
    #[test]
    fn a_run_shared_claim_is_released_only_by_its_last_holder() {
        let family = reservation();
        let claim = Arc::new(family.clone().try_claim_worker().expect("one claim"));
        assert!(
            family.clone().try_claim_worker().is_none(),
            "never a second"
        );
        let window = SegmentAdmission::RunShared(Arc::clone(&claim));
        assert!(window
            .reservation()
            .is_some_and(|shared| shared.same_reservation(&family)));
        drop(claim); // The encoder was reaped.
        assert!(
            family.clone().try_claim_worker().is_none(),
            "the window holds it"
        );
        drop(window); // The window ended.
        assert!(family.clone().try_claim_worker().is_some());
    }

    fn prestart_request(cache: &std::path::Path) -> PreStartRequest {
        PreStartRequest {
            at: 7,
            // A stand-in encoder that copies its NUT input to its output.
            encoder_args: vec!["/proc/self/fd/3".into()],
            encoder: "/bin/cat".into(),
            runtime_cache: cache.to_owned(),
            mode: plurx_core::transcode::dv_processing::DvProcessingMode::BaseRpu,
            decode: RendererDecode::Software,
            wait: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn a_prestarted_encoder_starts_once_its_window_begins_and_its_output_is_kept() {
        use std::io::Write;
        let cache = tempfile::tempdir().expect("cache");
        let (output, [_, nut]) =
            RunOutput::create(cache.path(), &window(48048, 96096)).expect("output");
        let (_exit_sender, exit) = watch::channel(None);
        let (mut started, drained) = spawn_prestart(
            prestart_request(cache.path()),
            output.fifo,
            "http://127.0.0.1:9/unused".into(),
            exit,
            CancellationToken::new(),
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            started.try_recv().is_err(),
            "no encoder before the helper begins the window"
        );
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&nut)
            .expect("helper writer");
        writer.write_all(b"window pictures").expect("write");
        drop(writer); // The helper closes the window's NUT output.
        let (mut child, job) = started.await.expect("sent").expect("spawned");
        let (bytes, diagnostic) = drained.await.expect("joined").expect("read");
        assert_eq!(bytes, b"window pictures");
        assert!(diagnostic.is_empty());
        assert!(child.wait().await.expect("reaped").success());
        drop(job);
    }

    #[tokio::test]
    async fn a_killed_prestart_reaps_its_encoder_and_releases_the_claim() {
        use std::io::Write;
        let cache = tempfile::tempdir().expect("cache");
        let (output, [_, nut]) =
            RunOutput::create(cache.path(), &window(48048, 96096)).expect("output");
        let family = reservation();
        let claim = Arc::new(family.clone().try_claim_worker().expect("claim"));
        let (_exit_sender, exit) = watch::channel(None);
        let cancel = CancellationToken::new();
        let (started, drained) = spawn_prestart(
            prestart_request(cache.path()),
            output.fifo,
            "http://127.0.0.1:9/unused".into(),
            exit,
            cancel.clone(),
        );
        // The helper began the window and is still writing it.
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&nut)
            .expect("helper writer");
        writer.write_all(b"first picture").expect("write");
        let directory = output.directory.path().to_owned();
        let mut prestart = PreStart {
            at: 7,
            encoder_args: vec!["/proc/self/fd/3".into()],
            bounds: output.bounds,
            directory: output.directory,
            held: output.held,
            claim,
            cancel,
            started: Some(started),
            child: None,
            drained: Some(drained),
        };
        assert!(prestart.started().await, "the encoder runs");
        let pid = prestart.child.as_ref().and_then(|(child, _)| child.id());
        assert!(pid.is_some());
        tokio::time::timeout(Duration::from_secs(5), prestart.kill())
            .await
            .expect("killed, reaped and joined while its writer is still open");
        assert!(
            !directory.exists(),
            "the window's private directory is gone"
        );
        assert!(
            family.clone().try_claim_worker().is_some(),
            "the claim went with the pre-start"
        );
        drop(writer);

        // A run killed before the helper begins the window starts nothing.
        let (output, _) = RunOutput::create(cache.path(), &window(96096, 144144)).expect("output");
        let (_exit_sender, exit) = watch::channel(None);
        let cancel = CancellationToken::new();
        let (started, drained) = spawn_prestart(
            prestart_request(cache.path()),
            output.fifo,
            "http://127.0.0.1:9/unused".into(),
            exit,
            cancel.clone(),
        );
        cancel.cancel();
        assert!(started.await.expect("sent").is_err());
        assert!(drained.await.expect("joined").is_err());
    }
}
