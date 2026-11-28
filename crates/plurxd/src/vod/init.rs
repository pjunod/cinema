use super::*;

/// Handoff §5's regenerate-and-verify: spawn the generation child at entry 0,
/// read its pipe only to the muxer init, kill the child, and answer the
/// served init the stored identity derives from it — or why it refused.
///
/// This is how a resurrected rendition with a complete (or gap-free-enough)
/// manifest gets its `init.mp4` back without producing a single segment: the
/// §2 ruling made the init reproducible independently of the segments, so a
/// verified head is proof enough to keep every adopted byte.
pub(super) async fn regenerate_init_head(
    recipe: &Recipe,
    source: &crate::fragment_index_cluster::SourceFence,
    identity: &InitIdentity,
    slots: &Arc<Semaphore>,
    runtime_cache: &Path,
) -> Result<Init, HeadRegenerationError> {
    let muxer = probe_init_head(recipe, source, slots, runtime_cache, None).await?;
    identity
        .served_init_for(&muxer)
        .map_err(|refused| HeadRegenerationError::Failed(refused.to_string()))
}

/// Materialize a private init through the same bounded head producer used
/// when an immutable rendition is restored from disk.
pub(super) async fn prepare_init_head(
    shared: &Shared,
    rendition: &Arc<Rendition>,
) -> Result<(), String> {
    if rendition.dir.has_init().await {
        if let Some(encoding) = rendition
            .recipe
            .encoding
            .as_ref()
            .filter(|encoding| encoding.is_source_copy())
        {
            let permit = crate::vodencode::EncodePermit::try_source_copy(
                &encoding.admissions,
                shared.store.as_ref(),
            )
            .await;
            if !matches!(permit, crate::vodencode::SourceCopyPermitRead::Admitted(_)) {
                return Err("cached split soundtrack could not admit a copy worker".into());
            }
        }
        return Ok(());
    }
    let source = rendition
        .source
        .as_ref()
        .ok_or("split child has no held source")?;
    let muxer = probe_init_head(
        &rendition.recipe,
        source,
        &shared.head_regeneration_slots,
        &shared.runtime_cache,
        rendition.retained_admission.current(),
    )
    .await
    .map_err(|error| error.to_string())?;
    super::generation::establish_or_verify(
        shared,
        rendition,
        &muxer,
        rendition.gen_epoch.load(Relaxed),
    )
    .await
    .map_err(|error| format!("establishing private split init: {error:?}"))
}

