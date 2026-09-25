use super::*;

/// GET /api/v1/hls/:session/:segment — capability auth (see module docs).
pub async fn segment(
    State(state): State<AppState>,
    AxPath((session, seg)): AxPath<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let request_deadline = segment_request_deadline();
    if let Some(response) = relay_if_remote(
        &state,
        &session,
        RelayResource::Segment {
            segment: seg.clone(),
        },
        RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await?
    {
        return Ok(response);
    }
    segment_local_before(
        &state,
        &session,
        &seg,
        &RelayHeaders::from_http(&headers),
        request_deadline,
    )
    .await
}

pub(super) fn requested_byte_range(
    value: Option<&str>,
    len: u64,
) -> Result<Option<(u64, u64)>, ()> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let raw = raw.strip_prefix("bytes=").ok_or(())?;
    if raw.contains(',') || len == 0 {
        return Err(());
    }
    let (start, end) = raw.split_once('-').ok_or(())?;
    let (start, end) = if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        (len.saturating_sub(suffix.min(len)), len - 1)
    } else {
        let start = start.parse::<u64>().map_err(|_| ())?;
        if start >= len {
            return Err(());
        }
        let end = if end.is_empty() {
            len - 1
        } else {
            end.parse::<u64>().map_err(|_| ())?.min(len - 1)
        };
        if end < start {
            return Err(());
        }
        (start, end)
    };
    Ok(Some((start, end)))
}

/// RFC 9110 If-Range is deliberately stricter than If-None-Match: only an
/// exact strong entity-tag authorizes a partial representation. Weak tags,
/// dates, malformed values and non-matches all ignore Range and return the
/// current complete representation as 200.
pub(super) fn range_for_current_etag<'a>(headers: &'a RelayHeaders, etag: &str) -> Option<&'a str> {
    let range = headers.range.as_deref()?;
    match headers.if_range.as_deref() {
        None => Some(range),
        Some(candidate)
            if !etag.starts_with("W/")
                && !candidate.trim().starts_with("W/")
                && candidate.trim() == etag =>
        {
            Some(range)
        }
        Some(_) => None,
    }
}

/// Whether the resolved HTTP range carries every byte of the immutable
/// object. Open-ended and suffix ranges can cover the full object just as a
/// range-less 200 does; frontier semantics follow bytes, not status codes.
pub(super) fn range_covers_object(range: Option<(u64, u64)>, len: u64) -> bool {
    match range {
        None => true,
        Some((0, end)) => len > 0 && end == len - 1,
        Some(_) => false,
    }
}

pub(super) fn etag_matches(request: Option<&str>, etag: &str) -> bool {
    let representation = etag.strip_prefix("W/").unwrap_or(etag);
    request.is_some_and(|request| {
        request.split(',').map(str::trim).any(|candidate| {
            candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == representation
        })
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VodResurrection {
    Absent,
    Ended,
    Resurrected,
    Unavailable,
    /// The durable route's owner is gone and no successor took it over
    /// (plan §10.3). A VOD handle is immutable and film-addressed, so a
    /// reopen on any node serves the same film from the same place — but it
    /// is a reopen, and the client is told so rather than left retrying.
    OwnerLost(crate::media_sessions::OwnerLossResume),
}

pub(super) fn vod_resurrection_unavailable() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "vod_resurrection_unavailable",
        "the durable stream attachment could not be restored within this request; retry shortly",
    )
}

pub(super) fn media_session_ended() -> ApiError {
    ApiError::typed(
        StatusCode::GONE,
        "media_session_ended",
        "this media session is no longer active",
    )
}

pub(super) fn media_owner_transition() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "media_owner_transition",
        "the media owner is changing; retry shortly",
    )
}

/// The answer for a route classified as an owner transition (plan §10.3).
///
/// Both answers carry where a reopen should land, because the thing a viewer
/// loses when an owner dies is not only the stream but the knowledge of where
/// they were. Which of the two it is turns on the route's own recipe rather
/// than on a clock — see `classify_owner_loss` for why a deadline cannot be
/// made correct here.
pub(super) fn owner_transition_answer(route: &MediaSessionRoute) -> ApiError {
    match crate::media_sessions::classify_owner_loss(route, unix_ms()) {
        crate::media_sessions::OwnerLoss::Transitioning(resume) => {
            media_owner_transition_with(resume)
        }
        crate::media_sessions::OwnerLoss::Unrecoverable(resume) => media_owner_lost(resume),
    }
}

/// A successor may still arrive, so this stays the retryable 503 it has always
/// been — but it now says where to reopen if the client stops waiting, and
/// that recovery will not be seamless. A recovered session is renumbered
/// across a `#EXT-X-DISCONTINUITY`, so `continuous: false` is true of the
/// retry path too, not only of the terminal one.
fn media_owner_transition_with(resume: crate::media_sessions::OwnerLossResume) -> ApiError {
    ApiError::typed_detail(
        StatusCode::SERVICE_UNAVAILABLE,
        "media_owner_transition",
        "the media owner is changing; retry shortly, or reopen playback from the position \
         in this response",
        owner_loss_detail(resume, false),
    )
}

/// The route can never be taken over, so no successor is coming and waiting
/// cannot change that.
///
/// 410 rather than another 503 on purpose: a client that understands no code
/// at all still reads "gone" and stops waiting, which is the correct
/// degradation for a session no node will serve again. `continuous: false` and
/// `reopen_required: true` are the machine-readable form of §10.3's "do not
/// guess transparency" — the seam is admitted in the body rather than papered
/// over by a status that invites the client to wait it out.
pub(super) fn media_owner_lost(resume: crate::media_sessions::OwnerLossResume) -> ApiError {
    ApiError::typed_detail(
        StatusCode::GONE,
        "media_owner_lost",
        "the node serving this media session is gone and this session cannot be taken over; \
         reopen playback from the position in this response",
        owner_loss_detail(resume, true),
    )
}

