use super::*;

/// Immutable publication identity for the cache location behind a session.
///
/// Integrity failures remove the row with all five fields as a compare-and-
/// delete. A request that started against generation A must never erase a
/// replacement generation B that another producer published meanwhile.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct CachedLocationIdentity {
    pub(super) recipe_hash: String,
    pub(super) node_id: String,
    pub(super) storage_class: String,
    pub(super) generation_id: Option<String>,
    pub(super) relative_dir: String,
    pub(super) manifest_digest: Option<String>,
}

pub(super) enum CacheOfferVerdict {
    Pending {
        identity: CachedLocationIdentity,
        started_at: Instant,
    },
    Ready {
        identity: CachedLocationIdentity,
        verified: bool,
        observed_at: Instant,
    },
}

pub(super) struct CacheOfferVerification {
    pub(super) verified: bool,
    pub(super) revoke_shared_member: bool,
}

/// Immutable master-playlist inputs for one rolling generation. A mutable
/// library row or a later probe refresh must never change the advertised
/// presentation behind an already-issued capability.
#[derive(Clone)]
pub(super) struct FrozenHlsPresentation {
    pub(super) file: plurx_core::domain::MediaFile,
    pub(super) context: HlsContext,
    pub(super) contract_fingerprint: String,
    /// Present when the frozen source/session facts completely determine the
    /// master. AVC and other non-init-derived codec families are stable
    /// generation metadata; HEVC/Dolby Vision masters remain attempt media
    /// because their exact tier/profile may be normalized from `init.mp4`.
    pub(super) sealed_stable_master_contract: Option<String>,
}

impl FrozenHlsPresentation {
    pub(super) fn new(
        mut file: plurx_core::domain::MediaFile,
        context: HlsContext,
        kind: &SessionKind,
    ) -> Self {
        if let SessionKind::Transcode { height } = kind {
            let geometry = plurx_core::transcode::output_size(&file, *height);
            file.width = geometry.map(|(width, _)| width);
            file.height = geometry.map(|(_, height)| height);
        }
        let identity = serde_json::json!({
            "version": 2,
            "file": &file,
            "kind": kind,
            "start_seconds": context.start_seconds,
            "media_origin_seconds": context.media_origin_seconds,
            "codecs": &context.codecs,
            "supplemental_codecs": &context.supplemental_codecs,
            "frame_rate": context.frame_rate,
        });
        let contract_fingerprint = hex::encode(Sha256::digest(identity.to_string().as_bytes()));
        let master_requires_attempt_init = context.codecs.split(',').next().is_some_and(|video| {
            let video = video.trim();
            ["hvc1", "hev1", "dvh1", "dvhe"]
                .into_iter()
                .any(|entry| video.starts_with(entry))
                || (matches!(kind, SessionKind::Copy { .. }) && video.starts_with("avc1"))
        });
        let sealed_stable_master_contract =
            (!master_requires_attempt_init).then(|| contract_fingerprint.clone());
        Self {
            file,
            context,
            contract_fingerprint,
            sealed_stable_master_contract,
        }
    }
}

/// Shape the source row into the bytes an encoded-VOD recipe emits.
///
/// Width and height come from the same `output_size` decision. Carrying one
/// resolved coordinate with the requested height would advertise an aspect
/// ratio the encoder never produced, and an unprobed source must omit both.
pub(super) fn encoded_vod_presentation_file(
    mut file: plurx_core::domain::MediaFile,
    target_height: i64,
    grade: OutputGrade,
) -> plurx_core::domain::MediaFile {
    let geometry = plurx_core::transcode::output_size(&file, target_height);
    file.width = geometry.map(|(width, _)| width);
    file.height = geometry.map(|(_, height)| height);
    file.hdr = (grade == OutputGrade::Hdr10).then(|| "hdr10".to_owned());
    file.hdr_format = (grade == OutputGrade::Hdr10).then(|| "HDR10".to_owned());
    file.dolby_vision = Default::default();
    file.bit_depth = Some(if grade == OutputGrade::Hdr10 { 10 } else { 8 });
    file.video_codec = Some(
        if grade == OutputGrade::Hdr10 {
            "hevc"
        } else {
            "h264"
        }
        .to_owned(),
    );
    file
}

