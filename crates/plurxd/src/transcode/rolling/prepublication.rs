use super::*;

/// Immutable, one-shot retry material retained before the initial producer is
/// admitted. No fallback re-runs policy, rereads a row, or reconstructs
/// ffmpeg arguments from mutable session state.
#[derive(Clone)]
pub(super) struct PrepublicationTranscodeRetry {
    pub(super) actor_recipe: crate::playback_control::ValidatedRetryRecipe,
    pub(super) args: Arc<[String]>,
    pub(super) encoder: Encoder,
    pub(super) pipeline: Pipeline,
    pub(super) file: plurx_core::domain::MediaFile,
    pub(super) target_height: i64,
    pub(super) software_threads: Option<usize>,
    /// The whole CPU cost of this retry's pipeline, read off its resolved
    /// plan. `Some` only when the encoder is retained, which is the mixed
    /// case; the demotion path uses `software_threads` instead.
    ///
    /// A total rather than a difference, because the difference depends on what
    /// the session holds *at the moment of the retry* and this recipe is
    /// frozen before then. The executor subtracts.
    pub(super) cpu_total: Option<usize>,
    /// The pool bound this retry will be admitted against, frozen with the
    /// rest of the recipe so the executor cannot read a different number than
    /// the one the recipe was validated under.
    pub(super) software_budget: usize,
    pub(super) software_pool: crate::admission::SwPool,
    pub(super) runtime_cache: PathBuf,
    /// Frozen with the rest of the recipe, for the same reason: the grammar
    /// that will read this attempt is a property of the plan it was resolved
    /// from, and resolving it again at execution time could answer differently
    /// from the resolution the actor validated.
    pub(super) observation: DiagnosticObservation,
    /// The same delivery decoded in software, frozen beside this one.
    ///
    /// Boxed because it is the same type: an alternate has no alternate of its
    /// own, and nothing constructs a second level. Held here rather than in a
    /// second executor slot so the two cannot be reached independently — the
    /// actor names exactly one of them in its decision, and
    /// `execute_prepublication_transcode_retry` resolves that name against
    /// this pair and refuses anything else.
    pub(super) decode_alternate: Option<Box<PrepublicationTranscodeRetry>>,
    /// The durable recovery budget this recipe spends when it is installed.
    ///
    /// `Some` only on a decode alternate that a session start found an unspent
    /// budget for. `None` on the colour-safe retry, on an alternate belonging
    /// to a session with no durable identity, and on an alternate whose
    /// playback has already recovered — in which last case there is no
    /// alternate at all.
    pub(super) recovery: Option<Box<DecodeRecoveryReservation>>,
}

/// The durable recovery budget a decode alternate spends, frozen beside it.
///
/// Held on the alternate and on nothing else. The colour-safe retry answers
/// "this encode route stopped" rather than "this source did not decode", so it
/// is not a recovery from a decode fault and does not spend the budget; and
/// [`PrepublicationCopyRetry`] has no field to carry one at all, which is how
/// the copy path is excluded — structurally, rather than by a runtime check a
/// later edit could forget to keep. A copy attempt's diagnostic identity is
/// `copy:<session>` rather than a plan digest, so the store would refuse its
/// reservation anyway; being unable to write the call is better than finding
/// out at the store, in the middle of a recovery.
#[derive(Clone)]
pub(super) struct DecodeRecoveryReservation {
    pub(super) ledger: crate::playback_control::ProducerRecoveryLedger,
    /// The plan the fault will be about: the delivered plan this session
    /// started on.
    ///
    /// Frozen here for the reason every other field of this recipe is frozen,
    /// and for one more — the fault that names the failed plan is reported to
    /// the *actor*, and the executor that installs the alternate never sees
    /// it. There is no install-time source for this value that is not a
    /// re-derivation, and re-deriving a plan in the middle of a recovery is
    /// exactly what this type exists to prevent.
    pub(super) failed_plan_digest: String,
}

