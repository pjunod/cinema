use super::*;

/// Only `segNNNNN.ts` names are valid segment requests.
pub(crate) fn is_safe_segment(name: &str) -> bool {
    // fMP4 (copy-video) HLS: one init object per ownership generation.
    if is_init_object(name) {
        return true;
    }
    // `segNNNNN.ts` (transcode) or `segNNNNN.m4s` (copy fMP4).
    name.strip_prefix("seg")
        .and_then(|rest| {
            rest.strip_suffix(".ts")
                .or_else(|| rest.strip_suffix(".m4s"))
        })
        .map(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or(false)
}

/// The numeric index of a segment filename (`segNNNNN.ts`/`.m4s`), or None for
/// the init segment, the playlist, or anything else.
pub(super) fn segment_index(name: &str) -> Option<i64> {
    name.strip_prefix("seg")
        .and_then(|rest| {
            rest.strip_suffix(".ts")
                .or_else(|| rest.strip_suffix(".m4s"))
        })
        .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|d| d.parse::<i64>().ok())
}

pub(super) fn producer_request_beyond_frontier(
    requested_segment: Option<i64>,
    published_segment: Option<i64>,
) -> bool {
    match (requested_segment, published_segment) {
        (Some(requested), Some(published)) => requested > published,
        // A missing initialization/unnumbered object cannot be produced after
        // the actor has ended the process, and no numbered segment was ever
        // published when the frontier is absent.
        (_, None) | (None, Some(_)) => true,
    }
}

/// Delete published segments that have fallen out of the retention window.
///
/// The window is measured back from the client's DOWNLOAD frontier, not from
/// its playhead, and is wide enough to cover the difference (see
/// [`RETENTION_SECS`]). The previous version counted a fixed number of
/// segments back from the furthest one fetched, which was two mistakes at
/// once: a segment count is not a duration on the copy path, and the frontier
/// is not where the viewer is. On the physical iPad that reproduced this,
/// AVPlayer fetched about 120 seconds ahead despite a 60-second preference;
/// retaining only 120 seconds therefore moved the playlist start onto the
/// playhead and left no reload margin.
///
/// `init.mp4` and the playlist are never candidates — neither carries an
/// EXTINF, so neither appears in the index.
fn ensure_retention_cleanup(session: &Session) {
    let should_start = {
        let queue = session
            .retention_cleanup_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !queue.is_empty()
            && session
                .retention_cleanup_active
                .compare_exchange(false, true, AcqRel, Acquire)
                .is_ok()
    };
    if !should_start {
        return;
    }

    #[cfg(test)]
    let cleanup_pause = session
        .retention_delete_pause
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let queue = Arc::clone(&session.retention_cleanup_queue);
    let active = Arc::clone(&session.retention_cleanup_active);
    let garbage_bytes = Arc::clone(&session.retention_garbage_bytes);
    let live_bytes = Arc::clone(&session.live_bytes);
    // The ledger entry outlives this Session's registry membership, so the
    // detached worker carries the key rather than a reference to the session.
    let scratch = session
        .scratch
        .as_ref()
        .map(|permit| (Arc::clone(permit.ledger()), permit.key()));
    tokio::spawn(async move {
        #[cfg(test)]
        if let Some(pause) = &cleanup_pause {
            pause.wait().await;
            pause.wait().await;
        }
        let pass_len = queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        for _ in 0..pass_len {
            let next = queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .first()
                .cloned();
            let Some((path, _)) = next else {
                break;
            };
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(
                        target: "plurxd::transcode",
                        garbage = ?path.file_name(),
                        %error,
                        "retention garbage cleanup failed; bytes remain charged for retry"
                    );
                    // Do not let one permanently busy/corrupt entry strand
                    // the independently deletable tail. Rotate this failure
                    // behind the current pass; its accounting remains intact
                    // and the next repair tick retries it once.
                    let mut queued = queue
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(position) = queued
                        .iter()
                        .position(|(queued_path, _)| queued_path == &path)
                    {
                        let failed = queued.remove(position);
                        queued.push(failed);
                    }
                    continue;
                }
            }

            // Serialize the queue removal and both accounting mutations with
            // refresh/reset stores. Otherwise a refresh between two atomic
            // decrements could publish a double-subtracted live total.
            let mut queued = queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(position) = queued
                .iter()
                .position(|(queued_path, _)| queued_path == &path)
            {
                let bytes = queued.remove(position).1;
                garbage_bytes.store(garbage_bytes.load(Relaxed).saturating_sub(bytes), Release);
                let live = live_bytes.load(Relaxed).saturating_sub(bytes);
                live_bytes.store(live, Release);
                if let Some((ledger, key)) = scratch.as_ref() {
                    // Not a walk: writes that landed since the last one are
                    // not in `live`, and must stay charged.
                    ledger.observe_unlinked(*key, live);
                }
            }
        }
        active.store(false, Release);
        #[cfg(test)]
        if let Some(pause) = &cleanup_pause {
            pause.wait().await;
        }
    });
}

