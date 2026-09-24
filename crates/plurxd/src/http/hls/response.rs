use super::*;

#[derive(Default, Deserialize)]
pub struct PlaylistQuery {
    pub native: Option<u8>,
    pub subtitle: Option<i64>,
    /// Temporary physical-device isolation mode for the Apple HDR master.
    /// The session UUID remains the capability; these modes only remove
    /// declarations from the playlist and never expose another resource.
    pub diagnostic: Option<String>,
}

pub(super) fn playlist_response(bytes: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/vnd.apple.mpegurl".to_owned(),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        bytes,
    )
        .into_response()
}

/// Linearize one fully prepared response against the exact rolling attempt or
/// immutable VOD attachment that supplied it. Callers may prepare a buffered
/// response locally, but must not expose it or construct a streaming reader or
/// body until this succeeds.
pub(super) async fn authorize_response_publication(
    state: &AppState,
    session: &str,
    owner: &crate::transcode::MediaResponseOwner,
    publication: crate::transcode::MediaResponsePublication,
    deadline: Instant,
) -> Result<crate::transcode::MediaResponseAuthorization, ApiError> {
    match tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        state
            .transcode
            .authorize_response_publication(session, owner, publication, deadline),
    )
    .await
    {
        Ok(Ok(authorization)) => Ok(authorization),
        Ok(Err(rejection)) => {
            Err(response_publication_rejection_before(state, session, rejection, deadline).await)
        }
        Err(_) => Err(response_publication_timeout()),
    }
}

/// Commit completion using the authorization issued for these exact response
/// bytes. The opaque token prevents EOF from reconstructing authority from a
/// reusable session id or an object name after a successor has taken over.
pub(super) async fn commit_authorized_media(
    state: &AppState,
    session: &str,
    authorization: crate::transcode::MediaResponseAuthorization,
    complete_object: bool,
    deadline: Instant,
) -> Result<(), ApiError> {
    match tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        state
            .transcode
            .commit_authorized_media(authorization, complete_object, deadline),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(rejection)) => {
            Err(response_publication_rejection_before(state, session, rejection, deadline).await)
        }
        Err(_) => Err(response_publication_timeout()),
    }
}

/// Fence a bodyless status against the exact attempt that classified it.
/// Dropping the authorization is intentional: no media body completed, so the
/// response must not renew demand or advance a delivery frontier.
pub(super) async fn authorize_attempt_status(
    state: &AppState,
    session: &str,
    owner: &crate::transcode::MediaResponseOwner,
    kind: &'static str,
    object_name: Option<&str>,
    deadline: Instant,
) -> Result<(), ApiError> {
    let _authorization = authorize_response_publication(
        state,
        session,
        owner,
        crate::transcode::MediaResponsePublication::attempt_status(kind, object_name),
        deadline,
    )
    .await?;
    Ok(())
}

pub(super) fn response_publication_deadline() -> Instant {
    tokio::time::Instant::now().into_std() + RESPONSE_PUBLICATION_LIFECYCLE_BUDGET
}

pub(super) fn response_publication_deadline_before(request_deadline: Instant) -> Instant {
    response_publication_deadline().min(request_deadline)
}

pub(super) fn playlist_request_deadlines(state: &AppState) -> (Instant, Instant) {
    let playlist_deadline = state.transcode.playlist_request_deadline();
    let request_deadline = playlist_deadline + RESPONSE_PUBLICATION_LIFECYCLE_BUDGET;
    (playlist_deadline, request_deadline)
}

pub(super) fn playlist_request_deadlines_before(
    state: &AppState,
    request_deadline: Instant,
) -> (Instant, Instant) {
    let reserved = request_deadline
        .checked_sub(RESPONSE_PUBLICATION_LIFECYCLE_BUDGET)
        .unwrap_or(request_deadline);
    (
        state.transcode.playlist_request_deadline().min(reserved),
        request_deadline,
    )
}

pub(super) fn resource_deadline_unix_ms(deadline: Instant) -> Option<i64> {
    let now_unix_ms = unix_ms();
    let remaining = deadline.checked_duration_since(Instant::now())?;
    let remaining_ms = i64::try_from(remaining.as_millis())
        .ok()
        .filter(|ms| *ms > 0)?;
    Some(now_unix_ms.saturating_add(remaining_ms))
}

pub(super) fn inherited_resource_deadline(request: &RelayRequest) -> Option<Instant> {
    let now = Instant::now();
    Some(now + request.owner_budget_at(unix_ms())?)
}

pub(super) fn segment_request_deadline() -> Instant {
    tokio::time::Instant::now().into_std() + SEGMENT_REQUEST_LIFECYCLE_BUDGET
}