pub(super) struct PreparedTranscodeRetry {
    pub(super) opts: TranscodeOptions,
    pub(super) encoder: Encoder,
    pub(super) software_threads: Option<usize>,
}

/// Frozen fallback for the copy reader's one structural-unsupported verdict.
/// The actor validates this exact identity before the executor may replace the
/// pipe producer with ffmpeg's direct HLS muxer.
#[derive(Clone)]
pub(super) struct PrepublicationCopyRetry {
    pub(super) actor_recipe: crate::playback_control::ValidatedRetryRecipe,
    pub(super) args: Arc<[String]>,
    pub(super) runtime_cache: PathBuf,
}

#[derive(Clone)]
pub(super) enum PrepublicationRetry {
    Transcode(Box<PrepublicationTranscodeRetry>),
    Copy(PrepublicationCopyRetry),
}

/// Cancellation settlement for the interval between scratch creation and
/// manager publication. Once a Session exists the guard carries a temporary
/// strong cleanup owner; it is released at registration and never forms a
/// task/session cycle. A dropped start future still terminates/reaps the child,
/// clears scratch, ends the actor, and returns admissions.
pub(super) struct PrepublicationStartSettlement {
    dir: PathBuf,
    session: Option<Arc<Session>>,
    settled: bool,
}

impl PrepublicationStartSettlement {
    pub(super) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            session: None,
            settled: false,
        }
    }

    pub(super) fn attach(&mut self, session: &Arc<Session>) {
        self.session = Some(Arc::clone(session));
    }

    pub(super) fn disarm(&mut self) {
        self.settled = true;
        self.session = None;
    }

    pub(super) fn settle(mut self) {
        self.settled = true;
        self.session = None;
    }
}

impl Drop for PrepublicationStartSettlement {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let session = self.session.take();
        let dir = self.dir.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Some(session) = session {
                    fail_prepublication_transaction(
                        &session,
                        "producer start was cancelled before manager publication".to_owned(),
                    )
                    .await;
                } else {
                    let _ = tokio::fs::remove_dir_all(dir).await;
                }
            });
        } else {
            tracing::error!(
                path = %dir.display(),
                "runtime ended before cancelled prepublication scratch could be removed"
            );
        }
    }
}

/// Build the same delivery options for a software-decoded alternate.
///
/// This contract is shared by live replacement and durable offline jobs. It
/// is intentionally outside the temporary live-HLS feature: offline recovery
/// is a shipping producer path in both builds, and decoder safety is not a
/// feature-gated behavior.
pub(super) fn decode_restricted_options(
    delivered: &ResolvedTranscode,
    alternate: &ResolvedTranscode,
    opts: &TranscodeOptions,
) -> Result<Option<TranscodeOptions>, String> {
    if alternate.decode().backend() != plurx_core::transcode::DecodeBackend::Software {
        return Err(
            "the decode-restricted alternate did not resolve to software decode".to_owned(),
        );
    }
    if alternate.decode().input_codec() != delivered.decode().input_codec() {
        return Err("the decode-restricted alternate changed the input codec".to_owned());
    }
    if alternate.encoder() != delivered.encoder() {
        return Err("the decode-restricted alternate changed the encoder".to_owned());
    }
    if alternate.plan_digest() == delivered.plan_digest() {
        return Ok(None);
    }
    let mut retry_opts = opts.clone();
    retry_opts.pipeline = alternate.options().pipeline;
    if retry_opts.pipeline.output_grade() != opts.pipeline.output_grade() {
        return Err("the decode-restricted alternate changed the frozen color contract".to_owned());
    }
    Ok(Some(retry_opts))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OfflineRecoveryState {
    Primary,
    Pending,
    RehomePending,
    Alternate,
}

impl OfflineRecoveryState {
    pub(super) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "primary" => Ok(Self::Primary),
            "recovery_pending" => Ok(Self::Pending),
            "rehome_pending" => Ok(Self::RehomePending),
            "alternate" => Ok(Self::Alternate),
            value => Err(format!("unknown offline decoder recovery state {value}")),
        }
    }
}

