use super::*;

/// The CutPolicy the plans were derived from and every generation cuts with.
pub(super) fn shipped_policy(timescale: u32) -> CutPolicy {
    CutPolicy::new(
        COPY_SEGMENT_SECONDS,
        COPY_FIRST_SEGMENT_SECONDS,
        COPY_SEGMENT_MAX_BYTES,
        COPY_SEGMENT_MAX_SECS,
        timescale,
    )
}

/// The rendition key: the copy recipe EXCLUDING `start_seconds` (the cache's
/// key discipline, plan §2.4), hashed into a directory-safe hex name.
pub(super) fn rendition_key(recipe: &Recipe, identity: &SourceIdentity) -> String {
    let mut hasher = Sha256::new();
    hasher.update(recipe.file.id.to_le_bytes());
    hasher.update(identity.size.to_le_bytes());
    hasher.update(identity.mtime_ms.to_le_bytes());
    hasher.update(identity.argv_fingerprint.as_bytes());
    hasher.update(recipe.audio_index.unwrap_or(-1).to_le_bytes());
    hasher.update([
        u8::from(recipe.aac),
        u8::from(recipe.video.preserves_dolby_vision()),
        u8::from(recipe.video.promotes_parameter_sets()),
    ]);
    hasher.update(recipe.file.audio_offset_ms.to_le_bytes());
    match recipe.cluster_cache_key.as_deref() {
        Some(cache_key) => {
            hasher.update(b"cluster-v2\0");
            hasher.update(cache_key.as_bytes());
        }
        None => hasher.update(b"legacy-v1\0"),
    }
    hex::encode(hasher.finalize())
}

/// The per-track durations `plan_copy`'s audio tail (the c58a4307 rule) is
/// computed from — and the split matters more than either number:
///
/// - `video_ms` comes honestly from the **fragment index** (the video-only
///   pipe's own summed ticks), because the tail is `audio_ms - video_ms` and
///   the container duration is the max of both tracks. Using the container
///   number for video makes the tail identically zero on every file, so any
///   title whose audio outruns its video would emit trailing audio the plan
///   never named — an out-of-plan chunk that poisons the rendition at the end
///   of every complete watch.
/// - `audio_ms` is the container's probed duration: audio is the track that
///   outruns, and the probe's number is what the c58a4307 rule was written
///   against.
pub(super) fn track_durations(
    index: &FragmentIndex,
    recipe: &Recipe,
    container_ms: i64,
) -> TrackDurations {
    TrackDurations {
        video_ms: index_video_ms(index),
        audio_ms: container_ms,
        audio_bits_per_second: audio_rate(recipe),
    }
}

/// The index's total video duration in ms — the sum of its fragment ticks on
/// its own timescale.
pub(super) fn index_video_ms(index: &FragmentIndex) -> i64 {
    ticks_to_ms(index.ticks(), index.timescale)
}

pub(super) fn ticks_to_ms(ticks: u64, timescale: u32) -> i64 {
    (ticks.saturating_mul(1000) / u64::from(timescale.max(1))) as i64
}

/// The audio rate the plan's byte headroom is computed from: what the
/// production pipe asks for (`-b:a`, mirrored from `copy_pipe_args`' branch)
/// when the audio is re-encoded, and a deliberately generous stand-in when it
/// is copied — `MediaFile` carries no per-stream audio rate, and `est_bytes`
/// feeds admission, never a refusal.
fn audio_rate(recipe: &Recipe) -> u32 {
    if recipe.aac {
        let channels = match recipe.audio_index {
            Some(index) => recipe
                .file
                .audio_streams
                .iter()
                .find(|stream| stream.index == index),
            None => recipe.file.audio_streams.first(),
        }
        .and_then(|stream| stream.channels);
        if channels == Some(6) {
            320_000
        } else {
            256_000
        }
    } else {
        640_000
    }
}