pub(super) async fn vod_playlist_before(
    state: &AppState,
    session: &str,
    deadline: Instant,
) -> Result<Option<crate::transcode::VodResponsePublication<Vec<u8>>>, ApiError> {
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        state.transcode.vod_playlist(session),
    )
    .await
    .map_err(|_| response_publication_timeout())
}

pub(super) async fn vod_segment_before(
    state: &AppState,
    session: &str,
    segment: &str,
    deadline: Instant,
) -> Result<
    Option<crate::transcode::VodResponsePublication<Option<crate::vodserve::SegmentReady>>>,
    ApiError,
> {
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        state.transcode.vod_segment(session, segment),
    )
    .await
    .map_err(|_| response_publication_timeout())
}

pub(super) async fn admitted_vod_publication<T>(
    state: &AppState,
    session: &str,
    publication: crate::transcode::VodResponsePublication<T>,
    kind: &'static str,
    object_name: Option<&str>,
    deadline: Instant,
) -> Result<(T, crate::transcode::MediaResponseOwner), ApiError> {
    let crate::transcode::VodResponsePublication { result, owner } = publication;
    match result {
        Ok(value) => Ok((value, owner)),
        Err(error) => {
            authorize_attempt_status(state, session, &owner, kind, object_name, deadline).await?;
            Err(vod_error(session, error))
        }
    }
}

pub(super) fn response_publication_rejection(
    rejection: crate::transcode::MediaResponsePublicationRejection,
) -> ApiError {
    match rejection {
        crate::transcode::MediaResponsePublicationRejection::OwnerGone => ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "response_owner_transition",
            "the stream owner changed while the response was prepared; retry shortly",
        ),
        crate::transcode::MediaResponsePublicationRejection::StateChanged => ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "response_state_changed",
            "the stream changed state while the response was prepared; retry shortly",
        ),
        crate::transcode::MediaResponsePublicationRejection::ProducerEnded(reason) => {
            let error = PlaylistError::ProducerEnded(reason);
            ApiError::typed(StatusCode::BAD_GATEWAY, error.code(), error.message())
        }
    }
}

pub(super) async fn response_publication_rejection_before(
    state: &AppState,
    session: &str,
    rejection: crate::transcode::MediaResponsePublicationRejection,
    deadline: Instant,
) -> ApiError {
    if !matches!(
        &rejection,
        crate::transcode::MediaResponsePublicationRejection::OwnerGone
    ) {
        return response_publication_rejection(rejection);
    }
    match state
        .media_sessions
        .authoritative_route_resolution_before(session, &state.node_id, deadline)
        .await
    {
        Ok(DurableRouteResolution::Absent) => ApiError::NotFound("transcode session"),
        Ok(DurableRouteResolution::Terminal(_)) => ApiError::typed(
            StatusCode::GONE,
            "media_session_ended",
            "this media session is no longer active",
        ),
        Ok(DurableRouteResolution::OwnerTransition(lost)) => owner_transition_answer(&lost),
        Ok(DurableRouteResolution::ActiveLocal(_))
        | Ok(DurableRouteResolution::ActiveRemote(_)) => response_publication_rejection(rejection),
        Err(_) => ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "response_owner_reclassification_unavailable",
            "the stream owner could not be reclassified before the request deadline; retry shortly",
        ),
    }
}

pub(super) fn response_publication_timeout() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "response_publication_timeout",
        "response publication did not settle before its control deadline; retry shortly",
    )
}

pub(super) fn response_publication_state_changed(error: &ApiError) -> bool {
    matches!(
        error,
        ApiError::Typed {
            code: "response_state_changed",
            ..
        }
    )
}

fn response_completion_slots() -> Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    Arc::clone(
        SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(RESPONSE_COMPLETION_CAPACITY))),
    )
}

pub(super) async fn reserve_response_completion(
    deadline: Instant,
) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    reserve_response_completion_from(response_completion_slots(), deadline).await
}

pub(super) async fn reserve_response_completion_from(
    slots: Arc<tokio::sync::Semaphore>,
    deadline: Instant,
) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    match tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        slots.acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_)) | Err(_) => Err(ApiError::typed(
            StatusCode::SERVICE_UNAVAILABLE,
            "response_completion_capacity",
            "response completion capacity is full; retry shortly",
        )),
    }
}