impl PrepublicationTranscodeRetry {
    /// The alternate a decode fault asks for: the same delivery, decoded in
    /// software — or `None` when there is no such thing to install.
    ///
    /// Deliberately *not* [`Self::prepare`]. That one answers a different
    /// question — "this encode route failed, what is the one-step colour-safe
    /// fallback" — and it answers it by changing the pipeline or the encoder.
    /// A decode fault says nothing about the encoder. The picture that reached
    /// the viewer's screen was fine when it arrived; what failed was reading
    /// the source, and the fix is to read it differently.
    ///
    /// **The pipeline is taken from the alternate plan, not from the delivered
    /// options, and that is the whole correctness of this function.** Forcing
    /// software decode can rewrite the renderer: a vendor-native graph accepts
    /// only its own decoder, so `resolve_transcode` moves `VppQsv` and
    /// `TonemapVaapi` to their colour-preserving software renderer on the way
    /// past. Copying the delivered pipeline instead would disagree with the
    /// plan and `build`'s guard would refuse the alternate — on exactly the two
    /// pipelines that decode on the GPU, which is the population that produces
    /// hardware decode faults in the first place. A first draft did precisely
    /// that and its test did not catch it, because the fixture was already on
    /// `Pipeline::Cpu`, where nothing is rewritten.
    ///
    /// Reading the pipeline off the plan rather than re-deriving the rule also
    /// keeps one authority: the rule lives in `resolve_transcode` and is
    /// private to it, and a copy here would drift the first time a renderer is
    /// added.
    ///
    /// `Ok(None)` for the two cases where an alternate is not a thing that
    /// exists, both of which would otherwise install a producer that cannot
    /// help:
    ///
    /// * a software encoder has no hardware slot to keep, so there is no mixed
    ///   transition to make and nothing this function returns would be true of
    ///   the recipe `build` produced;
    /// * a delivered plan that already decoded in software has no alternate —
    ///   the digests are equal, so the "recovery" would tear down the failed
    ///   child and respawn the identical command.
    /// * both the failed path and its same-codec software successor must use
    ///   the receipt-qualified identity. A qualified fault is not permission
    ///   to install a producer whose output cannot carry qualified evidence.
    ///
    /// `software_threads` is `None`, which is what makes this the mixed
    /// transition rather than a demotion. The encoder and its hardware slot
    /// stay; what grows is the CPU reservation, by the difference the executor
    /// takes from `cpu_total`. Returning `Some` here would hand the hardware
    /// slot back and leave a live hardware encoder with nothing reserved for
    /// it.
    ///
    pub(super) fn prepare_decode_restricted(
        delivered: &ResolvedTranscode,
        alternate: &ResolvedTranscode,
        opts: &TranscodeOptions,
        encoder: Encoder,
    ) -> Result<Option<PreparedTranscodeRetry>, String> {
        if encoder == Encoder::Software {
            return Ok(None);
        }
        Ok(
            decode_restricted_options(delivered, alternate, opts)?.map(|retry_opts| {
                PreparedTranscodeRetry {
                    opts: retry_opts,
                    encoder,
                    software_threads: None,
                }
            }),
        )
    }