pub(super) async fn gc_expired_segments(session: &Session) {
    // Never against a cache entry. Retention exists to bound the scratch a
    // live encoder is producing; a cached asset is a finished artifact that
    // other viewers will want whole. Pruning one would eat it from the front
    // as somebody watched — and leave the row still saying `complete`, so the
    // next viewer gets a playlist whose opening segments 404.
    if session.cached {
        return;
    }
    // A failed detached unlink remains queued and charged. The next repair
    // tick retries it, while the active/queued check below refuses to hand off
    // a second batch and thus bounds each session to one cleanup owner.
    ensure_retention_cleanup(session);
    // Segment names are reused by a replacement. Sample the attempt before
    // waiting, then own the producer transition through a bounded metadata
    // handoff to unservable, attempt-private names. A replacement that won
    // first makes the sample stale; one that arrives later cannot clear/reseed
    // the directory until no predecessor delete still names a served path.
    let producer_attempt = session.control.current_producer_attempt();
    let producer_transition = session.child_transition.lock().await;
    if session.retention_cleanup_active.load(Acquire)
        || !session
            .retention_cleanup_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    {
        return;
    }
    if session.replacing_child.load(Acquire)
        || session.control.current_producer_attempt() != producer_attempt
        || session.compatibility_producer_attempt() != producer_attempt
    {
        return;
    }
    let frontier = session.fetched_end_ms.load(Relaxed);
    let keep_from = frontier - RETENTION_SECS * 1000;
    if keep_from > 0 {
        let first_after_retention = {
            let index = session.segments.lock().await;
            index
                .prunable(keep_from)
                .last()
                .map(|segment| segment.index.saturating_add(1))
        };
        if let Some(first_after_retention) = first_after_retention {
            let mut clock = session.publication.lock().await;
            clock.retention_first_segment = Some(
                clock
                    .retention_first_segment
                    .map_or(first_after_retention, |current| {
                        current.max(first_after_retention)
                    }),
            );
        }
    }
    let now = Instant::now();
    let expired: Vec<(String, i64)> = {
        let index = session.segments.lock().await;
        index
            .segs
            .iter()
            .filter(|segment| {
                matches!(
                    segment.visibility,
                    SegmentVisibility::Grace { serve_until, .. } if now >= serve_until
                )
            })
            .take(RETENTION_HANDOFF_BATCH)
            .map(|segment| (segment.name.clone(), segment.bytes.max(0)))
            .collect()
    };
    if expired.is_empty() {
        return;
    }
    let sweep = RETENTION_SWEEP_ID.fetch_add(1, Relaxed);
    let mut moved = Vec::with_capacity(expired.len());
    for (name, indexed_bytes) in expired {
        let garbage = session.dir.join(format!(
            ".plurx-retention-{producer_attempt}-{sweep}-{name}"
        ));
        match tokio::fs::rename(session.dir.join(&name), &garbage).await {
            Ok(()) => {
                let measured_bytes = match tokio::fs::metadata(&garbage).await {
                    Ok(metadata) => i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                    Err(error) => {
                        tracing::warn!(
                            target: "plurxd::transcode",
                            producer_attempt,
                            segment = %name,
                            %error,
                            indexed_bytes,
                            "retention could not remeasure handed-off bytes; retaining the completed-segment accounting"
                        );
                        indexed_bytes
                    }
                };
                moved.push((name, garbage, measured_bytes.max(indexed_bytes)));
            }
            Err(error) => tracing::warn!(
                target: "plurxd::transcode",
                producer_attempt,
                segment = %name,
                %error,
                "retention could not hand off a predecessor segment"
            ),
        }
    }
    if moved.is_empty() {
        return;
    }
    // Forget sizes only for grace paths whose rename completed. A failed
    // handoff leaves the original promised identity and accounting intact for
    // the next sweep, but the response path already refuses it at deadline.
    let moved_paths: std::collections::HashMap<&str, (&PathBuf, i64)> = moved
        .iter()
        .map(|(name, path, bytes)| (name.as_str(), (path, *bytes)))
        .collect();
    let mut index = session.segments.lock().await;
    let mut garbage = Vec::with_capacity(moved.len());
    for seg in index.segs.iter_mut() {
        if let Some((path, bytes)) = moved_paths.get(seg.name.as_str()) {
            garbage.push(((*path).clone(), *bytes));
            seg.bytes = 0;
            seg.visibility = SegmentVisibility::Deleted;
        }
    }
    index.revision = index.revision.wrapping_add(1);
    let handed_off_bytes = garbage
        .iter()
        .fold(0_i64, |total, (_, bytes)| total.saturating_add(*bytes));
    {
        let mut cleanup_queue = session
            .retention_cleanup_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cleanup_queue.extend(garbage);
        let garbage_bytes = session
            .retention_garbage_bytes
            .load(Relaxed)
            .saturating_add(handed_off_bytes);
        session
            .retention_garbage_bytes
            .store(garbage_bytes, Release);
        // Rename changes served ownership, not disk use. Keep every handed-off
        // byte charged until detached cleanup confirms the path is absent.
        let live = index.total_bytes().saturating_add(garbage_bytes);
        session.live_bytes.store(live, Relaxed);
        // And say so in the ledger in the same breath. The periodic walk
        // would find these files anyway -- they are regular files in the same
        // flat directory -- but "anyway" is up to a poll interval away, and
        // the global cap is compared against the ledger on every admission.
        // Not a walk, so it leaves the landed-since-last-walk bytes alone.
        if let Some(permit) = session.scratch.as_ref() {
            permit.ledger().observe_unlinked(permit.key(), live);
        }
    }
    drop(index);
    drop(producer_transition);

    // Slow payload unlinks neither own the producer-path transition nor stall
    // the sequential global repair loop. Hidden names are not valid HLS
    // objects, and a successor may safely reuse every original segment name.
    ensure_retention_cleanup(session);
}

