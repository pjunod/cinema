use super::*;

/// Spawn a real generation positioned at plan entry `at` and hand its stdout
/// to [`run_generation`].
pub(super) async fn spawn_generation(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    at: u32,
    permit: Option<crate::vodencode::EncodePermit>,
) {
    if rendition.closed.load(Relaxed) || rendition.failure().is_some() {
        return;
    }
    let private_operation = match rendition.private_storage.as_ref() {
        Some(storage) => match storage.operation() {
            Some(operation) => Some(operation),
            None => return,
        },
        None => None,
    };
    let source_dispatch = match rendition.source_owners.begin_generation() {
        Ok(dispatch) => dispatch,
        Err(cause) => {
            record_failure(
                shared,
                rendition,
                crate::playback_control::ProducerDecisionReason::ProducerLaunchFailed,
                cause.to_owned(),
            );
            return;
        }
    };
    let source_authority = match source_dispatch.authorize_before_spawn().await {
        Ok(authority) => authority,
        Err(cause) => {
            record_failure(
                shared,
                rendition,
                crate::playback_control::ProducerDecisionReason::ProducerLaunchFailed,
                cause,
            );
            return;
        }
    };
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
    debug_assert_eq!(
        recipe.encoding.is_some() || rendition.key.starts_with("source-"),
        permit.is_some(),
    );
    // A family may retain this reservation through several generations, but
    // only the exact unreaped process may use it. Refuse a duplicate launch
    // before opening inputs or spawning a second producer under one credit.
    let permit = match permit {
        Some(reservation) => match rendition
            .retained_admission
            .bind(reservation)
            .try_claim_worker()
        {
            Some(worker) => Some(worker),
            None => {
                record_failure(
                    shared,
                    rendition,
                    crate::playback_control::ProducerDecisionReason::ProducerLaunchFailed,
                    "the producer reservation still belongs to an unreaped worker".to_owned(),
                );
                return;
            }
        },
        None => None,
    };
    #[cfg(target_os = "linux")]
    if let Some((encoding, runtime)) = recipe.encoding.as_ref().and_then(|encoding| {
        encoding
            .dv_runtime
            .as_ref()
            .map(|runtime| (encoding, runtime))
    }) {
        let mut report_revision = runtime.report_revision.load(Acquire);
        // Keep the last verified interval while this attachment prefetches.
        // Explicit seek/replacement/failure revoke its publication allowance.
        if let Some(authority) = &source_authority {
            if let Err(cause) = authority.validate_before_spawn() {
                runtime.refuse_episode();
                record_failure(
                    shared,
                    rendition,
                    crate::playback_control::ProducerDecisionReason::ProducerLaunchFailed,
                    cause,
                );
                return;
            }
        }
        let prepared = {
            let mut prepared = runtime.prepared.lock().expect("DV prepared window");
            if prepared.as_ref().is_some_and(|(index, _)| *index == at) {
                prepared.take().map(|(_, window)| window)
            } else {
                None
            }
        };
        let verified = if let Some(prepared) = prepared {
            // Prepared before attachment/control: a later seek must not award
            // this original interval new report authority.
            report_revision = 0;
            drop(permit);
            rendition
                .source_owners
                .registered(&source_dispatch, &prepared.registration);
            drop(source_dispatch);
            if !runtime.source.unchanged() || !runtime.tools.is_current().await {
                Err("DV source/backend changed before prepared publication".to_owned())
            } else {
                Ok(prepared)
            }
        } else {
            let Some(worker) = permit else {
                runtime.refuse_episode();
                record_failure(
                    shared,
                    rendition,
                    crate::playback_control::ProducerDecisionReason::ProducerLaunchFailed,
                    "DV graph has no retained admission".into(),
                );
                return;
            };
            let owners = Arc::clone(rendition);
            let hook = Box::new(move |registration: &crate::prodrun::ProducerRegistration| {
                owners
                    .source_owners
                    .registered(&source_dispatch, registration);
                drop(source_dispatch);
            });
            runtime
                .execute(
                    &recipe.file,
                    &encoding.plan,
                    &encoding.options,
                    encoding.grid,
                    entry,
                    &encoding.executable.path,
                    crate::dv_segment::SegmentAdmission::Retained(worker),
                    Arc::clone(&rendition.slot),
                    tokio_util::sync::CancellationToken::new(),
                    Some(hook),
                )
                .await
                .and_then(|(window, actual)| match &encoding.dv_processing {
                    plurx_core::transcode::dv_processing::DvSelection::Selected(frozen)
                        if actual.semantic_digest() == frozen.semantic_digest() =>
                    {
                        Ok(window)
                    }
                    _ => Err("completed DV graph differs from frozen rendition".into()),
                })
        };
        let verified = match verified {
            Ok(window) => window,
            Err(cause) => {
                runtime.refuse_episode();
                record_failure(
                    shared,
                    rendition,
                    crate::playback_control::ProducerDecisionReason::ProducerLaunchFailed,
                    format!("DV window refused before publication: {cause}"),
                );
                return;
            }
        };
        if let Some(authority) = &source_authority {
            if let Err(cause) = authority.validate_before_spawn() {
                runtime.refuse_episode();
                record_failure(
                    shared,
                    rendition,
                    crate::playback_control::ProducerDecisionReason::ProducerLaunchFailed,
                    cause,
                );
                return;
            }
        }
        if rendition.closed.load(Relaxed)
            || rendition.failure().is_some()
            || !recipe_engine_is_current(recipe).await
        {
            return;
        }
        shared
            .pool
            .metrics_handle()
            .count_producer_generation(VodProducerKind::Encoded);
        let epoch = rendition.gen_epoch.load(Relaxed);
        let retirement = tokio_util::sync::CancellationToken::new();
        let done = retirement.clone().drop_guard();
        let watch_rendition = Arc::clone(rendition);
        let watch_retirement = retirement.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = watch_retirement.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(20)) => {
                        if watch_rendition.closed.load(Relaxed)
                            || watch_rendition.gen_epoch.load(Relaxed) != epoch {
                            watch_retirement.cancel();
                            break;
                        }
                    }
                }
            }
        });
        let receipt = verified.receipt;
        let encoded_payloads = verified.encoded_payloads;
        let (outcome, completed) = run_generation(
            Arc::clone(shared),
            Arc::clone(rendition),
            Box::new(std::io::Cursor::new(verified.bytes)),
            at,
            epoch,
            retirement,
        )
        .await;
        let trailer_complete = completed.is_some();
        let mut publication_complete = false;
        if let Some((sink, all_fresh)) = completed {
            // Snapshot attachments before the publication lock: control takes
            // session ownership before its media/manifest fences.
            let physical_owners: Vec<_> = shared
                .sessions
                .lock()
                .await
                .values()
                .filter(|session| {
                    session.tombstone.is_none()
                        && session
                            .live_rendition()
                            .is_some_and(|current| Arc::ptr_eq(current, rendition))
                })
                .map(|session| Arc::clone(&session.incarnation))
                .collect();
            let completion_accepted = sink.completed_output_accepted().await;
            // The driver changes the epoch under this same publication lock.
            // A trailer alone cannot award evidence to a replaced generation.
            let manifest = rendition.manifest.lock().await;
            if completion_accepted
                && rendition.gen_epoch.load(Relaxed) == epoch
                && !rendition.closed.load(Relaxed)
                && rendition.failure().is_none()
                && rendition
                    .source
                    .as_ref()
                    .is_some_and(|source| source.unchanged())
                && manifest
                    .state(at)
                    .is_some_and(|state| state.is_materialized() && state.bytes() > 0)
                && matches!(&outcome, Some(Outcome::Ran { produced_through: Some(through) }) if *through >= at)
            {
                publication_complete = true;
                // Traversing immutable cached media is valid playback, but
                // those bytes are not this encoder window's evidence.
                if let (
                    true,
                    Some(receipt),
                    plurx_core::transcode::dv_processing::DvSelection::Selected(plan),
                ) = (all_fresh, receipt, &encoding.dv_processing)
                {
                    runtime.published_window(
                        receipt,
                        &encoded_payloads,
                        &plan.semantic_digest(),
                        &physical_owners,
                        report_revision,
                    );
                }
            }
        }
        if let Some(outcome) = outcome {
            if rendition.gen_epoch.load(Relaxed) == epoch
                && !rendition.closed.load(Relaxed)
                && rendition.cancelled_preparation_epoch.load(Acquire) != epoch.saturating_add(1)
                && (!trailer_complete
                    || !publication_complete
                    || matches!(&outcome, Outcome::Failed(_)))
            {
                // Exclude the episode before on_generation_end wakes clients.
                runtime.refuse_episode();
                record_failure(
                    shared,
                    rendition,
                    crate::playback_control::ProducerDecisionReason::ProducerLaunchFailed,
                    "processed window did not complete publication".to_owned(),
                );
            }
            on_generation_end(shared, rendition, outcome, epoch).await;
        }
        drop(done);
        return;
    }
    let attested = attested_source_setup(rendition);
    let source_current = if rendition.key.starts_with("source-") {
        let Some(held) = rendition
            .source
            .as_ref()
            .filter(|source| source.unchanged())
        else {
            record_failure(
                shared,
                rendition,
                crate::playback_control::ProducerDecisionReason::SourceChanged,
                "Source held descriptor changed before spawn".to_owned(),
            );
            return;
        };
        match crate::fragment_index_cluster::open_source_playback_fence(
            &recipe.file,
            Some(held.object_version()),
        )
        .await
        {
            Ok(current) => Some(current),
            Err(cause) => {
                record_failure(
                    shared,
                    rendition,
                    crate::playback_control::ProducerDecisionReason::SourceChanged,
                    cause,
                );
                return;
            }
        }
    } else {
        None
    };
    let audio_open = if source_current.is_some()
        && recipe.encoding.is_some()
        && !recipe.file.audio_streams.is_empty()
    {
        Ok(source_current)
    } else {
        reopen_encoded_audio(rendition.source.as_ref(), recipe).await
    };
    let audio_source = match audio_open {
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
    let (child, child_job, stdout, stderr) = {
        let mut args = recipe_pipe_args(recipe, start_seconds, attested);
        if rendition.key.starts_with("source-") && recipe.encoding.is_none() {
            bound_source_copy_threads(&mut args);
        }
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
        let env = recipe_child_env(recipe);
        if let Some(authority) = &source_authority {
            if let Err(cause) = authority.validate_before_spawn() {
                record_failure(
                    shared,
                    rendition,
                    crate::playback_control::ProducerDecisionReason::ProducerLaunchFailed,
                    cause,
                );
                return;
            }
        }
        let spawned = match crate::producer_spawn::spawn(
            &program,
            &args,
            crate::producer_spawn::SpawnOptions {
                runtime_cache: &shared.runtime_cache,
                progress: crate::producer_spawn::Progress::None,
                descriptors,
                env: &env,
                work: crate::process_control::ChildWork::realtime("VOD transcode"),
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
    rendition
        .last_child_pid
        .store(child.id().unwrap_or(0), Relaxed);
    // Transfer the raw child and physical resources synchronously before the
    // first registration await. Losing this caller can only lose its waiter.
    let (registration_tx, registration_rx) = tokio::sync::oneshot::channel();
    tokio::spawn({
        let rendition = Arc::clone(rendition);
        let shared = Arc::clone(shared);
        async move {
            let _private_operation = private_operation;
            if _private_operation.is_some() {
                shared.hooks.get().before_private_registration().await;
            }
            rendition.hooks.before_producer_registration().await;
            let (registration, writers) = rendition
                .slot
                .attach_registered_job_owned(
                    child,
                    child_job,
                    at,
                    permit.map(|permit| Box::new(permit) as Box<dyn Send>),
                )
                .await;
            rendition
                .source_owners
                .registered(&source_dispatch, &registration);
            drop(source_dispatch);
            if let Err((registration, writers, stdout, stderr)) =
                registration_tx.send((registration, writers, stdout, stderr))
            {
                drop(stdout);
                drop(stderr);
                writers.settled();
                let _ = rendition
                    .slot
                    .request_registered_retirement(&registration)
                    .await;
                let _ = registration.wait_confirmed_reap().await;
            }
        }
    });
    let Ok((registration, writers, stdout, stderr)) = registration_rx.await else {
        record_failure(
            shared,
            rendition,
            crate::playback_control::ProducerDecisionReason::ProducerLaunchFailed,
            "owned producer registration did not complete".to_owned(),
        );
        return;
    };
    // A child launched before a close/failure still belongs to the actual
    // registered reaper. Wait errors retain its permit and descendant job.
    if rendition.closed.load(Relaxed) || rendition.failure().is_some() {
        drop(stdout);
        drop(stderr);
        writers.settled();
        let _ = rendition
            .slot
            .request_registered_retirement(&registration)
            .await;
        let _ = registration.wait_confirmed_reap().await;
        return;
    }
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
    let (supervised_shared, supervised) = (Arc::clone(&shared), Arc::clone(&rendition));
    let writer = tokio::spawn(async move {
        let key = rendition.key.clone();
        let (retiring, retired) = tokio::sync::oneshot::channel();
        let (outcome, diagnostic) = tokio::join!(
            async {
                let outcome = run_generation(
                    Arc::clone(&shared),
                    Arc::clone(&rendition),
                    Box::new(stdout),
                    at,
                    epoch,
                    registration.retirement(),
                )
                .await;
                // Killing starts before diagnostic drain is joined, while the
                // owned reaper waits for this task's actual writer settlement.
                let _ = super::driver::request_registered_driver_retirement(
                    &shared,
                    &rendition,
                    &registration,
                )
                .await;
                let _ = retiring.send(());
                outcome
            },
            async {
                let diagnostic = crate::ffmpeg::drain_diagnostics(stderr);
                tokio::pin!(diagnostic);
                tokio::select! {
                    text=&mut diagnostic=>text,
                    _=retired=>match tokio::time::timeout(Duration::from_secs(5),&mut diagnostic).await{
                        Ok(text)=>text,
                        Err(_)=>"producer diagnostic drain exceeded its retirement deadline".to_owned(),
                    },
                }
            }
        );
        writers.settled();
        match rendition
            .slot
            .wait_registered_retirement(&registration)
            .await
        {
            Ok(receipt) => {
                debug_assert!(receipt.matches(&registration));
            }
            Err(error) => {
                tracing::warn!(target: "plurxd::vodserve",%error,"generation retirement was superseded")
            }
        }
        // Completion is metadata about already-settled writes. It may wait
        // for the manifest only after the reaper has released the writer's
        // resources and any cleanup owner can release its publication gate.
        let (outcome, completed) = outcome;
        if let Some((sink, _)) = completed {
            vodgen::Sink::completed_output(&sink).await;
        }
        let diagnostic = crate::ffmpeg::classify_diagnostic(&diagnostic);
        if !diagnostic.informational.is_empty() {
            tracing::debug!(target: "plurxd::vodserve", rendition = %key, generation = epoch, informational = %diagnostic.informational, "VOD producer informational output");
        }
        if !diagnostic.actionable.is_empty() {
            tracing::warn!(target: "plurxd::vodserve", rendition = %key, generation = epoch, diagnostic = %diagnostic.actionable, "VOD producer diagnostic");
        }
        if let Some(outcome) = outcome {
            on_generation_end(&shared, &rendition, outcome, epoch).await;
        }
    });
    // The writer task owns this generation's segment writes and its end. A
    // panic unwinds past `on_generation_end` and drops the writer barrier, so
    // the reaper releases the slot but nothing tells the rendition. Only the
    // join can: record the failure, which wakes waiters with a terminal class
    // and kicks the driver to reclaim the producer.
    tokio::spawn(on_writer_panic(writer, move || {
        record_failure(
            &supervised_shared,
            &supervised,
            crate::playback_control::ProducerDecisionReason::ReaderFailed,
            "producer writer task panicked".to_owned(),
        );
    }));
    tracing::info!(
        target: "plurxd::vodserve",
        rendition = %rendition_key_field(at), "spawned a producer generation"
    );
}

/// Join a generation writer and report only a panic. Every other end is the
/// writer's own to report through `on_generation_end`.
async fn on_writer_panic(writer: tokio::task::JoinHandle<()>, on_panic: impl FnOnce()) {
    if writer.await.is_err_and(|error| error.is_panic()) {
        on_panic();
    }
}

pub(super) async fn recipe_engine_is_current(recipe: &Recipe) -> bool {
    if let Some(encoding) = recipe.encoding.as_ref() {
        // One blocking batch for the whole encoding attestation. The
        // executable used to be statted inline here, ahead of the batch and
        // short-circuiting it, so every producer launch and every segment
        // materialisation paid a synchronous `metadata` on a runtime worker.
        if !encoding.executable.matches_macos_plan(&encoding.plan)
            || !encoding
                .engine
                .is_current_with_executable(&encoding.executable)
                .await
        {
            return false;
        }
    }
    recipe.cluster_cache_key.is_none() || crate::ffmpeg::fragment_index_engine_is_current().await
}

/// What a producer for `recipe` adds to its environment: a text burn's frozen
/// Fontconfig configuration, so the child sees the fonts the recipe captured
/// and nothing installed since.
pub(super) fn recipe_child_env(recipe: &Recipe) -> Vec<(&'static str, &std::ffi::OsStr)> {
    recipe
        .encoding
        .as_ref()
        .map(|encoding| encoding.engine.child_env())
        .unwrap_or_default()
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
        let plan = encoding.media_plan(file.duration_ms.unwrap_or(0));
        let end = plan
            .entries
            .last()
            .map_or(0, |entry| entry.start_ticks + entry.duration_ticks);
        let duration_seconds = if encoding.shared_audio.is_some() {
            // Encoding maps the source duration onto the shared film end once.
            file.duration_ms.unwrap_or(0) as f64 / 1_000.0
        } else {
            end as f64 / f64::from(plan.timescale)
        };
        let mut args = encoding.args(&file, start_seconds, duration_seconds);
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
        let mut args = plurx_core::transcode::copy_pipe_args_with_audio_delivery(
            &recipe.file,
            start_seconds,
            recipe.audio_index,
            recipe.aac,
            Pacing::unpaced(),
            recipe.video,
            recipe.audio_delivery.as_ref(),
        );
        if attested {
            replace_inputs_with_attested_descriptor(&mut args);
        }
        args
    }
}

/// Input options apply to each opened demuxer; output audio and filter bounds
/// prevent automatic parallelism. Ordinary Local copy argv is unchanged.
pub(super) fn bound_source_copy_threads(args: &mut Vec<String>) {
    let mut index = 0;
    while index < args.len() {
        if args[index] == "-i" {
            args.splice(index..index, ["-threads".to_owned(), "1".to_owned()]);
            index += 2;
        }
        index += 1;
    }
    args.splice(
        0..0,
        [
            "-filter_threads".to_owned(),
            "1".to_owned(),
            "-filter_complex_threads".to_owned(),
            "1".to_owned(),
        ],
    );
    // These are output options and must precede the existing output target.
    let output = args.len().saturating_sub(1);
    args.splice(output..output, ["-threads:a".to_owned(), "1".to_owned()]);
}

pub(super) async fn reopen_encoded_audio(
    source: Option<&crate::fragment_index_cluster::SourceFence>,
    recipe: &Recipe,
) -> Result<Option<crate::fragment_index_cluster::SourceFence>, String> {
    if recipe.encoding.as_ref().is_some_and(|encoding| {
        encoding.shared_audio.is_none() && encoding.plan.options().input_has_audio
    }) && !recipe.file.audio_streams.is_empty()
    {
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
    stdout: Box<dyn AsyncRead + Send + Unpin>,
    at: u32,
    epoch: u64,
    retirement: tokio_util::sync::CancellationToken,
) -> (Option<Outcome>, Option<(RenditionSink, bool)>) {
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
                return (
                    Some(Outcome::Failed(Failure::Stream(format!(
                        "reading the generation's init: {error}"
                    )))),
                    None,
                );
            }
            Ok((consumed, muxer)) => {
                if let Err(outcome) = establish_or_verify(&rendition, &muxer).await {
                    return (Some(outcome), None);
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
            None => return (None, None),
        }
    };
    let generation = Generation {
        plan: rendition.plan.clone(),
        index: rendition.index.clone(),
        encoded_frame_ticks: rendition
            .recipe
            .encoding
            .as_ref()
            .map(|encoding| encoding.grid.denominator),
        encoded_video_origin: {
            #[cfg(target_os = "linux")]
            {
                rendition
                    .recipe
                    .encoding
                    .as_ref()
                    .filter(|encoding| encoding.dv_runtime.is_some())
                    .map(|encoding| {
                        plurx_core::transcode::vod_reconstructed_video_origin(
                            encoding.grid,
                            rendition.plan.entry(at).expect("spawn entry").start_ticks
                                / u64::from(encoding.grid.denominator),
                        )
                        .expect("independently validated reconstructed video origin")
                    })
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        },
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
        retain_hevc_parameter_sets: rendition.recipe.video.retains_hevc_parameter_sets(),
    };
    let sink = RenditionSink {
        shared: Arc::clone(&shared),
        rendition: Arc::clone(&rendition),
        epoch,
        retirement,
    };
    let completion = DeferredCompletionSink::new(&sink);
    let outcome = vodgen::run(src, generation, &completion, &rendition.key).await;
    let completed = completion.completed.load(Acquire);
    let all_fresh = completion.all_fresh();
    (Some(outcome), completed.then_some((sink, all_fresh)))
}

/// The identity half of a pre-read generation. `false` means the generation
/// is over (drift handled or failure recorded) and the caller must return.
async fn establish_or_verify(rendition: &Arc<Rendition>, muxer: &Init) -> Result<(), Outcome> {
    if !recipe_engine_is_current(&rendition.recipe).await {
        return Err(Outcome::Failed(Failure::EngineChanged(
            "the immutable media engine changed before init publication".to_owned(),
        )));
    }
    if let Some(audio) = rendition
        .recipe
        .encoding
        .as_ref()
        .and_then(|encoding| encoding.shared_audio.as_ref())
    {
        if let Err(error) = audio.verify_init(muxer) {
            return Err(Outcome::Failed(Failure::Stream(format!(
                "shared soundtrack init: {error}"
            ))));
        }
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
                    return Err(Outcome::Failed(Failure::InitDrift(refused.to_string())));
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
                        return Err(Outcome::Failed(Failure::Stream(format!(
                            "establishing the init identity: {error}"
                        ))));
                    }
                };
                let served = identity
                    .served_init_for(muxer)
                    .expect("an identity just established from this muxer init serves it");
                if let Err(error) = store_identity_observed(
                    &rendition.identity_path(),
                    &identity,
                    rendition.private_storage.as_ref(),
                )
                .await
                {
                    tracing::warn!(
                        target: "plurxd::vodserve",
                        rendition = %rendition.key,
                        "persisting identity.json: {error}"
                    );
                    if rendition.private_storage.is_some() {
                        rendition.revoke_preparation();
                        drop(state);
                        return Err(Outcome::Failed(Failure::Sink(error)));
                    }
                }
                *state = IdentityState {
                    identity: Some(identity),
                    from_disk: false,
                };
                served
            }
        }
    };
    let pending = if let Some(storage) = rendition.private_storage.as_ref() {
        match storage.begin(served.bytes.len() as u64) {
            Some(pending) => Some(pending),
            None => {
                rendition.revoke_preparation();
                return Err(Outcome::Failed(Failure::Sink(
                    io::ErrorKind::OutOfMemory.into(),
                )));
            }
        }
    } else {
        None
    };
    if let Err(error) = rendition.dir.write_init(&served.bytes).await {
        return Err(Outcome::Failed(Failure::Sink(error)));
    }
    if let Some(pending) = pending {
        pending.commit(false);
    }
    rendition.clear_demand(INIT_DEMAND_INDEX);
    rendition.init_notify.notify_waiters();
    Ok(())
}

