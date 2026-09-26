use super::*;

/// GET /api/v1/hls/:session/index.m3u8 — capability auth (see module docs).
///
/// Existing clients receive the historical media playlist. The `?native=1`
/// form remains as a compatibility bridge for Apple sessions created before
/// the dedicated master-playlist path existed.
pub async fn playlist(
    State(state): State<AppState>,
    AxPath(session): AxPath<String>,
    Query(query): Query<PlaylistQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (playlist_deadline, request_deadline) = playlist_request_deadlines(&state);
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::Playlist {
            native: query.native,
            subtitle: query.subtitle,
        },
        RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await?
    {
        return Ok(response);
    }
    playlist_local_before(&state, &session, query, playlist_deadline, request_deadline).await
}

pub(super) async fn playlist_local_before(
    state: &AppState,
    session: &str,
    query: PlaylistQuery,
    playlist_deadline: Instant,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    let initial_vod_deadline = response_publication_deadline_before(request_deadline);
    // A VOD session's child media playlist is the plan's immutable artifact.
    // The dedicated master path wraps it when native subtitles were requested;
    // the legacy `?native=1` bridge still needs that same wrapper.
    if let Some(answer) = vod_playlist_before(state, session, initial_vod_deadline).await? {
        let (bytes, playlist_owner) = admitted_vod_publication(
            state,
            session,
            answer,
            "vod-playlist",
            None,
            initial_vod_deadline,
        )
        .await?;
        if query.native == Some(1) {
            let (context, file, owner) = session_file(state, session, initial_vod_deadline).await?;
            let context =
                exact_hls_context_before(state, session, context, &owner, initial_vod_deadline)
                    .await?;
            let response =
                playlist_response(master_playlist(&file, query.subtitle, &context).into_bytes());
            return complete_buffered_response_before(
                state,
                session,
                &owner,
                crate::transcode::MediaResponsePublication::generation_metadata("master-playlist"),
                true,
                response,
                initial_vod_deadline,
            )
            .await;
        }
        let response = playlist_response(bytes);
        return complete_buffered_response_before(
            state,
            session,
            &playlist_owner,
            crate::transcode::MediaResponsePublication::attempt_media(
                "playlist",
                Some("index.m3u8"),
            ),
            true,
            response,
            initial_vod_deadline,
        )
        .await;
    }
    if query.native != Some(1) {
        return video_playlist_local_before(
            state,
            session,
            "index.m3u8",
            playlist_deadline,
            request_deadline,
        )
        .await;
    }
    let deadline = response_publication_deadline_before(request_deadline);
    let (context, file, owner) = session_file(state, session, deadline).await?;
    let context = exact_hls_context_before(state, session, context, &owner, deadline).await?;
    let response = playlist_response(master_playlist(&file, query.subtitle, &context).into_bytes());
    complete_buffered_response_before(
        state,
        session,
        &owner,
        crate::transcode::MediaResponsePublication::generation_metadata("master-playlist"),
        true,
        response,
        deadline,
    )
    .await
}

/// The multivariant playlist used by Apple clients for native subtitles and
/// HDR variant signaling.
///
/// It has a distinct path from the child media playlist so AVPlayer cannot
/// collapse the two resources when applying URL-query cache normalization.
pub async fn master_playlist_response(
    State(state): State<AppState>,
    AxPath(session): AxPath<String>,
    Query(query): Query<PlaylistQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (playlist_deadline, request_deadline) = playlist_request_deadlines(&state);
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::Master {
            subtitle: query.subtitle,
            diagnostic: query.diagnostic.clone(),
        },
        RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await?
    {
        return Ok(response);
    }
    master_playlist_response_local_before(
        &state,
        &session,
        query,
        playlist_deadline,
        request_deadline,
    )
    .await
}