    pub(super) fn prepare(
        file: &plurx_core::domain::MediaFile,
        opts: &TranscodeOptions,
        encoder: Encoder,
        software_rate_control: EffectiveRateControl,
    ) -> Result<PreparedTranscodeRetry, String> {
        let mut retry_opts = opts.clone();
        let retry_encoder = if opts.pipeline.on_gpu() {
            retry_opts.pipeline = opts.pipeline.fallback().ok_or_else(|| {
                format!(
                    "pipeline {} has no color-safe one-step retry",
                    opts.pipeline.name()
                )
            })?;
            encoder
        } else {
            retry_opts.effective_rate_control = software_rate_control;
            Encoder::Software
        };
        let software_threads = (retry_encoder == Encoder::Software)
            .then(|| Workload::of(file, opts.target_height).software_threads());

        if retry_opts.target_height != opts.target_height
            || retry_opts.pipeline.output_grade() != opts.pipeline.output_grade()
            || retry_opts.audio_index != opts.audio_index
            || retry_opts.subtitle_burn != opts.subtitle_burn
        {
            return Err(
                "one-step retry changed the frozen height, track, subtitle, or color contract"
                    .to_owned(),
            );
        }
        retry_opts.software_threads = software_threads.map(|threads| threads as u32);
        Ok(PreparedTranscodeRetry {
            opts: retry_opts,
            encoder: retry_encoder,
            software_threads,
        })
    }

    // Keep the frozen source, retry policy, resolved decoder plan, and
    // detached admission ownership separate: each belongs to a different
    // immutable snapshot.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build(
        file: &plurx_core::domain::MediaFile,
        prepared: PreparedTranscodeRetry,
        plan: &ResolvedTranscode,
        pacing: Pacing,
        // The upload base this recipe's muxer writes through: a lane of the
        // session's `scratch_put` endpoint, never a bare directory.
        output: &str,
        presentation_contract_fingerprint: &str,
        software_pool: crate::admission::SwPool,
        software_budget: usize,
        runtime_cache: PathBuf,
        measured_decoders: &plurx_core::transcode::decoder_inventory::MeasuredDecoders,
        automatic_recovery_enabled: bool,
        // What this recipe is for, in the operator-facing identity. `build` is
        // shared by the colour-safe retry and the software-decode alternate,
        // and the identity is `pub` on a serialized type — so labelling both
        // `one-step-color-safe` would name the alternate for the thing it is
        // explicitly not, and contradict the reason the decision puts beside
        // it.
        kind: &'static str,
    ) -> Result<Self, String> {
        let PreparedTranscodeRetry {
            opts: retry_opts,
            encoder: retry_encoder,
            software_threads,
        } = prepared;
        // Encoder and pipeline alone are not "this plan is for this retry".
        // They happen to catch the plan of the route being replaced, because
        // `prepare` always changes one of them — but a plan for a different
        // title, height, audio track or rate control would pass, and the retry
        // publishes under that plan's key. Check the whole prepared request.
        if plan.encoder() != retry_encoder
            || plan.options().pipeline != retry_opts.pipeline
            || plan.options().target_height != retry_opts.target_height
            || plan.options().audio_index != retry_opts.audio_index
            || plan.options().effective_rate_control != retry_opts.effective_rate_control
            || plan.cache_identity() != &DecodeCacheIdentity::from_media_file(file)
        {
            return Err("one-step retry plan disagrees with its prepared encode route".to_owned());
        }
        let observation =
            DiagnosticObservation::for_plan(plan, measured_decoders, automatic_recovery_enabled);
        let execution = TranscodeExecution::from_options(file, &retry_opts, pacing, output)
            .map_err(|error| error.to_string())?
            .observing_qualified_grammar(observation.qualified_logging());
        let args = transcode::hls_args(plan, &execution);
        let fingerprint_body = serde_json::json!({
            "version": 2,
            "args": &args,
            "plan": plan.plan_digest(),
            "encoder": retry_encoder.label(),
            "pipeline": retry_opts.pipeline.name(),
            "presentation_contract_fingerprint": presentation_contract_fingerprint,
        });
        let fingerprint = hex::encode(Sha256::digest(fingerprint_body.to_string().as_bytes()));
        let identity = format!(
            "{kind}:{}:{}",
            retry_encoder.label(),
            retry_opts.pipeline.name()
        );
        let startup_kind = if retry_encoder == Encoder::Software {
            crate::playback_control::ProducerStartupKind::Software
        } else if plan.decode().backend() == plurx_core::transcode::DecodeBackend::Software {
            crate::playback_control::ProducerStartupKind::MixedSoftwareDecode
        } else {
            crate::playback_control::ProducerStartupKind::Hardware
        };
        Ok(Self {
            actor_recipe: crate::playback_control::ValidatedRetryRecipe::new(
                identity,
                fingerprint,
                presentation_contract_fingerprint.to_owned(),
                startup_kind,
            ),
            args: args.into(),
            encoder: retry_encoder,
            pipeline: retry_opts.pipeline,
            file: file.clone(),
            target_height: retry_opts.target_height,
            software_threads,
            // Read off the resolved retry plan, not off the encoder's name:
            // the mixed cost is a property of the pipeline this retry will
            // actually run, and the estimate is the one type that knows it.
            cpu_total: (retry_encoder != Encoder::Software).then(|| {
                crate::admission::TranscodeResourceEstimate::of(
                    plan,
                    &Workload::of(file, retry_opts.target_height),
                )
                .cpu_threads
            }),
            software_budget,
            software_pool,
            runtime_cache,
            observation,
            decode_alternate: None,
            recovery: None,
        })
    }

    /// Freeze the software-decode alternate beside this recipe.
    ///
    /// A builder rather than a tenth argument to [`Self::build`], which
    /// already carries `#[allow(clippy::too_many_arguments)]` and whose
    /// argument list is the thing that attribute is apologising for.
    pub(super) fn with_decode_alternate(
        mut self,
        alternate: Option<PrepublicationTranscodeRetry>,
    ) -> Self {
        self.decode_alternate = alternate.map(Box::new);
        self
    }
    /// Freeze the durable recovery budget onto this alternate.
    ///
    /// Separate from [`Self::with_decode_alternate`] and applied to the
    /// alternate rather than to the recipe that holds it, because the budget
    /// belongs to the recipe that spends it. Threading it through the holder
    /// would put a ledger on the colour-safe retry, where the next reader
    /// would reasonably conclude that an ordinary retry spends the recovery.
    pub(super) fn with_recovery(mut self, recovery: DecodeRecoveryReservation) -> Self {
        self.recovery = Some(Box::new(recovery));
        self
    }
}