async fn probe_init_head(
    recipe: &Recipe,
    source: &crate::fragment_index_cluster::SourceFence,
    slots: &Arc<Semaphore>,
    runtime_cache: &Path,
    reservation: Option<crate::vodencode::EncodePermit>,
) -> Result<Init, HeadRegenerationError> {
    let _permit = Arc::clone(slots)
        .try_acquire_owned()
        .map_err(|_| HeadRegenerationError::Busy)?;
    if !recipe_engine_is_current(recipe).await {
        return Err(HeadRegenerationError::Failed(
            "the immutable media engine changed before head regeneration".to_owned(),
        ));
    }
    if !source.unchanged().await {
        return Err(HeadRegenerationError::Failed(
            "source changed before head regeneration".to_owned(),
        ));
    }
    let permit = if let Some(encoding) = &recipe.encoding {
        let permit = if let Some(reserved) = reservation {
            Some(reserved)
        } else if encoding
            .shared_audio
            .as_ref()
            .is_some_and(|audio| audio.is_source_copy())
        {
            match crate::vodencode::EncodePermit::try_source_copy(
                &encoding.admissions,
                &*encoding.store,
            )
            .await
            {
                crate::vodencode::SourceCopyPermitRead::Admitted(permit) => Some(permit),
                _ => None,
            }
        } else {
            let permit = encoding.try_permit().await;
            encoding.cancel_wait();
            permit
        };
        Some(
            permit
                .and_then(crate::vodencode::EncodePermit::try_claim_worker)
                .ok_or(HeadRegenerationError::Busy)?,
        )
    } else {
        None
    };
    let args = recipe_pipe_args(recipe, 0.0, cfg!(unix));
    let audio_source = reopen_encoded_audio(Some(source), recipe)
        .await
        .map_err(HeadRegenerationError::Failed)?;
    #[cfg(unix)]
    let descriptors = crate::producer_spawn::Descriptors::from_files(
        Some(&source.handle),
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
    let spawned = crate::producer_spawn::spawn(
        &program,
        &args,
        crate::producer_spawn::SpawnOptions {
            runtime_cache,
            progress: crate::producer_spawn::Progress::None,
            descriptors,
            env: &env,
            work: crate::process_control::ChildWork::realtime("VOD init segment"),
        },
    )
    .map_err(|error| {
        HeadRegenerationError::Failed(format!("spawning the head regeneration: {error}"))
    })?;
    let mut owner = HeadChildOwner::new_job_owned(spawned.child, spawned.child_job);
    owner.permit = permit;
    let stdout = spawned.stdout;
    let stderr = spawned.stderr;
    let copied = recipe.encoding.as_ref().and_then(|encoding| {
        let source = encoding.shared_audio.as_ref()?.source_copy()?;
        Some((
            source.clone(),
            encoding.media_plan(recipe.file.duration_ms.unwrap_or(0)),
        ))
    });
    let head = async {
        match copied {
            Some(copied) => {
                read_regenerated_head(
                    owner,
                    stdout,
                    HEAD_REGENERATION_TIMEOUT,
                    HEAD_REGENERATION_MAX_BYTES,
                    Some(copied),
                )
                .await
            }
            None => {
                read_regenerated_head_before(
                    owner,
                    stdout,
                    HEAD_REGENERATION_TIMEOUT,
                    HEAD_REGENERATION_MAX_BYTES,
                )
                .await
            }
        }
    };
    let (muxer, diagnostic) = tokio::join!(head, crate::ffmpeg::drain_diagnostics(stderr));
    let diagnostic = crate::ffmpeg::classify_diagnostic(&diagnostic);
    if !diagnostic.informational.is_empty() {
        tracing::debug!(target: "plurxd::vodserve", informational = %diagnostic.informational, "VOD head regeneration informational output");
    }
    if !diagnostic.actionable.is_empty() {
        tracing::warn!(target: "plurxd::vodserve", diagnostic = %diagnostic.actionable, "VOD head regeneration diagnostic");
    }
    let muxer = muxer?;
    if !source.unchanged().await {
        return Err(HeadRegenerationError::Failed(
            "source changed during head regeneration".to_owned(),
        ));
    }
    if !recipe_engine_is_current(recipe).await {
        return Err(HeadRegenerationError::Failed(
            "the immutable media engine changed during head regeneration".to_owned(),
        ));
    }
    if let Some(audio) = recipe
        .encoding
        .as_ref()
        .and_then(|encoding| encoding.shared_audio.as_ref())
    {
        audio
            .verify_init(&muxer)
            .map_err(|error| HeadRegenerationError::Failed(error.to_string()))?;
    }
    Ok(muxer)
}

/// Read exactly one regeneration child's muxer head within the supplied
/// budget, then always kill and confirm-reap the child before returning. The
/// owner also transfers reap to a detached task if this future is cancelled.
pub(super) async fn read_regenerated_head_before(
    child: impl Into<HeadChildOwner>,
    stdout: tokio::process::ChildStdout,
    budget: Duration,
    max_bytes: usize,
) -> Result<Init, HeadRegenerationError> {
    read_regenerated_head(child.into(), stdout, budget, max_bytes, None).await
}

async fn read_regenerated_head(
    mut child: HeadChildOwner,
    mut stdout: tokio::process::ChildStdout,
    budget: Duration,
    max_bytes: usize,
    copied: Option<(plurx_core::transcode::VodSourceAudio, SegmentPlan)>,
) -> Result<Init, HeadRegenerationError> {
    // The stdout arrives separately from the child because the owner below
    // takes the child by value, and the read needs the pipe after that move.
    let head = tokio::time::timeout(
        budget,
        read_muxer_head_bounded(&mut stdout, max_bytes, copied),
    )
    .await;
    // Only the head is wanted; the rest of the pipe is not read.
    drop(stdout);
    child.terminate_and_reap().await;
    let (_consumed, muxer) = head
        .map_err(|_| {
            HeadRegenerationError::Failed(format!(
                "head regeneration exceeded its {:.1}s deadline",
                budget.as_secs_f64()
            ))
        })?
        .map_err(|error| {
            if error.kind() == io::ErrorKind::FileTooLarge {
                HeadRegenerationError::Oversize
            } else {
                HeadRegenerationError::Failed(format!(
                    "reading the head regeneration's init: {error}"
                ))
            }
        })?;
    Ok(muxer)
}

pub(super) async fn read_muxer_init_bounded<R: AsyncRead + Unpin>(
    src: &mut R,
    max_bytes: usize,
) -> io::Result<(Vec<u8>, Init)> {
    read_muxer_head_bounded(src, max_bytes, None).await
}

async fn read_muxer_head_bounded<R: AsyncRead + Unpin>(
    src: &mut R,
    max_bytes: usize,
    copied: Option<(plurx_core::transcode::VodSourceAudio, SegmentPlan)>,
) -> io::Result<(Vec<u8>, Init)> {
    let mut consumed = Vec::new();
    let mut reader = FragmentReader::new();
    if copied.is_some() {
        reader.reject_reinitialization();
    }
    let mut copied = copied;
    let mut copied_head = None;
    let mut buf = vec![0u8; (256 * 1024).min(max_bytes.max(1))];
    loop {
        let remaining = max_bytes.saturating_sub(consumed.len());
        if remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "the pipe did not emit its init within the byte limit",
            ));
        }
        let take = buf.len().min(remaining);
        let n = src.read(&mut buf[..take]).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the pipe ended before its moov arrived",
            ));
        }
        consumed.extend_from_slice(&buf[..n]);
        reader.push(&buf[..n]);
        loop {
            match reader.next_unit() {
                Ok(Some(Unit::Init(mut init))) => {
                    sanitize_stale_dolby_brand(&mut init);
                    if let Some((source, plan)) = copied.take() {
                        let segmenter = plurx_core::fmp4::PlannedAudioSegmenter::with_source_copy(
                            init.clone(),
                            plan,
                            0,
                            Some(source),
                        )
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                        copied_head = Some((init, segmenter));
                    } else {
                        return Ok((consumed, init));
                    }
                }
                Ok(Some(Unit::Fragment(fragment))) => {
                    if let Some((init, mut segmenter)) = copied_head.take() {
                        segmenter
                            .push(fragment)
                            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                        return Ok((consumed, init));
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(error) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("parsing the generation's pipe: {error}"),
                    ));
                }
            }
        }
    }
}