fn owner_loss_detail(
    resume: crate::media_sessions::OwnerLossResume,
    reopen_required: bool,
) -> serde_json::Value {
    serde_json::json!({
        "film_position_ms": resume.film_position_ms,
        "film_frontier_ms": resume.film_frontier_ms,
        "reopen_required": reopen_required,
        "continuous": false,
    })
}

/// Try to resurrect a reaped VOD session from its durable route (plan §2.5)
/// without minting a second HTTP wait budget. Only an active, unexpired route
/// this node owns qualifies; Store/attachment uncertainty remains retryable
/// and must never be relabelled as authoritative absence.
pub(super) async fn vod_resurrected_before(
    state: &AppState,
    session: &str,
    deadline: Instant,
) -> VodResurrection {
    if tokio::time::Instant::now().into_std() >= deadline {
        return VodResurrection::Unavailable;
    }
    // Capture the process-local release generation before the durable route
    // read. A DELETE may complete while that read is in flight; retaining the
    // exact token prevents the delayed request from minting a fresh permissive
    // gate and resurrecting a terminal capability.
    let Some(adoption) = state.transcode.session_adoption_token(session) else {
        tracing::warn!(
            target: "plurxd::http::hls",
            session = %crate::transcode::session_log_id(session),
            "public VOD resurrection admission is full"
        );
        return VodResurrection::Unavailable;
    };
    // Lease loss also needs to classify this as VOD before the authoritative
    // route read returns; the marker and token cover the same full window.
    let _vod_preparing = state.transcode.begin_vod_preparation(session);
    let route = match state
        .media_sessions
        .authoritative_route_resolution_before(session, &state.node_id, deadline)
        .await
    {
        Ok(DurableRouteResolution::Absent) => return VodResurrection::Absent,
        Ok(DurableRouteResolution::Terminal(_)) => return VodResurrection::Ended,
        Ok(DurableRouteResolution::OwnerTransition(lost)) => {
            return match crate::media_sessions::classify_owner_loss(&lost, unix_ms()) {
                crate::media_sessions::OwnerLoss::Transitioning(_) => VodResurrection::Unavailable,
                crate::media_sessions::OwnerLoss::Unrecoverable(resume) => {
                    VodResurrection::OwnerLost(resume)
                }
            }
        }
        Ok(DurableRouteResolution::ActiveRemote(_)) => return VodResurrection::Unavailable,
        Ok(DurableRouteResolution::ActiveLocal(route)) => route,
        Err(_) => return VodResurrection::Unavailable,
    };
    match tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        state.transcode.vod_resurrect_before(
            &route.recipe_json,
            session,
            route.user_id,
            adoption,
            deadline,
            false,
        ),
    )
    .await
    {
        Ok(true) => VodResurrection::Resurrected,
        Ok(false) | Err(_) => VodResurrection::Unavailable,
    }
}

/// Map a VOD serving refusal to its typed response (plan §2.3).
pub(super) fn vod_error(session: &str, err: crate::vodserve::VodError) -> ApiError {
    use crate::vodserve::VodError;
    let log = |code: &str, message: &str| {
        tracing::warn!(
            target: "plurxd::http::hls",
            session = %crate::transcode::session_log_id(session),
            code,
            "vod request refused: {message}"
        );
    };
    match err {
        // The third outcome: the deadline passed with the segment still
        // unproduced. Retryable by contract, and says so.
        VodError::Pending { retry_after } => {
            let secs = retry_after.as_secs().max(1);
            log("segment_pending", "the segment is not materialized yet");
            ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "segment_pending",
                format!("the segment is being produced; retry in {secs}s"),
            )
        }
        // Two caps, two answers. The client can act on the difference — one
        // says this player is asking for too much at once and should slow its
        // own requests, the other says the node has no parked-request capacity
        // left and retrying harder makes it worse — and an operator reading
        // these refusals needs them apart to tell one seek storm from a
        // ceiling sized for a smaller deployment. They were one code, so
        // neither could.
        VodError::Busy(crate::waitpool::WaitRefused::SessionBusy) => {
            log(
                "segment_wait_busy",
                "this session's blocked-GET cap reached",
            );
            ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "segment_wait_busy",
                "too many blocked fetches for this session; retry shortly",
            )
        }
        VodError::Busy(crate::waitpool::WaitRefused::PoolFull) => {
            log("node_wait_capacity", "the node's blocked-GET cap reached");
            ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "node_wait_capacity",
                "this server is at its limit for waiting fetches; retry shortly",
            )
        }
        VodError::ProducerFailed(reason) => {
            log("producer_failed", &reason);
            ApiError::typed(StatusCode::BAD_GATEWAY, "producer_failed", &reason)
        }
        VodError::Gone(cause) => {
            log("media_session_ended", &format!("{cause:?}"));
            ApiError::typed(
                StatusCode::GONE,
                "media_session_ended",
                "this session has ended and will not resume",
            )
        }
        VodError::Io(error) => ApiError::Internal(error.to_string()),
    }
}

async fn resolved_vod_segment_response(
    state: &AppState,
    session: &str,
    seg: &str,
    headers: &RelayHeaders,
    answer: crate::transcode::VodResponsePublication<Option<crate::vodserve::SegmentReady>>,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    let publication_deadline = response_publication_deadline_before(request_deadline);
    let crate::transcode::VodResponsePublication { result, owner } = answer;
    match result {
        Ok(Some(ready)) => {
            vod_segment_response_before(
                state,
                session,
                seg,
                headers,
                ready,
                owner,
                request_deadline,
            )
            .await
        }
        Ok(None) => {
            authorize_attempt_status(
                state,
                session,
                &owner,
                segment_publication_kind(seg, None),
                Some(seg),
                publication_deadline,
            )
            .await?;
            Err(ApiError::NotFound("segment"))
        }
        Err(error) => {
            authorize_attempt_status(
                state,
                session,
                &owner,
                segment_publication_kind(seg, None),
                Some(seg),
                publication_deadline,
            )
            .await?;
            Err(vod_error(session, error))
        }
    }
}