impl PrepublicationCopyRetry {
    fn build(
        args: Vec<String>,
        presentation_contract_fingerprint: &str,
        runtime_cache: PathBuf,
    ) -> Self {
        let fingerprint_body = serde_json::json!({
            "version": 1,
            "args": &args,
            "encoder": "copy",
            "pipeline": "ffmpeg-hls-muxer",
            "presentation_contract_fingerprint": presentation_contract_fingerprint,
        });
        let fingerprint = hex::encode(Sha256::digest(fingerprint_body.to_string().as_bytes()));
        Self {
            actor_recipe: crate::playback_control::ValidatedRetryRecipe::new(
                "copy-direct-hls-muxer".to_owned(),
                fingerprint,
                presentation_contract_fingerprint.to_owned(),
                crate::playback_control::ProducerStartupKind::Copy,
            )
            .with_exit_classifier(crate::playback_control::ProducerExitClassifier::Immediate),
            args: args.into(),
            runtime_cache,
        }
    }
}

/// Freeze the native-HLS successor that the actor may select after the copy
/// reader reports a structurally unsupported input.
///
/// The direct muxer accepts the same parameter-set promotion filter as the
/// initial pipe. Dolby Vision conversion remains excluded because that route
/// depends on Plurx's in-process RPU rewrite after muxing.
pub(super) fn build_prepublication_copy_retry(
    segmenting: bool,
    video_options: transcode::CopyVideoOptions,
    args: Vec<String>,
    presentation_contract_fingerprint: &str,
    runtime_cache: PathBuf,
) -> Option<PrepublicationCopyRetry> {
    (segmenting && !video_options.converts_dolby_vision()).then(|| {
        PrepublicationCopyRetry::build(args, presentation_contract_fingerprint, runtime_cache)
    })
}

