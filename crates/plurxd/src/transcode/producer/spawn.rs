use super::*;

/// Apply one `key=value` line of ffmpeg's `-progress` output.
///
/// Values can be the literal `N/A` (before the first frame lands, and for
/// `speed` on a copy that has not been running long enough to estimate),
/// which parses to nothing rather than to zero — zero would read as "stalled"
/// and zero would read as "not moving" respectively, both wrong.
pub fn apply_progress_line(progress: &Progress, generation: u64, line: &str) -> bool {
    // A line from a superseded attempt is not evidence about the running one.
    if progress.generation() != generation {
        return false;
    }
    let Some((key, value)) = line.split_once('=') else {
        return false;
    };
    let value = value.trim();
    if value.is_empty() || value == "N/A" {
        return false;
    }
    match key.trim() {
        // Both are microseconds despite the `_ms` name — an ffmpeg quirk, not
        // a typo here. `out_time_us` is the newer spelling; accept either so
        // this keeps working across builds.
        "out_time_us" | "out_time_ms" => {
            if let Ok(us) = value.parse::<i64>() {
                progress.note_out_time(us / 1000);
                return true;
            }
        }
        "speed" => {
            if let Ok(x) = value.trim_end_matches('x').parse::<f64>() {
                progress.speed_milli.store((x * 1000.0) as i64, Relaxed);
                return true;
            }
        }
        _ => {}
    }
    false
}

fn apply_and_observe_progress_line(
    progress: &Progress,
    control: Option<&crate::playback_control::RollingControlHandle>,
    generation: u64,
    line: &str,
) {
    if !apply_progress_line(progress, generation, line) {
        return;
    }
    if let Some(control) = control {
        control.observe_producer_progress(
            generation,
            progress.out_time_ms(),
            progress.speed_milli(),
            progress.recent_speed_milli(),
        );
    }
}

/// Spawn an ffmpeg HLS transcode, draining its stderr (at `-loglevel error`)
/// into the logs so a failure is visible instead of a silently dead session,
/// and its stdout — which carries `-progress` telemetry — into `progress`.
pub(super) type FfmpegDescriptors = crate::producer_spawn::Descriptors;

#[cfg(windows)]
pub(super) fn windows_session_descriptors(session: &Session) -> Result<FfmpegDescriptors, String> {
    let source = session
        .source_handle
        .as_ref()
        .ok_or_else(|| "Windows session has no held media source".to_owned())?;
    let output = session
        .output_handle
        .as_ref()
        .ok_or_else(|| "Windows session has no held output directory".to_owned())?;
    let mut descriptors = FfmpegDescriptors::default()
        .with_file("media source", source)?
        .with_directory("session output directory", output)?;
    if let Some(subtitle) = session.subtitle_handle.as_ref() {
        descriptors = descriptors.with_file("subtitle source", subtitle)?;
    }
    Ok(descriptors)
}

#[cfg(windows)]
pub(super) fn windows_offline_descriptors(
    source: Option<&BoundPretranscodeSource>,
    output: &plurx_core::fs_secure::SecureDirectory,
    subtitle: Option<&std::fs::File>,
) -> Result<FfmpegDescriptors, String> {
    let mut descriptors =
        FfmpegDescriptors::default().with_directory("offline output directory", output)?;
    if let Some(source) = source {
        descriptors = descriptors.with_file("media source", &source.handle)?;
    }
    if let Some(subtitle) = subtitle {
        descriptors = descriptors.with_file("subtitle source", subtitle)?;
    }
    Ok(descriptors)
}

#[derive(Clone)]
pub(super) struct FfmpegProgressObserver {
    progress: Arc<Progress>,
    generation: u64,
    control: Option<crate::playback_control::RollingControlHandle>,
}

impl FfmpegProgressObserver {
    pub(super) fn offline(progress: Arc<Progress>, generation: u64) -> Self {
        Self {
            progress,
            generation,
            control: None,
        }
    }

    pub(super) fn rolling(
        progress: Arc<Progress>,
        generation: u64,
        control: crate::playback_control::RollingControlHandle,
    ) -> Self {
        Self {
            progress,
            generation,
            control: Some(control),
        }
    }

    fn apply_line(&self, line: &str) {
        apply_and_observe_progress_line(
            &self.progress,
            self.control.as_ref(),
            self.generation,
            line,
        );
    }
}

