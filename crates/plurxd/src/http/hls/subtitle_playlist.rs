use super::*;

/// One native WebVTT rendition's media playlist. Its segments mirror the
/// video rendition so AVPlayer sees matching playlist types and timelines.
/// Every child resource is still cut from the one cached sidecar.
pub async fn subtitle_playlist(
    State(state): State<AppState>,
    AxPath((session, index)): AxPath<(String, i64)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (playlist_deadline, request_deadline) = playlist_request_deadlines(&state);
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::SubtitlePlaylist { index },
        RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await?
    {
        return Ok(response);
    }
    subtitle_playlist_local_before(&state, &session, index, playlist_deadline, request_deadline)
        .await
}

#[cfg(test)]
pub(super) async fn subtitle_playlist_local(
    state: &AppState,
    session: &str,
    index: i64,
) -> Result<Response, ApiError> {
    let (playlist_deadline, request_deadline) = playlist_request_deadlines(state);
    subtitle_playlist_local_before(state, session, index, playlist_deadline, request_deadline).await
}

pub(super) async fn subtitle_playlist_local_before(
    state: &AppState,
    session: &str,
    index: i64,
    playlist_deadline: Instant,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    let initial_vod_deadline = response_publication_deadline_before(request_deadline);
    // Resolve the typed rolling playlist verdict before asking for frozen
    // subtitle facts. A failed actor is stored before End; consulting the
    // live-only presentation facade first would erase that still-registered
    // failure into a 404 and bypass its exact 502 owner fence.
    if let Some(answer) = vod_playlist_before(state, session, initial_vod_deadline).await? {
        let (video, owner) = admitted_vod_publication(
            state,
            session,
            answer,
            "vod-subtitle-playlist",
            None,
            initial_vod_deadline,
        )
        .await?;
        return complete_subtitle_playlist_response(
            state,
            session,
            index,
            video,
            owner,
            initial_vod_deadline,
        )
        .await;
    }

    let mut publication_deadline = None;
    for reclassification in 0..=2 {
        let (video, owner) = match state
            .transcode
            .playlist_with_owner_before(session, playlist_deadline)
            .await
        {
            Ok(answer) => answer,
            Err(err) => {
                if matches!(&err.error, PlaylistError::SessionGone) {
                    match vod_resurrected_before(state, session, playlist_deadline).await {
                        VodResurrection::Absent => {}
                        VodResurrection::Ended => return Err(media_session_ended()),
                        VodResurrection::OwnerLost(resume) => return Err(media_owner_lost(resume)),
                        VodResurrection::Unavailable => return Err(vod_resurrection_unavailable()),
                        VodResurrection::Resurrected => {
                            let answer = tokio::time::timeout_at(
                                tokio::time::Instant::from_std(playlist_deadline),
                                state.transcode.vod_playlist(session),
                            )
                            .await
                            .map_err(|_| vod_resurrection_unavailable())?
                            .ok_or_else(vod_resurrection_unavailable)?;
                            let response_deadline =
                                *publication_deadline.get_or_insert_with(|| {
                                    response_publication_deadline_before(request_deadline)
                                });
                            let (video, owner) = admitted_vod_publication(
                                state,
                                session,
                                answer,
                                "vod-subtitle-playlist",
                                None,
                                response_deadline,
                            )
                            .await?;
                            return complete_subtitle_playlist_response(
                                state,
                                session,
                                index,
                                video,
                                owner,
                                response_deadline,
                            )
                            .await;
                        }
                    }
                }
                match admitted_playlist_error(
                    state,
                    session,
                    err,
                    *publication_deadline.get_or_insert_with(|| {
                        response_publication_deadline_before(request_deadline)
                    }),
                )
                .await
                {
                    Ok(error) => return Err(error),
                    Err(())
                        if reclassification < 2
                            && tokio::time::Instant::now().into_std()
                                < publication_deadline
                                    .expect("publication deadline initialized") =>
                    {
                        continue;
                    }
                    Err(()) => {
                        return Err(ApiError::typed(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "playlist_state_changed",
                            "the stream changed state while the subtitle playlist was prepared; retry shortly",
                        ));
                    }
                }
            }
        };
        let result = complete_subtitle_playlist_response(
            state,
            session,
            index,
            video,
            owner,
            *publication_deadline
                .get_or_insert_with(|| response_publication_deadline_before(request_deadline)),
        )
        .await;
        match result {
            Err(error)
                if response_publication_state_changed(&error)
                    && reclassification < 2
                    && tokio::time::Instant::now().into_std()
                        < publication_deadline.expect("publication deadline initialized") =>
            {
                continue;
            }
            result => return result,
        }
    }
    unreachable!("bounded subtitle playlist reclassification loop always returns")
}

