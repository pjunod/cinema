use super::*;

/// Spawn a real generation positioned at plan entry `at` and hand its stdout
/// to [`run_generation`].
pub(super) async fn spawn_generation(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    at: u32,
    permit: Option<crate::vodencode::EncodePermit>,
) {
    if !recipe_engine_is_current(&rendition.recipe).await {
        record_failure(
            shared,
            rendition,
            crate::playback_control::ProducerDecisionReason::EngineChanged,
            "the immutable media engine changed; restart is required".to_owned(),
        );
        return;
    }
    // A spawn position must be a VIDEO entry — the scheduler can name an
    // audio-tail index (a blocked GET on the tail is real demand), but the
    // generation that serves it starts at the last video boundary and its
    // `finish` produces the tail entries.
    let at = video_entry_at_or_before(&rendition.plan, at);
    let Some(entry) = rendition.plan.entry(at) else {
        tracing::warn!(
            target: "plurxd::vodserve",
            rendition = %rendition.key, "no plan entry {at} to spawn at"
        );
        return;
    };
    let start_seconds = entry.start_ticks as f64 / f64::from(rendition.timescale);
    let recipe = &rendition.recipe;
    debug_assert_eq!(recipe.encoding.is_some(), permit.is_some());
    let attested = attested_source_setup(rendition);
    let audio_source = match reopen_encoded_audio(rendition.source.as_ref(), recipe).await {
        Ok(source) => source,
        Err(cause) => {
            record_failure(
                shared,
                rendition,
                crate::playback_control::ProducerDecisionReason::SourceChanged,
                cause,
            );
            return;
        }
    };
    // One ffmpeg, converting or not. The conversion happens on the far side of
    // the muxer now — `dvpipe` rewrites the RPUs inside the fragments this
    // process writes — so the producer is the producer it always was.
    let (mut child, child_job, stdout, stderr) = {
        let args = recipe_pipe_args(recipe, start_seconds, attested);
        #[cfg(unix)]
        let descriptors = crate::producer_spawn::Descriptors::from_files(
            rendition.source.as_ref().map(|source| &source.handle),
            audio_source.as_ref().map(|audio| &audio.handle),
            recipe
                .encoding
                .as_ref()
                .and_then(|encoding| encoding.subtitle.as_deref()),
            false,
        );
        #[cfg(windows)]
        let descriptors = crate::producer_spawn::Descriptors::default();
        let program = recipe_program(recipe);
        let spawned = match crate::producer_spawn::spawn(
            &program,
            &args,
            crate::producer_spawn::SpawnOptions {
                runtime_cache: &shared.runtime_cache,
                progress: crate::producer_spawn::Progress::None,
                descriptors,
                env: &[],
            },
        ) {
            Ok(spawned) => spawned,
            Err(error) => {
                let cause = format!("spawning the producer: {error}");
                record_failure(
                    shared,
                    rendition,
                    crate::playback_control::ProducerDecisionReason::ProducerLaunchFailed,
                    cause,
                );
                return;
            }
        };
        (
            spawned.child,
            spawned.child_job,
            spawned.stdout,
            spawned.stderr,
        )
    };
    // The rendition can be closed between the spawn above and the attach
    // below (a purge committing on the maintain task). Attaching would leave
    // a live ffmpeg in a slot whose driver has already exited — a child
    // nothing reaps until the Arc drops.
    //
    // A failure recorded in that same gap is the other half of the same
    // hazard, and it was not guarded. The driver does not exit on a failure,
    // it switches to reclaiming, and a reclaiming pass that has already read
    // an absent belief will not look again until something kicks it — so an
    // attach landing just behind it puts a live child in a slot whose only
    // remaining reader answers `ProducerFailed`. Refuse the attach instead,
    // here, where the child is still ours to kill.
    if rendition.closed.load(Relaxed) || rendition.failure().is_some() {
        let _ = child.kill().await;
        return;
    }
    rendition
        .last_child_pid
        .store(child.id().unwrap_or(0), Relaxed);
    rendition
        .slot
        .attach_job_owned(
            child,
            child_job,
            at,
            permit.map(|permit| Box::new(permit) as Box<dyn Send>),
        )
        .await;
    shared.pool.metrics_handle().count_producer_generation(
        if rendition.recipe.encoding.is_some() {
            VodProducerKind::Encoded
        } else {
            VodProducerKind::Copy
        },
    );
    let epoch = rendition.gen_epoch.load(Relaxed);
    let shared = Arc::clone(shared);
    let rendition = Arc::clone(rendition);
    tokio::spawn(async move {
        let key = rendition.key.clone();
        let (_, diagnostic) = tokio::join!(
            run_generation(shared, rendition, stdout, at, epoch),
            crate::ffmpeg::drain_diagnostics(stderr),
        );
        if !diagnostic.trim().is_empty() {
            tracing::warn!(target: "plurxd::vodserve", rendition = %key, generation = epoch, %diagnostic, "VOD producer diagnostic");
        }
    });
    tracing::info!(
        target: "plurxd::vodserve",
        rendition = %rendition_key_field(at), "spawned a producer generation"
    );
}