/// Serve one VOD segment (or the init) with the immutable-cache headers the
/// plan's §2.1 URIs deserve. Range and conditional requests are honoured; the
/// Apple High-tier init rewrite is applied exactly as on the live path.
#[cfg(test)]
pub(super) async fn vod_segment_response(
    state: &AppState,
    session: &str,
    seg: &str,
    headers: &RelayHeaders,
    ready: crate::vodserve::SegmentReady,
    owner: crate::transcode::MediaResponseOwner,
) -> Result<Response, ApiError> {
    vod_segment_response_before(
        state,
        session,
        seg,
        headers,
        ready,
        owner,
        segment_request_deadline(),
    )
    .await
}

async fn vod_segment_response_before(
    state: &AppState,
    session: &str,
    seg: &str,
    headers: &RelayHeaders,
    ready: crate::vodserve::SegmentReady,
    owner: crate::transcode::MediaResponseOwner,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    let publication_deadline = response_publication_deadline_before(request_deadline);
    let mut ready = ready;
    if ready.len == 0 {
        authorize_attempt_status(
            state,
            session,
            &owner,
            segment_publication_kind(seg, None),
            Some(seg),
            publication_deadline,
        )
        .await?;
        return Err(ApiError::NotFound("segment"));
    }
    let content_type = segment_content_type(seg);
    let artifact_etag = format!("\"{}\"", ready.etag);
    let buffered_init =
        crate::transcode::is_init_object(seg) && ready.len <= INIT_INSPECTION_LIMIT_BYTES;
    if !buffered_init && etag_matches(headers.if_none_match.as_deref(), &artifact_etag) {
        let response = (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, artifact_etag.clone()),
                (header::ACCEPT_RANGES, "bytes".to_owned()),
                (
                    header::CACHE_CONTROL,
                    "private, max-age=3600, immutable".to_owned(),
                ),
            ],
        )
            .into_response();
        return complete_buffered_response_before(
            state,
            session,
            &owner,
            crate::transcode::MediaResponsePublication::attempt_media(
                "segment-not-modified",
                Some(seg),
            ),
            true,
            response,
            publication_deadline,
        )
        .await;
    }
    let requested_range =
        match requested_byte_range(range_for_current_etag(headers, &artifact_etag), ready.len) {
            Ok(range) => range,
            Err(()) if !buffered_init => {
                let response = (
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [
                        (header::CONTENT_RANGE, format!("bytes */{}", ready.len)),
                        (header::ACCEPT_RANGES, "bytes".to_owned()),
                        (header::ETAG, artifact_etag.clone()),
                    ],
                )
                    .into_response();
                authorize_attempt_status(
                    state,
                    session,
                    &owner,
                    "segment-range-not-satisfiable",
                    Some(seg),
                    publication_deadline,
                )
                .await?;
                return Ok(response);
            }
            // A transformed initialization response has a distinct validator.
            // Delay its 416 until after the representation is known.
            Err(()) => None,
        };
    // Small objects — the init above all — are answered from memory so the
    // Apple rewrite can run; segments stream.
    if buffered_init {
        let mut init = Vec::with_capacity(ready.len.min(64 * 1024) as usize);
        let read = tokio::time::timeout_at(
            tokio::time::Instant::from_std(publication_deadline),
            ready.file.read_to_end(&mut init),
        )
        .await;
        match read {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                authorize_attempt_status(
                    state,
                    session,
                    &owner,
                    segment_publication_kind(seg, None),
                    Some(seg),
                    publication_deadline,
                )
                .await?;
                return Err(ApiError::Internal(error.to_string()));
            }
            Err(_) => {
                authorize_attempt_status(
                    state,
                    session,
                    &owner,
                    segment_publication_kind(seg, None),
                    Some(seg),
                    publication_deadline,
                )
                .await?;
                return Err(response_publication_timeout());
            }
        }
        if init.len() as u64 != ready.len {
            authorize_attempt_status(
                state,
                session,
                &owner,
                segment_publication_kind(seg, None),
                Some(seg),
                publication_deadline,
            )
            .await?;
            return Err(ApiError::Internal(format!(
                "VOD init ended after {} of {} advertised bytes",
                init.len(),
                ready.len
            )));
        }
        let mut transformed = false;
        if let Some(file) = state.transcode.vod_file_for_owner(&owner) {
            if normalize_high_tier_hevc_init(&file, &mut init) {
                transformed = true;
                tracing::info!(
                    target: "plurxd::http::hls",
                    session = %crate::transcode::session_log_id(session),
                    "translated the HEVC High-tier initialization record for Apple HLS"
                );
            }
        }
        let etag = if transformed {
            format!("\"{}-apple-high-tier-v1\"", ready.etag)
        } else {
            artifact_etag
        };
        if etag_matches(headers.if_none_match.as_deref(), &etag) {
            let response = (
                StatusCode::NOT_MODIFIED,
                [
                    (header::ETAG, etag),
                    (header::ACCEPT_RANGES, "bytes".to_owned()),
                    (
                        header::CACHE_CONTROL,
                        "private, max-age=3600, immutable".to_owned(),
                    ),
                ],
            )
                .into_response();
            return complete_buffered_response_before(
                state,
                session,
                &owner,
                crate::transcode::MediaResponsePublication::attempt_media(
                    "segment-not-modified",
                    Some(seg),
                ),
                true,
                response,
                publication_deadline,
            )
            .await;
        }
        let requested_range =
            match requested_byte_range(range_for_current_etag(headers, &etag), ready.len) {
                Ok(range) => range,
                Err(()) => {
                    let response = (
                        StatusCode::RANGE_NOT_SATISFIABLE,
                        [
                            (header::CONTENT_RANGE, format!("bytes */{}", ready.len)),
                            (header::ACCEPT_RANGES, "bytes".to_owned()),
                            (header::ETAG, etag),
                        ],
                    )
                        .into_response();
                    authorize_attempt_status(
                        state,
                        session,
                        &owner,
                        "segment-range-not-satisfiable",
                        Some(seg),
                        publication_deadline,
                    )
                    .await?;
                    return Ok(response);
                }
            };
        let (status, body, content_range) = match requested_range {
            Some((start, end)) => {
                let start = usize::try_from(start).map_err(|_| ApiError::NotFound("segment"))?;
                let end = usize::try_from(end).map_err(|_| ApiError::NotFound("segment"))?;
                if end >= init.len() {
                    return Err(ApiError::NotFound("segment"));
                }
                (
                    StatusCode::PARTIAL_CONTENT,
                    init[start..=end].to_vec(),
                    Some(format!("bytes {start}-{end}/{}", init.len())),
                )
            }
            None => (StatusCode::OK, init, None),
        };
        let mut response = (StatusCode::OK, body).into_response();
        *response.status_mut() = status;
        let headers_mut = response.headers_mut();
        headers_mut.insert(header::CONTENT_TYPE, content_type.parse().expect("mime"));
        headers_mut.insert(header::ETAG, etag.parse().expect("etag"));
        headers_mut.insert(header::ACCEPT_RANGES, "bytes".parse().expect("ranges"));
        headers_mut.insert(
            header::CACHE_CONTROL,
            "private, max-age=3600, immutable".parse().expect("cache"),
        );
        if let Some(range) = content_range {
            headers_mut.insert(header::CONTENT_RANGE, range.parse().expect("range"));
        }
        return complete_buffered_response_before(
            state,
            session,
            &owner,
            crate::transcode::MediaResponsePublication::attempt_media(
                segment_publication_kind(seg, requested_range),
                Some(seg),
            ),
            range_covers_object(requested_range, ready.len),
            response,
            publication_deadline,
        )
        .await;
    }
    let etag = artifact_etag;
    let (status, len, content_range) = match requested_range {
        Some((start, end)) => {
            use tokio::io::AsyncSeekExt;
            let seek = tokio::time::timeout_at(
                tokio::time::Instant::from_std(publication_deadline),
                ready.file.seek(std::io::SeekFrom::Start(start)),
            )
            .await;
            match seek {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    authorize_attempt_status(
                        state,
                        session,
                        &owner,
                        segment_publication_kind(seg, requested_range),
                        Some(seg),
                        publication_deadline,
                    )
                    .await?;
                    return Err(ApiError::Internal(error.to_string()));
                }
                Err(_) => {
                    authorize_attempt_status(
                        state,
                        session,
                        &owner,
                        segment_publication_kind(seg, requested_range),
                        Some(seg),
                        publication_deadline,
                    )
                    .await?;
                    return Err(response_publication_timeout());
                }
            }
            (
                StatusCode::PARTIAL_CONTENT,
                end - start + 1,
                Some(format!("bytes {start}-{end}/{}", ready.len)),
            )
        }
        None => (StatusCode::OK, ready.len, None),
    };
    let complete_object = range_covers_object(requested_range, ready.len);
    let completion_permit = match reserve_response_completion(publication_deadline).await {
        Ok(permit) => permit,
        Err(error) => {
            authorize_attempt_status(
                state,
                session,
                &owner,
                segment_publication_kind(seg, requested_range),
                Some(seg),
                publication_deadline,
            )
            .await?;
            return Err(error);
        }
    };
    let authorization = authorize_response_publication(
        state,
        session,
        &owner,
        crate::transcode::MediaResponsePublication::attempt_media(
            segment_publication_kind(seg, requested_range),
            Some(seg),
        ),
        publication_deadline,
    )
    .await?;
    // Taken before `ready.file` is moved into the reader: the pump outlives
    // this function, and the session registry lock is long released by the
    // time it runs.
    let delivery = std::sync::Arc::clone(&ready.delivery);
    let reader = tokio_util::io::ReaderStream::with_capacity(
        tokio::io::AsyncReadExt::take(ready.file, len),
        MEDIA_BODY_READ_BUFFER,
    );
    let body_deadline = tokio::time::Instant::now() + MAX_ADMITTED_MEDIA_BODY_LIFETIME;
    let (sender, receiver) =
        tokio::sync::mpsc::channel::<DrivenLocalChunk>(LOCAL_MEDIA_BODY_CHANNEL_CAPACITY);
    let terminal = StreamedBodyTerminal::new();
    let pump_terminal = terminal.clone();
    let pump_session = session.to_owned();
    let completion: StreamedResponseCompletion = (
        Arc::clone(&state.transcode),
        pump_session.clone(),
        authorization,
        complete_object,
        completion_permit,
    );
    tokio::spawn(async move {
        let mut reader = reader;
        let mut completion = Some(completion);
        let mut delivered = 0_u64;
        let fail = |kind, message: String| pump_terminal.fail(kind, message);
        loop {
            let progress_deadline =
                (tokio::time::Instant::now() + MEDIA_BODY_NO_PROGRESS_TIMEOUT).min(body_deadline);
            let next = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(body_deadline) => {
                    fail(
                        std::io::ErrorKind::TimedOut,
                        "media response exceeded its maximum admitted body lifetime".to_owned(),
                    );
                    tracing::warn!(
                        target: "plurxd::http::hls",
                        session = %crate::transcode::session_log_id(&pump_session),
                        delivered_bytes = delivered,
                        expected_bytes = len,
                        "VOD response exceeded its maximum admitted body lifetime"
                    );
                    return;
                }
                () = sender.closed() => return,
                _ = tokio::time::sleep_until(progress_deadline) => {
                    fail(
                        std::io::ErrorKind::TimedOut,
                        "media response made no progress before its body deadline".to_owned(),
                    );
                    tracing::warn!(
                        target: "plurxd::http::hls",
                        session = %crate::transcode::session_log_id(&pump_session),
                        delivered_bytes = delivered,
                        expected_bytes = len,
                        "VOD response made no storage progress before its body deadline"
                    );
                    return;
                }
                next = reader.next() => next,
            };
            let bytes = match next {
                Some(Ok(bytes)) => bytes,
                Some(Err(error)) => {
                    fail(error.kind(), error.to_string());
                    return;
                }
                None if delivered == len => return,
                None => {
                    fail(
                        std::io::ErrorKind::UnexpectedEof,
                        format!(
                            "VOD response reached EOF after {delivered} of {len} advertised bytes"
                        ),
                    );
                    tracing::warn!(
                        target: "plurxd::http::hls",
                        session = %crate::transcode::session_log_id(&pump_session),
                        delivered_bytes = delivered,
                        expected_bytes = len,
                        "VOD response reached EOF before its advertised length"
                    );
                    return;
                }
            };
            // One storage read is handed downstream in acknowledgement-sized
            // pieces. `Bytes::split_to` is a refcount bump on the buffer this
            // read already filled, not a copy, so the proof granularity below
            // stays at MEDIA_BODY_ACK_GRANULARITY however large
            // MEDIA_BODY_READ_BUFFER is. Without this split the read size
            // *is* the ack unit, and any object at or below it would be
            // counted and completed by a single body poll.
            let mut bytes = bytes;
            while !bytes.is_empty() {
                let piece = bytes.split_to(bytes.len().min(MEDIA_BODY_ACK_GRANULARITY));
                let bytes_len = piece.len() as u64;
                let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
                let send = sender.send(DrivenLocalChunk {
                    bytes: piece,
                    accepted: accepted_tx,
                });
                tokio::pin!(send);
                let sent = tokio::select! {
                    biased;
                    _ = tokio::time::sleep_until(body_deadline) => {
                        fail(
                            std::io::ErrorKind::TimedOut,
                            "media response exceeded its maximum admitted body lifetime".to_owned(),
                        );
                        false
                    }
                    result = &mut send => result.is_ok(),
                };
                if !sent {
                    return;
                }
                let downstream_deadline = (tokio::time::Instant::now()
                    + MEDIA_BODY_NO_PROGRESS_TIMEOUT)
                    .min(body_deadline);
                let accepted = tokio::select! {
                    biased;
                    Ok(()) = accepted_rx => true,
                    _ = tokio::time::sleep_until(body_deadline) => {
                        fail(
                            std::io::ErrorKind::TimedOut,
                            "media response exceeded its maximum admitted body lifetime".to_owned(),
                        );
                        false
                    }
                    _ = tokio::time::sleep_until(downstream_deadline) => {
                        fail(
                            std::io::ErrorKind::TimedOut,
                            "media response made no downstream progress before its body deadline".to_owned(),
                        );
                        false
                    }
                    () = sender.closed() => false,
                };
                if !accepted {
                    return;
                }
                // Counted here — where the bytes actually left. `accepted` is the
                // downstream acknowledgement, so nothing is credited to this
                // viewer's rate until the chunk has been taken. A meter advanced
                // at read time instead would measure the disk.
                //
                // The buffered init object is deliberately *not* counted. It is
                // handed to the response whole, so crediting it would date bytes
                // at handoff rather than at delivery and inflate the first window
                // of a session that has delivered nothing yet. One small object
                // missing from the total is the honest trade; an unmeasured
                // session correctly reports no rate at all rather than a fast
                // one, even though that advisory value no longer gates handoff.
                delivery.note(bytes_len);
                delivered = delivered.saturating_add(bytes_len);
                if delivered == len {
                    if let Some((manager, session, authorization, complete_object, permit)) =
                        completion.take()
                    {
                        settle_streamed_response_completion(
                            manager,
                            session,
                            authorization,
                            complete_object,
                            permit,
                        );
                    }
                    return;
                }
            }
        }
    });
    let body = driven_local_body(receiver, terminal, body_deadline);
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers_mut = response.headers_mut();
    headers_mut.insert(header::CONTENT_TYPE, content_type.parse().expect("mime"));
    headers_mut.insert(header::CONTENT_LENGTH, len.into());
    headers_mut.insert(header::ETAG, etag.parse().expect("etag"));
    headers_mut.insert(header::ACCEPT_RANGES, "bytes".parse().expect("ranges"));
    headers_mut.insert(
        header::CACHE_CONTROL,
        "private, max-age=3600, immutable".parse().expect("cache"),
    );
    if let Some(range) = content_range {
        headers_mut.insert(header::CONTENT_RANGE, range.parse().expect("range"));
    }
    Ok(response)
}