async fn complete_subtitle_playlist_response(
    state: &AppState,
    session: &str,
    index: i64,
    video: Vec<u8>,
    owner: crate::transcode::MediaResponseOwner,
    deadline: Instant,
) -> Result<Response, ApiError> {
    let (_, file) = match state
        .transcode
        .hls_presentation_for_owner_before(session, &owner, deadline)
        .await
    {
        Ok(presentation) => presentation,
        Err(rejection) => {
            return Err(
                response_publication_rejection_before(state, session, rejection, deadline).await,
            )
        }
    };
    let Some(track) = file.subtitle_streams.get(index as usize) else {
        authorize_attempt_status(state, session, &owner, "subtitle-playlist", None, deadline)
            .await?;
        return Err(ApiError::NotFound("subtitle track"));
    };
    if !is_native_text_subtitle(&track.codec) {
        authorize_attempt_status(state, session, &owner, "subtitle-playlist", None, deadline)
            .await?;
        return Err(ApiError::BadRequest(
            "this subtitle requires burn-in".into(),
        ));
    }
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        crate::subtitles::warm_vtt_with_store(
            &state.subs_dir,
            &file,
            index,
            &state.subtitle_source_access(),
        ),
    )
    .await
    .map_err(|_| response_publication_timeout())?;
    let response = playlist_response(subtitle_media_playlist(&video).into_bytes());
    #[cfg(test)]
    state
        .transcode
        .pause_subtitle_playlist_commit_for_test()
        .await;
    // Carry the owner resolved with the exact video bytes. A rolling wait may
    // span fallback, and a VOD attachment may be replaced under the same id;
    // a fresh lookup here would authorize the wrong incarnation in both cases.
    let object_name = format!("subs/{index}/index.m3u8");
    complete_buffered_response_before(
        state,
        session,
        &owner,
        crate::transcode::MediaResponsePublication::attempt_media(
            "subtitle-playlist",
            Some(&object_name),
        ),
        true,
        response,
        deadline,
    )
    .await
}

/// Capability-authenticated VTT data for AVPlayer's autonomous child fetch.
/// Each resource mirrors one video segment's time window. Cues are shifted
/// onto the session-relative video timeline, so a session opened at a
/// resume/seek offset still presents captions at the right frame.
pub async fn subtitle_vtt(
    State(state): State<AppState>,
    AxPath((session, index, segment)): AxPath<(String, i64, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let request_deadline = response_publication_deadline();
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::SubtitleSegment {
            index,
            segment: segment.clone(),
        },
        RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await?
    {
        return Ok(response);
    }
    subtitle_vtt_local_before(&state, &session, index, &segment, request_deadline).await
}

pub(super) trait SubtitleSegmentSource: Send + Sync {
    fn read_whole<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>>;

    fn read_window<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
        anchor_seconds: i64,
        window_seconds: i64,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>>;

    fn warm_whole<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
    ) -> BoxFuture<'a, ()>;

    /// Start a bounded window for one playback.
    ///
    /// The session id and the control sequence it settled on are what let the
    /// subtitle owner answer M7's actual question — how many extractions does
    /// one viewer have running — rather than the key-shaped question of how
    /// many spans are in flight across the server.
    #[allow(clippy::too_many_arguments)]
    fn warm_window<'a>(
        &'a self,
        session: &'a str,
        sequence: Option<u64>,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
        anchor_seconds: i64,
        window_seconds: i64,
    ) -> BoxFuture<'a, bool>;

    /// Is a producer for exactly this window span alive right now?
    ///
    /// Only asked after a `read_window` miss, and only to decide whether
    /// waiting a moment longer could turn this request's answer from an empty
    /// track into real cues. On the trait rather than called directly so the
    /// boundary fixture measures the same decision production makes.
    fn window_flight_is_live<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
        anchor_seconds: i64,
        window_seconds: i64,
    ) -> BoxFuture<'a, bool>;

    /// What the whole-track sidecar for this track is doing. Observation
    /// only; it starts nothing.
    fn whole_track_state<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
    ) -> BoxFuture<'a, crate::subtitles::SidecarState>;
}