pub(super) async fn recipe_engine_is_current(recipe: &Recipe) -> bool {
    if let Some(encoding) = recipe.encoding.as_ref() {
        // One blocking batch for the whole encoding attestation. The
        // executable used to be statted inline here, ahead of the batch and
        // short-circuiting it, so every producer launch and every segment
        // materialisation paid a synchronous `metadata` on a runtime worker.
        if !encoding
            .engine
            .is_current_with_executable(&encoding.executable)
            .await
        {
            return false;
        }
    }
    recipe.cluster_cache_key.is_none() || crate::ffmpeg::fragment_index_engine_is_current().await
}

pub(super) fn recipe_program(recipe: &Recipe) -> std::path::PathBuf {
    recipe.encoding.as_ref().map_or_else(
        || ffmpeg_bin().into(),
        |encoding| encoding.executable.path.clone(),
    )
}

pub(super) fn recipe_pipe_args(recipe: &Recipe, start_seconds: f64, attested: bool) -> Vec<String> {
    if let Some(encoding) = &recipe.encoding {
        let mut file = recipe.file.clone();
        if attested {
            file.path = "/dev/fd/3".into();
        }
        // Plan duration is rounded to a complete output frame, just like the
        // terminal -t. Neither audio padding nor the source's final VFR gap
        // may turn a short final entry into an unplanned audio-only tail.
        let plan = encoding.grid.plan(file.duration_ms.unwrap_or(0), 0);
        let end = plan
            .entries
            .last()
            .map_or(0, |entry| entry.start_ticks + entry.duration_ticks);
        let mut args = encoding.args(&file, start_seconds, end as f64 / f64::from(plan.timescale));
        if attested && !file.audio_streams.is_empty() {
            let mut inputs = 0;
            for index in 0..args.len().saturating_sub(1) {
                if args[index] == "-i" {
                    inputs += 1;
                    if inputs == 2 {
                        args[index + 1] = "/dev/fd/4".into();
                        break;
                    }
                }
            }
        }
        args
    } else {
        let mut args = copy_pipe_args_with_dolby_vision(
            &recipe.file,
            start_seconds,
            recipe.audio_index,
            recipe.aac,
            Pacing::unpaced(),
            recipe.video,
        );
        if attested {
            replace_inputs_with_attested_descriptor(&mut args);
        }
        args
    }
}

pub(super) async fn reopen_encoded_audio(
    source: Option<&crate::fragment_index_cluster::SourceFence>,
    recipe: &Recipe,
) -> Result<Option<crate::fragment_index_cluster::SourceFence>, String> {
    if recipe.encoding.is_some() && !recipe.file.audio_streams.is_empty() {
        if let Some(source) = source {
            return source.reopen(&recipe.file).await.map(Some);
        }
    }
    Ok(None)
}