pub(super) async fn master_playlist_response_local_before(
    state: &AppState,
    session: &str,
    query: PlaylistQuery,
    playlist_deadline: Instant,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    let deadline = response_publication_deadline_before(request_deadline);
    let (context, file, owner) = session_file(state, session, deadline).await?;
    let context = exact_hls_context_before(state, session, context, &owner, deadline).await?;
    // Apple's multivariant eligibility check rejects UHD Blu-ray-style HEVC
    // High-tier declarations before VideoToolbox sees bytes it can decode.
    // With no native text renditions, the wrapper buys this session nothing:
    // serve the same proven fMP4 media playlist from this capability URL. The
    // initialization-segment response translates the tier declaration into
    // the Apple eligibility envelope without changing the coded picture.
    // Bitmap/styled subtitles already use burn-in, so they do not depend on a
    // multivariant subtitle group.
    if query.diagnostic.is_none() && should_serve_high_tier_media_playlist(&file, &context) {
        tracing::info!(
            target: "plurxd::http::hls",
            session = %crate::transcode::session_log_id(session),
            codecs = %context.codecs,
            "serving high-tier HEVC through the direct media-playlist envelope"
        );
        return video_playlist_local_before(
            state,
            session,
            "master.m3u8",
            playlist_deadline,
            request_deadline,
        )
        .await;
    }
    let response = playlist_response(
        master_playlist_diagnostic(&file, query.subtitle, &context, query.diagnostic.as_deref())
            .into_bytes(),
    );
    complete_buffered_response_before(
        state,
        session,
        &owner,
        crate::transcode::MediaResponsePublication::generation_metadata("master-playlist"),
        true,
        response,
        deadline,
    )
    .await
}

/// Translate a session's own verdict into the response the client acts on.
///
/// Every one of these used to be `ApiError::NotFound("transcode session")` — a
/// bare 404 that hls.js escalates to a fatal `levelLoadError` whatever caused
/// it, so a session still inside the server's startup recovery was reported to
/// the viewer as permanently broken and a session that had genuinely failed
/// was reported as nothing at all. The status separates the three cases the
/// client can actually act on differently, and the typed body carries the
/// reason a person can read.
///
/// - 404: the session is gone. Re-open, or accept that it is over.
/// - 503: still starting. **Retryable**, and hls.js's own level-load retry is
///   the right response — the stream is being built right now.
/// - 502: the producer or the session failed. Terminal; report it.
pub(super) fn playlist_error(session: &str, err: PlaylistError) -> ApiError {
    let status = match &err {
        PlaylistError::SessionGone => StatusCode::NOT_FOUND,
        PlaylistError::StartupTimedOut(_) => StatusCode::SERVICE_UNAVAILABLE,
        PlaylistError::ProducerExited(_)
        | PlaylistError::ProducerEnded(_)
        | PlaylistError::SessionFailed(_)
        | PlaylistError::InsufficientCapacity(_) => StatusCode::BAD_GATEWAY,
    };
    tracing::warn!(
        target: "plurxd::http::hls",
        session = %crate::transcode::session_log_id(session),
        code = err.code(),
        retryable = err.retryable(),
        "HLS playlist request refused: {}",
        err.message()
    );
    ApiError::typed(status, err.code(), err.message())
}

pub(super) async fn admitted_playlist_error(
    state: &AppState,
    session: &str,
    err: crate::transcode::PlaylistPublicationError,
    deadline: Instant,
) -> Result<ApiError, ()> {
    if let Some(owner) = err.owner.as_ref() {
        match state
            .transcode
            .authorize_playlist_error_publication(session, owner, &err.error, deadline)
            .await
        {
            Ok(()) => {}
            Err(crate::transcode::MediaResponsePublicationRejection::StateChanged) => {
                // The same generation changed attempt/publication/decision
                // state during admission. Re-resolve instead of relabeling a
                // live producer as an anonymous fatal 404.
                return Err(());
            }
            Err(crate::transcode::MediaResponsePublicationRejection::OwnerGone) => {
                return Ok(response_publication_rejection_before(
                    state,
                    session,
                    crate::transcode::MediaResponsePublicationRejection::OwnerGone,
                    deadline,
                )
                .await);
            }
            Err(crate::transcode::MediaResponsePublicationRejection::ProducerEnded(reason)) => {
                return Ok(playlist_error(
                    session,
                    PlaylistError::ProducerEnded(reason),
                ));
            }
        }
    }
    Ok(playlist_error(session, err.error))
}