/// How long a subtitle segment may wait for a window that is already being
/// extracted for it.
///
/// AVPlayer gives a subtitle segment about two seconds and stalls the muxed
/// video while it waits, so this is not a budget to spend freely — it exists
/// to win the *last* moment of a warm, which is the common case at a window
/// boundary once the previous segment's request kicked the next window. A
/// request that arrives at the start of an extraction still gets its empty
/// answer immediately and leaves the recovery to the client's readiness
/// retry; only a flight already in progress is worth standing still for.
pub(super) const SUBTITLE_SEGMENT_PUBLICATION_WAIT: Duration = Duration::from_millis(1_500);

/// How often that wait re-reads the window path.
pub(super) const SUBTITLE_SEGMENT_PUBLICATION_POLL: Duration = Duration::from_millis(100);

/// What the wait leaves of the response publication budget for the work that
/// still has to happen after it: the whole-track warm, the settled-target
/// read, the window kick and the response publication itself. Without it a
/// request that caught its cues at the deadline fails on the very next await.
const SUBTITLE_SEGMENT_PUBLICATION_RESERVE: Duration = Duration::from_millis(750);

struct ProductionSubtitleSegmentSource(crate::subtitle_source::StoreAccess);

impl SubtitleSegmentSource for ProductionSubtitleSegmentSource {
    fn read_whole<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(crate::subtitles::read_cached_vtt_with_store(
            dir, file, index, &self.0,
        ))
    }

    fn read_window<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
        anchor_seconds: i64,
        window_seconds: i64,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(crate::subtitles::read_cached_window(
            dir,
            file,
            index,
            anchor_seconds,
            window_seconds,
        ))
    }

    fn warm_whole<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
    ) -> BoxFuture<'a, ()> {
        Box::pin(crate::subtitles::warm_vtt_with_store(
            dir, file, index, &self.0,
        ))
    }

    fn warm_window<'a>(
        &'a self,
        session: &'a str,
        sequence: Option<u64>,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
        anchor_seconds: i64,
        window_seconds: i64,
    ) -> BoxFuture<'a, bool> {
        Box::pin(crate::subtitles::warm_vtt_window(
            session,
            sequence,
            dir,
            file,
            index,
            anchor_seconds,
            window_seconds,
            &self.0,
        ))
    }

    fn window_flight_is_live<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
        anchor_seconds: i64,
        window_seconds: i64,
    ) -> BoxFuture<'a, bool> {
        Box::pin(crate::subtitles::window_flight_is_live(
            dir,
            file,
            index,
            anchor_seconds,
            window_seconds,
        ))
    }

    fn whole_track_state<'a>(
        &'a self,
        dir: &'a Path,
        file: &'a MediaFile,
        index: i64,
    ) -> BoxFuture<'a, crate::subtitles::SidecarState> {
        Box::pin(crate::subtitles::sidecar_state_with_store(
            dir, file, index, &self.0,
        ))
    }
}