/// Whether this rendition's producers read the source through the attested
/// file descriptor rather than by name.
///
/// The attestation is what makes a session's video and its audio provably the
/// same bytes: a path can be replaced between two opens, a descriptor cannot.
fn attested_source_setup(rendition: &Rendition) -> bool {
    cfg!(unix) && rendition.source.is_some()
}

/// Point every `-i` at the inherited descriptor.
///
/// Every input in a copy argv is the same source — the video, and the second
/// open a copied audio track's A/V correction needs — so they all become fd 3.
#[allow(clippy::needless_range_loop)]
fn replace_inputs_with_attested_descriptor(args: &mut [String]) {
    for index in 0..args.len().saturating_sub(1) {
        if args[index] == "-i" {
            args[index + 1] = "/dev/fd/3".to_owned();
        }
    }
}

/// The video pipeline one copy session runs, from what that session asked for.
///
/// The conversion rides **in** the options rather than beside them, so it
/// reaches `copy_video_args` and therefore the argv fingerprint and the
/// fragment-index identity. A converted stream has different bytes and
/// different segment boundaries; sharing an identity with the unconverted one
/// would hand a session a playlist whose cut points describe different media,
/// and the session would fail its landing on every fragment.
///
/// Every later reader — the rendition's generation, its playlist facts, its
/// index pass — asks these options rather than carrying a second copy of the
/// answer, which is why this is the one place the two flags meet.
pub(super) fn copy_video_pipeline(
    file: &MediaFile,
    probe_json: Option<&str>,
    have_dovi: bool,
    preserve_dolby_vision: bool,
    convert_dolby_vision: bool,
) -> CopyVideoOptions {
    CopyVideoOptions::from_probe(file, probe_json, have_dovi, preserve_dolby_vision)
        .with_dolby_vision_conversion(convert_dolby_vision)
}

fn rendition_key_field(at: u32) -> String {
    format!("generation@{at}")
}

/// Read one generation to its end: establish or verify the init identity,
/// then let [`vodgen::run`] cut the plan's entries into the sink.
async fn run_generation(
    shared: Arc<Shared>,
    rendition: Arc<Rendition>,
    stdout: tokio::process::ChildStdout,
    at: u32,
    epoch: u64,
) {
    let mut stdout = stdout;
    let need_pre_read = {
        let identity = rendition.identity.lock().await;
        identity.identity.is_none() || !rendition.dir.has_init().await
    };
    let src: Box<dyn AsyncRead + Send + Unpin> = if need_pre_read {
        // The generation's own pipe carries the muxer init; read up to it,
        // establish or verify, write the served init — and then replay the
        // consumed bytes in front of the live pipe so vodgen sees the whole
        // stream (it verifies the init itself before a single write).
        match read_muxer_init(&mut stdout).await {
            Err(error) => {
                on_generation_end(
                    &shared,
                    &rendition,
                    Outcome::Failed(Failure::Stream(format!(
                        "reading the generation's init: {error}"
                    ))),
                    epoch,
                )
                .await;
                return;
            }
            Ok((consumed, muxer)) => {
                if !establish_or_verify(&shared, &rendition, &muxer, epoch).await {
                    return;
                }
                Box::new(std::io::Cursor::new(consumed).chain(stdout))
            }
        }
    } else {
        Box::new(stdout)
    };
    let identity = {
        let state = rendition.identity.lock().await;
        match &state.identity {
            Some(identity) => identity.clone(),
            // The pre-read established it, or the rendition already had it;
            // reaching here without one is the pre-read having purged and
            // bailed, which returns above.
            None => return,
        }
    };
    let generation = Generation {
        plan: rendition.plan.clone(),
        index: rendition.index.clone(),
        encoded_audio_anchor: rendition.recipe.encoding.as_ref().map(|_| {
            let start = rendition.plan.entry(at).expect("spawn entry").start_ticks as f64
                / f64::from(rendition.timescale);
            plurx_core::transcode::vod_audio_anchor(start)
        }),
        identity,
        start_entry: at,
        policy: rendition.policy,
        // The recipe's own answer, which is also the answer the index this
        // generation is matched against was built with — they share one
        // `CopyVideoOptions`, so they cannot disagree.
        convert_dolby_vision: rendition.recipe.video.converts_dolby_vision(),
    };
    let sink = RenditionSink {
        shared: Arc::clone(&shared),
        rendition: Arc::clone(&rendition),
        epoch,
    };
    let outcome = vodgen::run(src, generation, &sink, &rendition.key).await;
    on_generation_end(&shared, &rendition, outcome, epoch).await;
}

