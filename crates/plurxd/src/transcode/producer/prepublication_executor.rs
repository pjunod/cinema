use super::*;

pub(super) async fn fail_prepublication_transaction(session: &Arc<Session>, reason: String) {
    // Publish the typed local failure before actor End can make a concurrent
    // playlist lookup collapse the generation into an anonymous 404.
    session.fail(PlaylistError::SessionFailed(reason));
    if let Some(ticket) = spawn_context_retirement_owner(session, None, "failed") {
        let _ = ticket.wait().await;
        return;
    }
    let Some(settled) = spawn_prepublication_cleanup_owner(session) else {
        return;
    };
    let _ = settled.await;
}

/// Close a decode alternate's reservation, and never fail a session over it.
///
/// The budget is not refunded either way, so the outcome recorded is a fact
/// about what happened rather than a permission for what happens next. What
/// makes it worth writing at all is the next continuation of this playback:
/// a settled row reads as a spent budget, and a row left `Reserved` reads as a
/// live one — which withholds every later alternate for this playback. That is
/// the safe direction to fail in, and it is why a settlement that does not
/// land is logged at error rather than retried or escalated.
async fn settle_decode_recovery(
    recovery: &DecodeRecoveryReservation,
    outcome: crate::playback_control::RecoveryOutcome,
    decision_sequence: u64,
    sid: &str,
) {
    match recovery.ledger.settle(outcome, unix_ms()).await {
        Ok(crate::playback_control::RecoverySettlement::Settled) => {}
        Ok(settlement) => tracing::error!(
            session = %session_log_id(sid),
            decision_sequence,
            outcome = ?outcome,
            settlement = settlement.label(),
            "a recovery reservation could not be closed; later continuations of \
             this playback will find it still held"
        ),
        Err(error) => tracing::error!(
            session = %session_log_id(sid),
            decision_sequence,
            outcome = ?outcome,
            %error,
            "settling a recovery reservation failed; later continuations of \
             this playback will find it still held"
        ),
    }
}