/// Wait out the tail of a window extraction that is already running for this
/// exact span, and hand back its bytes if they land in time.
///
/// Bounded twice: by [`SUBTITLE_SEGMENT_PUBLICATION_WAIT`], which is the
/// engine's constraint, and by the response publication deadline, which is
/// the request's. Whichever is sooner wins, and missing both is not an error
/// — the caller's empty segment is still a correct answer.
async fn await_window_publication<S: SubtitleSegmentSource + ?Sized>(
    source: &S,
    dir: &Path,
    file: &MediaFile,
    index: i64,
    anchor: i64,
    window_seconds: i64,
    publication_deadline: Instant,
) -> Option<Vec<u8>> {
    // The handler still has to warm, settle and publish after this returns, so
    // the wait may not spend the whole remaining budget — a request that
    // caught its cues and then timed out on the work behind them has failed
    // *because* it succeeded. Leave the rest of the lifecycle its own slack.
    let reserved = publication_deadline.checked_sub(SUBTITLE_SEGMENT_PUBLICATION_RESERVE);
    let give_up = match reserved {
        Some(reserved) => (Instant::now() + SUBTITLE_SEGMENT_PUBLICATION_WAIT).min(reserved),
        // Already inside the reserve. Answer now.
        None => return None,
    };
    loop {
        // Read first: a window that published while this request was deciding
        // to wait is served without paying a poll interval for it, and the
        // loop cannot overshoot `give_up` by a whole sleep before noticing.
        if let Ok(Some(bytes)) = tokio::time::timeout_at(
            tokio::time::Instant::from_std(give_up),
            source.read_window(dir, file, index, anchor, window_seconds),
        )
        .await
        .unwrap_or(Ok(None))
        {
            return Some(bytes);
        }
        if Instant::now() + SUBTITLE_SEGMENT_PUBLICATION_POLL >= give_up {
            return None;
        }
        tokio::time::sleep(SUBTITLE_SEGMENT_PUBLICATION_POLL).await;
    }
}

pub(super) async fn subtitle_vtt_local_before(
    state: &AppState,
    session: &str,
    index: i64,
    segment: &str,
    publication_deadline: Instant,
) -> Result<Response, ApiError> {
    subtitle_vtt_local_before_with_source(
        state,
        session,
        index,
        segment,
        publication_deadline,
        &ProductionSubtitleSegmentSource(state.subtitle_source_access()),
    )
    .await
}