/// The identity half of a pre-read generation. `false` means the generation
/// is over (drift handled or failure recorded) and the caller must return.
async fn establish_or_verify(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    muxer: &Init,
    epoch: u64,
) -> bool {
    if !recipe_engine_is_current(&rendition.recipe).await {
        on_generation_end(
            shared,
            rendition,
            Outcome::Failed(Failure::EngineChanged(
                "the immutable media engine changed before init publication".to_owned(),
            )),
            epoch,
        )
        .await;
        return false;
    }
    let served = {
        let mut state = rendition.identity.lock().await;
        match &state.identity {
            Some(identity) => match identity.served_init_for(muxer) {
                Ok(served) => served,
                Err(refused) => {
                    if matches!(refused, InitRefused::PromotionDrift { .. }) {
                        tracing::error!(
                            target: "plurxd::vodserve",
                            rendition = %rendition.key,
                            "promotion is no longer a pure function of stored inputs: {refused}"
                        );
                    }
                    drop(state);
                    on_generation_end(
                        shared,
                        rendition,
                        Outcome::Failed(Failure::InitDrift(refused.to_string())),
                        epoch,
                    )
                    .await;
                    return false;
                }
            },
            None => {
                let identity = match InitIdentity::establish(
                    muxer,
                    rendition
                        .index
                        .as_ref()
                        .map(|index| index.promotion.clone())
                        .unwrap_or_default(),
                ) {
                    Ok(identity) => identity,
                    Err(error) => {
                        drop(state);
                        on_generation_end(
                            shared,
                            rendition,
                            Outcome::Failed(Failure::Stream(format!(
                                "establishing the init identity: {error}"
                            ))),
                            epoch,
                        )
                        .await;
                        return false;
                    }
                };
                let served = identity
                    .served_init_for(muxer)
                    .expect("an identity just established from this muxer init serves it");
                if let Err(error) = store_identity(&rendition.identity_path(), &identity).await {
                    tracing::warn!(
                        target: "plurxd::vodserve",
                        rendition = %rendition.key,
                        "persisting identity.json: {error}"
                    );
                }
                *state = IdentityState {
                    identity: Some(identity),
                    from_disk: false,
                };
                served
            }
        }
    };
    if let Err(error) = rendition.dir.write_init(&served.bytes).await {
        on_generation_end(
            shared,
            rendition,
            Outcome::Failed(Failure::Sink(error)),
            epoch,
        )
        .await;
        return false;
    }
    rendition.clear_demand(INIT_DEMAND_INDEX);
    rendition.init_notify.notify_waiters();
    true
}