/// What one attempt asks its child's stderr to be read as.
///
/// `grammar` is `Some` only when a retained contract covers the exact binary
/// about to run *and* this attempt asked for the log flags that contract was
/// qualified under. Everything else reads the stream, bounds it, and logs it
/// without claiming any of it is evidence.
#[derive(Clone, Default)]
pub(super) struct DiagnosticObservation {
    pub(super) plan_digest: String,
    pub(super) contract_id: Option<String>,
    pub(super) grammar: Option<crate::decoder_health::DiagnosticGrammar>,
}

/// Where a latched fault goes.
///
/// Holds the actor handle, the attempt it belongs to, and the identity §7.4's
/// `ProducerDecodeFault` carries. Built once at spawn so the reader task owns
/// everything it needs and borrows nothing.
#[derive(Clone)]
pub(super) struct DecodeFaultSink {
    control: crate::playback_control::RollingControlHandle,
    producer_attempt: u64,
    plan_digest: String,
    input_video_stream: u32,
    diagnostic_contract: Option<String>,
}

impl DecodeFaultSink {
    pub(super) fn report(
        &self,
        fault: crate::decoder_health::DecodeFaultKind,
        records: u64,
        action_qualified: bool,
    ) {
        self.control.observe_producer_decode_fault(
            self.producer_attempt,
            self.plan_digest.clone(),
            fault,
            self.input_video_stream,
            records,
            self.diagnostic_contract.clone(),
            action_qualified,
        );
    }
}

impl DiagnosticObservation {
    /// The sink this attempt's reader reports a latch to, when there is both a
    /// grammar that can latch one and an actor to receive it.
    pub(super) fn fault_sink(&self, observer: &FfmpegProgressObserver) -> Option<DecodeFaultSink> {
        let grammar = self.grammar.as_ref()?;
        let control = observer.control.clone()?;
        Some(DecodeFaultSink {
            control,
            producer_attempt: observer.generation,
            plan_digest: self.plan_digest.clone(),
            input_video_stream: grammar.selected_stream(),
            diagnostic_contract: self.contract_id.clone(),
        })
    }

    /// The observation this plan's attempt is entitled to.
    ///
    /// A grammar only when the installed policy has a contract covering the
    /// exact binary about to run, for this codec and the decoder measured on
    /// the plan's exact backend. Everything else is read, bounded and logged
    /// without being evidence — which is the honest state on a fleet whose
    /// FFmpeg prints its decode failures without naming a stream.
    pub(super) fn for_plan(
        plan: &ResolvedTranscode,
        measured: &plurx_core::transcode::decoder_inventory::MeasuredDecoders,
        automatic_recovery_enabled: bool,
    ) -> Self {
        Self::for_plan_against(
            crate::decoder_health::diagnostic_policy(),
            plan,
            measured,
            automatic_recovery_enabled,
        )
    }

    pub(super) fn for_plan_against(
        policy: &crate::decoder_health::DiagnosticPolicy,
        plan: &ResolvedTranscode,
        measured: &plurx_core::transcode::decoder_inventory::MeasuredDecoders,
        automatic_recovery_enabled: bool,
    ) -> Self {
        // The inventory is measured on every boot so the settings surface can
        // advise before an operator opts in. Reading it must not make an
        // unqualified plan start asking for diagnostic flags: that would
        // change the shipping command merely because the daemon upgraded.
        let named_decoder = if plan.enforces_receipt() || automatic_recovery_enabled {
            plan.decode()
                .input_codec()
                .and_then(|codec| measured.implementation(codec, plan.decode().backend()))
        } else {
            None
        };
        Self::resolve(
            policy,
            plan.plan_digest(),
            plan.decode().input_codec(),
            plan.decode().backend(),
            named_decoder,
            plan.decode().input_video_stream(),
            automatic_recovery_enabled,
        )
    }