pub(super) async fn subtitle_vtt_local_before_with_source<S: SubtitleSegmentSource + ?Sized>(
    state: &AppState,
    session: &str,
    index: i64,
    segment: &str,
    publication_deadline: Instant,
    source: &S,
) -> Result<Response, ApiError> {
    let (context, file, owner) = session_file(state, session, publication_deadline).await?;
    let Some(track) = file.subtitle_streams.get(index as usize) else {
        authorize_attempt_status(
            state,
            session,
            &owner,
            "subtitle-segment",
            None,
            publication_deadline,
        )
        .await?;
        return Err(ApiError::NotFound("subtitle track"));
    };
    if !is_native_text_subtitle(&track.codec) {
        authorize_attempt_status(
            state,
            session,
            &owner,
            "subtitle-segment",
            None,
            publication_deadline,
        )
        .await?;
        return Err(ApiError::BadRequest(
            "this subtitle requires burn-in".into(),
        ));
    }
    let sequence = segment
        .strip_prefix("seg")
        .and_then(|value| value.strip_suffix(".vtt"))
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|sequence| i64::try_from(sequence).ok());
    let Some(sequence) = sequence else {
        authorize_attempt_status(
            state,
            session,
            &owner,
            "subtitle-segment",
            None,
            publication_deadline,
        )
        .await?;
        return Err(ApiError::NotFound("subtitle segment"));
    };
    let window = state
        .transcode
        .segment_window_for_owner_before(session, sequence, &owner, publication_deadline)
        .await;
    let window = match window {
        Ok(window) => window,
        Err(rejection) => {
            return Err(response_publication_rejection_before(
                state,
                session,
                rejection,
                publication_deadline,
            )
            .await)
        }
    };
    let Some((segment_start, segment_end)) = window else {
        authorize_attempt_status(
            state,
            session,
            &owner,
            "subtitle-segment",
            None,
            publication_deadline,
        )
        .await?;
        return Err(ApiError::NotFound("subtitle segment"));
    };
    let subtitle_window_seconds = tokio::time::timeout_at(
        tokio::time::Instant::from_std(publication_deadline),
        state.subtitle_window_seconds(),
    )
    .await
    .map_err(|_| response_publication_timeout())?;
    let (bytes, cache_control, slice_timeline) = match tokio::time::timeout_at(
        tokio::time::Instant::from_std(publication_deadline),
        source.read_whole(&state.subs_dir, &file, index),
    )
    .await
    .map_err(|_| response_publication_timeout())?
    {
        Ok(Some(bytes)) => {
            tracing::info!(
                target: "plurxd::http::hls",
                session = %crate::transcode::session_log_id(session),
                file_id = file.id,
                index,
                codec = %track.codec,
                language = track.language.as_deref().unwrap_or("und"),
                title = track.title.as_deref().unwrap_or(""),
                start_seconds = context.start_seconds,
                "serving native HLS WebVTT subtitle"
            );
            (bytes, "private, max-age=3600", true)
        }
        Ok(None) | Err(_) => {
            // The whole-track sidecar is not there yet. Before falling back to
            // an empty segment, ask whether a bounded window covering the
            // position this segment actually wants is available or worth
            // starting.
            //
            // Cue times in a sidecar are absolute source time and
            // `segment_start` is session-local, so the demand position is the
            // session's media origin plus this segment's start — the same
            // mapping `slice_webvtt` undoes below.
            let demand_seconds = (context.media_origin_seconds + segment_start).max(0.0) as i64;
            let anchor =
                crate::subtitles::window_anchor_seconds(demand_seconds, subtitle_window_seconds);
            let window_bytes = tokio::time::timeout_at(
                tokio::time::Instant::from_std(publication_deadline),
                source.read_window(
                    &state.subs_dir,
                    &file,
                    index,
                    anchor,
                    subtitle_window_seconds,
                ),
            )
            .await
            .map_err(|_| response_publication_timeout())?;
            // A window for this exact span may be seconds from publishing —
            // the common case at a window boundary, where the previous
            // segment's request already kicked this one. Standing still for
            // the tail of a flight that is *already running* turns an empty
            // segment into real cues without adding an extraction, and
            // without ever awaiting one that has not started.
            let window_bytes = match window_bytes {
                Ok(Some(bytes)) => Some(bytes),
                _ if source
                    .window_flight_is_live(
                        &state.subs_dir,
                        &file,
                        index,
                        anchor,
                        subtitle_window_seconds,
                    )
                    .await =>
                {
                    await_window_publication(
                        source,
                        &state.subs_dir,
                        &file,
                        index,
                        anchor,
                        subtitle_window_seconds,
                        publication_deadline,
                    )
                    .await
                }
                _ => None,
            };
            if let Some(bytes) = window_bytes {
                // Start the whole-track warm even though this request is
                // answered. A window is a bridge: it persists on disk across
                // restarts while the whole-track sidecar may not exist yet, so
                // returning here without warming would leave a viewer parked
                // past the first window served by a window forever, with the
                // authoritative extraction never kicked from this route.
                tokio::time::timeout_at(
                    tokio::time::Instant::from_std(publication_deadline),
                    source.warm_whole(&state.subs_dir, &file, index),
                )
                .await
                .map_err(|_| response_publication_timeout())?;
                tracing::info!(
                    target: "plurxd::http::hls",
                    session = %crate::transcode::session_log_id(session),
                    file_id = file.id,
                    index,
                    anchor,
                    "serving a windowed WebVTT subtitle while the whole track warms"
                );
                // `no-store`: a window covers this position and not the next
                // one, and the whole-track sidecar will supersede it. Letting
                // a player pin these bytes would pin a partial answer.
                (bytes, "no-store", true)
            } else {
                // AVPlayer gives a subtitle segment only about two seconds to
                // answer and blocks the muxed video while it waits. Extracting
                // an embedded text track is a full-source scan that can
                // legitimately take minutes on a large MKV over a NAS, so
                // awaiting `ensure_vtt` here turns healthy Dolby Vision, HDR
                // and H.264 streams into a black screen. Publish a
                // syntactically valid empty segment now and let the
                // deduplicated cache extraction finish independently.
                // `no-store` lets a player retry this window once the sidecar
                // is ready instead of pinning the temporary empty answer.
                //
                // Both warms are started, and neither is awaited. The window
                // is the bridge over the head of playback; the whole track is
                // what supersedes it and what every other consumer needs. The
                // window declines itself past the midpoint of the file, where
                // it would read the same bytes as the whole track for a
                // disposable result.
                // Read the whole track's state BEFORE warming it. `warm_vtt`
                // enlists the key in `warmups` synchronously and clears it
                // from a spawned task, and `sidecar_state` reports any
                // enlisted key as `Warming` — so asking afterwards races the
                // warm this very request just started, and a failed track
                // answers `Failed` or `Warming` depending on which task the
                // scheduler ran. A refusal that is a coin flip is worse than
                // no refusal.
                let whole_track = source
                    .whole_track_state(&state.subs_dir, &file, index)
                    .await;
                // The whole-track warm keeps its original contract, including
                // that a timeout here fails the request rather than being
                // swallowed: it is the path every other consumer depends on.
                // A track inside its failure memo is the one exception: the
                // warm cannot start an extraction while the memo stands, so
                // enlisting the key would buy nothing and would corrupt the
                // reading above for every request behind this one.
                if whole_track != crate::subtitles::SidecarState::Failed {
                    tokio::time::timeout_at(
                        tokio::time::Instant::from_std(publication_deadline),
                        source.warm_whole(&state.subs_dir, &file, index),
                    )
                    .await
                    .map_err(|_| response_publication_timeout())?;
                }
                // The destination is read here, immediately before the warm,
                // and not once at the top of the handler: a seek that lands
                // between the two reads is exactly the case M7 is about, and
                // the later read is the one that can still refuse the work.
                //
                // `None` is not staleness. It covers a session that is gone, an
                // actor that retired and a client that has not exchanged yet,
                // and in all three there is no ordering fact to be stale
                // against — so a first play still warms.
                let settled = tokio::time::timeout_at(
                    tokio::time::Instant::from_std(publication_deadline),
                    state.transcode.settled_target_for_session(session),
                )
                .await;
                // The window is best effort by construction — it is a bridge,
                // and the empty segment below is already a correct answer — so
                // a timeout means "no window", not a failed request.
                let windowing = match settled {
                    Ok(Some(target))
                        if !target.covered_by_window(anchor, subtitle_window_seconds) =>
                    {
                        // The client has settled somewhere this window does not
                        // reach. Starting it would spend a full-source scan on
                        // a destination that is already history, which is the
                        // waste the seek-coalescing contract exists to refuse.
                        false
                    }
                    Ok(authority) => tokio::time::timeout_at(
                        tokio::time::Instant::from_std(publication_deadline),
                        source.warm_window(
                            session,
                            authority.map(|target| target.sequence),
                            &state.subs_dir,
                            &file,
                            index,
                            anchor,
                            subtitle_window_seconds,
                        ),
                    )
                    .await
                    .unwrap_or(false),
                    // Reading the authority did not fit inside the publication
                    // deadline. Starting an extraction this request could not
                    // justify is the failure mode being removed, so the empty
                    // segment answers alone.
                    Err(_) => false,
                };
                tracing::debug!(
                    target: "plurxd::http::hls",
                    session = %crate::transcode::session_log_id(session),
                    file_id = file.id,
                    index,
                    anchor,
                    windowing,
                    "serving an empty subtitle segment while its sidecar cache warms"
                );
                // An empty segment says "there are no cues here", which is
                // true while a sidecar is warming and a lie once it has
                // failed — and players keep the bytes in memory whatever
                // `no-store` says, so the lie is what a client is left with.
                // A refusal with `Retry-After` is the honest answer, and the
                // memo's own remaining time is the only moment a retry could
                // achieve anything.
                //
                // Behind an operator switch, and off by default, because the
                // cost of being honest here is not yet measured: AVPlayer
                // blocks the muxed video for about two seconds on a subtitle
                // segment, and whether each engine keeps playing video
                // through a subtitle 503 or stalls the picture has to be
                // observed per engine before this becomes the default. The
                // Developer tab reports what has been observed and does not
                // gate the switch on it.
                if whole_track == crate::subtitles::SidecarState::Failed
                    && state.subtitle_not_ready_503().await
                {
                    let retry_after =
                        crate::subtitles::failure_memo_remaining(&state.subs_dir, &file, index)
                            .await
                            .map(|remaining| remaining.as_secs().max(1))
                            .unwrap_or(1);
                    tracing::info!(
                        target: "plurxd::http::hls",
                        session = %crate::transcode::session_log_id(session),
                        file_id = file.id,
                        index,
                        retry_after,
                        "refusing a subtitle segment whose sidecar extraction failed"
                    );
                    authorize_attempt_status(
                        state,
                        session,
                        &owner,
                        "subtitle-segment",
                        None,
                        publication_deadline,
                    )
                    .await?;
                    return Ok((
                        StatusCode::SERVICE_UNAVAILABLE,
                        [(header::RETRY_AFTER, retry_after.to_string())],
                        [(header::CACHE_CONTROL, "no-store")],
                    )
                        .into_response());
                }
                (b"WEBVTT\n\n".to_vec(), "no-store", false)
            }
        }
    };
    let response = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/vtt; charset=utf-8"),
            (header::CACHE_CONTROL, cache_control),
        ],
        // The session's MEDIA origin, not the offset that was requested. A
        // copy session seeks with `-noaccurate_seek`, so its timeline begins
        // at the keyframe before the requested start — shifting cues by the
        // request made them lead the picture by up to a whole GOP (1–6 s on a
        // 4K film) on every resumed or seeked copy session, which is the
        // flagship Apple path.
        if slice_timeline {
            slice_webvtt(
                &bytes,
                context.media_origin_seconds,
                segment_start,
                segment_end,
            )
        } else {
            // Keep the cold-cache fallback byte-for-byte minimal. It carries
            // no cues to shift, and this exact body is the acceptance handle
            // proving the request escaped before either producer finished.
            bytes
        },
    )
        .into_response();
    let object_name = format!("subs/{index}/{segment}");
    complete_buffered_response_before(
        state,
        session,
        &owner,
        crate::transcode::MediaResponsePublication::attempt_media(
            "subtitle-segment",
            Some(&object_name),
        ),
        true,
        response,
        publication_deadline,
    )
    .await
}

pub(super) fn quoted(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '"' => '\'',
            '\r' | '\n' => ' ',
            other => other,
        })
        .collect()
}

/// BCP-47 for the `LANGUAGE=` attribute. One line, because the knowledge of
/// which spellings mean the same language belongs to `plurx_core::tracks` —
/// this module used to keep a ten-language copy of it, which silently passed
/// "dut"/"cze"/"gre" through as non-BCP-47 and defeated viewer-language
/// matching for every language the copy had not learned.
pub(super) fn language_tag(raw: Option<&str>) -> &str {
    plurx_core::tracks::bcp47_tag(raw)
}