pub(super) async fn segment_local_before(
    state: &AppState,
    session: &str,
    seg: &str,
    headers: &RelayHeaders,
    request_deadline: Instant,
) -> Result<Response, ApiError> {
    const APPLE_INIT_REWRITE_LIMIT_BYTES: u64 = INIT_INSPECTION_LIMIT_BYTES;

    // The VOD presentation's three-outcome contract dispatches first; `None`
    // falls through to the live path untouched. A session neither registry
    // knows may be a reaped VOD handle whose durable route is still live —
    // resurrect it and ask once more before giving up.
    if let Some(answer) = vod_segment_before(state, session, seg, request_deadline).await? {
        return resolved_vod_segment_response(
            state,
            session,
            seg,
            headers,
            answer,
            request_deadline,
        )
        .await;
    }

    let rolling = tokio::time::timeout_at(
        tokio::time::Instant::from_std(request_deadline),
        state.transcode.segment_for_publication(session, seg),
    )
    .await
    .map_err(|_| response_publication_timeout())?;
    let mut opened = match rolling {
        Ok(crate::transcode::SegmentPublication::Ready(opened)) => *opened,
        Ok(crate::transcode::SegmentPublication::Missing(owner)) => {
            if let Some(owner) = owner.as_ref() {
                authorize_attempt_status(
                    state,
                    session,
                    owner,
                    segment_publication_kind(seg, None),
                    Some(seg),
                    response_publication_deadline_before(request_deadline),
                )
                .await?;
            } else if crate::transcode::is_safe_segment(seg) {
                match vod_resurrected_before(state, session, request_deadline).await {
                    VodResurrection::Absent => {}
                    VodResurrection::Ended => return Err(media_session_ended()),
                    VodResurrection::OwnerLost(resume) => return Err(media_owner_lost(resume)),
                    VodResurrection::Unavailable => return Err(vod_resurrection_unavailable()),
                    VodResurrection::Resurrected => {
                        let answer = vod_segment_before(state, session, seg, request_deadline)
                            .await?
                            .ok_or_else(vod_resurrection_unavailable)?;
                        return resolved_vod_segment_response(
                            state,
                            session,
                            seg,
                            headers,
                            answer,
                            request_deadline,
                        )
                        .await;
                    }
                }
            }
            return Err(ApiError::NotFound("segment"));
        }
        Ok(crate::transcode::SegmentPublication::Pending(owner)) => {
            authorize_attempt_status(
                state,
                session,
                &owner,
                segment_publication_kind(seg, None),
                Some(seg),
                response_publication_deadline_before(request_deadline),
            )
            .await?;
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "segment_pending",
                "the segment is still being produced; retry shortly",
            ));
        }
        Ok(crate::transcode::SegmentPublication::Unavailable(owner)) => {
            authorize_attempt_status(
                state,
                session,
                &owner,
                segment_publication_kind(seg, None),
                Some(seg),
                response_publication_deadline_before(request_deadline),
            )
            .await?;
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "segment_inspection_unavailable",
                "the segment exists but its storage metadata is temporarily unavailable",
            ));
        }
        Ok(crate::transcode::SegmentPublication::Failed(error)) => {
            return match admitted_playlist_error(
                state,
                session,
                error,
                response_publication_deadline_before(request_deadline),
            )
            .await
            {
                Ok(error) => Err(error),
                Err(()) => Err(response_publication_rejection(
                    crate::transcode::MediaResponsePublicationRejection::StateChanged,
                )),
            };
        }
        Err(crate::transcode::SegmentOpenError::Capacity) => {
            return Err(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "response_snapshot_capacity",
                "authenticated media response capacity is full; retry shortly",
            ));
        }
    };
    let response_owner = opened.response_owner();
    // A live media object is published only after bytes exist. Treat an empty
    // file as an incomplete/corrupt publication instead of advertising the
    // saturating `0..=0` calculation below as one byte and hanging the client.
    if opened.len == 0 {
        authorize_attempt_status(
            state,
            session,
            &response_owner,
            segment_publication_kind(seg, None),
            Some(seg),
            response_publication_deadline_before(request_deadline),
        )
        .await?;
        opened.delivery.finish_without_body();
        return Err(ApiError::NotFound("segment"));
    }
    let content_type = segment_content_type(seg);
    let etag = response_owner
        .rolling_etag(session, seg, opened.len)
        .expect("a live segment carries a rolling response owner");
    if etag_matches(headers.if_none_match.as_deref(), &etag) {
        let response = (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag),
                (header::ACCEPT_RANGES, "bytes".to_owned()),
                (
                    header::CACHE_CONTROL,
                    "private, max-age=3600, immutable".to_owned(),
                ),
            ],
        )
            .into_response();
        let publication_deadline = response_publication_deadline_before(request_deadline);
        let authorization = match authorize_response_publication(
            state,
            session,
            &response_owner,
            crate::transcode::MediaResponsePublication::attempt_media(
                "segment-not-modified",
                Some(seg),
            ),
            publication_deadline,
        )
        .await
        {
            Ok(authorization) => authorization,
            Err(error) => {
                opened.delivery.finish_without_body();
                return Err(error);
            }
        };
        if let Err(error) =
            commit_authorized_media(state, session, authorization, true, publication_deadline).await
        {
            opened.delivery.finish_without_body();
            return Err(error);
        }
        opened.delivery.finish_without_body();
        return Ok(response);
    }
    let requested_range =
        match requested_byte_range(range_for_current_etag(headers, &etag), opened.len) {
            Ok(range) => range,
            Err(()) => {
                let response = (
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [
                        (header::CONTENT_RANGE, format!("bytes */{}", opened.len)),
                        (header::ACCEPT_RANGES, "bytes".to_owned()),
                        (header::ETAG, etag),
                    ],
                )
                    .into_response();
                if let Err(error) = authorize_attempt_status(
                    state,
                    session,
                    &response_owner,
                    "segment-range-not-satisfiable",
                    Some(seg),
                    response_publication_deadline_before(request_deadline),
                )
                .await
                {
                    opened.delivery.finish_without_body();
                    return Err(error);
                }
                opened.delivery.finish_without_body();
                return Ok(response);
            }
        };
    if crate::transcode::is_init_object(seg) && opened.len <= APPLE_INIT_REWRITE_LIMIT_BYTES {
        let publication_deadline = response_publication_deadline_before(request_deadline);
        let mut init = Vec::with_capacity(opened.len.min(64 * 1024) as usize);
        let mut delivery = opened.delivery;
        let started = Instant::now();
        let read_elapsed = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(publication_deadline),
            opened
                .file
                .take(APPLE_INIT_REWRITE_LIMIT_BYTES)
                .read_to_end(&mut init),
        )
        .await
        {
            Ok(Ok(_)) => started.elapsed(),
            Ok(Err(error)) => {
                delivery.fail(&error);
                authorize_attempt_status(
                    state,
                    session,
                    &response_owner,
                    segment_publication_kind(seg, None),
                    Some(seg),
                    publication_deadline,
                )
                .await?;
                return Err(ApiError::Internal(error.to_string()));
            }
            Err(_) => {
                delivery.finish_without_body();
                authorize_attempt_status(
                    state,
                    session,
                    &response_owner,
                    segment_publication_kind(seg, None),
                    Some(seg),
                    publication_deadline,
                )
                .await?;
                return Err(response_publication_timeout());
            }
        };
        if init.len() as u64 != opened.len {
            let error = std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "init ended after {} of {} advertised bytes",
                    init.len(),
                    opened.len
                ),
            );
            delivery.fail(&error);
            authorize_attempt_status(
                state,
                session,
                &response_owner,
                segment_publication_kind(seg, None),
                Some(seg),
                publication_deadline,
            )
            .await?;
            return Err(ApiError::Internal(error.to_string()));
        }
        if let Ok((context, file, _)) = session_file(state, session, publication_deadline).await {
            if strip_unadvertised_dolby_vision(&context, &mut init) {
                tracing::info!(
                    target: "plurxd::http::hls",
                    session = %crate::transcode::session_log_id(session),
                    "removed a Dolby Vision record the playlist does not advertise"
                );
            }
            if normalize_high_tier_hevc_init(&file, &mut init) {
                tracing::info!(
                    target: "plurxd::http::hls",
                    session = %crate::transcode::session_log_id(session),
                    "translated the HEVC High-tier initialization record for Apple HLS"
                );
            }
        }
        let (status, body, content_range) = match requested_range {
            Some((start, end)) => {
                let start = usize::try_from(start).map_err(|_| ApiError::NotFound("segment"))?;
                let end = usize::try_from(end).map_err(|_| ApiError::NotFound("segment"))?;
                (
                    StatusCode::PARTIAL_CONTENT,
                    init[start..=end].to_vec(),
                    Some(format!("bytes {start}-{end}/{}", init.len())),
                )
            }
            None => (StatusCode::OK, init, None),
        };
        let mut response = Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, body.len())
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::ETAG, etag)
            .header(header::CACHE_CONTROL, "private, max-age=3600, immutable");
        if let Some(content_range) = content_range {
            response = response.header(header::CONTENT_RANGE, content_range);
        }
        let response_bytes = body.len() as u64;
        let response = response
            .body(Body::from(body))
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        let authorization = match authorize_response_publication(
            state,
            session,
            &response_owner,
            crate::transcode::MediaResponsePublication::attempt_media(
                segment_publication_kind(seg, requested_range),
                Some(seg),
            ),
            publication_deadline,
        )
        .await
        {
            Ok(authorization) => authorization,
            Err(error) => {
                delivery.finish_without_body();
                return Err(error);
            }
        };
        if let Err(error) = commit_authorized_media(
            state,
            session,
            authorization,
            range_covers_object(requested_range, opened.len),
            publication_deadline,
        )
        .await
        {
            delivery.finish_without_body();
            return Err(error);
        }
        // The storage inspection reads the complete init so it can normalize
        // codec metadata, but client-delivery accounting follows only the
        // bytes placed in this response (especially for a Range request).
        delivery.expect_at_most(response_bytes);
        delivery.note_read(response_bytes, read_elapsed);
        delivery.finish();
        return Ok(bound_admitted_media_body(response));
    }
    if crate::transcode::is_init_object(seg) && opened.len > APPLE_INIT_REWRITE_LIMIT_BYTES {
        tracing::warn!(
            target: "plurxd::http::hls",
            session = %crate::transcode::session_log_id(session),
            init_bytes = opened.len,
            limit_bytes = APPLE_INIT_REWRITE_LIMIT_BYTES,
            "skipped Apple HEVC tier normalization because init.mp4 exceeds the inspection bound"
        );
    }
    let (status, start, end) = requested_range
        .map(|(start, end)| (StatusCode::PARTIAL_CONTENT, start, end))
        .unwrap_or((StatusCode::OK, 0, opened.len.saturating_sub(1)));
    let publication_deadline = response_publication_deadline_before(request_deadline);
    if start > 0 {
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(publication_deadline),
            opened.file.seek(std::io::SeekFrom::Start(start)),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                opened.delivery.fail(&error);
                authorize_attempt_status(
                    state,
                    session,
                    &response_owner,
                    segment_publication_kind(seg, requested_range),
                    Some(seg),
                    publication_deadline,
                )
                .await?;
                return Err(ApiError::Internal(error.to_string()));
            }
            Err(_) => {
                opened.delivery.finish_without_body();
                authorize_attempt_status(
                    state,
                    session,
                    &response_owner,
                    segment_publication_kind(seg, requested_range),
                    Some(seg),
                    publication_deadline,
                )
                .await?;
                return Err(response_publication_timeout());
            }
        }
    }
    let opened_len = end.saturating_sub(start).saturating_add(1);
    let total_len = opened.len;
    let complete_object = range_covers_object(requested_range, opened.len);
    let completion_permit = match reserve_response_completion(publication_deadline).await {
        Ok(permit) => permit,
        Err(error) => {
            opened.delivery.finish_without_body();
            authorize_attempt_status(
                state,
                session,
                &response_owner,
                segment_publication_kind(seg, requested_range),
                Some(seg),
                publication_deadline,
            )
            .await?;
            return Err(error);
        }
    };
    let authorization = match authorize_response_publication(
        state,
        session,
        &response_owner,
        crate::transcode::MediaResponsePublication::attempt_media(
            segment_publication_kind(seg, requested_range),
            Some(seg),
        ),
        publication_deadline,
    )
    .await
    {
        Ok(authorization) => authorization,
        Err(error) => {
            opened.delivery.finish_without_body();
            return Err(error);
        }
    };
    // The object is open and its length is known. Pin it for the body's
    // lifetime: cleanup may unlink the name while these bytes are still going
    // out, and the directory scan that measures scratch cannot see an
    // unlinked-but-open file.
    let mut authorization = authorization;
    authorization.pin_scratch_object(total_len);
    let reader = tokio_util::io::ReaderStream::with_capacity(
        opened.file.take(opened_len),
        MEDIA_BODY_READ_BUFFER,
    );
    let mut delivery = opened.delivery;
    delivery.expect_at_most(opened_len);
    let body_deadline = tokio::time::Instant::now() + MAX_ADMITTED_MEDIA_BODY_LIFETIME;
    let (sender, receiver) =
        tokio::sync::mpsc::channel::<DrivenLocalChunk>(LOCAL_MEDIA_BODY_CHANNEL_CAPACITY);
    let terminal = StreamedBodyTerminal::new();
    let pump_terminal = terminal.clone();
    let pump_session = session.to_owned();
    let completion: StreamedResponseCompletion = (
        Arc::clone(&state.transcode),
        pump_session.clone(),
        authorization,
        complete_object,
        completion_permit,
    );
    // This producer is the sole owner of the file, delivery tracker, and EOF
    // authorization after headers are exposed. Both deadlines keep advancing
    // even if downstream stops polling; receiver Drop ends it immediately.
    tokio::spawn(async move {
        let mut reader = reader;
        let mut delivery = delivery;
        let mut completion = Some(completion);
        let mut delivered = 0_u64;
        let fail = |kind, message: String| pump_terminal.fail(kind, message);
        loop {
            let started = Instant::now();
            let progress_deadline =
                (tokio::time::Instant::now() + MEDIA_BODY_NO_PROGRESS_TIMEOUT).min(body_deadline);
            let next = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(body_deadline) => {
                    let error = std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "media response exceeded its maximum admitted body lifetime",
                    );
                    delivery.fail_transport(&error, "body_lifetime_exceeded");
                    fail(error.kind(), error.to_string());
                    return;
                }
                () = sender.closed() => return,
                _ = tokio::time::sleep_until(progress_deadline) => {
                    let error = std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "media response made no storage progress before its body deadline",
                    );
                    delivery.fail(&error);
                    fail(error.kind(), error.to_string());
                    return;
                }
                next = reader.next() => next,
            };
            let (bytes, read_elapsed) = match next {
                Some(Ok(bytes)) => (bytes, started.elapsed()),
                Some(Err(error)) => {
                    delivery.fail(&error);
                    fail(error.kind(), error.to_string());
                    return;
                }
                None if delivered == opened_len => return,
                None => {
                    let error = std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!(
                            "media response reached EOF after {delivered} of {opened_len} advertised bytes"
                        ),
                    );
                    delivery.fail(&error);
                    fail(error.kind(), error.to_string());
                    return;
                }
            };
            // See the VOD pump: the storage read size above and the delivery
            // proof granularity below are separate decisions. Splitting here
            // keeps the ack -- and therefore the byte count, the slow-read
            // report and the completion that renews the lease and moves the
            // fetched-segment frontier -- at MEDIA_BODY_ACK_GRANULARITY.
            let read_len = bytes.len() as u64;
            let mut storage_read_noted = false;
            let mut bytes = bytes;
            while !bytes.is_empty() {
                let piece = bytes.split_to(bytes.len().min(MEDIA_BODY_ACK_GRANULARITY));
                let bytes_len = piece.len() as u64;
                let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
                let send = sender.send(DrivenLocalChunk {
                    bytes: piece,
                    accepted: accepted_tx,
                });
                tokio::pin!(send);
                let sent = tokio::select! {
                    biased;
                    _ = tokio::time::sleep_until(body_deadline) => {
                        let error = std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "media response exceeded its maximum admitted body lifetime",
                        );
                        delivery.fail_transport(&error, "body_lifetime_exceeded");
                        fail(error.kind(), error.to_string());
                        false
                    }
                    result = &mut send => result.is_ok(),
                };
                if !sent {
                    return;
                }
                let downstream_deadline = (tokio::time::Instant::now()
                    + MEDIA_BODY_NO_PROGRESS_TIMEOUT)
                    .min(body_deadline);
                let accepted = tokio::select! {
                    biased;
                    Ok(()) = accepted_rx => true,
                    _ = tokio::time::sleep_until(body_deadline) => {
                        let error = std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "media response exceeded its maximum admitted body lifetime",
                        );
                        delivery.fail_transport(&error, "body_lifetime_exceeded");
                        fail(error.kind(), error.to_string());
                        false
                    }
                    _ = tokio::time::sleep_until(downstream_deadline) => {
                        let error = std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "media response made no downstream progress before its body deadline",
                        );
                        delivery.fail_transport(&error, "downstream_no_progress");
                        fail(error.kind(), error.to_string());
                        false
                    }
                    () = sender.closed() => false,
                };
                if !accepted {
                    return;
                }
                delivery.note_delivered(bytes_len);
                if !storage_read_noted {
                    // The stall signal is a property of the storage read, not of
                    // the acknowledgement unit, so it is evaluated once per read
                    // against the bytes storage actually produced in that time --
                    // but only after the body has taken the first piece of it, so
                    // a chunk the consumer never accepted still reports nothing.
                    storage_read_noted = true;
                    delivery.note_storage_read(read_len, read_elapsed);
                }
                delivered = delivered.saturating_add(bytes_len);
                if delivered == opened_len {
                    if delivery.finish() {
                        if let Some((manager, session, authorization, complete_object, permit)) =
                            completion.take()
                        {
                            settle_streamed_response_completion(
                                manager,
                                session,
                                authorization,
                                complete_object,
                                permit,
                            );
                        }
                    }
                    return;
                }
            }
        }
    });
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, opened_len)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, etag)
        // A finished segment never changes: ffmpeg writes `.tmp` and
        // renames. The URI carries a capability-scoped session id.
        .header(header::CACHE_CONTROL, "private, max-age=3600, immutable");
    if status == StatusCode::PARTIAL_CONTENT {
        response = response.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total_len}"),
        );
    }
    response
        // Streamed rather than buffered: a 4K copy segment is ~35 MB, and
        // reading it into memory before the first byte goes out is an
        // allocation and a copy per request for data on its way to a socket.
        .body(driven_local_body(receiver, terminal, body_deadline))
        .map_err(|error| ApiError::Internal(error.to_string()))
}

/// MIME types from Apple's HLS authoring profile. An initialization section
/// is an MP4 file, while each `.m4s` resource is an ISO BMFF media segment.
/// Labeling both as `video/mp4` makes the bytes decodable in isolation but can
/// cause AVPlayer's multivariant validator to reject the rendition before it
/// ever opens the decoder.
pub(super) fn segment_content_type(name: &str) -> &'static str {
    if name.ends_with(".ts") {
        "video/mp2t"
    } else if name.ends_with(".m4s") {
        "video/iso.segment"
    } else {
        "video/mp4"
    }
}