/// What a generation's ending means for the rendition.
async fn on_generation_end(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    outcome: Outcome,
    epoch: u64,
) {
    if rendition.cancelled_preparation_epoch.load(Acquire) == epoch.saturating_add(1) {
        // Background cancellation is not a foreground producer verdict. The
        // key/readers fence refuses to terminate an epoch acquired by a live
        // reader; its existing driver owns subsequent reconciliation.
        super::copy_preparation::fence_cancelled_epoch(shared, rendition, epoch).await;
        rendition.kick();
        return;
    }
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
    match outcome {
        Outcome::Failed(Failure::InitDrift(cause)) => {
            on_init_drift(shared, rendition, cause).await;
        }
        Outcome::Failed(Failure::Sink(error)) if quality_reservations_unknown(&error) => {
            // Unknown is not a verdict on the rendition. Nothing was
            // published, the producer is reaped, and the next demand respawns
            // and asks the Store again.
            tracing::warn!(
                target: "plurxd::vodserve",
                rendition = %rendition.key,
                "holding publication because continuous dependencies are unknown: {error}"
            );
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
            #[cfg(target_os = "linux")]
            let planned_window = rendition
                .recipe
                .encoding
                .as_ref()
                .is_some_and(|encoding| encoding.dv_runtime.is_some())
                && produced_through.is_some();
            #[cfg(not(target_os = "linux"))]
            let planned_window = false;
            if !finished && !planned_window && shared.pool.blocked_on(&rendition.key).is_some() {
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
        let _dependency_guard = shared
            .rendition_build_gate(&rendition.key)
            .lock_owned()
            .await;
        let dependencies = shared
            .store
            .quality_reserved_intervals(&rendition.key)
            .await;
        if !matches!(dependencies, Ok(ref intervals) if intervals.is_empty()) {
            record_failure(
                shared,
                rendition,
                crate::playback_control::ProducerDecisionReason::RenditionInitChanged,
                format!("retaining reserved or unverified media after init drift: {cause}"),
            );
            return;
        }
        {
            let mut manifest = rendition.manifest.lock().await;
            let freed = rendition.dir.purge(&mut manifest).await;
            rendition.free_unadmitted(shared, freed.bytes);
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
    pub(super) retirement: tokio_util::sync::CancellationToken,
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

/// Whether any continuous-quality ledger can name this rendition. The
/// reservation publisher verifies every reserved video interval against a
/// continuous AVC High recipe without input audio and every AAC interval
/// against a shared-soundtrack recipe before committing it, and a rendition
/// key hashes its recipe, so a copy remux or an ordinary transcode can never
/// carry a pin. Those publish without a Store round trip.
pub(super) fn quality_reservations_possible(recipe: &Recipe) -> bool {
    recipe.encoding.as_ref().is_some_and(|encoding| {
        encoding.shared_audio.is_some()
            || (!encoding.plan.options().input_has_audio
                && encoding.plan.options().video_sample_envelope
                    == plurx_core::transcode::VideoSampleEnvelope::ContinuousAvcHigh50)
    })
}

const QUALITY_RESERVATION_LOOKUP_ATTEMPTS: u32 = 3;

/// The sink could not learn a continuous rendition's reserved intervals.
/// Carried inside the sink's `io::Error` so the generation's end can tell it
/// from a real write fault.
#[derive(Debug)]
struct QualityReservationsUnknown(String);

impl std::fmt::Display for QualityReservationsUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "reserved media verification unavailable: {}", self.0)
    }
}

impl std::error::Error for QualityReservationsUnknown {}

pub(super) fn quality_reservations_unknown_error(cause: String) -> io::Error {
    io::Error::other(QualityReservationsUnknown(cause))
}

pub(super) fn quality_reservations_unknown(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<QualityReservationsUnknown>())
}

/// A cached publication is traversed, never replaced: its bytes must still be
/// exactly the manifest's, and any reservation must match them.
pub(super) async fn verify_retained_publication(
    path: &Path,
    plan: &plurx_core::segplan::SegmentPlan,
    entry: u32,
    expected: u64,
    dependencies: &[plurx_core::playback::continuous_quality::QualityInterval],
) -> io::Result<()> {
    let cached = super::vod_serve_serve::read_quality_artifact(
        path,
        expected.min(plurx_core::playback::continuous_quality::MAX_QUALITY_PINNED_BYTES),
    )
    .await
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if cached.len() as u64 != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "reserved artifact differs from its manifest",
        ));
    }
    verify_reserved_publication(plan, entry, &cached, dependencies)
}

