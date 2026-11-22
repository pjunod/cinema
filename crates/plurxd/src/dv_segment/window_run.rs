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
    ) -> bool {
        let same_admission = match admission {
            SegmentAdmission::Retained(worker) => {
                worker.reservation().same_reservation(&self.reservation)
            }
            SegmentAdmission::Owned(_) => false,
        };
        run_continues(
            self.outputs.front().map(|output| output.bounds),
            self.ended(),
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
        self.cancel.cancel();
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
        })
    }
}

impl Drop for WindowRun {
    fn drop(&mut self) {
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
}