    /// The same decision from the plan facts it actually uses, against an
    /// explicit policy — so the rule can be tested without a process-wide
    /// policy and without assembling a plan.
    pub(super) fn resolve(
        policy: &crate::decoder_health::DiagnosticPolicy,
        plan_digest: String,
        codec: Option<&str>,
        backend: plurx_core::transcode::DecodeBackend,
        named_decoder: Option<&str>,
        input_video_stream: u32,
        automatic_recovery_enabled: bool,
    ) -> Self {
        let unqualified = Self {
            plan_digest: plan_digest.clone(),
            ..Self::default()
        };
        let Some(codec) = codec else {
            return unqualified;
        };
        // Only a measured decoder may be matched to a retained contract.
        //
        // Guessing the family name here was a real defect: a hardware backend
        // substitutes `<codec>_qsv` and prints that in `[dec:…]`, and a
        // software plan with no measured implementation emits no `-c:v` at all
        // so FFmpeg picks its own default — `av1` selects the native decoder
        // where it would otherwise choose `libdav1d`. A contract found under
        // the guessed name yields a grammar that matches nothing, and a
        // grammar that matches nothing certifies every stream as clean. That
        // is the exact substitution this milestone exists to remove, so the
        // answer is no unless the command says the decoder out loud.
        //
        // The backend used to be refused here outright, and that refusal was
        // right about the danger and wrong about the remedy: it made the
        // recovery this effort exists for unreachable, because only a
        // software-decode plan could ever latch a qualified fault and only a
        // hardware-decode plan has a software alternate to be given. The
        // refusal now lives where the danger actually is — the backend is part
        // of the contract's coverage key, so a contract qualified on software
        // `h264` cannot answer for a VideoToolbox `h264`, which prints the
        // identical `[dec:h264 @ …]` context. Naming the backend is what makes
        // the two distinguishable; refusing one of them made them moot.
        if let Some(contract) = named_decoder.and_then(|decoder| {
            policy.contract_for(
                codec,
                decoder,
                backend.name(),
                crate::decoder_health::QUALIFIED_STDERR_MODE,
            )
        }) {
            return Self {
                plan_digest,
                contract_id: Some(contract.id.clone()),
                // Input file 0: every movie command builds the video input first,
                // and a subtitle input that follows it is a later index.
                grammar: Some(
                    crate::decoder_health::DiagnosticGrammar::new(
                        contract.clone(),
                        0,
                        input_video_stream,
                    )
                    .with_automatic_action(automatic_recovery_enabled),
                ),
            };
        }
        if automatic_recovery_enabled {
            return Self {
                plan_digest,
                contract_id: None,
                grammar: Some(crate::decoder_health::DiagnosticGrammar::advisory(
                    codec,
                    0,
                    input_video_stream,
                )),
            };
        }
        unqualified
    }

    /// Whether this attempt may ask its child for the qualified log flags.
    ///
    /// Asking without a grammar buys nothing and changes the arguments that
    /// ship, so the answer is the same question as "is there a grammar".
    pub(super) fn qualified_logging(&self) -> bool {
        self.grammar.is_some()
    }

    /// An attempt with no semantic plan behind it — a stream copy. It is still
    /// read and still bounded; there is simply no decode to attribute.
    ///
    /// It is named by the session it belongs to rather than by a plan digest,
    /// because an empty digest names nothing and is indistinguishable across
    /// attempts — and a receipt that names nothing is evidence about nothing.
    pub(super) fn copy(session_id: &str) -> Self {
        Self {
            plan_digest: format!("copy:{}", session_log_id(session_id)),
            ..Self::default()
        }
    }
}

/// A running child and the observation that belongs to it.
///
/// §7.1: "The session/producer remains the owner of that handle." The two
/// travel together from the spawn to the install so that a producer cannot
/// exist without the reader that decides whether its output is trustworthy.
pub(super) struct ObservedFfmpeg {
    child: Child,
    child_job: crate::process_control::ChildJob,
    diagnostics: crate::decoder_health::ObservedDiagnostics,
}

impl ObservedFfmpeg {
    pub(super) fn into_parts(
        self,
    ) -> (
        Child,
        crate::process_control::ChildJob,
        crate::decoder_health::ObservedDiagnostics,
    ) {
        (self.child, self.child_job, self.diagnostics)
    }
}