/// What a generation's ending means for the rendition.
async fn on_generation_end(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    outcome: Outcome,
    epoch: u64,
) {
    if rendition.closed.load(Relaxed) {
        return;
    }
    if rendition.gen_epoch.load(Relaxed) != epoch {
        // The driver already killed or replaced this generation on purpose;
        // its ending carries no verdict.
        return;
    }
    let _ = rendition.marker_prewarm_generation.compare_exchange(
        epoch.saturating_add(1),
        0,
        AcqRel,
        Acquire,
    );
    // Reap the child so the belief goes honestly absent, keeping its progress.
    let _ = perform_driver_step(
        shared,
        rendition,
        Step::Terminate {
            why: Termination::Idle,
        },
    )
    .await;
    match outcome {
        Outcome::Failed(Failure::InitDrift(cause)) => {
            on_init_drift(shared, rendition, cause).await;
        }
        Outcome::Failed(failure) => {
            record_failure(
                shared,
                rendition,
                classify_failure(&failure),
                describe_failure(&failure),
            );
        }
        Outcome::Ran { produced_through } => {
            let last = rendition.plan.len().saturating_sub(1) as u32;
            let finished = produced_through == Some(last) || {
                let manifest = rendition.manifest.lock().await;
                !manifest.is_empty() && manifest.next_gap(0).is_none()
            };
            if !finished && shared.pool.blocked_on(&rendition.key).is_some() {
                // The producer died on its own with somebody mid-wait: the
                // typed refusal is the truth (plan §2.3 outcome 3).
                record_failure(
                    shared,
                    rendition,
                    crate::playback_control::ProducerDecisionReason::PartialSuccessExit,
                    format!("producer exited early (produced through {produced_through:?})"),
                );
            } else {
                tracing::debug!(
                    target: "plurxd::vodserve",
                    rendition = %rendition.key,
                    "generation ended (produced through {produced_through:?})"
                );
            }
        }
    }
    rendition.kick();
}

/// The §5 mismatch arm: an adopted identity that this pipeline no longer
/// reproduces, with nothing admitted, purges to planned-only and establishes
/// fresh on the next spawn. Anything else is real drift and fails typed.
pub(super) async fn on_init_drift(shared: &Arc<Shared>, rendition: &Arc<Rendition>, cause: String) {
    let from_disk = rendition.identity.lock().await.from_disk;
    let admitted = rendition.manifest.lock().await.is_admitted();
    if from_disk && !admitted {
        {
            let mut manifest = rendition.manifest.lock().await;
            let freed = rendition.dir.purge(&mut manifest).await;
            sub_saturating(&shared.working_set, freed.bytes);
            if let Some(error) = freed.error {
                tracing::warn!(
                    target: "plurxd::vodserve",
                    rendition = %rendition.key,
                    "purging after adopted-identity drift: {error}"
                );
            }
        }
        let _ = tokio::fs::remove_file(rendition.identity_path()).await;
        *rendition.identity.lock().await = IdentityState::default();
        tracing::info!(
            target: "plurxd::vodserve",
            rendition = %rendition.key,
            "adopted identity no longer matches this pipeline; purged to \
             planned-only to establish fresh: {cause}"
        );
        return;
    }
    // The mismatch is local to this immutable rendition. It refuses further
    // publication under the playlist identity the client already holds, but
    // it is not evidence that the process-wide engine baseline moved.
    record_failure(
        shared,
        rendition,
        crate::playback_control::ProducerDecisionReason::RenditionInitChanged,
        cause,
    );
}

pub(super) fn record_failure(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    decision: crate::playback_control::ProducerDecisionReason,
    cause: String,
) {
    tracing::warn!(
        target: "plurxd::vodserve",
        rendition = %rendition.key,
        decision = decision.status(),
        "producer failed: {cause}"
    );
    {
        // First failure wins. A publication fence records the cause — the
        // source moved, the engine moved — and the generation it stops then
        // ends with a *consequence*: the sink refused the bytes, the pipe
        // closed. Overwriting would file the consequence as the diagnosis and
        // publish a class that names the wrong subsystem.
        let mut failed = rendition.failed.lock().expect("failed lock");
        if failed.is_none() {
            *failed = Some(RenditionFailure {
                decision,
                cause: cause.clone(),
            });
        }
    }
    shared.pool.fail(&rendition.key, &cause);
    // Init waiters block on their own Notify, not the wait pool — without
    // this, a GET waiting for `init.mp4` sleeps its whole budget to learn
    // what every segment waiter was told immediately.
    rendition.init_notify.notify_waiters();
    // And wake the driver, whose only remaining job for this rendition is to
    // reclaim the producer. Without it the first reclaiming pass waits for the
    // next maintenance tick's `kick_all`, so a failure recorded a moment after
    // one tick leaves a doomed ffmpeg holding a codec session for the whole
    // interval. This is a `notify_one` on the rendition's own wake, so it
    // cannot outlive the driver task or wake anything else.
    rendition.kick();
}