/// `segNNNNN.m4s` → its plan index; anything else — traversal included, by
/// the same digit discipline `is_safe_segment` enforces — is `None`.
pub(super) fn planned_index(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("seg")?.strip_suffix(".m4s")?;
    if digits.is_empty() || digits.len() > 9 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The plan entry containing `start_seconds` — where the session's first
/// demand points, not where the plan starts. Always a VIDEO entry: a start
/// inside the audio tail positions at the last video entry instead, because
/// that is the generation that produces the tail.
pub(super) fn entry_containing(plan: &SegmentPlan, start_seconds: f64) -> u32 {
    if start_seconds <= 0.0 {
        return video_entry_at_or_before(plan, 0);
    }
    let ticks = (start_seconds * f64::from(plan.timescale.max(1))) as u64;
    let mut at = 0u32;
    for entry in &plan.entries {
        if entry.start_ticks <= ticks {
            at = entry.index;
        } else {
            break;
        }
    }
    video_entry_at_or_before(plan, at)
}

/// Apply one accepted control snapshot to the playback's speculative ledger.
/// The accepted playback anchor has already committed with the control
/// sequence. Marker attribution uses only ranges credited by `RenditionSink`
/// and still materialized at this exact instant.
pub(super) async fn apply_marker_prewarm_control(
    rendition: &Arc<Rendition>,
    session_id: &str,
    sequence: u64,
    snapshot: &crate::playback_control::PlaybackDemandSnapshot,
    destinations: &[MarkerDestination],
) -> Option<MarkerPrewarmOutcome> {
    let ledger = rendition
        .readers
        .lock()
        .await
        .get(session_id)
        .map(|reader| Arc::clone(&reader.marker_prewarm))?;
    let manifest = rendition.manifest.lock().await;
    let publications = rendition
        .publication_versions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut ledger = ledger
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // A landing observed before its fire-and-forget beacon is a one-control
    // reorder latch, not durable skip intent. Any later accepted control
    // proves the beacon did not accompany that landing; retaining it would
    // let a natural traversal manufacture a hit on a future replay.
    ledger.awaiting_beacon = None;

    let outcome = if snapshot.render_state == crate::playback_control::RenderState::Seeking {
        ledger.enabled = false;
        ledger.deactivate();
        None
    } else {
        let landing_entry =
            entry_containing(&rendition.plan, snapshot.position_ms as f64 / 1_000.0);
        let outcome = ledger.observe_control_landing(
            snapshot.position_ms,
            landing_entry,
            &manifest,
            &publications,
        );
        ledger.update_control(sequence, snapshot, destinations);
        outcome
    };
    drop(ledger);
    drop(publications);
    drop(manifest);
    rendition.kick();
    outcome
}

/// Read only the persisted annotation index and project each exact client
/// landing time onto the immutable VOD plan. A store miss is intentionally a
/// no-op: marker probing belongs to `/decision`, and prewarm must never add a
/// detector or source read to the control path.
pub(super) async fn stored_marker_destinations(
    store: &dyn Store,
    file: &MediaFile,
    plan: &SegmentPlan,
    seconds_per_segment: f64,
) -> Vec<MarkerDestination> {
    let source = crate::http::stream::annotation_source_identity(file);
    let set = match store.timeline_annotation_set(file.id, &source).await {
        Ok(Some(set)) => set,
        Ok(None) => return Vec::new(),
        Err(error) => {
            tracing::warn!(
                file_id = file.id,
                %error,
                "could not read timeline annotations for marker prewarm"
            );
            return Vec::new();
        }
    };
    let per = if seconds_per_segment > 0.0 {
        seconds_per_segment
    } else {
        1.0
    };
    let window_entries = ((f64::from(AHEAD_HORIZON_SECONDS) / per).ceil() as u32).max(1);
    let last_entry = plan.entries.last().map_or(0, |entry| entry.index);
    let mut destinations = set
        .annotations
        .into_iter()
        .map(|annotation| {
            let target_entry = entry_containing(plan, annotation.end_ms as f64 / 1_000.0);
            MarkerDestination {
                kind: annotation.kind,
                start_ms: annotation.start_ms,
                end_ms: annotation.end_ms,
                target_entry,
                window_end_entry: target_entry.saturating_add(window_entries).min(last_entry),
                eligible: matches!(
                    annotation.kind,
                    AnnotationKind::Intro | AnnotationKind::Credits
                ),
            }
        })
        .collect::<Vec<_>>();
    destinations.sort_by_key(|destination| (destination.start_ms, destination.end_ms));
    destinations
}

/// The last VIDEO entry at or before `at` — the only kind a generation may be
/// positioned on. Audio-tail entries carry no video boundary of their own
/// ([`crate::titlestore::Manifest::is_audio_tail`]'s "a scheduler must not
/// chase them as if they were independently seekable" — this is that caller
/// arriving): `vodgen` categorically refuses a generation started inside the
/// tail, so an unclamped spawn there would answer `producer_failed` on every
/// seek to the end of an affected film. The tail entries are produced by this
/// generation's own `finish`.
pub(super) fn video_entry_at_or_before(plan: &SegmentPlan, at: u32) -> u32 {
    let mut best = 0u32;
    for entry in &plan.entries {
        if entry.index > at {
            break;
        }
        if entry.kind == PlanEntryKind::Video {
            best = entry.index;
        }
    }
    best
}

pub(super) fn plan_duration_ms(plan: &SegmentPlan) -> i64 {
    (plan.duration_ticks().saturating_mul(1000) / u64::from(plan.timescale.max(1))) as i64
}

/// The current playback window, never the interval between every position
/// visited during this session. Admitted GETs have independent narrow pins,
/// including the publication-to-open race, so seeking does not need to retain
/// the entire intervening film.
pub(super) fn reader_window(reader: &Reader, seconds_per_segment: f64) -> ReaderWindow {
    let playhead = reader.frontier;
    let frontier = reader.frontier.saturating_add(1);
    let per = if seconds_per_segment > 0.0 {
        seconds_per_segment
    } else {
        1.0
    };
    // `ceil`, matching `Position::horizon_segments`, which is what bounds how
    // far the producer may run. Truncating made the protected range shorter
    // than the range the ahead-fill is allowed to reach, so a sweep under
    // pressure could evict the segment the producer was about to write again.
    let ahead = ((f64::from(AHEAD_HORIZON_SECONDS) / per).ceil() as u32).max(1);
    ReaderWindow {
        back: 2,
        playhead,
        frontier,
        ahead,
    }
}

pub(super) async fn open_ready(
    path: &Path,
    etag_stem: &str,
    delivery: &Arc<crate::meter::Meter>,
) -> io::Result<SegmentReady> {
    let file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    Ok(SegmentReady {
        file,
        len,
        etag: format!("{etag_stem}-{len}"),
        delivery: Arc::clone(delivery),
    })
}

pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0)
}

pub(super) fn sub_saturating(counter: &AtomicU64, bytes: u64) {
    let _ = counter.fetch_update(Relaxed, Relaxed, |current| {
        Some(current.saturating_sub(bytes))
    });
}