/// A sensible video bitrate (kbps) for a target height.
/// Wall-clock milliseconds, for comparing a stored prior's age against the
/// starvation verdict's TTL.
pub(super) fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub(super) fn read_scratch_sample(
    generation: &AtomicU64,
    bytes: &AtomicI64,
    sampled_at_unix_ms: &AtomicI64,
) -> (i64, i64) {
    for _ in 0..3 {
        let before = generation.load(Acquire);
        if before & 1 != 0 {
            std::hint::spin_loop();
            continue;
        }
        let bytes = bytes.load(Relaxed);
        let sampled_at = sampled_at_unix_ms.load(Relaxed);
        // Conventional seqlock reader ordering: data loads must complete
        // before the relaxed validation read, while the fence pairs with the
        // writer's release publication.
        std::sync::atomic::fence(Acquire);
        let after = generation.load(Relaxed);
        if before == after {
            return (bytes, sampled_at);
        }
    }
    (0, 0)
}

pub(super) fn fresh_scratch_bytes(bytes: i64, sampled_at_unix_ms: i64, now_unix_ms: i64) -> i64 {
    let max_age_ms = i64::try_from(SCRATCH_SAMPLE_MAX_AGE.as_millis()).unwrap_or(i64::MAX);
    if sampled_at_unix_ms > 0
        && now_unix_ms >= sampled_at_unix_ms
        && now_unix_ms.saturating_sub(sampled_at_unix_ms) <= max_age_ms
    {
        bytes.max(0)
    } else {
        0
    }
}