pub(super) fn frozen_video_frame_rate(probe_json: Option<&str>) -> Option<f64> {
    fn fraction(raw: &str) -> Option<f64> {
        let (numerator, denominator) = raw.split_once('/')?;
        let numerator = numerator.parse::<f64>().ok()?;
        let denominator = denominator.parse::<f64>().ok()?;
        (numerator.is_finite() && denominator.is_finite() && denominator > 0.0)
            .then_some(numerator / denominator)
            .filter(|value| value.is_finite() && *value > 0.0)
    }

    let probe: serde_json::Value = serde_json::from_str(probe_json?).ok()?;
    probe.get("streams")?.as_array()?.iter().find_map(|stream| {
        (stream.get("codec_type")?.as_str()? == "video")
            .then(|| {
                stream
                    .get("avg_frame_rate")
                    .and_then(serde_json::Value::as_str)
                    .and_then(fraction)
                    .or_else(|| {
                        stream
                            .get("r_frame_rate")
                            .and_then(serde_json::Value::as_str)
                            .and_then(fraction)
                    })
            })
            .flatten()
    })
}

/// A process handle paired permanently with the actor attempt that installed
/// it. The attempt cannot be reconstructed from current session state: actor
/// admission advances before a replacement kills its predecessor, and that
/// exact overlap is where an untagged exit gets blamed on the successor.
#[cfg(test)]
pub(super) struct LifecycleTestPause {
    pub(super) reached: tokio::sync::Notify,
    pub(super) release: tokio::sync::Notify,
}

#[cfg(test)]
impl LifecycleTestPause {
    pub(super) fn new() -> Self {
        Self {
            reached: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        }
    }
}

pub(super) struct AttemptChild {
    pub(super) producer_attempt: u64,
    pid: Option<u32>,
    /// The reader that decides whether this attempt's output can be trusted.
    ///
    /// It lives with the child rather than with the session because it is
    /// scoped to one attempt: a successor gets its own counters, and a
    /// predecessor's fault must not follow the attempt that replaced it.
    terminal: Arc<std::sync::Mutex<Option<AttemptChildTerminal>>>,
    terminal_notify: Arc<tokio::sync::Notify>,
    commands: tokio::sync::mpsc::UnboundedSender<AttemptChildCommand>,
    #[cfg(test)]
    signal_after_authorization_pause: Arc<std::sync::Mutex<Option<Arc<std::sync::Barrier>>>>,
    #[cfg(test)]
    signal_after_flow_reservation_pause: Arc<std::sync::Mutex<Option<Arc<std::sync::Barrier>>>>,
    /// Test-only seam after exact-attempt termination has been requested but
    /// before the supervisor can observe and publish the terminal wait.
    #[cfg(test)]
    pub(super) terminate_before_reap_pause: Arc<std::sync::Mutex<Option<Arc<LifecycleTestPause>>>>,
}

#[derive(Clone)]
enum AttemptChildTerminal {
    Exited(std::process::ExitStatus, Instant),
    WaitFailed(std::io::ErrorKind, String),
}

enum AttemptChildCommand {
    Signal {
        signal: crate::process_control::ProcessSignal,
        reply: tokio::sync::oneshot::Sender<std::io::Result<bool>>,
    },
    Terminate {
        reply: Option<tokio::sync::oneshot::Sender<std::io::Result<()>>>,
    },
}

impl AttemptChildTerminal {
    fn wait_result(&self) -> std::io::Result<()> {
        match self {
            Self::Exited(_, _) => Ok(()),
            Self::WaitFailed(kind, message) => Err(std::io::Error::new(*kind, message.clone())),
        }
    }
}

impl AttemptChild {
    /// Construct the child and, with it, the reader that decides whether its
    /// output can be trusted.
    ///
    /// The reader is a constructor argument rather than something attached
    /// afterwards, because the supervisor is spawned inside this function and
    /// takes the reader when the process ends: a child that is already dead
    /// when construction returns could otherwise reach that take before the
    /// reader was stored, and settle nothing at all.
    #[cfg(test)]
    pub(super) fn new(
        producer_attempt: u64,
        mut child: Child,
        control: crate::playback_control::RollingControlHandle,
        diagnostics: Option<crate::decoder_health::ObservedDiagnostics>,
    ) -> Self {
        let child_job = match crate::process_control::ChildJob::attach(&child) {
            Ok(job) => Some(job),
            Err(error) => {
                let _ = child.start_kill();
                tracing::error!(%error, "child could not be assigned to its process job");
                None
            }
        };
        Self::new_with_job(producer_attempt, child, child_job, control, diagnostics)
    }