/// An already reserved URI cannot acquire different bytes on regeneration.
/// The caller holds the same exact-key gate as reservation and eviction.
pub(super) fn verify_reserved_publication(
    plan: &plurx_core::segplan::SegmentPlan,
    entry: u32,
    bytes: &[u8],
    dependencies: &[plurx_core::playback::continuous_quality::QualityInterval],
) -> io::Result<()> {
    let planned = plan.entry(entry).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "publication is outside the media plan",
        )
    })?;
    for dependency in dependencies {
        if !dependency.valid() || dependency.timescale != plan.timescale {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "reserved media clock is unverifiable",
            ));
        }
        if dependency.from_tick < planned.end_ticks()
            && planned.start_ticks < dependency.through_tick
            && (dependency.from_tick != planned.start_ticks
                || dependency.through_tick != planned.end_ticks()
                || !dependency.matches_bytes(bytes))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "regenerated media differs from its reserved immutable artifact",
            ));
        }
    }
    Ok(())
}

/// The pipe writer reports the verified trailer without taking publication
/// locks. Its existing owner applies the completion metadata after confirmed
/// reap, so temporary contention cannot lose proof or hold encoder admission.
pub(super) struct DeferredCompletionSink<'a> {
    sink: &'a RenditionSink,
    pub(super) completed: AtomicBool,
    materialized: AtomicBool,
    replayed: AtomicBool,
}