/// One recorded rendition failure: what a client can act on, and what an
/// operator reads.
#[derive(Debug, Clone)]
pub(super) struct RenditionFailure {
    /// The bounded class, in the same vocabulary rolling delivery publishes.
    /// This is what becomes `DeliveryView::producer_decision`, and through it
    /// a `terminal` or `retry_resource` action the clients already handle.
    pub(super) decision: crate::playback_control::ProducerDecisionReason,
    /// The sentence. Never parsed, only shown.
    pub(super) cause: String,
}

/// Classify one generation outcome.
///
/// `Stream` is genuinely the reader failing on the producer's output, which is
/// what `ReaderFailed` already names on the rolling side; the other two have
/// no rolling equivalent, because nothing rolling lands fragments at planned
/// film times or writes through an immutable sink.
pub(super) fn classify_failure(
    failure: &Failure,
) -> crate::playback_control::ProducerDecisionReason {
    use crate::playback_control::ProducerDecisionReason as Reason;
    match failure {
        // Handled before this point by `on_init_drift`; classified here so the
        // match stays exhaustive rather than defaulting a new variant, and
        // with the same local class that path records.
        Failure::InitDrift(_) => Reason::RenditionInitChanged,
        Failure::EngineChanged(_) => Reason::EngineChanged,
        Failure::Landing(_) => Reason::MediaLandingFailed,
        Failure::Stream(_) => Reason::ReaderFailed,
        Failure::Sink(_) => Reason::ProducerWriteFailed,
    }
}

pub(super) fn describe_failure(failure: &Failure) -> String {
    match failure {
        Failure::InitDrift(why) => format!("init drift: {why}"),
        Failure::EngineChanged(why) => format!("engine changed: {why}"),
        Failure::Landing(why) => format!("landing failed: {why}"),
        Failure::Stream(why) => format!("stream failed: {why}"),
        Failure::Sink(error) => format!("sink refused: {error}"),
    }
}

/// The film-indexed sink one generation writes through: bytes to the
/// directory under the manifest lock, the node-wide counter, the wait pool,
/// the slot's progress, and the driver's wake — in that order.
pub(super) struct RenditionSink {
    pub(super) shared: Arc<Shared>,
    pub(super) rendition: Arc<Rendition>,
    /// The generation epoch this sink was built for. A write from a stale
    /// epoch — a killed generation's queued materialize landing after a
    /// Restart — is refused under the manifest lock: letting it through would
    /// advance the NEW producer's belief with the OLD generation's progress
    /// (one spurious kill of the healthy replacement) and, worse, let a dead
    /// generation keep writing bytes under the replacement's feet.
    pub(super) epoch: u64,
}

/// Assign the successful publication a monotonic identity and, only when the
/// scheduler currently attributes work to marker prewarm, credit that exact
/// identity to the active playback ledgers. The caller holds `manifest`, so a
/// landing observation cannot interleave between publication and provenance.
pub(super) fn clear_marker_prewarm_dispatch(rendition: &Rendition) {
    *rendition
        .marker_prewarm_dispatch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    rendition.active_marker_prewarms.store(0, Release);
}