pub(super) async fn execute_prepublication_transcode_retry(
    session: Arc<Session>,
    retry: &PrepublicationTranscodeRetry,
    decision_sequence: u64,
    failed_attempt: u64,
    actor_recipe: &crate::playback_control::ValidatedRetryRecipe,
    first_reason: crate::playback_control::ProducerDecisionReason,
    sid: &str,
) -> Result<(), String> {
    // The actor names exactly one of the two frozen recipes. Resolve the name
    // rather than assuming the colour-safe one: a qualified decode fault
    // installs the software-decode alternate, and installing the wrong one
    // would respawn the identical command the fault was about.
    let retry = if actor_recipe == &retry.actor_recipe {
        retry
    } else if retry
        .decode_alternate
        .as_deref()
        .is_some_and(|alternate| actor_recipe == &alternate.actor_recipe)
    {
        retry
            .decode_alternate
            .as_deref()
            .expect("the alternate matched on the line above")
    } else {
        return Err(format!(
            "actor retry recipe did not match immutable executor recipe ({first_reason:?})"
        ));
    };
    // The durable budget, taken before anything is torn down.
    //
    // Ordering is the correctness here. `begin_child_replacement` is the point
    // after which this session has no producer until a new one is installed,
    // so a refusal that arrives later turns "this playback already recovered"
    // into "this playback has nothing playing". Reserving first means a
    // refusal leaves the failed producer exactly where the actor left it.
    //
    // Only the alternate carries a ledger; see `DecodeRecoveryReservation`.
    let recovery = retry.recovery.as_deref();
    if let Some(recovery) = recovery {
        let reservation = recovery
            .ledger
            .reserve(
                failed_attempt,
                decision_sequence,
                &recovery.failed_plan_digest,
                // The successor's own plan, read off the recipe that is about
                // to run, so the row records the plan this session installs
                // rather than one computed a second time from policy.
                &retry.observation.plan_digest,
                unix_ms(),
            )
            .await
            .map_err(|error| format!("the recovery budget could not be reserved: {error}"))?;
        if !matches!(
            reservation,
            crate::playback_control::RecoveryReservation::Held(_)
        ) {
            // Session start already read this epoch and withheld the alternate
            // when it found a row, so an alternate that reaches here and is
            // refused means the row appeared between that read and now: two
            // nodes reaching the same playback's fault at once, or a
            // predecessor settling late. The budget is not this attempt's to
            // spend, and installing anyway is the second automatic recovery
            // the ledger exists to make impossible.
            tracing::warn!(
                session = %session_log_id(sid),
                decision_sequence,
                failed_attempt,
                reservation = reservation.label(),
                "a decode alternate was refused its recovery budget"
            );
            return Err(format!(
                "this playback's recovery budget is {}",
                reservation.label()
            ));
        }
    }
    let mut replacement = session.begin_child_replacement().await;
    // Cancellation after the actor decision has been observed must fence the
    // generation even though retry admission intentionally happens later.
    replacement.mark_actor_decision_pending(PlaylistError::SessionFailed(format!(
        "producer retry was cancelled after actor decision {first_reason:?}"
    )));
    let transaction = async {
        terminate_exact_prepublication_child(&session, failed_attempt).await?;
        clear_session_dir(&session.dir)
            .await
            .map_err(|error| format!("clearing predecessor scratch: {error}"))?;
        session.confirm_predecessor_scratch_cleared();
        session.clear_compatibility_before_retry().await;

        let producer_attempt = session
            .control
            .admit_producer_retry(decision_sequence, &retry.actor_recipe.fingerprint)
            .await
            .map_err(|reason| format!("actor rejected exact retry admission: {reason:?}"))?;
        session
            .bind_retry_compatibility_attempt(producer_attempt)
            .await;

        if let Some(threads) = retry.software_threads {
            // A real demotion: the encoder becomes software, so the hardware
            // slot goes back. Forced, because the viewer is already watching
            // and this is the documented mid-session fallback.
            let work = Workload::of(&retry.file, retry.target_height);
            let permit = retry.software_pool.take_forced(threads);
            if permit.threads() != threads {
                return Err(format!(
                    "software admission granted {} threads for immutable {threads}-thread recipe",
                    permit.threads()
                ));
            }
            session.demote_to_software(work, permit);
        } else if let Some(total) = retry.cpu_total {
            // Not a demotion. The encoder — and its hardware slot — stay
            // exactly where they are; what changes is how much of the pipeline
            // runs on the CPU. The session is already paying for what it
            // reserved at admission, so what it owes is the difference. Asking
            // for the whole estimate again would make it pay twice to end up
            // owning once — and on a box where twice does not fit, the retry
            // would fail over capacity the session already held.
            let held = session.software_threads_held();
            if let Some(delta) = total.checked_sub(held).filter(|delta| *delta > 0) {
                // Registering the wait is what makes a background producer
                // checkpoint and yield; without it the pool refuses a live
                // caller outright while background work runs, and this failure
                // arrives after the predecessor is already gone.
                let _queued = retry.software_pool.wait_for_capacity();
                let deadline = Instant::now() + MIXED_RECOVERY_CAPACITY_WAIT;
                let permit = loop {
                    if let Some(permit) = retry
                        .software_pool
                        .try_take_delta(retry.software_budget, delta)
                    {
                        break permit;
                    }
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(format!(
                            "no CPU capacity for a {delta}-thread mixed decode retry within {:.1}s",
                            MIXED_RECOVERY_CAPACITY_WAIT.as_secs_f64()
                        ));
                    }
                    tokio::time::sleep(ADMISSION_POLL.min(deadline - now)).await;
                };
                session.add_cpu_decode_reservation(permit);
            }
        }

        session
            .spawn_and_install_prepublication_child(producer_attempt, || {
                spawn_ffmpeg(
                    retry.args.as_ref(),
                    retry.encoder.label(),
                    sid,
                    FfmpegProgressObserver::rolling(
                        Arc::clone(&session.progress),
                        producer_attempt,
                        session.control.clone(),
                    ),
                    &retry.runtime_cache,
                    {
                        #[cfg(unix)]
                        let descriptors = FfmpegDescriptors::from_raw_fds(
                            None,
                            None,
                            session
                                .subtitle_handle
                                .as_ref()
                                .map(std::os::fd::AsRawFd::as_raw_fd),
                            false,
                        );
                        #[cfg(windows)]
                        let descriptors = windows_session_descriptors(&session)?;
                        descriptors
                    },
                    retry.observation.clone(),
                )
                .map_err(|error| format!("spawning immutable fallback recipe: {error}"))
            })
            .await?;
        *session.encoder_label.lock().await = retry.encoder.label();
        // One writer for the class, and it knows which of the three shapes
        // this attempt is. A mixed pipeline filed under the all-hardware
        // bucket poisons that bucket's measured speed for the work actually in
        // it, and the measurement is what admits the next session.
        let work = Workload::of(&retry.file, retry.target_height);
        *session.class.lock().expect("class mutex") = if retry.encoder == Encoder::Software {
            work.class(crate::admission::SOFTWARE)
        } else if retry.cpu_total.is_some() {
            work.mixed_class(retry.encoder.family_name())
        } else {
            work.class(retry.encoder.label())
        };
        session
            .control
            .decision_applied(decision_sequence, Some(producer_attempt))
            .await
            .map_err(|reason| format!("actor rejected DecisionApplied: {reason:?}"))?;
        Ok::<u64, String>(producer_attempt)
    }
    .await;
    let producer_attempt = match transaction {
        Ok(producer_attempt) => producer_attempt,
        Err(error) => {
            // ChildReplacement::Drop is intentionally generic for unknown
            // callers. Settle it explicitly here so the actor's first exact
            // decision reason remains the Session's first-writer failure.
            session.fail(PlaylistError::SessionFailed(format!(
                "prepublication recovery failed after {first_reason:?}: {error}"
            )));
            if let Err(acknowledgement_error) = session
                .control
                .decision_applied(decision_sequence, None)
                .await
            {
                tracing::error!(
                    session = %session_log_id(sid),
                    decision_sequence,
                    ?acknowledgement_error,
                    "actor rejected failed retry DecisionApplied acknowledgement"
                );
            }
            // The budget is spent by the attempt, not by the outcome. A
            // recovery that was reserved and then failed to install has been
            // attempted, and refunding it here would hand the same playback a
            // second try at the decode that just failed twice.
            if let Some(recovery) = recovery {
                settle_decode_recovery(
                    recovery,
                    crate::playback_control::RecoveryOutcome::Exhausted,
                    decision_sequence,
                    sid,
                )
                .await;
            }
            replacement.settle_terminal_rejection();
            return Err(error);
        }
    };
    replacement.complete();
    if let Some(recovery) = recovery {
        settle_decode_recovery(
            recovery,
            crate::playback_control::RecoveryOutcome::Installed,
            decision_sequence,
            sid,
        )
        .await;
    }
    tracing::info!(
        session = %session_log_id(sid),
        producer_attempt,
        encoder = retry.encoder.label(),
        pipeline = retry.pipeline.name(),
        "actor-owned prepublication fallback installed"
    );
    Ok(())
}