pub(super) async fn terminate_exact_prepublication_child(
    session: &Session,
    producer_attempt: u64,
) -> Result<(), String> {
    let mut slot = session.child.lock().await;
    let child = slot
        .as_mut()
        .ok_or_else(|| format!("producer attempt {producer_attempt} had no installed child"))?;
    if child.producer_attempt != producer_attempt {
        return Err(format!(
            "actor selected producer attempt {producer_attempt}, but child slot contains {}",
            child.producer_attempt
        ));
    }
    child
        .request_termination()
        .map_err(|error| format!("terminating producer attempt {producer_attempt}: {error}"))?;
    tokio::time::timeout(
        PREPUBLICATION_REAP_ATTEMPT_TIMEOUT,
        child.wait_for_terminal(),
    )
    .await
    .map_err(|_| {
        format!(
            "producer attempt {producer_attempt} reap exceeded {:?}",
            PREPUBLICATION_REAP_ATTEMPT_TIMEOUT
        )
    })?
    .map_err(|error| format!("reaping producer attempt {producer_attempt}: {error}"))?;
    if child
        .try_wait_observed(&session.control)
        .map_err(|error| format!("confirming producer attempt {producer_attempt} reap: {error}"))?
        .is_none()
    {
        return Err(format!(
            "producer attempt {producer_attempt} termination returned before confirmed reap"
        ));
    }
    // The diagnostics are deliberately left alone here. The child's supervisor
    // owns them: it is the one task that knows how the process actually ended,
    // so it derives the disposition rather than assuming one, and it holds no
    // session lock while it settles. Taking them here would be a race for the
    // same handle — whoever won would decide whether the attempt got a receipt
    // at all — and two of this function's callers hold `child_transition`
    // across the call, whose own comment forbids sleeping under it.
    //
    // The child is confirmed reaped by this point, so the supervisor's own
    // `wait` has already returned and its settle is imminent.
    *slot = None;
    Ok(())
}

/// Log what one attempt's observation concluded.
///
/// M3b owns the reading; M3b2 is what carries the fault to the actor and M3c
/// is what lets a receipt refuse a cache artifact. Until then this is the
/// visible half: an operator can see that a producer which exited cleanly was
/// failing to decode the whole time, which before this could not be seen at
/// all.
pub(super) fn report_producer_health(
    attempt: &str,
    receipt: &crate::decoder_health::ProducerHealthReceipt,
) {
    let fault = receipt
        .terminal_fault
        .map(crate::decoder_health::DecodeFaultKind::name);
    if fault.is_some() || receipt.video_decode_error_records > 0 {
        tracing::warn!(
            attempt,
            plan = %receipt.plan_digest,
            qualification = receipt.qualification.name(),
            exit = receipt.exit_disposition.name(),
            decode_errors = receipt.video_decode_error_records,
            contract_qualified_errors = receipt.contract_qualified_error_records,
            fault = fault.unwrap_or("none"),
            contract = receipt.diagnostic_contract.as_deref().unwrap_or("none"),
            "producer decode diagnostics"
        );
        return;
    }
    tracing::debug!(
        attempt,
        plan = %receipt.plan_digest,
        qualification = receipt.qualification.name(),
        exit = receipt.exit_disposition.name(),
        observation_complete = receipt.observation_complete,
        contract = receipt.diagnostic_contract.as_deref().unwrap_or("none"),
        "producer decode diagnostics"
    );
}

pub(super) async fn terminate_current_prepublication_child(
    session: &Session,
) -> Result<(), String> {
    let producer_attempt = {
        let slot = session.child.lock().await;
        slot.as_ref().map(|child| child.producer_attempt)
    };
    let Some(producer_attempt) = producer_attempt else {
        return Ok(());
    };
    terminate_exact_prepublication_child(session, producer_attempt).await
}
