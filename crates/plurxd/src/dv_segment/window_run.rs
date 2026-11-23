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
    /// Pre-started encoders that may still read the source and are not yet a
    /// window's registered encoder ([`LiveReader`]).
    prestarts: Arc<watch::Sender<usize>>,
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
///
/// `staged` stops the helper when `stage_stop` is cancelled. Ending the run
/// (`cancel`) cancels it only once every pre-started encoder that is not yet
/// a window's has been killed and reaped, so the helper's reap, which a
/// detaching source owner waits for, means no run process reads the source.
pub(crate) fn spawn_helper(
    staged: impl std::future::Future<Output = Result<Vec<u8>, String>> + Send + 'static,
    cancel: CancellationToken,
    stage_stop: CancellationToken,
    input: Arc<SourceInputLease>,
    directories: Vec<Arc<tempfile::TempDir>>,
) -> RunHelper {
    let (exit_sender, exit) = watch::channel(None);
    let (settled_sender, settled) = watch::channel(false);
    let (idle, idle_receiver) = watch::channel(None);
    let prestarts = Arc::new(watch::channel(0_usize).0);
    let mut live = prestarts.subscribe();
    let stopper_cancel = cancel.clone();
    tokio::spawn(async move {
        let stopper = async move {
            stopper_cancel.cancelled().await;
            let _ = live.wait_for(|count| *count == 0).await;
            stage_stop.cancel();
            std::future::pending::<std::convert::Infallible>().await
        };
        let staged = async move {
            tokio::select! {
                result = staged => result,
                never = stopper => match never {},
            }
        };
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
        prestarts,
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

/// Has the helper begun the window whose directory this is? It creates the
/// window's `renderer.jsonl` only after the previous window is complete.
fn begun(directory: &std::fs::File) -> bool {
    SegmentEvidence::open(directory, c"renderer.jsonl", libc::O_RDONLY).is_ok()
}

/// A run window's helper completion: the next window's `renderer.jsonl`
/// exists (the helper creates it only after this window's observations are
/// flushed and its NUT output is closed), or the helper exited successfully.
pub(crate) async fn window_complete(
    next: Option<Arc<std::fs::File>>,
    mut exit: watch::Receiver<Option<Result<(), String>>>,
    source: Arc<SourceFence>,
) -> Result<(), String> {
    let advanced = |next: &Option<Arc<std::fs::File>>| next.as_deref().is_some_and(begun);
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

/// What a window that attaches a pre-started encoder takes over from the
/// pre-start task: the child (unreaped, perhaps already exited), its share of
/// the worker claim, its use of the run's source reader, its private
/// directory and its bounded output.
struct Handover {
    child: tokio::process::Child,
    child_job: crate::process_control::ChildJob,
    spawned: Instant,
    claim: Arc<crate::vodencode::EncodeWorkerPermit>,
    source_input: SourceInputUse,
    directory: Arc<tempfile::TempDir>,
    held: Arc<std::fs::File>,
    output: DrainedOutput,
}

/// One pre-started encoder counted in [`RunHelper::prestarts`] until it is
/// a window's (attached and registered) encoder or has been reaped.
struct LiveReader(Arc<watch::Sender<usize>>);
impl LiveReader {
    fn new(count: &Arc<watch::Sender<usize>>) -> Self {
        count.send_modify(|count| *count += 1);
        Self(Arc::clone(count))
    }
}
impl Drop for LiveReader {
    fn drop(&mut self) {
        self.0.send_modify(|count| *count = count.saturating_sub(1));
    }
}

/// Kill and reap a child this owner holds.
async fn kill_and_reap(
    mut child: tokio::process::Child,
    child_job: crate::process_control::ChildJob,
) {
    let _ = child.start_kill();
    let _ = child.wait().await;
    drop(child_job);
}

/// A window encoder's stdout and stderr, each read to its bound plus one
/// byte. Exceeding either fails at once: the reader stops and drops its pipe,
/// so the encoder cannot finish, instead of waiting for the window deadline.
async fn drain_bounded_output(
    stdout: impl tokio::io::AsyncRead + Unpin,
    stderr: impl tokio::io::AsyncRead + Unpin,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let output = async {
        let mut bytes = Vec::new();
        stdout
            .take(ENCODER_OUTPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > ENCODER_OUTPUT_LIMIT {
            return Err("DV encoder private-output bound exceeded".to_owned());
        }
        Ok(bytes)
    };
    let diagnostic = async {
        let mut bytes = Vec::new();
        stderr
            .take(ENCODER_DIAGNOSTIC_LIMIT + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > ENCODER_DIAGNOSTIC_LIMIT {
            return Err("DV encoder diagnostic bound exceeded".to_owned());
        }
        Ok(bytes)
    };
    tokio::try_join!(output, diagnostic)
}

/// The run's next window's encoder, started as soon as the previous window's
/// encoder was reaped instead of after that window's verification,
/// publication and the next generation's spawn.
///
/// The pre-start task owns the encoder, the worker claim, a use of the run's
/// source reader and the window's directory until a window takes them
/// ([`Self::take`]). Cancelling it (the run's end, idle expiry, or dropping
/// this handle) makes the task kill and reap the encoder and only then
/// release the claim, reader and directory; [`Self::settled`] reports that.
/// The run itself holds the claim only weakly, so nothing it keeps can hold
/// the claim past that reap.
struct PreStart {
    at: u32,
    encoder_args: Vec<String>,
    bounds: Bounds,
    claim: std::sync::Weak<crate::vodencode::EncodeWorkerPermit>,
    /// `Some(Ok)` once the encoder is spawned, `Some(Err)` if it could not be.
    spawned: watch::Receiver<Option<Result<(), String>>>,
    take: Option<tokio::sync::oneshot::Sender<tokio::sync::oneshot::Sender<Handover>>>,
    cancel: CancellationToken,
    /// Set when the task has ended: the encoder handed over, or killed and
    /// reaped with everything it held released.
    settled: watch::Receiver<bool>,
}

impl PreStart {
    fn failed(&self) -> bool {
        matches!(&*self.spawned.borrow(), Some(Err(_)))
    }
    /// Wait for the spawn outcome. Cancel-safe: nothing is consumed.
    async fn spawned(&mut self) -> bool {
        matches!(
            self.spawned.wait_for(Option::is_some).await.as_deref(),
            Ok(Some(Ok(())))
        )
    }
    /// Take the encoder for the window attaching it. `None` if the task ended
    /// first; if this future is dropped before the handover completes, the
    /// task kills and reaps the encoder itself.
    async fn take(&mut self) -> Option<Handover> {
        let take = self.take.take()?;
        let (reply, handed) = tokio::sync::oneshot::channel();
        take.send(reply).ok()?;
        handed.await.ok()
    }
}

impl Drop for PreStart {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// The pre-start task. Waits until the helper has begun the window, spawns
/// its encoder on the FIFO and reads its output; until a window takes the
/// encoder, cancellation kills and reaps it here.
#[allow(clippy::too_many_arguments)]
fn spawn_prestart(
    request: PreStartRequest,
    fifo: std::fs::File,
    url: String,
    mut exit: watch::Receiver<Option<Result<(), String>>>,
    cancel: CancellationToken,
    claim: Arc<crate::vodencode::EncodeWorkerPermit>,
    source_input: SourceInputUse,
    directory: Arc<tempfile::TempDir>,
    held: Arc<std::fs::File>,
    reader: LiveReader,
) -> (
    watch::Receiver<Option<Result<(), String>>>,
    tokio::sync::oneshot::Sender<tokio::sync::oneshot::Sender<Handover>>,
    watch::Receiver<bool>,
) {
    let (spawned_sender, spawned) = watch::channel(None);
    let (take, mut taken) =
        tokio::sync::oneshot::channel::<tokio::sync::oneshot::Sender<Handover>>();
    let (settled_sender, settled) = watch::channel(false);
    let task_settled = settled.clone();
    tokio::spawn(async move {
        let deadline = Instant::now() + request.wait;
        let started = async {
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
        } = match started {
            Ok(spawned) => spawned,
            Err(cause) => {
                let _ = spawned_sender.send_replace(Some(Err(cause)));
                drop((reader, claim, source_input, directory, held));
                let _ = settled_sender.send_replace(true);
                return;
            }
        };
        let spawned_at = Instant::now();
        let _ = spawned_sender.send_replace(Some(Ok(())));
        let (output_sender, output) = tokio::sync::oneshot::channel();
        let mut output_sender = Some(output_sender);
        let drain = drain_bounded_output(stdout, stderr);
        tokio::pin!(drain);
        let reply = loop {
            tokio::select! {
                reply = &mut taken => break reply.ok(),
                () = cancel.cancelled() => break None,
                result = &mut drain, if output_sender.is_some() => {
                    // Finished, or over its bound, before a window took it.
                    if let Some(sender) = output_sender.take() {
                        let _ = sender.send(result);
                    }
                }
            }
        };
        match reply {
            Some(reply) => {
                let handover = Handover {
                    child,
                    child_job,
                    spawned: spawned_at,
                    claim,
                    source_input,
                    directory,
                    held,
                    output: DrainedOutput {
                        output,
                        settled: task_settled,
                    },
                };
                match reply.send(handover) {
                    Ok(()) => {
                        // The window registers its encoder from here on.
                        drop(reader);
                        if let Some(sender) = output_sender.take() {
                            let _ = sender.send(drain.await);
                        }
                    }
                    Err(returned) => {
                        // The window gave up while taking it.
                        let Handover {
                            child, child_job, ..
                        } = returned;
                        kill_and_reap(child, child_job).await;
                        drop(reader);
                    }
                }
            }
            None => {
                kill_and_reap(child, child_job).await;
                drop((claim, source_input, directory, held));
                drop(reader);
            }
        }
        let _ = settled_sender.send_replace(true);
    });
    (spawned, take, settled)
}

/// What a killed run still has to release: its pre-start's reap and the
/// helper's settlement. Ending a run is synchronous; whoever claims next
/// waits for this first.
pub(crate) struct RunSettlement {
    helper: watch::Receiver<bool>,
    prestart: Option<watch::Receiver<bool>>,
}

impl RunSettlement {
    pub(crate) async fn wait(self) {
        if let Some(mut prestart) = self.prestart {
            let _ = prestart.wait_for(|settled| *settled).await;
        }
        let mut helper = self.helper;
        let _ = helper.wait_for(|settled| *settled).await;
    }
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
    /// Runs [`Self::end`] dropped without waiting. Taking or claiming waits
    /// for them first, so a claim they held is released before anyone
    /// claims the reservation again.
    pending: std::sync::Mutex<Vec<RunSettlement>>,
}

impl WindowRunSlot {
    /// End the run now if it is resting, and in any case before it can be
    /// continued or stored again.
    pub(crate) fn end(&self) {
        self.ends.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Ok(mut held) = self.run.try_lock() {
            if let Some((_, run)) = held.take() {
                self.pending
                    .lock()
                    .expect("DV window run settlements")
                    .push(run.settlement());
            }
        }
    }
    /// Wait until every run ended without waiting has released what it held.
    async fn settle_pending(&self) {
        let pending = std::mem::take(&mut *self.pending.lock().expect("DV window run settlements"));
        for settlement in pending {
            settlement.wait().await;
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
        self.settle_pending().await;
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
    /// reservation. A run whose pre-start holds the claim for anything else
    /// (or that was ended) is killed and settled, which releases the claim,
    /// so the caller can claim the reservation itself; a run without a
    /// pre-start holds no claim and is left alone.
    pub(crate) async fn claim_for(
        &self,
        at: u32,
        reservation: &crate::vodencode::EncodePermit,
    ) -> Option<Arc<crate::vodencode::EncodeWorkerPermit>> {
        self.settle_pending().await;
        let mut held = self.run.lock().await;
        let (claim, kill) = match held.as_ref() {
            Some((stored, _)) if *stored != self.ends() => (None, true),
            Some((_, run)) => match run.prestarted_claim(at, reservation) {
                Some(claim) => (Some(claim), false),
                None => (None, run.holds_claim()),
            },
            None => (None, false),
        };
        if kill {
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
    /// May the pre-started window still be rendered by this run? Not after
    /// the run's end, a failed spawn, or a helper that failed before it
    /// finished that window (the next window not begun).
    fn prestart_viable(&self, prestart: &PreStart) -> bool {
        !self.cancel.is_cancelled()
            && !prestart.failed()
            && match &*self.helper.exit.borrow() {
                Some(Err(_)) => self.outputs.front().is_some_and(|next| begun(&next.held)),
                _ => true,
            }
    }
    /// Does a pre-start of this run still hold the family's worker claim?
    fn holds_claim(&self) -> bool {
        self.prestart
            .as_ref()
            .is_some_and(|prestart| prestart.claim.strong_count() > 0)
    }
    fn prestarted_claim(
        &self,
        at: u32,
        reservation: &crate::vodencode::EncodePermit,
    ) -> Option<Arc<crate::vodencode::EncodeWorkerPermit>> {
        let prestart = self.prestart.as_ref()?;
        if prestart.at != at || !self.prestart_viable(prestart) {
            return None;
        }
        prestart
            .claim
            .upgrade()
            .filter(|claim| claim.reservation().same_reservation(reservation))
    }
    /// What killing this run must wait for.
    pub(crate) fn settlement(&self) -> RunSettlement {
        RunSettlement {
            helper: self.helper.settled.clone(),
            prestart: self
                .prestart
                .as_ref()
                .map(|prestart| prestart.settled.clone()),
        }
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
        self.start_prestart(request, claim);
    }
    /// [`Self::prestart`] once its claim and checks are settled.
    fn start_prestart(
        &mut self,
        request: PreStartRequest,
        claim: crate::vodencode::EncodeWorkerPermit,
    ) {
        let Some(RunOutput {
            bounds,
            directory,
            held,
            fifo,
        }) = self.outputs.pop_front()
        else {
            return;
        };
        let claim = Arc::new(claim);
        let cancel = self.cancel.child_token();
        let at = request.at;
        let encoder_args = request.encoder_args.clone();
        let (spawned, take, settled) = spawn_prestart(
            request,
            fifo,
            self.input.url.clone(),
            self.helper.exit(),
            cancel.clone(),
            Arc::clone(&claim),
            self.input.physical_owner(),
            directory,
            held,
            LiveReader::new(&self.helper.prestarts),
        );
        self.prestart = Some(PreStart {
            at,
            encoder_args,
            bounds,
            claim: Arc::downgrade(&claim),
            spawned,
            take: Some(take),
            cancel,
            settled,
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
                !self.prestart_viable(prestart),
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
            let spawned = tokio::select! {
                spawned = prestart.spawned() => spawned,
                _ = cancel.cancelled() => return Err("DV window run window cancelled".into()),
                () = tokio::time::sleep_until(deadline) => {
                    return Err("DV window run window deadline expired".into());
                }
            };
            return Ok(spawned
                && self
                    .prestart
                    .as_ref()
                    .is_some_and(|prestart| self.prestart_viable(prestart)));
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
    pub(crate) async fn kill(self) {
        let settlement = self.settlement();
        drop(self);
        settlement.wait().await;
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
        /// A pre-start this window does not attach is killed and reaped, and
        /// its claim released, before the window reports its failure.
        async fn abandon(prestart: PreStart, cause: String) -> Result<SegmentProducer, String> {
            let mut settled = prestart.settled.clone();
            drop(prestart);
            let _ = settled.wait_for(|settled| *settled).await;
            Err(cause)
        }
        if (window.start, window.end) != prestart.bounds
            || stream.request.encoder_args != prestart.encoder_args
        {
            return abandon(
                prestart,
                "the requested window is not the run's pre-started window".into(),
            )
            .await;
        }
        let index = self.windows - 1 - u8::try_from(self.outputs.len()).unwrap_or(0);
        stream.stages.run_window(index, self.windows);
        let stages = Arc::clone(&stream.stages);
        let request = stream.request;
        let cancel = request.cancel.child_token();
        let reservation = match request.producer.try_reserve_empty() {
            Ok(reservation) => reservation,
            Err(error) => return abandon(prestart, error.to_string()).await,
        };
        let Some(Handover {
            child,
            child_job,
            spawned,
            claim,
            source_input,
            directory,
            held,
            output,
        }) = prestart.take().await
        else {
            return abandon(
                prestart,
                "DV pre-started window encoder did not start".into(),
            )
            .await;
        };
        if let Err(cause) = check_source(&request.source) {
            kill_and_reap(child, child_job).await;
            return Err(cause);
        }
        // The encoder started before this window was asked for.
        stages.mark_at(WindowStage::EncoderSpawned, spawned);
        let (registration, writers) = reservation.attach_registered_job_owned(
            child,
            child_job,
            request.at,
            Box::new(WindowResources {
                _source_input: source_input,
                _admission: request.admission,
                _run_claim: Some(claim),
                _directory: Arc::clone(&directory),
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
            evidence: Some(SegmentEvidence { directory, held }),
            run_helper: Some(self.registration.clone()),
            drained: Some(output),
        })
    }
}

impl Drop for WindowRun {
    fn drop(&mut self) {
        // The pre-start (a child token) kills and reaps its own encoder.
        self.cancel.cancel();
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

    fn prestart_request(cache: &std::path::Path, at: u32) -> PreStartRequest {
        PreStartRequest {
            at,
            // A stand-in encoder that copies its NUT input to its output.
            encoder_args: vec!["/proc/self/fd/3".into()],
            encoder: "/bin/cat".into(),
            runtime_cache: cache.to_owned(),
            mode: plurx_core::transcode::dv_processing::DvProcessingMode::BaseRpu,
            decode: RendererDecode::Software,
            wait: Duration::from_secs(5),
        }
    }

    /// A run over windows 0-2 whose helper the test plays: its exit is the
    /// test's, it is already settled (so killing the run waits only for its
    /// pre-start), its source reader is a loopback fixture and its
    /// registration a real sleeping child's.
    struct Fixture {
        cache: tempfile::TempDir,
        family: crate::vodencode::EncodePermit,
        run: Option<WindowRun>,
        /// `--window-run 3 END1 DIR1 NUT1 END2 DIR2 NUT2`.
        tail: Vec<String>,
        exit: watch::Sender<Option<Result<(), String>>>,
        _settled: watch::Sender<bool>,
        slot: ProducerSlot,
        registration: ProducerRegistration,
        _source: std::fs::File,
    }

    impl Fixture {
        async fn finish(self) {
            let _ = self
                .slot
                .request_registered_retirement(&self.registration)
                .await;
            let _ = self.registration.wait_confirmed_reap().await;
        }
    }

    async fn fixture() -> Fixture {
        let cache = tempfile::tempdir().expect("cache");
        let family = reservation();
        let (created, tail) = outputs(
            cache.path(),
            &window(0, 48048),
            &[window(48048, 96096), window(96096, 144144)],
        )
        .expect("outputs");
        let (exit, exit_receiver) = watch::channel(None);
        let (settled, settled_receiver) = watch::channel(true);
        let (idle, _) = watch::channel(None);
        let helper = RunHelper {
            exit: exit_receiver,
            settled: settled_receiver,
            idle,
            prestarts: Arc::new(watch::channel(0_usize).0),
        };
        let (input, source) =
            crate::source_input::SourceInput::qualification_fixture(b"source").await;
        let sleeper = producer_spawn::spawn(
            std::path::Path::new("/bin/sleep"),
            &["30".to_owned()],
            SpawnOptions {
                runtime_cache: cache.path(),
                progress: Progress::None,
                descriptors: Descriptors::default(),
                env: &[],
                work: crate::process_control::ChildWork::realtime("DV run fixture helper"),
            },
        )
        .expect("helper stand-in");
        let slot = ProducerSlot::new();
        let (registration, writers) = slot
            .attach_registered_job_owned(sleeper.child, sleeper.child_job, 0, None)
            .await;
        writers.settled();
        let run = WindowRun::new(
            RunLaunch {
                later: Vec::new(),
                reservation: family.clone(),
                deadline: Instant::now() + Duration::from_secs(60),
                key: Vec::new(),
            },
            48,
            created,
            helper,
            CancellationToken::new(),
            SourceInputLease::new(input),
            fence(),
            registration.clone(),
        );
        Fixture {
            cache,
            family,
            run: Some(run),
            tail,
            exit,
            _settled: settled,
            slot,
            registration,
            _source: source,
        }
    }

    fn helper_writer(fixture: &Fixture) -> std::fs::File {
        use std::io::Write;
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&fixture.tail[4])
            .expect("helper writer");
        writer.write_all(b"first picture").expect("write");
        writer
    }

    /// The generation asking for window 1 shares the claim of the encoder
    /// pre-started for it; a run without a pre-start holds no claim and is
    /// left alone; a run whose pre-start holds the claim for another window
    /// is killed and settled, releasing it.
    #[tokio::test]
    async fn claim_for_shares_only_a_prestart_claim_for_its_own_window() {
        let mut f = fixture().await;
        let slot = WindowRunSlot::default();
        let unreaped = f
            .family
            .clone()
            .try_claim_worker()
            .expect("the previous window's encoder, not yet reaped");
        *slot.run.lock().await = Some((slot.ends(), f.run.take().expect("run")));
        assert!(slot.claim_for(1, &f.family).await.is_none());
        assert!(
            slot.run.lock().await.is_some(),
            "a run without a pre-start is not killed for another holder's claim"
        );
        drop(unreaped);
        {
            let mut held = slot.run.lock().await;
            let (_, run) = held.as_mut().expect("run");
            let claim = f
                .family
                .clone()
                .try_claim_worker()
                .expect("the run's claim");
            run.start_prestart(prestart_request(f.cache.path(), 1), claim);
        }
        let shared = slot
            .claim_for(1, &f.family)
            .await
            .expect("shared with its own window");
        assert!(
            f.family.clone().try_claim_worker().is_none(),
            "never a second"
        );
        drop(shared);
        assert!(
            slot.run.lock().await.is_some(),
            "its window continues the run"
        );
        assert!(slot.claim_for(2, &f.family).await.is_none());
        assert!(slot.run.lock().await.is_none(), "killed for another window");
        assert!(
            f.family.clone().try_claim_worker().is_some(),
            "its pre-start was reaped and the claim released"
        );
        f.finish().await;
    }

    #[tokio::test]
    async fn a_prestarted_window_continues_only_while_its_helper_can_finish_it() {
        let mut f = fixture().await;
        let mut run = f.run.take().expect("run");
        let claim = f.family.clone().try_claim_worker().expect("claim");
        run.start_prestart(prestart_request(f.cache.path(), 1), claim);
        let args = vec!["/proc/self/fd/3".to_owned()];
        let shared = SegmentAdmission::RunShared(
            run.prestarted_claim(1, &f.family)
                .expect("pre-started claim"),
        );
        let next = window(48048, 96096);
        assert!(run.continues(&next, 48, &[], &shared, &args));
        assert!(
            !run.continues(&next, 48, &[], &shared, &["another".to_owned()]),
            "another encoder recipe is not this encoder"
        );
        let writer = helper_writer(&f);
        let cancel = CancellationToken::new();
        let soon = || Instant::now() + Duration::from_secs(5);
        assert_eq!(run.ready(&cancel, soon()).await, Ok(true));
        // The helper failed before finishing the window: not this run's.
        let _ = f
            .exit
            .send_replace(Some(Err("streaming renderer failed: exit 9".into())));
        assert!(!run.continues(&next, 48, &[], &shared, &args));
        assert_eq!(run.ready(&cancel, soon()).await, Ok(false));
        assert!(run.prestarted_claim(1, &f.family).is_none());
        // Unless it had begun the next window: this one is complete.
        std::fs::write(std::path::Path::new(&f.tail[6]).join("renderer.jsonl"), b"")
            .expect("next window begun");
        assert!(run.continues(&next, 48, &[], &shared, &args));
        drop(writer);
        drop(shared);
        run.kill().await;
        assert!(
            f.family.clone().try_claim_worker().is_some(),
            "killed, reaped and released"
        );
        f.finish().await;
    }

    /// A window that gives up on a pre-started encoder (its wait lost to its
    /// deadline) drops the handle: the task kills and reaps the encoder, and
    /// only then releases the claim, reader and directory.
    #[tokio::test]
    async fn a_dropped_prestart_reaps_its_encoder_before_releasing_the_claim() {
        let mut f = fixture().await;
        let mut run = f.run.take().expect("run");
        let claim = f.family.clone().try_claim_worker().expect("claim");
        run.start_prestart(prestart_request(f.cache.path(), 1), claim);
        let writer = helper_writer(&f);
        let mut prestart = run.prestart.take().expect("pre-start");
        assert!(prestart.spawned().await, "the encoder runs");
        let directory = std::path::PathBuf::from(&f.tail[3]);
        let mut settled = prestart.settled.clone();
        drop(prestart);
        tokio::time::timeout(Duration::from_secs(5), settled.wait_for(|settled| *settled))
            .await
            .expect("reaped while its writer is still open")
            .expect("settlement reported");
        assert!(!directory.exists(), "its private directory is removed");
        assert!(f.family.clone().try_claim_worker().is_some());
        drop(writer);
        drop(run);
        f.finish().await;
    }

    #[tokio::test]
    async fn drained_output_over_its_bound_fails_at_once() {
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            drain_bounded_output(tokio::io::repeat(0), tokio::io::empty()),
        )
        .await
        .expect("fails when the bound is passed, not at a deadline");
        assert_eq!(
            output,
            Err("DV encoder private-output bound exceeded".to_owned())
        );
        assert_eq!(
            drain_bounded_output(tokio::io::empty(), tokio::io::repeat(1)).await,
            Err("DV encoder diagnostic bound exceeded".to_owned())
        );
    }

    /// A failed window, Stop or refused episode ends the run synchronously.
    /// The retried generation whose claim fails asks the slot, which waits for
    /// the pre-start's reap; the claim then succeeds, never a launch failure.
    #[tokio::test]
    async fn ending_a_run_settles_its_prestart_before_the_next_claim() {
        let mut f = fixture().await;
        let slot = WindowRunSlot::default();
        let mut run = f.run.take().expect("run");
        let claim = f.family.clone().try_claim_worker().expect("claim");
        run.start_prestart(prestart_request(f.cache.path(), 1), claim);
        let writer = helper_writer(&f);
        *slot.run.lock().await = Some((slot.ends(), run));
        slot.end();
        assert!(slot.run.lock().await.is_none());
        let claimed = match f.family.clone().try_claim_worker() {
            Some(claim) => claim,
            None => {
                assert!(slot.claim_for(1, &f.family).await.is_none());
                f.family
                    .clone()
                    .try_claim_worker()
                    .expect("released once the pre-start was reaped")
            }
        };
        drop(claimed);
        drop(writer);
        f.finish().await;
    }
}