/// Once storage has produced every advertised byte, completion owns its own
/// bounded task. The body consumer is allowed to stop polling immediately
/// after the final chunk; dropping that consumer must not discard an exact
/// EOF token or cancel it halfway through the actor/registry projection.
pub(super) fn settle_streamed_response_completion(
    manager: Arc<crate::transcode::TranscodeManager>,
    session: String,
    authorization: crate::transcode::MediaResponseAuthorization,
    complete_object: bool,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let deadline = response_publication_deadline();
    tokio::spawn(async move {
        let _completion_permit = permit;
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            manager.commit_authorized_media(authorization, complete_object, deadline),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(rejection)) => tracing::debug!(
                session = %crate::transcode::session_log_id(&session),
                ?rejection,
                "discarded exact response completion after publication state changed"
            ),
            Err(_) => tracing::warn!(
                session = %crate::transcode::session_log_id(&session),
                "exact response completion exceeded its control deadline"
            ),
        }
    });
}

pub(super) type StreamedResponseCompletion = (
    Arc<crate::transcode::TranscodeManager>,
    String,
    crate::transcode::MediaResponseAuthorization,
    bool,
    tokio::sync::OwnedSemaphorePermit,
);

pub(super) const LOCAL_MEDIA_BODY_CHANNEL_CAPACITY: usize = 1;

pub(super) struct DrivenLocalChunk {
    pub(super) bytes: Bytes,
    pub(super) accepted: tokio::sync::oneshot::Sender<()>,
}

#[derive(Clone)]
pub(super) struct StreamedBodyTerminal {
    failure: Arc<std::sync::Mutex<Option<(std::io::ErrorKind, String)>>>,
    signal: tokio_util::sync::CancellationToken,
}

impl StreamedBodyTerminal {
    pub(super) fn new() -> Self {
        Self {
            failure: Arc::new(std::sync::Mutex::new(None)),
            signal: tokio_util::sync::CancellationToken::new(),
        }
    }

    pub(super) fn fail(&self, kind: std::io::ErrorKind, message: String) {
        let mut failure = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if failure.is_none() {
            *failure = Some((kind, message));
        }
        drop(failure);
        self.signal.cancel();
    }

    fn take_error(&self) -> Option<std::io::Error> {
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .map(|(kind, message)| std::io::Error::new(kind, message))
    }
}

/// Build the public side of a driven local body. The producer task owns the
/// file, delivery tracker, authorization, and completion permit, so socket
/// backpressure cannot prevent either body deadline from advancing or retain
/// those resources after the receiver disappears.
pub(super) fn driven_local_body(
    receiver: tokio::sync::mpsc::Receiver<DrivenLocalChunk>,
    terminal: StreamedBodyTerminal,
    body_deadline: tokio::time::Instant,
) -> Body {
    let stream = futures_util::stream::unfold(
        (receiver, terminal, body_deadline, false),
        |(mut receiver, terminal, body_deadline, finished)| async move {
            if finished {
                return None;
            }
            if let Some(error) = terminal.take_error() {
                return Some((Err(error), (receiver, terminal, body_deadline, true)));
            }
            if tokio::time::Instant::now() >= body_deadline {
                return Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "media response exceeded its maximum admitted body lifetime",
                    )),
                    (receiver, terminal, body_deadline, true),
                ));
            }
            let chunk = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(body_deadline) => {
                    return Some((
                        Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "media response exceeded its maximum admitted body lifetime",
                        )),
                        (receiver, terminal, body_deadline, true),
                    ));
                }
                () = terminal.signal.cancelled() => {
                    let error = terminal
                        .take_error()
                        .unwrap_or_else(|| std::io::Error::other("media response producer failed"));
                    return Some((Err(error), (receiver, terminal, body_deadline, true)));
                }
                chunk = receiver.recv() => chunk,
            };
            let Some(chunk) = chunk else {
                if let Some(error) = terminal.take_error() {
                    return Some((Err(error), (receiver, terminal, body_deadline, true)));
                }
                return None;
            };
            // Recheck both fences after wakeup and before acknowledging this
            // exact chunk. If timeout/failure won concurrently with recv, the
            // ack sender drops, so the pump cannot count or commit the bytes.
            if tokio::time::Instant::now() >= body_deadline || terminal.signal.is_cancelled() {
                let error = terminal.take_error().unwrap_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "media response exceeded its maximum admitted body lifetime",
                    )
                });
                return Some((Err(error), (receiver, terminal, body_deadline, true)));
            }
            let _ = chunk.accepted.send(());
            Some((Ok(chunk.bytes), (receiver, terminal, body_deadline, false)))
        },
    );
    Body::from_stream(stream)
}

pub(super) fn segment_publication_kind(
    segment: &str,
    requested_range: Option<(u64, u64)>,
) -> &'static str {
    if requested_range.is_some() {
        "segment-range"
    } else if crate::transcode::is_init_object(segment) {
        "init-segment"
    } else {
        "media-segment"
    }
}