impl<'a> DeferredCompletionSink<'a> {
    fn all_fresh(&self) -> bool {
        self.materialized.load(Acquire) && !self.replayed.load(Acquire)
    }

    pub(super) fn new(sink: &'a RenditionSink) -> Self {
        Self {
            sink,
            completed: AtomicBool::new(false),
            materialized: AtomicBool::new(false),
            replayed: AtomicBool::new(false),
        }
    }
}

impl vodgen::Sink for DeferredCompletionSink<'_> {
    async fn completed_output(&self) {
        self.completed.store(true, Release);
    }

    async fn materialize(&self, entry: u32, bytes: Vec<u8>) -> io::Result<()> {
        let fresh = self.sink.materialize_observed(entry, bytes).await?;
        self.materialized.store(true, Release);
        if !fresh {
            self.replayed.store(true, Release);
        }
        Ok(())
    }
}

impl RenditionSink {
    /// Retirement can hold a publication gate while joining this writer.
    /// Abandon a queued lock acquisition when that exact generation retires,
    /// but never cancel directory writes or their accounting after acquisition.
    async fn publication_lock<T>(
        &self,
        lock: impl std::future::Future<Output = T>,
    ) -> io::Result<T> {
        tokio::select! {
            biased;
            _ = self.retirement.cancelled() => Err(io::ErrorKind::NotFound.into()),
            guard = lock => Ok(guard),
        }
    }