async fn execute_prepublication_copy_retry(
    session: Arc<Session>,
    retry: &PrepublicationCopyRetry,
    decision_sequence: u64,
    failed_attempt: u64,
    actor_recipe: &crate::playback_control::ValidatedRetryRecipe,
    first_reason: crate::playback_control::ProducerDecisionReason,
    sid: &str,
) -> Result<(), String> {
    if actor_recipe != &retry.actor_recipe {
        return Err(format!(
            "actor copy retry recipe did not match immutable executor recipe ({first_reason:?})"
        ));
    }
    if first_reason != crate::playback_control::ProducerDecisionReason::Unsupported {
        return Err(format!(
            "copy fallback was admitted for non-structural reason {first_reason:?}"
        ));
    }
    let mut replacement = session.begin_child_replacement().await;
    replacement.mark_actor_decision_pending(PlaylistError::SessionFailed(format!(
        "copy retry was cancelled after actor decision {first_reason:?}"
    )));
    let transaction = async {
        terminate_exact_prepublication_child(&session, failed_attempt).await?;
        clear_session_dir(&session.dir)
            .await
            .map_err(|error| format!("clearing copy-reader predecessor scratch: {error}"))?;
        session.confirm_predecessor_scratch_cleared();
        session.clear_compatibility_before_retry().await;

        let producer_attempt = session
            .control
            .admit_producer_retry(decision_sequence, &retry.actor_recipe.fingerprint)
            .await
            .map_err(|reason| format!("actor rejected exact copy retry admission: {reason:?}"))?;
        session
            .bind_retry_compatibility_attempt(producer_attempt)
            .await;
        session
            .spawn_and_install_prepublication_child(producer_attempt, || {
                spawn_ffmpeg(
                    retry.args.as_ref(),
                    "copy",
                    sid,
                    FfmpegProgressObserver::rolling(
                        Arc::clone(&session.progress),
                        producer_attempt,
                        session.control.clone(),
                    ),
                    &retry.runtime_cache,
                    {
                        #[cfg(unix)]
                        let descriptors = FfmpegDescriptors::default();
                        #[cfg(windows)]
                        let descriptors = windows_session_descriptors(&session)?;
                        descriptors
                    },
                    DiagnosticObservation::copy(sid),
                )
                .map_err(|error| format!("spawning immutable copy fallback: {error}"))
            })
            .await?;
        session
            .control
            .decision_applied(decision_sequence, Some(producer_attempt))
            .await
            .map_err(|reason| format!("actor rejected copy DecisionApplied: {reason:?}"))?;
        Ok::<u64, String>(producer_attempt)
    }
    .await;
    let producer_attempt = match transaction {
        Ok(producer_attempt) => producer_attempt,
        Err(error) => {
            session.fail(PlaylistError::SessionFailed(format!(
                "copy recovery failed after {first_reason:?}: {error}"
            )));
            if let Err(acknowledgement_error) = session
                .control
                .decision_applied(decision_sequence, None)
                .await
            {
                tracing::error!(
                    session = %session_log_id(sid),
                    decision_sequence,
                    ?acknowledgement_error,
                    "actor rejected failed copy retry DecisionApplied acknowledgement"
                );
            }
            replacement.settle_terminal_rejection();
            return Err(error);
        }
    };
    replacement.complete();
    tracing::info!(
        session = %session_log_id(sid),
        producer_attempt,
        encoder = "copy",
        pipeline = "ffmpeg-hls-muxer",
        "actor-owned copy fallback installed"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PrepublicationExecutorExit {
    ActorTerminal,
    SessionGone,
    FailedClosed { producer_attempt: u64 },
    ActorFailureApplied,
    ActorCompletionApplied,
}

impl PrepublicationExecutorExit {
    pub(super) fn monitor_cleanup_attempt(self) -> Option<u64> {
        match self {
            Self::FailedClosed { producer_attempt } => Some(producer_attempt),
            Self::ActorTerminal
            | Self::SessionGone
            | Self::ActorFailureApplied
            | Self::ActorCompletionApplied => None,
        }
    }
}

pub(super) fn completion_playlist_evidence(
    probe_sequence: u64,
    producer_attempt: u64,
    bytes: Option<&[u8]>,
) -> crate::playback_control::RollingProducerCompletionEvidence {
    let text = bytes.and_then(|bytes| std::str::from_utf8(bytes).ok());
    let segments = text.map(parse_playlist).unwrap_or_default();
    let end_list = text.is_some_and(|text| {
        let lines = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        lines.first().copied() == Some("#EXTM3U")
            && lines.last().copied() == Some("#EXT-X-ENDLIST")
            && lines
                .iter()
                .filter(|line| **line == "#EXT-X-ENDLIST")
                .count()
                == 1
    });
    crate::playback_control::RollingProducerCompletionEvidence {
        probe_sequence,
        producer_attempt,
        end_list,
        final_segment: segments.last().map(|segment| segment.index),
        final_end_ms: segments.last().map(|segment| segment.end_ms),
    }
}

async fn publish_copy_reader_outcome(
    session: &Session,
    sid: &str,
    producer_attempt: u64,
    outcome: copyseg::Outcome,
) {
    let classification = match outcome {
        copyseg::Outcome::Completed(counts) => {
            tracing::info!(
                session = %session_log_id(sid),
                producer_attempt,
                build = crate::version::BUILD,
                "{}",
                copyseg::summary(&counts)
            );
            crate::playback_control::CopyProducerExitClassification::Completed
        }
        copyseg::Outcome::ReaderFailed { reason, counts } => {
            tracing::error!(
                session = %session_log_id(sid),
                producer_attempt,
                counts = %copyseg::summary(&counts),
                "copy segmenter reader failed: {reason}"
            );
            crate::playback_control::CopyProducerExitClassification::ReaderFailed
        }
        copyseg::Outcome::InvalidHevcConfiguration(reason) => {
            tracing::error!(
                session = %session_log_id(sid),
                producer_attempt,
                "copy segmenter produced invalid HEVC configuration: {reason}"
            );
            crate::playback_control::CopyProducerExitClassification::InvalidConfiguration
        }
        copyseg::Outcome::Unsupported(reason) => {
            tracing::warn!(
                session = %session_log_id(sid),
                producer_attempt,
                "copy segmenter rejected the stream shape: {reason}"
            );
            crate::playback_control::CopyProducerExitClassification::Unsupported
        }
        copyseg::Outcome::Cancelled(counts) => {
            tracing::debug!(
                session = %session_log_id(sid),
                producer_attempt,
                counts = %copyseg::summary(&counts),
                "copy segmenter stopped after lifecycle teardown"
            );
            return;
        }
    };
    let deadline = Instant::now() + COPY_READER_INGRESS_TIMEOUT;
    if let Err(rejection) = session
        .control
        .classify_copy_producer_exit_before(producer_attempt, classification, deadline)
        .await
    {
        tracing::warn!(
            session = %session_log_id(sid),
            producer_attempt,
            ?classification,
            ?rejection,
            "copy reader classification was not accepted by the exact producer attempt"
        );
    }
}

#[allow(clippy::too_many_arguments)] // one copy producer's worth of identity
pub(super) fn spawn_copy_reader_owner(
    session: Arc<Session>,
    stdout: tokio::process::ChildStdout,
    dir: PathBuf,
    sid: String,
    producer_attempt: u64,
    // The options the pipe's argv was built from, not a bool derived from
    // them. There is exactly one function that answers whether a copy leaves a
    // Dolby Vision record behind, and it reads these; taking its answer here
    // would put a second, plausible `false` in the tree — one word, and a
    // forced-Original Profile 7 title ships an init declaring an enhancement
    // layer it does not have, with every test green. Taking the options
    // instead means the only way to get it wrong is to pass options the child
    // did not get, which reads as wrong on sight.
    source: plurx_core::domain::MediaFile,
    video: plurx_core::transcode::CopyVideoOptions,
    grants: Option<crate::copyseg::WriteGrants>,
) {
    tokio::spawn(async move {
        // The outer owner retains the pipe until the actor has accepted the
        // typed reader fact. Structural `Unsupported` deliberately stops
        // reading early; dropping stdout before classification lets ffmpeg's
        // resulting EPIPE race in as a non-zero process verdict and suppress
        // the only permitted direct-HLS retry. The Arc also preserves this
        // ordering if the reader task panics: `ReaderFailed` is sequenced
        // while the pipe is still open, then the final owner closes it.
        let stdout = Arc::new(Mutex::new(stdout));
        let reader_stdout = Arc::clone(&stdout);
        let worker_sid = sid.clone();
        // The completion barrier is installed *before* the worker is spawned
        // and moved into it, so there is no instant in which retirement can
        // observe no writer for a reader that is about to exist. Registration
        // after the spawn was the race: ffmpeg's confirmed reap is necessary
        // and insufficient, because this task drains the pipe and writes the
        // last segment and playlist on its own schedule. In the incident its
        // completion log followed the retirement-settled log.
        //
        // It belongs to the worker and not to this outer owner: the actor may
        // reject a late classification as `SessionEnded`, and that rejection
        // says nothing about whether the worker has stopped writing.
        let writer = match session.scratch.as_ref() {
            Some(permit) => match permit.ledger().register_writer(permit.key()) {
                Some(writer) => Some(writer),
                None => {
                    // Retirement fenced this allocation before the worker was
                    // polled. `None` means "do not start writing", and
                    // starting anyway would put bytes into a directory whose
                    // final inventory has already been measured and committed.
                    tracing::debug!(
                        session = %session_log_id(&sid),
                        producer_attempt,
                        "copy reader refused: this incarnation was retired before it could write"
                    );
                    publish_copy_reader_outcome(
                        &session,
                        &sid,
                        producer_attempt,
                        copyseg::Outcome::Cancelled(Default::default()),
                    )
                    .await;
                    return;
                }
            },
            None => None,
        };
        let worker = tokio::spawn(async move {
            let _writer = writer;
            let mut stdout = reader_stdout.lock_owned().await;
            copyseg::run(
                &mut *stdout,
                dir,
                &worker_sid,
                copyseg::Limits::default(),
                &source,
                video,
                grants,
            )
            .await
        });
        let outcome = match worker.await {
            Ok(outcome) => outcome,
            Err(join_error) => {
                if session.control.is_retired()
                    || session.control.current_producer_attempt() != producer_attempt
                {
                    return;
                }
                tracing::error!(
                    session = %session_log_id(&sid),
                    producer_attempt,
                    %join_error,
                    "copy reader task failed"
                );
                let deadline = Instant::now() + COPY_READER_INGRESS_TIMEOUT;
                if let Err(rejection) = session
                    .control
                    .classify_copy_producer_exit_before(
                        producer_attempt,
                        crate::playback_control::CopyProducerExitClassification::ReaderFailed,
                        deadline,
                    )
                    .await
                {
                    tracing::warn!(
                        session = %session_log_id(&sid),
                        producer_attempt,
                        ?rejection,
                        "failed copy reader task could not classify its exact producer attempt"
                    );
                }
                return;
            }
        };
        publish_copy_reader_outcome(&session, &sid, producer_attempt, outcome).await;
        drop(stdout);
    });
}

/// Read one exact-attempt completion snapshot under the actor's classification
/// deadline. This is a one-shot probe, not a polling recovery owner: the actor
/// alone decides completion versus partial-success failure from the returned
/// ENDLIST/frontier evidence.
async fn classify_successful_transcode_exit(
    session: &Arc<Session>,
    probe: &crate::playback_control::RollingProducerExitProbe,
    sid: &str,
) -> Result<
    crate::playback_control::RollingProducerCompletionDisposition,
    crate::playback_control::ProducerAttemptRejection,
> {
    let bytes = if session.control.current_producer_attempt() == probe.producer_attempt
        && session.compatibility_producer_attempt() == probe.producer_attempt
    {
        tokio::time::timeout_at(
            tokio::time::Instant::from_std(probe.deadline),
            plurx_core::transcode::manifest::read_bounded_playlist(&session.dir, "index.m3u8"),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()
        .filter(|_| {
            session.control.current_producer_attempt() == probe.producer_attempt
                && session.compatibility_producer_attempt() == probe.producer_attempt
        })
    } else {
        None
    };
    let evidence = completion_playlist_evidence(
        probe.probe_sequence,
        probe.producer_attempt,
        bytes.as_deref(),
    );
    let end_list = evidence.end_list;
    let final_segment = evidence.final_segment;
    let final_end_ms = evidence.final_end_ms;
    let disposition = session
        .control
        .classify_producer_exit_before(evidence, probe.deadline)
        .await;
    tracing::info!(
        session = %session_log_id(sid),
        producer_attempt = probe.producer_attempt,
        probe_sequence = probe.probe_sequence,
        end_list,
        final_segment = ?final_segment,
        final_end_ms = ?final_end_ms,
        disposition = ?disposition,
        "actor-owned transcode exit completion probe settled"
    );
    disposition
}

async fn run_prepublication_producer_executor(
    session: Weak<Session>,
    mut registration: crate::playback_control::RollingProducerExecutorRegistration,
    manager_publication: tokio::sync::oneshot::Receiver<()>,
    retry: Option<PrepublicationRetry>,
    sid: String,
) -> PrepublicationExecutorExit {
    // Registration with the actor must precede the initial deadline, but the
    // executor must not *act* until the manager has published the exact initial
    // attempt. Otherwise an immediate process exit can admit a successor and
    // make registration of the captured initial attempt spuriously stale.
    if manager_publication.await.is_err() {
        return PrepublicationExecutorExit::SessionGone;
    }
    loop {
        match registration.next_decision().await {
            crate::playback_control::RollingProducerExecutorPoll::ClassifyExit(probe) => {
                let Some(session) = session.upgrade() else {
                    return PrepublicationExecutorExit::SessionGone;
                };
                let disposition = classify_successful_transcode_exit(&session, &probe, &sid).await;
                if matches!(
                    disposition,
                    Ok(
                        crate::playback_control::RollingProducerCompletionDisposition::CompleteVerifiedDuration
                            | crate::playback_control::RollingProducerCompletionDisposition::CompleteUnverifiedDuration
                    )
                ) {
                    let disposition = disposition.expect("matched successful completion");
                    // Transfer the already-exited child and its admission
                    // resources before awaiting. A cancelled executor cannot
                    // strand either one after the actor has made completion
                    // immutable.
                    let settlement = spawn_published_completion_cleanup_owner(
                        &session,
                        probe.producer_attempt,
                        disposition,
                        &sid,
                    );
                    match settlement.await {
                        Ok(PublishedFailureCleanupOutcome::Reaped) => {
                            if let Err(error) = registration.settle_expected().await {
                                tracing::error!(
                                    session = %session_log_id(&sid),
                                    producer_attempt = probe.producer_attempt,
                                    disposition = ?disposition,
                                    ?error,
                                    "completed producer reaped but executor settlement was rejected"
                                );
                                return PrepublicationExecutorExit::FailedClosed {
                                    producer_attempt: probe.producer_attempt,
                                };
                            }
                            return PrepublicationExecutorExit::ActorCompletionApplied;
                        }
                        Ok(PublishedFailureCleanupOutcome::RetirementTookOwnership) => {
                            return PrepublicationExecutorExit::ActorTerminal;
                        }
                        Err(_) => {
                            tracing::error!(
                                session = %session_log_id(&sid),
                                producer_attempt = probe.producer_attempt,
                                disposition = ?disposition,
                                "completed producer cleanup owner ended without settlement"
                            );
                            return PrepublicationExecutorExit::FailedClosed {
                                producer_attempt: probe.producer_attempt,
                            };
                        }
                    }
                }
            }
            crate::playback_control::RollingProducerExecutorPoll::Decision(decision) => {
                let Some(session) = session.upgrade() else {
                    return PrepublicationExecutorExit::SessionGone;
                };
                match decision.as_ref() {
                    crate::playback_control::ProducerDecision::Retry {
                        decision_sequence,
                        failed_attempt,
                        recipe,
                        reason,
                    } => {
                        let result = match retry.as_ref() {
                            Some(PrepublicationRetry::Transcode(retry)) => {
                                execute_prepublication_transcode_retry(
                                    Arc::clone(&session),
                                    retry,
                                    *decision_sequence,
                                    *failed_attempt,
                                    recipe,
                                    *reason,
                                    &sid,
                                )
                                .await
                            }
                            Some(PrepublicationRetry::Copy(retry)) => {
                                execute_prepublication_copy_retry(
                                    Arc::clone(&session),
                                    retry,
                                    *decision_sequence,
                                    *failed_attempt,
                                    recipe,
                                    *reason,
                                    &sid,
                                )
                                .await
                            }
                            None => Err(format!(
                                "actor selected a retry without an immutable recipe ({reason:?})"
                            )),
                        };
                        if let Err(error) = result {
                            fail_prepublication_transaction(
                                &session,
                                format!("prepublication recovery failed after {reason:?}: {error}"),
                            )
                            .await;
                            return PrepublicationExecutorExit::ActorFailureApplied;
                        }
                    }
                    crate::playback_control::ProducerDecision::Fail {
                        decision_sequence,
                        failed_attempt,
                        reason,
                        cleanup,
                        ..
                    } => {
                        if cleanup.cleanup_policy
                            == crate::playback_control::CleanupPolicy::RetainPublished
                        {
                            // Transfer ownership before the first await. The
                            // detached owner keeps admitted bytes/catalogs
                            // intact and holds capacity until exact reap even
                            // if this executor is cancelled.
                            let settlement = spawn_published_failure_cleanup_owner(
                                &session,
                                *failed_attempt,
                                *decision_sequence,
                                *reason,
                                &sid,
                            );
                            match settlement.await {
                                Ok(PublishedFailureCleanupOutcome::Reaped) => {
                                    if let Err(error) = registration.settle_expected().await {
                                        tracing::error!(
                                            session = %session_log_id(&sid),
                                            producer_attempt = *failed_attempt,
                                            decision_sequence = *decision_sequence,
                                            reason = ?reason,
                                            cleanup_policy = "retain_published",
                                            ?error,
                                            "published producer failure reaped but executor settlement was rejected"
                                        );
                                        return PrepublicationExecutorExit::FailedClosed {
                                            producer_attempt: *failed_attempt,
                                        };
                                    }
                                    return PrepublicationExecutorExit::ActorFailureApplied;
                                }
                                Ok(PublishedFailureCleanupOutcome::RetirementTookOwnership) => {
                                    return PrepublicationExecutorExit::ActorTerminal;
                                }
                                Err(_) => {
                                    tracing::error!(
                                        session = %session_log_id(&sid),
                                        producer_attempt = *failed_attempt,
                                        decision_sequence = *decision_sequence,
                                        reason = ?reason,
                                        cleanup_policy = "retain_published",
                                        "published producer cleanup owner ended without settlement"
                                    );
                                    return PrepublicationExecutorExit::FailedClosed {
                                        producer_attempt: *failed_attempt,
                                    };
                                }
                            }
                        }

                        debug_assert_eq!(
                            cleanup.cleanup_policy,
                            crate::playback_control::CleanupPolicy::DiscardPrepublication
                        );
                        // Publish the actor's exact terminal verdict before
                        // DecisionApplied/End can make readers observe only a
                        // retired capability and return an anonymous 404.
                        session.fail(
                            if *reason
                                == crate::playback_control::ProducerDecisionReason::ProcessExit
                            {
                                PlaylistError::ProducerExited(
                                    "the actor observed a non-zero producer exit".to_owned(),
                                )
                            } else {
                                PlaylistError::SessionFailed(format!(
                                    "producer failed before publication: {reason:?}"
                                ))
                            },
                        );
                        let mut replacement = session.begin_child_replacement().await;
                        replacement.mark_admission_pending();
                        let cleanup = async {
                            terminate_exact_prepublication_child(&session, *failed_attempt).await?;
                            clear_session_dir(&session.dir).await.map_err(|error| {
                                format!("clearing failed producer scratch: {error}")
                            })?;
                            session.confirm_predecessor_scratch_cleared();
                            session.clear_compatibility_before_retry().await;
                            session
                                .control
                                .decision_applied(*decision_sequence, None)
                                .await
                                .map_err(|error| {
                                    format!("acknowledging actor failure: {error:?}")
                                })?;
                            session.control.end().await.map_err(|error| {
                                format!("terminalizing actor failure: {error:?}")
                            })?;
                            Ok::<(), String>(())
                        }
                        .await;
                        match cleanup {
                            Ok(()) => {
                                session.release_hardware_after_confirmed_reap();
                                session.release_software_after_confirmed_reap();
                                replacement.complete();
                            }
                            Err(error) => {
                                replacement.settle_terminal_rejection();
                                drop(replacement);
                                fail_prepublication_transaction(
                                    &session,
                                    format!("prepublication failure cleanup failed after {reason:?}: {error}"),
                                )
                                .await;
                            }
                        }
                        return PrepublicationExecutorExit::ActorFailureApplied;
                    }
                }
            }
            crate::playback_control::RollingProducerExecutorPoll::Terminal(_) => {
                return PrepublicationExecutorExit::ActorTerminal;
            }
            crate::playback_control::RollingProducerExecutorPoll::Unavailable => {
                if let Some(session) = session.upgrade() {
                    let producer_attempt = session.control.current_producer_attempt();
                    if session.actor_prepublication_producer.load(Acquire) {
                        fail_prepublication_transaction(
                            &session,
                            "prepublication executor became unavailable".to_owned(),
                        )
                        .await;
                    } else {
                        tracing::error!(
                            session = %session_log_id(&sid),
                            "producer decision observer became unavailable after first-media handoff; actor lifetime fence retained"
                        );
                        return PrepublicationExecutorExit::FailedClosed { producer_attempt };
                    }
                    return PrepublicationExecutorExit::ActorFailureApplied;
                }
                return PrepublicationExecutorExit::SessionGone;
            }
        }
    }
}

pub(super) fn spawn_prepublication_executor_owner(
    session: &Arc<Session>,
    registration: crate::playback_control::RollingProducerExecutorRegistration,
    manager_publication: tokio::sync::oneshot::Receiver<()>,
    retry: Option<PrepublicationRetry>,
    sid: String,
) {
    let executor_session = Arc::downgrade(session);
    let monitor_session = Arc::downgrade(session);
    let monitor_sid = sid.clone();
    tokio::spawn(async move {
        let worker = tokio::spawn(run_prepublication_producer_executor(
            executor_session,
            registration,
            manager_publication,
            retry,
            sid,
        ));
        let unexpected_exit = match worker.await {
            Err(join_error) => Some((format!("executor task failed: {join_error}"), None)),
            Ok(exit) => exit.monitor_cleanup_attempt().map(|producer_attempt| {
                (
                    "executor returned failed-closed".to_owned(),
                    Some(producer_attempt),
                )
            }),
        };
        if let Some((unexpected_exit, exact_attempt)) = unexpected_exit {
            if let Some(session) = monitor_session.upgrade() {
                let actor_still_owns_prepublication = match session.control.snapshot().await {
                    Some(snapshot) => {
                        snapshot.terminal.is_none()
                            && !snapshot.producer_control.producer_media_published
                    }
                    None => true,
                };
                if session.actor_prepublication_producer.load(Acquire)
                    && actor_still_owns_prepublication
                {
                    fail_prepublication_transaction(
                        &session,
                        format!(
                            "prepublication executor stopped without cleanup: {unexpected_exit}"
                        ),
                    )
                    .await;
                } else {
                    let producer_attempt =
                        exact_attempt.unwrap_or_else(|| session.control.current_producer_attempt());
                    spawn_published_executor_loss_cleanup_owner(
                        &session,
                        producer_attempt,
                        &monitor_sid,
                    );
                    tracing::error!(
                        session = %session_log_id(&monitor_sid),
                        producer_attempt,
                        reason = %unexpected_exit,
                        "producer executor stopped after actor lifetime ownership activated; exact cleanup owner retained admitted media"
                    );
                }
            }
        }
    });
}