/// Publish a response whose complete HTTP body has already been prepared in
/// memory. Constructing the value is not visibility; returning it is, so the
/// actor/registry fence and completion commit stay immediately before return.
pub(super) fn bound_admitted_media_body(response: Response) -> Response {
    // A prepared playlist/init/subtitle body still needs a post-header owner:
    // without this wrapper an unpolled in-memory Body could survive forever,
    // invalidating the shared admitted-media lifetime used by handoff and
    // terminal fallback proofs.
    let (parts, body) = response.into_parts();
    let body_deadline = tokio::time::Instant::now() + MAX_ADMITTED_MEDIA_BODY_LIFETIME;
    let stream = futures_util::stream::unfold(
        (Box::pin(body.into_data_stream()), body_deadline, false),
        |(mut body, body_deadline, finished)| async move {
            if finished {
                return None;
            }
            let item = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(body_deadline) => {
                    return Some((
                        Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "prepared media response exceeded its maximum admitted body lifetime",
                        )),
                        (body, body_deadline, true),
                    ));
                }
                item = body.next() => item,
            };
            match item {
                Some(Ok(bytes)) => Some((Ok(bytes), (body, body_deadline, false))),
                Some(Err(error)) => Some((
                    Err(std::io::Error::other(error.to_string())),
                    (body, body_deadline, true),
                )),
                None => None,
            }
        },
    );
    Response::from_parts(parts, Body::from_stream(stream))
}

pub(super) async fn complete_buffered_response_before(
    state: &AppState,
    session: &str,
    owner: &crate::transcode::MediaResponseOwner,
    publication: crate::transcode::MediaResponsePublication,
    complete_object: bool,
    response: Response,
    deadline: Instant,
) -> Result<Response, ApiError> {
    let authorization =
        authorize_response_publication(state, session, owner, publication, deadline).await?;
    commit_authorized_media(state, session, authorization, complete_object, deadline).await?;
    Ok(bound_admitted_media_body(response))
}

pub(super) async fn session_file(
    state: &AppState,
    session: &str,
    deadline: Instant,
) -> Result<
    (
        crate::transcode::HlsContext,
        MediaFile,
        crate::transcode::MediaResponseOwner,
    ),
    ApiError,
> {
    let mut resurrection_attempted = false;
    loop {
        match state
            .transcode
            .hls_presentation_before(session, deadline)
            .await
        {
            crate::transcode::HlsPresentationResolution::Ready(context, file, owner) => {
                return Ok((context, file, owner));
            }
            crate::transcode::HlsPresentationResolution::Failed(error) => {
                return match admitted_playlist_error(state, session, error, deadline).await {
                    Ok(error) => Err(error),
                    Err(()) => Err(response_publication_rejection(
                        crate::transcode::MediaResponsePublicationRejection::StateChanged,
                    )),
                };
            }
            crate::transcode::HlsPresentationResolution::StateChanged => {
                return Err(response_publication_rejection(
                    crate::transcode::MediaResponsePublicationRejection::StateChanged,
                ));
            }
            crate::transcode::HlsPresentationResolution::Gone if !resurrection_attempted => {
                resurrection_attempted = true;
                match vod_resurrected_before(state, session, deadline).await {
                    VodResurrection::Resurrected => continue,
                    VodResurrection::Absent => {
                        return Err(ApiError::NotFound("transcode session"));
                    }
                    VodResurrection::Ended => return Err(media_session_ended()),
                    VodResurrection::OwnerLost(resume) => return Err(media_owner_lost(resume)),
                    VodResurrection::Unavailable => {
                        return Err(vod_resurrection_unavailable());
                    }
                }
            }
            crate::transcode::HlsPresentationResolution::Gone => {
                return Err(vod_resurrection_unavailable());
            }
        }
    }
}

/// Maximum frame rate from ffprobe's persisted source description.
///
/// Fractions are kept until the playlist is rendered so NTSC rates retain
/// their 24000/1001 or 30000/1001 meaning. `avg_frame_rate` is preferred;
/// `r_frame_rate` is the fallback for older probe output.
#[cfg(test)]
pub(super) fn video_frame_rate(probe_json: &str) -> Option<f64> {
    fn fraction(raw: &str) -> Option<f64> {
        let (numerator, denominator) = raw.split_once('/')?;
        let numerator = numerator.parse::<f64>().ok()?;
        let denominator = denominator.parse::<f64>().ok()?;
        let rate = numerator / denominator;
        (rate.is_finite() && rate > 0.0).then_some(rate)
    }

    let probe: serde_json::Value = serde_json::from_str(probe_json).ok()?;
    let stream = probe
        .get("streams")?
        .as_array()?
        .iter()
        .find(|stream| stream.get("codec_type").and_then(|v| v.as_str()) == Some("video"))?;
    ["avg_frame_rate", "r_frame_rate"]
        .into_iter()
        .find_map(|key| stream.get(key).and_then(|v| v.as_str()).and_then(fraction))
}