pub(super) async fn credit_marker_prewarm_publication(
    rendition: &Rendition,
    producer_epoch: u64,
    entry: u32,
) -> u64 {
    let publication = rendition
        .publication_serial
        .fetch_add(1, Relaxed)
        .saturating_add(1);
    if let Some(version) = rendition
        .publication_versions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_mut(entry as usize)
    {
        *version = Some(publication);
    }
    if rendition.active_marker_prewarms.load(Acquire) == 0 {
        return publication;
    }
    let attribution = {
        let mut dispatch = rendition
            .marker_prewarm_dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let attribution = dispatch.as_ref().and_then(|dispatch| {
            (dispatch.producer_epoch == producer_epoch
                && entry >= dispatch.candidate.target_entry
                && entry <= dispatch.candidate.window_end_entry)
                .then(|| (dispatch.candidate, dispatch.owners.clone()))
        });
        if dispatch.as_ref().is_some_and(|dispatch| {
            dispatch.producer_epoch != producer_epoch
                || entry >= dispatch.candidate.window_end_entry
        }) {
            *dispatch = None;
            rendition.active_marker_prewarms.store(0, Release);
        }
        attribution
    };
    let Some((candidate, owners)) = attribution else {
        return publication;
    };
    for owner in owners {
        owner
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .credit_dispatched(candidate, owner.record_nonce, entry, publication);
    }
    publication
}

impl vodgen::Sink for RenditionSink {
    async fn materialize(&self, entry: u32, bytes: Vec<u8>) -> io::Result<()> {
        if self.rendition.closed.load(Relaxed) {
            // The quiet teardown: `NotFound` is how vodgen learns the session
            // ended normally rather than faulted.
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        // Both fences record their own class before returning. vodgen sees
        // only an `io::Error` and wraps it as `Failure::Sink`, which would
        // otherwise be classified `producer_write_failed` — a sink fault,
        // which is not what happened. `record_failure` is first-wins, so the
        // class recorded here survives the generation ending underneath it.
        if let Some(drift) = self
            .rendition
            .source
            .as_ref()
            .and_then(|source| source.drift())
        {
            // The sentence names what was observed, not what was concluded.
            // "source changed" alone was a confident claim about a thing that
            // may not have happened: the same refusal covers a rewritten
            // source, an inode whose metadata moved without its bytes, and an
            // `fstat` that failed outright. A viewer sees a refusal either
            // way; an operator now sees which one, and a failing CI lane says
            // which field moved instead of leaving it to be guessed at.
            let cause = format!("source changed before fragment publication: {drift}");
            record_failure(
                &self.shared,
                &self.rendition,
                crate::playback_control::ProducerDecisionReason::SourceChanged,
                cause.clone(),
            );
            return Err(io::Error::new(io::ErrorKind::InvalidData, cause));
        }
        if !recipe_engine_is_current(&self.rendition.recipe).await {
            let cause = "immutable media engine changed before fragment publication".to_owned();
            record_failure(
                &self.shared,
                &self.rendition,
                crate::playback_control::ProducerDecisionReason::EngineChanged,
                cause.clone(),
            );
            return Err(io::Error::new(io::ErrorKind::InvalidData, cause));
        }
        let len = bytes.len() as u64;
        {
            let mut manifest = self.rendition.manifest.lock().await;
            // Checked under the manifest lock, so a driver bumping the epoch
            // cannot interleave between the check and the write.
            if self.rendition.gen_epoch.load(Relaxed) != self.epoch {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            let before = manifest.state(entry).map(|s| s.bytes()).unwrap_or(0);
            self.rendition
                .dir
                .materialize(&mut manifest, entry, &bytes, now_ms())
                .await?;
            // Publication and provenance linearize under the same manifest
            // lock. A skip can therefore observe neither fact or both, never
            // real prewarm bytes with a missing credit.
            credit_marker_prewarm_publication(&self.rendition, self.epoch, entry).await;
            if !manifest.is_admitted() {
                sub_saturating(&self.shared.working_set, before);
                self.shared.working_set.fetch_add(len, Relaxed);
            }
            if manifest.next_gap(0).is_none() {
                self.shared.try_admit(&self.rendition, &mut manifest).await;
            }
            self.rendition.clear_demand(entry);
        }
        self.rendition.slot.produced(entry).await;
        self.shared.pool.satisfy(&self.rendition.key, entry);
        self.rendition.kick();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------