    /// The exact-key gate plus every reserved interval of this rendition.
    ///
    /// Only continuous renditions ask the Store. An unanswered lookup is
    /// retried with the gate released, so reservation and eviction are never
    /// queued behind an unavailable Store; if it stays unanswered the
    /// publication is refused as unknown. That is the safe direction: an
    /// unverified regeneration could replace bytes a client already
    /// scheduled, while holding only delays a fragment the next generation
    /// retries. It never retires the rendition.
    async fn reserved_dependencies(
        &self,
    ) -> io::Result<(
        tokio::sync::OwnedMutexGuard<()>,
        Vec<plurx_core::playback::continuous_quality::QualityInterval>,
    )> {
        if !quality_reservations_possible(&self.rendition.recipe) {
            let guard = self
                .publication_lock(
                    self.shared
                        .rendition_build_gate(&self.rendition.key)
                        .lock_owned(),
                )
                .await?;
            return Ok((guard, Vec::new()));
        }
        let mut cause = String::new();
        for attempt in 0..QUALITY_RESERVATION_LOOKUP_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(250 << (attempt - 1))).await;
            }
            if self.rendition.closed.load(Relaxed)
                || self.rendition.gen_epoch.load(Relaxed) != self.epoch
            {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            let guard = self
                .publication_lock(
                    self.shared
                        .rendition_build_gate(&self.rendition.key)
                        .lock_owned(),
                )
                .await?;
            match tokio::time::timeout(
                Duration::from_secs(1),
                self.shared
                    .store
                    .quality_reserved_intervals(&self.rendition.key),
            )
            .await
            {
                Ok(Ok(dependencies)) => return Ok((guard, dependencies)),
                Ok(Err(error)) => cause = error.to_string(),
                Err(_) => cause = "the Store did not answer within one second".to_owned(),
            }
        }
        Err(quality_reservations_unknown_error(cause))
    }
}