#[allow(clippy::too_many_arguments)] // the priority class joined seven existing inputs
pub(super) fn spawn_ffmpeg(
    args: &[String],
    work: crate::process_control::ChildWork,
    encoder_label: &'static str,
    session_id: &str,
    progress_observer: FfmpegProgressObserver,
    runtime_cache: &std::path::Path,
    descriptors: FfmpegDescriptors,
    observation: DiagnosticObservation,
) -> Result<ObservedFfmpeg, String> {
    let crate::producer_spawn::Spawned {
        child,
        child_job,
        stdout,
        stderr,
    } = crate::producer_spawn::spawn(
        std::path::Path::new(&ffmpeg_bin()),
        args,
        crate::producer_spawn::SpawnOptions {
            runtime_cache,
            progress: crate::producer_spawn::Progress::Stdout,
            descriptors,
            // The muxer's uploads go to a loopback endpoint. An inherited
            // `http_proxy` would send them, token and all, to the proxy.
            env: &[("http_proxy", std::ffi::OsStr::new(""))],
            work,
        },
    )?;
    // Built before the progress observer is moved into its own task: the sink
    // needs the actor handle and the attempt, and both live on the observer.
    let fault_sink = observation.fault_sink(&progress_observer);
    // Both readers are owned. They used to be detached `tokio::spawn`s, which
    // is why a reader that died looked exactly like a stream that was clean —
    // and a stream that looked clean is what let a broken title into the
    // cache. §7.1: detached best-effort logging is insufficient for cache
    // qualification.
    let progress = Some(tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            progress_observer.apply_line(&line);
        }
    }));
    // The fault reaches the actor when it latches, not when the stream ends: a
    // producer that stops decoding and keeps running holds its stderr open for
    // the rest of the film, and a fault delivered then arrives after every
    // success fact it was supposed to precede.
    let reader = Some({
        let sid = session_id.to_owned();
        let started = Instant::now();
        let grammar = observation.grammar.clone();
        tokio::spawn(async move {
            let accumulator = crate::decoder_health::read_diagnostics_reporting(
                stderr,
                grammar,
                |line| log_ffmpeg_stderr(&sid, encoder_label, line),
                |_| false,
                |fault, records, action_qualified| {
                    if let Some(sink) = fault_sink.as_ref() {
                        sink.report(fault, records, action_qualified);
                    }
                },
            )
            .await;
            // Stderr closing means the process ended. Logging it (with how long
            // it ran) distinguishes "ffmpeg died early" from "ffmpeg is still
            // running but produced nothing".
            tracing::warn!(
                target: "plurxd::transcode",
                session = %session_log_id(&sid), encoder = encoder_label,
                elapsed_s = started.elapsed().as_secs(),
                "transcode ffmpeg process ended"
            );
            accumulator
        })
    });
    Ok(ObservedFfmpeg {
        child,
        child_job,
        diagnostics: crate::decoder_health::ObservedDiagnostics::new(
            observation.plan_digest,
            observation.contract_id,
            reader,
            progress,
        ),
    })
}

/// Spawn a copy-session ffmpeg whose **stdout is the media**, not telemetry.
///
/// The one structural difference from [`spawn_ffmpeg`]: with the fragmented
/// stream on stdout, `-progress` has to go somewhere else, so it goes to
/// stderr alongside the log. Progress blocks are `key=value` on a line of
/// their own and ffmpeg's own messages are not, so the drain sorts them and
/// feeds the actor progress ingress plus the activity page. Losing that would
/// leave a segmenter session with no `speed`, no `out_time`, and no advancing
/// progress evidence.
pub(super) fn spawn_ffmpeg_pipe(
    args: &[String],
    work: crate::process_control::ChildWork,
    session_id: &str,
    progress_observer: FfmpegProgressObserver,
    runtime_cache: &std::path::Path,
    descriptors: FfmpegDescriptors,
    observation: DiagnosticObservation,
) -> Result<(ObservedFfmpeg, tokio::process::ChildStdout), String> {
    let crate::producer_spawn::Spawned {
        child,
        child_job,
        stdout,
        stderr,
    } = crate::producer_spawn::spawn(
        std::path::Path::new(&ffmpeg_bin()),
        args,
        crate::producer_spawn::SpawnOptions {
            runtime_cache,
            progress: crate::producer_spawn::Progress::Stderr,
            descriptors,
            env: &[("http_proxy", std::ffi::OsStr::new(""))],
            work,
        },
    )?;
    let reader = Some({
        let sid = session_id.to_owned();
        let started = Instant::now();
        let grammar = observation.grammar.clone();
        tokio::spawn(async move {
            // Progress blocks share this stream with the log, so they are
            // sorted out before classification: a `key=value` line can never
            // become a decode record, and the log is not drowned in telemetry.
            let accumulator = crate::decoder_health::read_diagnostics(
                stderr,
                grammar,
                |line| log_ffmpeg_stderr(&sid, "copy", line),
                |line| {
                    if plurx_core::transcode::progress::is_progress_line(line) {
                        progress_observer.apply_line(line);
                        true
                    } else {
                        false
                    }
                },
            )
            .await;
            tracing::warn!(
                target: "plurxd::transcode",
                session = %session_log_id(&sid), encoder = "copy",
                elapsed_s = started.elapsed().as_secs(),
                "transcode ffmpeg process ended"
            );
            accumulator
        })
    });
    Ok((
        ObservedFfmpeg {
            child,
            child_job,
            diagnostics: crate::decoder_health::ObservedDiagnostics::new(
                observation.plan_digest,
                observation.contract_id,
                reader,
                None,
            ),
        },
        stdout,
    ))
}
