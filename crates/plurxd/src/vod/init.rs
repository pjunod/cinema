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
    let _permit = Arc::clone(slots)
        .try_acquire_owned()
        .map_err(|_| HeadRegenerationError::Busy)?;
    if !recipe_engine_is_current(recipe).await {
        return Err(HeadRegenerationError::Failed(
            "the immutable media engine changed before head regeneration".to_owned(),
        ));
    }
    if !source.unchanged() {
        return Err(HeadRegenerationError::Failed(
            "source changed before head regeneration".to_owned(),
        ));
    }
    let permit = if let Some(encoding) = &recipe.encoding {
        let permit = encoding.try_permit().await;
        encoding.cancel_wait();
        Some(permit.ok_or(HeadRegenerationError::Busy)?)
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
    let spawned = crate::producer_spawn::spawn(
        &program,
        &args,
        crate::producer_spawn::SpawnOptions {
            runtime_cache,
            progress: crate::producer_spawn::Progress::None,
            descriptors,
            env: &[],
        },
    )
    .map_err(|error| {
        HeadRegenerationError::Failed(format!("spawning the head regeneration: {error}"))
    })?;
    let mut owner = HeadChildOwner::new_job_owned(spawned.child, spawned.child_job);
    owner.permit = permit;
    let stdout = spawned.stdout;
    let stderr = spawned.stderr;
    let (muxer, diagnostic) = tokio::join!(
        read_regenerated_head_before(
            owner,
            stdout,
            HEAD_REGENERATION_TIMEOUT,
            HEAD_REGENERATION_MAX_BYTES,
        ),
        crate::ffmpeg::drain_diagnostics(stderr)
    );
    if !diagnostic.trim().is_empty() {
        tracing::warn!(target: "plurxd::vodserve", %diagnostic, "VOD head regeneration diagnostic");
    }
    let muxer = muxer?;
    if !source.unchanged() {
        return Err(HeadRegenerationError::Failed(
            "source changed during head regeneration".to_owned(),
        ));
    }
    if !recipe_engine_is_current(recipe).await {
        return Err(HeadRegenerationError::Failed(
            "the immutable media engine changed during head regeneration".to_owned(),
        ));
    }
    identity
        .served_init_for(&muxer)
        .map_err(|refused| HeadRegenerationError::Failed(refused.to_string()))
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
    // The stdout arrives separately from the child because the owner below
    // takes the child by value, and the read needs the pipe after that move.
    let mut child = child.into();
    let mut stdout = stdout;
    let head = tokio::time::timeout(budget, read_muxer_init_bounded(&mut stdout, max_bytes)).await;
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
    let mut consumed = Vec::new();
    let mut reader = FragmentReader::new();
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
pub(super) async fn store_identity(path: &Path, identity: &InitIdentity) -> io::Result<()> {
    let stored = StoredIdentity {
        muxer_init: identity.muxer_init.clone(),
        served_init: identity.served_init.clone(),
        promotion: identity.promotion.clone(),
    };
    let bytes = serde_json::to_vec(&stored)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, &bytes).await?;
    tokio::fs::rename(&tmp, path).await
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