impl vodgen::Sink for RenditionSink {
    async fn completed_output(&self) {
        self.completed_output_accepted().await;
    }

    async fn materialize(&self, entry: u32, bytes: Vec<u8>) -> io::Result<()> {
        self.materialize_observed(entry, bytes).await.map(|_| ())
    }
}

impl RenditionSink {
    /// Whether the exact completion callback admitted its engine and source.
    async fn completed_output_accepted(&self) -> bool {
        if !recipe_engine_is_current(&self.rendition.recipe).await {
            return false;
        }
        let manifest = self.rendition.manifest.lock().await;
        // Only vodgen's verified normal trailer reaches this callback. The
        // driver may already have retired an all-done child, but that cannot
        // invalidate its successfully published bytes. The observer still
        // checks the original Sink epoch; mixed/repeated writes lose proof.
        if !self.rendition.closed.load(Relaxed)
            && self
                .rendition
                .source
                .as_ref()
                .is_some_and(|source| source.unchanged())
        {
            let mut measurement = self
                .rendition
                .output_measurement
                .lock()
                .expect("output measurement lock");
            measurement.complete(
                self.epoch,
                !manifest.is_empty() && manifest.next_gap(0).is_none(),
            );
            if let Some(rates) = measurement.complete_rates() {
                tracing::debug!(target: "plurxd::vodserve",
                    output_identity = %hex::encode(rates.identity),
                    wire_bytes = rates.wire_bytes, duration_micros = rates.duration_micros,
                    average_bps = rates.average_bps, rfc_peak_bps = rates.rfc_peak_bps,
                    segment_burst_bps = rates.segment_burst_bps,
                    "complete full-mux VOD measurement; retained wire consumer not issued");
            }
            drop(measurement);
            drop(manifest);
            super::retained::RetainedArtifactRegistry::offer(&self.shared, &self.rendition);
            true
        } else {
            false
        }
    }
    async fn materialize_observed(&self, entry: u32, bytes: Vec<u8>) -> io::Result<bool> {
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
        if self
            .rendition
            .recipe
            .encoding
            .as_ref()
            .is_some_and(|encoding| {
                encoding.continuous_object_fits(&self.rendition.plan, entry, bytes.len() as u64)
                    == Some(false)
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "continuous object exceeds its advertised container-inclusive delivery budget",
            ));
        }
        let (dependency_guard, dependencies) = self.reserved_dependencies().await?;
        let retained = {
            let manifest = self
                .publication_lock(self.rendition.manifest.lock())
                .await?;
            if self.rendition.gen_epoch.load(Relaxed) != self.epoch {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            manifest
                .state(entry)
                .filter(|state| state.is_materialized())
                .map(|state| state.bytes())
        };
        // A completed URI may already be in an HTTP client's hands before its
        // Scheduled acknowledgement pins the bytes. Publication, not that
        // later acknowledgement, makes the cached artifact immutable.
        let retain_published = retained.is_some();
        if retain_published {
            // A restarted encoder can use different rate-control history.
            // Traverse the original publication instead of replacing it.
            let expected = retained.expect("materialized published interval");
            verify_retained_publication(
                &self
                    .rendition
                    .dir
                    .path()
                    .join(segment_name(u64::from(entry))),
                &self.rendition.plan,
                entry,
                expected,
                &dependencies,
            )
            .await?;
        } else {
            verify_reserved_publication(&self.rendition.plan, entry, &bytes, &dependencies)?;
        }
        if self
            .rendition
            .source
            .as_ref()
            .is_some_and(|source| !source.unchanged())
        {
            let cause = "source changed while verifying reserved media".to_owned();
            record_failure(
                &self.shared,
                &self.rendition,
                crate::playback_control::ProducerDecisionReason::SourceChanged,
                cause.clone(),
            );
            return Err(io::Error::new(io::ErrorKind::InvalidData, cause));
        }
        if retain_published {
            {
                let _manifest = self
                    .publication_lock(self.rendition.manifest.lock())
                    .await?;
                if self.rendition.gen_epoch.load(Relaxed) != self.epoch {
                    return Err(io::Error::from(io::ErrorKind::NotFound));
                }
                self.rendition.clear_demand(entry);
            }
            drop(dependency_guard);
            // This is process progress through existing bytes, not a fresh
            // publication, working-set charge or marker-prewarm credit.
            self.rendition.slot.produced(entry).await;
            self.shared.pool.satisfy(&self.rendition.key, entry);
            self.rendition.kick();
            return Ok(false);
        }
        let len = bytes.len() as u64;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        let init = self
            .publication_lock(self.rendition.identity.lock())
            .await?
            .identity
            .clone();
        {
            let mut manifest = self
                .publication_lock(self.rendition.manifest.lock())
                .await?;
            // Checked under the manifest lock, so a driver bumping the epoch
            // cannot interleave between the check and the write.
            if self.rendition.gen_epoch.load(Relaxed) != self.epoch {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            let before = manifest.state(entry).map(|s| s.bytes()).unwrap_or(0);
            let preparation = self.rendition.preparation();
            let pending = if let Some(storage) = self.rendition.private_storage.as_ref() {
                if before != 0 || manifest.is_admitted() {
                    self.rendition.revoke_preparation();
                    return Err(io::ErrorKind::InvalidData.into());
                }
                match storage.begin(len) {
                    Some(pending) => Some(pending),
                    None => {
                        self.rendition.revoke_preparation();
                        return Err(io::ErrorKind::OutOfMemory.into());
                    }
                }
            } else {
                None
            };
            if pending.is_some() {
                self.shared.hooks.get().before_private_materialize().await;
            } else {
                self.shared.hooks.get().before_ordinary_materialize().await;
            }
            self.rendition
                .dir
                .materialize(&mut manifest, entry, &bytes, now_ms())
                .await?;
            let publication =
                credit_marker_prewarm_publication(&self.rendition, self.epoch, entry).await;
            if let Some(init) = init.as_ref() {
                self.rendition
                    .output_measurement
                    .lock()
                    .expect("output measurement lock")
                    .observe(
                        &self.rendition,
                        init,
                        self.epoch,
                        entry,
                        output_measurement::ObservedOutputMember {
                            bytes: len,
                            digest,
                            publication,
                        },
                    );
            }
            // Publication and provenance linearize under the same manifest
            // lock. A skip can therefore observe neither fact or both, never
            // real prewarm bytes with a missing credit.
            if !manifest.is_admitted() && self.rendition.private_storage.is_none() {
                sub_saturating(&self.shared.working_set, before);
                self.shared.working_set.fetch_add(len, Relaxed);
            }
            if let Some(pending) = pending {
                pending.commit(true);
            }
            if manifest.next_gap(0).is_none() && self.rendition.private_storage.is_none() {
                self.shared.try_admit(&self.rendition, &mut manifest).await;
            }
            if let Some(preparation) = preparation {
                preparation.progress.notify_waiters();
            }
            self.rendition.clear_demand(entry);
        }
        drop(dependency_guard);
        self.rendition.slot.produced(entry).await;
        if let (Some(encoding), Some(planned)) = (
            &self.rendition.recipe.encoding,
            self.rendition.plan.entry(entry),
        ) {
            if planned.kind == plurx_core::segplan::PlanEntryKind::Video {
                encoding.note_active_segment(
                    self.epoch,
                    entry,
                    ticks_to_ms(planned.end_ticks(), self.rendition.timescale),
                );
            }
        }
        self.shared.pool.satisfy(&self.rendition.key, entry);
        self.rendition.kick();
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod writer_supervision_tests {
    #[tokio::test]
    async fn deferred_replay_keeps_original_bytes_without_fresh_receipt_authority() {
        use super::*;
        use crate::vodgen::Sink;
        let base = crate::test_tempdir().expect("deferred replay fixture");
        let serve = super::super::tests::bare_serve(base.path());
        let rendition = super::super::tests::synthetic_rendition(base.path()).await;
        let sink = RenditionSink {
            shared: Arc::clone(&serve.shared),
            rendition: Arc::clone(&rendition),
            epoch: rendition.gen_epoch.load(Relaxed),
            retirement: tokio_util::sync::CancellationToken::new(),
        };
        let fresh = DeferredCompletionSink::new(&sink);
        let original = b"immutable-original-window";
        fresh
            .materialize(0, original.to_vec())
            .await
            .expect("fresh publication");
        fresh.completed_output().await;
        assert!(fresh.completed.load(Acquire));
        assert!(
            fresh.all_fresh(),
            "fresh accepted bytes may support their own receipt"
        );
        let charged = serve.shared.working_set.load(Relaxed);
        let serial = rendition.publication_serial.load(Relaxed);
        let replay = DeferredCompletionSink::new(&sink);
        replay
            .materialize(0, b"different-new-encoder-window".to_vec())
            .await
            .expect("valid retained traversal");
        replay.completed_output().await;
        assert!(
            replay.completed.load(Acquire),
            "a trailer alone also occurs on replay"
        );
        assert!(
            !replay.all_fresh(),
            "retained bytes cannot inherit the new encoder window receipt"
        );
        assert_eq!(
            tokio::fs::read(rendition.dir.path().join(segment_name(0)))
                .await
                .expect("retained bytes"),
            original
        );
        assert_eq!(serve.shared.working_set.load(Relaxed), charged);
        assert_eq!(rendition.publication_serial.load(Relaxed), serial);
    }

    #[tokio::test]
    async fn a_panicked_generation_writer_is_reported_and_a_clean_end_is_not() {
        let reported = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let panicked = tokio::spawn(async { panic!("writer fixture panics") });
        let seen = std::sync::Arc::clone(&reported);
        super::on_writer_panic(panicked, move || {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
        .await;
        assert_eq!(reported.load(std::sync::atomic::Ordering::SeqCst), 1);
        let clean = tokio::spawn(async {});
        let seen = std::sync::Arc::clone(&reported);
        super::on_writer_panic(clean, move || {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
        .await;
        assert_eq!(reported.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