    pub(super) fn new_with_job(
        producer_attempt: u64,
        mut child: Child,
        child_job: Option<crate::process_control::ChildJob>,
        control: crate::playback_control::RollingControlHandle,
        diagnostics: Option<crate::decoder_health::ObservedDiagnostics>,
    ) -> Self {
        let pid = child.id();
        // Owned by the supervisor alone. Nothing else may take it: two owners
        // would race for one handle, and whoever won would decide whether the
        // attempt got a receipt at all.
        let supervisor_diagnostics = std::sync::Mutex::new(diagnostics);
        let supervisor_control = control.clone();
        let terminal = Arc::new(std::sync::Mutex::new(None));
        let terminal_notify = Arc::new(tokio::sync::Notify::new());
        let (commands, mut command_receiver) = tokio::sync::mpsc::unbounded_channel();
        let supervisor_terminal = Arc::clone(&terminal);
        let supervisor_terminal_notify = Arc::clone(&terminal_notify);
        #[cfg(test)]
        let signal_after_authorization_pause =
            Arc::new(std::sync::Mutex::new(None::<Arc<std::sync::Barrier>>));
        #[cfg(test)]
        let supervisor_signal_pause = Arc::clone(&signal_after_authorization_pause);
        #[cfg(test)]
        let signal_after_flow_reservation_pause =
            Arc::new(std::sync::Mutex::new(None::<Arc<std::sync::Barrier>>));
        #[cfg(test)]
        let supervisor_flow_reservation_pause = Arc::clone(&signal_after_flow_reservation_pause);
        #[cfg(test)]
        let terminate_before_reap_pause =
            Arc::new(std::sync::Mutex::new(None::<Arc<LifecycleTestPause>>));
        #[cfg(test)]
        let supervisor_terminate_pause = Arc::clone(&terminate_before_reap_pause);
        tokio::spawn(async move {
            let _child_job = child_job;
            let mut command_open = true;
            let mut terminate_replies = Vec::new();
            let terminal = loop {
                if !command_open {
                    break Self::record_wait_result(&control, producer_attempt, child.wait().await);
                }
                tokio::select! {
                    // `Child::wait` is cancel safe. Poll it first so a ready
                    // reap always wins over a simultaneous signal command;
                    // the cached pid therefore cannot have been reused when
                    // the command branch runs.
                    biased;
                    result = child.wait() => {
                        break Self::record_wait_result(&control, producer_attempt, result);
                    }
                    command = command_receiver.recv() => {
                        let Some(command) = command else {
                            command_open = false;
                            continue;
                        };
                        match command {
                            AttemptChildCommand::Signal { signal, reply } => {
                                let flow_signal = matches!(
                                    signal,
                                    crate::process_control::ProcessSignal::Suspend
                                        | crate::process_control::ProcessSignal::Resume
                                );
                                let result = loop {
                                    let deferred = {
                                        let transition = control.lock_producer_transition();
                                        if control.current_producer_attempt() != producer_attempt
                                            || !control
                                                .producer_transition_is_live(&transition)
                                        {
                                            break Ok(false);
                                        }
                                        #[cfg(test)]
                                        if let Some(pause) = supervisor_signal_pause
                                            .lock()
                                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                                            .take()
                                        {
                                            pause.wait();
                                            pause.wait();
                                        }
                                        if flow_signal
                                            && !control.reserve_producer_flow_applied(
                                                &transition,
                                                producer_attempt,
                                            )
                                        {
                                            // Attempt changes and retirement
                                            // use this same fence, so after
                                            // the liveness check `false`
                                            // means only bounded capacity.
                                            true
                                        } else {
                                            #[cfg(test)]
                                            if flow_signal {
                                                if let Some(pause) = supervisor_flow_reservation_pause
                                                    .lock()
                                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                                    .take()
                                                {
                                                    pause.wait();
                                                    pause.wait();
                                                }
                                            }
                                            // The syscall and successful
                                            // acknowledgement publication are
                                            // one exact-attempt transaction.
                                            // A failed syscall only releases
                                            // the reserved barrier slot.
                                            let result =
                                                signal_owned_pid(pid, producer_attempt, signal);
                                            if flow_signal {
                                                control.finish_producer_flow_applied(
                                                    &transition,
                                                    producer_attempt,
                                                    signal
                                                        == crate::process_control::ProcessSignal::Suspend,
                                                    matches!(&result, Ok(true)),
                                                );
                                            }
                                            break result;
                                        }
                                    };
                                    debug_assert!(deferred);
                                    // Never wait while holding the transition
                                    // fence: the actor needs that fence to
                                    // drain the barriers that make capacity.
                                    // After waking, re-authorize the attempt;
                                    // retirement or replacement may have won.
                                    if !control.wait_for_producer_flow_capacity().await {
                                        break Ok(false);
                                    }
                                };
                                let _ = reply.send(result);
                            }
                            AttemptChildCommand::Terminate { reply } => {
                                match signal_owned_pid(
                                    pid,
                                    producer_attempt,
                                    crate::process_control::ProcessSignal::Terminate,
                                ) {
                                    Ok(_) => {
                                        if let Some(reply) = reply {
                                            terminate_replies.push(reply);
                                        }
                                        #[cfg(test)]
                                        let pause = supervisor_terminate_pause
                                            .lock()
                                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                                            .take();
                                        #[cfg(test)]
                                        if let Some(pause) = pause {
                                            pause.reached.notify_one();
                                            pause.release.notified().await;
                                        }
                                    }
                                    Err(error) => {
                                        if let Some(reply) = reply {
                                            let _ = reply.send(Err(error));
                                        } else {
                                            tracing::error!(
                                                producer_attempt,
                                                %error,
                                                "dropped producer could not be terminated"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            };
            *supervisor_terminal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(terminal.clone());
            supervisor_terminal_notify.notify_waiters();
            supervisor_terminal_notify.notify_one();
            for reply in terminate_replies {
                let _ = reply.send(terminal.wait_result());
            }
            // The one place that knows how this attempt's process actually
            // ended, and the one place with no session lock held. §7.4 keeps
            // diagnostic completion separate from process exit: the receipt is
            // settled *after* the terminal is published, because a process can
            // exit long before its stderr reaches EOF and "the process
            // finished" is not "we saw everything it said".
            let diagnostics = supervisor_diagnostics
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(diagnostics) = diagnostics {
                let receipt = diagnostics
                    .settle(
                        crate::decoder_health::DIAGNOSTIC_DRAIN_BUDGET,
                        Self::attempt_exit_disposition(&terminal),
                    )
                    .await;
                report_producer_health(&format!("attempt {producer_attempt}"), &receipt);
                supervisor_control.observe_producer_diagnostics_complete(producer_attempt, receipt);
            }
        });
        Self {
            producer_attempt,
            pid,
            terminal,
            terminal_notify,
            commands,
            #[cfg(test)]
            signal_after_authorization_pause,
            #[cfg(test)]
            signal_after_flow_reservation_pause,
            #[cfg(test)]
            terminate_before_reap_pause,
        }
    }

    /// How this attempt's process ended, in the receipt's vocabulary.
    ///
    /// Only a process that exited zero on its own ran to the end of its input.
    /// Everything else — a non-zero exit, a signal, a wait this daemon could
    /// not perform — is a failed termination, and a failed termination never
    /// qualifies whatever its stream looked like.
    fn attempt_exit_disposition(
        terminal: &AttemptChildTerminal,
    ) -> crate::decoder_health::ExitDisposition {
        match terminal {
            AttemptChildTerminal::Exited(status, _) if status.success() => {
                crate::decoder_health::ExitDisposition::CleanEnd
            }
            AttemptChildTerminal::Exited(_, _) | AttemptChildTerminal::WaitFailed(_, _) => {
                crate::decoder_health::ExitDisposition::FailedTermination
            }
        }
    }

    fn record_wait_result(
        control: &crate::playback_control::RollingControlHandle,
        producer_attempt: u64,
        result: std::io::Result<std::process::ExitStatus>,
    ) -> AttemptChildTerminal {
        match result {
            Ok(status) => {
                Self::observe_status(control, producer_attempt, &status);
                AttemptChildTerminal::Exited(status, Instant::now())
            }
            Err(error) => {
                tracing::error!(
                    producer_attempt,
                    %error,
                    "producer process exit could not be observed"
                );
                AttemptChildTerminal::WaitFailed(error.kind(), error.to_string())
            }
        }
    }

    fn observe_status(
        control: &crate::playback_control::RollingControlHandle,
        producer_attempt: u64,
        status: &std::process::ExitStatus,
    ) {
        #[cfg(unix)]
        let signal = {
            use std::os::unix::process::ExitStatusExt as _;
            status.signal()
        };
        #[cfg(not(unix))]
        let signal = None;
        control.observe_producer_exit(producer_attempt, status.success(), status.code(), signal);
    }

    pub(super) fn try_wait_observed(
        &mut self,
        _control: &crate::playback_control::RollingControlHandle,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.try_wait()
    }

    pub(super) fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            None => Ok(None),
            Some(AttemptChildTerminal::Exited(status, _)) => Ok(Some(status)),
            Some(AttemptChildTerminal::WaitFailed(kind, message)) => {
                Err(std::io::Error::new(kind, message))
            }
        }
    }

    pub(super) fn id(&self) -> Option<u32> {
        self.terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
            .then_some(self.pid)
            .flatten()
    }

    #[cfg(test)]
    pub(super) fn pause_signal_after_authorization(&self, pause: Arc<std::sync::Barrier>) {
        *self
            .signal_after_authorization_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pause);
    }

    #[cfg(test)]
    pub(super) fn pause_signal_after_flow_reservation(&self, pause: Arc<std::sync::Barrier>) {
        *self
            .signal_after_flow_reservation_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pause);
    }

    pub(super) async fn signal(
        &self,
        signal: crate::process_control::ProcessSignal,
    ) -> std::io::Result<bool> {
        if self
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
        {
            return Ok(false);
        }
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .commands
            .send(AttemptChildCommand::Signal { signal, reply })
            .is_err()
        {
            return self.signal_result_or_broken_pipe("producer exited before signal request");
        }
        match response.await {
            Ok(result) => result,
            Err(_) => {
                self.signal_result_or_broken_pipe("producer supervisor stopped before signaling")
            }
        }
    }

    #[cfg(test)]
    pub(super) async fn kill(&mut self) -> std::io::Result<()> {
        if let Some(terminal) = self
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return terminal.wait_result();
        }
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .commands
            .send(AttemptChildCommand::Terminate { reply: Some(reply) })
            .is_err()
        {
            return self.terminal_result_or_broken_pipe("producer exited before terminate request");
        }
        match response.await {
            Ok(result) => result,
            Err(_) => {
                self.terminal_result_or_broken_pipe("producer supervisor stopped before reaping")
            }
        }
    }

    /// Request SIGKILL without registering a response sender in the process
    /// supervisor. Prepublication repair observes the shared terminal latch,
    /// so a D-state child cannot accumulate one abandoned oneshot per retry.
    pub(super) fn request_termination(&self) -> std::io::Result<()> {
        if let Some(terminal) = self
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return terminal.wait_result();
        }
        if self
            .commands
            .send(AttemptChildCommand::Terminate { reply: None })
            .is_err()
        {
            return self.terminal_result_or_broken_pipe("producer exited before terminate request");
        }
        Ok(())
    }

    pub(super) async fn wait_for_terminal(&self) -> std::io::Result<()> {
        loop {
            let notified = self.terminal_notify.notified();
            if let Some(terminal) = self
                .terminal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                return terminal.wait_result();
            }
            notified.await;
        }
    }

    fn terminal_result_or_broken_pipe(&self, message: &'static str) -> std::io::Result<()> {
        self.terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .map_or_else(
                || Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, message)),
                |terminal| terminal.wait_result(),
            )
    }

    fn signal_result_or_broken_pipe(&self, message: &'static str) -> std::io::Result<bool> {
        if self
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
        {
            Ok(false)
        } else {
            Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, message))
        }
    }

    pub(super) fn terminal_exit_snapshot(
        &self,
    ) -> Option<crate::playback_control::RollingProducerExitSnapshot> {
        let terminal = self
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        let AttemptChildTerminal::Exited(status, observed_at) = terminal else {
            return None;
        };
        #[cfg(unix)]
        let signal = {
            use std::os::unix::process::ExitStatusExt as _;
            status.signal()
        };
        #[cfg(not(unix))]
        let signal = None;
        Some(crate::playback_control::RollingProducerExitSnapshot {
            success: status.success(),
            code: status.code(),
            signal,
            observed_idle_ms: observed_at.elapsed().as_millis().min(i64::MAX as u128) as i64,
        })
    }
}

impl Drop for AttemptChild {
    fn drop(&mut self) {
        let _ = self
            .commands
            .send(AttemptChildCommand::Terminate { reply: None });
    }
}

/// Signal a pid only from the task that owns and has not reaped its Child.
/// A biased, cancel-safe `Child::wait` select proves the pid is still reserved
/// before the command branch can reach this call.
fn signal_owned_pid(
    pid: Option<u32>,
    producer_attempt: u64,
    signal: crate::process_control::ProcessSignal,
) -> std::io::Result<bool> {
    let Some(pid) = pid else {
        return Ok(false);
    };
    crate::process_control::signal(pid, signal).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("signaling producer attempt {producer_attempt}: {error}"),
        )
    })
}