/// The video rendition referenced by the native-subtitle HLS master.
pub async fn video_playlist(
    State(state): State<AppState>,
    AxPath(session): AxPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (playlist_deadline, request_deadline) = playlist_request_deadlines(&state);
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::VideoPlaylist,
        RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await?
    {
        return Ok(response);
    }
    video_playlist_local_before(
        &state,
        &session,
        "video.m3u8",
        playlist_deadline,
        request_deadline,
    )
    .await
}

pub(super) async fn video_playlist_local_before(
    state: &AppState,
    session: &str,
    object_name: &'static str,
    playlist_deadline: Instant,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    let initial_vod_deadline = response_publication_deadline_before(request_deadline);
    if let Some(answer) = vod_playlist_before(state, session, initial_vod_deadline).await? {
        let (bytes, owner) = admitted_vod_publication(
            state,
            session,
            answer,
            "vod-video-playlist",
            None,
            initial_vod_deadline,
        )
        .await?;
        let response = playlist_response(bytes);
        return complete_buffered_response_before(
            state,
            session,
            &owner,
            crate::transcode::MediaResponsePublication::attempt_media(
                "playlist",
                Some(object_name),
            ),
            true,
            response,
            initial_vod_deadline,
        )
        .await;
    }
    let mut publication_deadline = None;
    for reclassification in 0..=2 {
        match state
            .transcode
            .playlist_with_owner_before(session, playlist_deadline)
            .await
        {
            Ok((bytes, owner)) => {
                let response_deadline = *publication_deadline
                    .get_or_insert_with(|| response_publication_deadline_before(request_deadline));
                let response = playlist_response(bytes);
                let result = complete_buffered_response_before(
                    state,
                    session,
                    &owner,
                    crate::transcode::MediaResponsePublication::attempt_media(
                        "playlist",
                        Some(object_name),
                    ),
                    true,
                    response,
                    response_deadline,
                )
                .await;
                match result {
                    Err(error)
                        if response_publication_state_changed(&error)
                            && reclassification < 2
                            && tokio::time::Instant::now().into_std() < response_deadline =>
                    {
                        continue;
                    }
                    result => return result,
                }
            }
            Err(err) if matches!(&err.error, PlaylistError::SessionGone) => {
                match vod_resurrected_before(state, session, playlist_deadline).await {
                    VodResurrection::Absent => return Err(playlist_error(session, err.error)),
                    VodResurrection::Ended => return Err(media_session_ended()),
                    VodResurrection::OwnerLost(resume) => return Err(media_owner_lost(resume)),
                    VodResurrection::Unavailable => return Err(vod_resurrection_unavailable()),
                    VodResurrection::Resurrected => {}
                }
                let answer = tokio::time::timeout_at(
                    tokio::time::Instant::from_std(playlist_deadline),
                    state.transcode.vod_playlist(session),
                )
                .await
                .map_err(|_| vod_resurrection_unavailable())?
                .ok_or_else(vod_resurrection_unavailable)?;
                let response_deadline = *publication_deadline
                    .get_or_insert_with(|| response_publication_deadline_before(request_deadline));
                let (bytes, owner) = admitted_vod_publication(
                    state,
                    session,
                    answer,
                    "vod-video-playlist",
                    None,
                    response_deadline,
                )
                .await?;
                let response = playlist_response(bytes);
                return complete_buffered_response_before(
                    state,
                    session,
                    &owner,
                    crate::transcode::MediaResponsePublication::attempt_media(
                        "playlist",
                        Some(object_name),
                    ),
                    true,
                    response,
                    response_deadline,
                )
                .await;
            }
            Err(err) => match admitted_playlist_error(
                state,
                session,
                err,
                *publication_deadline
                    .get_or_insert_with(|| response_publication_deadline_before(request_deadline)),
            )
            .await
            {
                Ok(error) => return Err(error),
                Err(())
                    if reclassification < 2
                        && tokio::time::Instant::now().into_std()
                            < publication_deadline.expect("publication deadline initialized") =>
                {
                    continue;
                }
                Err(()) => {
                    return Err(ApiError::typed(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "playlist_state_changed",
                        "the stream changed state while the playlist response was prepared; retry shortly",
                    ));
                }
            },
        }
    }
    unreachable!("bounded playlist reclassification loop returns on every terminal branch")
}