/// Read a generation's pipe up to (and through) its muxer init, keeping every
/// consumed byte so the whole stream can be replayed in front of the live
/// pipe for [`vodgen::run`].
pub(super) async fn read_muxer_init<R: AsyncRead + Unpin>(
    src: &mut R,
) -> io::Result<(Vec<u8>, Init)> {
    let mut consumed = Vec::new();
    let mut reader = FragmentReader::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = src.read(&mut buf).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the pipe ended before its moov arrived",
            ));
        }
        consumed.extend_from_slice(&buf[..n]);
        reader.push(&buf[..n]);
        loop {
            match reader.next_unit() {
                Ok(Some(Unit::Init(mut init))) => {
                    sanitize_stale_dolby_brand(&mut init);
                    return Ok((consumed, init));
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(error) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("parsing the generation's pipe: {error}"),
                    ));
                }
            }
        }
    }
}

/// The persisted form of [`InitIdentity`] — a serde mirror, because the type
/// itself does not derive `Serialize`.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredIdentity {
    muxer_init: String,
    served_init: String,
    promotion: plurx_core::fmp4::PromotionInputs,
}

pub(super) async fn load_identity(path: &Path) -> Option<InitIdentity> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let stored: StoredIdentity = serde_json::from_slice(&bytes).ok()?;
    Some(InitIdentity {
        muxer_init: stored.muxer_init,
        served_init: stored.served_init,
        promotion: stored.promotion,
    })
}

/// Persist the identity tmp-then-rename, like every other rendition file:
/// absent or complete, never partial.
#[cfg(test)]
pub(super) async fn store_identity(path: &Path, identity: &InitIdentity) -> io::Result<()> {
    store_identity_observed(path, identity, None, None).await
}

pub(super) async fn store_identity_observed(
    path: &Path,
    identity: &InitIdentity,
    allowance: Option<&Arc<super::copy_preparation::PreparationAllowance>>,
    resume_work: Option<&super::resume_preparation::ResumePreparation>,
) -> io::Result<()> {
    let stored = StoredIdentity {
        muxer_init: identity.muxer_init.clone(),
        served_init: identity.served_init.clone(),
        promotion: identity.promotion.clone(),
    };
    let bytes = serde_json::to_vec(&stored)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if resume_work.is_some_and(|work| !work.charge(bytes.len() as u64)) {
        return Err(io::ErrorKind::OutOfMemory.into());
    }
    let pending = allowance
        .map(|allowance| {
            allowance
                .begin(bytes.len() as u64)
                .ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))
        })
        .transpose()?;
    let reservation = if resume_work.is_some() {
        crate::playback_cache::reserve_without_eviction(path, bytes.len() as u64).await?
    } else {
        crate::playback_cache::reserve(path, bytes.len() as u64).await?
    };
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, &bytes).await?;
    tokio::fs::rename(&tmp, path).await?;
    if let Some(reservation) = reservation {
        reservation.commit(bytes.len() as u64);
    }
    if let Some(pending) = pending {
        pending.commit(false);
    }
    Ok(())
}

pub(super) async fn sync_file(path: &Path) -> io::Result<()> {
    match tokio::fs::File::open(path).await {
        Ok(file) => file.sync_all().await,
        // No identity yet is a rendition that never generated — admissible
        // only in tests that fake materialization; nothing to sync.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
