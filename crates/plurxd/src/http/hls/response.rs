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

/// Once the body has accepted every advertised byte, completion owns its own
/// bounded task. The body consumer is allowed to stop polling immediately
/// after the final chunk; dropping that consumer must not discard an exact
/// EOF token or cancel it halfway through the actor/registry projection.
pub(super) fn settle_streamed_response_completion(
    manager: Arc<crate::transcode::TranscodeManager>,
    session: String,
    authorization: crate::transcode::MediaResponseAuthorization,
    complete_object: bool,
    permit: tokio::sync::OwnedSemaphorePermit,
    observed: Option<link_receipts::CompletionObserver>,
) {
    let deadline = response_publication_deadline();
    let observed_eof = (Instant::now(), unix_ms());
    tokio::spawn(async move {
        let _completion_permit = permit;
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            manager.commit_authorized_media(authorization, complete_object, deadline),
        )
        .await
        {
            Ok(Ok(())) => {
                if let Some(observed) = observed {
                    observed(observed_eof.0, observed_eof.1);
                }
            }
            Ok(Err(rejection)) => tracing::debug!(
                target: "plurxd::http::hls",
                session = %crate::transcode::session_log_id(&session),
                ?rejection,
                "discarded exact response completion after publication state changed"
            ),
            Err(_) => tracing::warn!(
                target: "plurxd::http::hls",
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

// One 128 KiB storage backing already supplies 32 ready proof frames. Stop
// gathering at this retained-backing cap, even for a short read. This avoids
// starting another storage operation while downstream delay could pollute
// its elapsed-time sample. ReaderStream's reservation and tokio's blocking
// buffer still keep retained payload comfortably below the 1 MiB ceiling.
const LOCAL_MEDIA_BATCH_READS: usize = 1;
const _: () = assert!((LOCAL_MEDIA_BATCH_READS + 2) * MEDIA_BODY_READ_BUFFER <= 1024 * 1024);

struct ResidentRead {
    bytes: Bytes,
    elapsed: Duration,
}

#[derive(Default)]
struct AcceptedPrefix {
    pieces: Vec<(usize, tokio::time::Instant)>,
}

pub(super) struct ResidentBatch {
    reads: std::collections::VecDeque<ResidentRead>,
    accepted: Arc<std::sync::Mutex<AcceptedPrefix>>,
    changed: Arc<tokio::sync::Notify>,
    progress_started: tokio::time::Instant,
}

#[cfg(test)]
pub(super) fn test_resident_chunk(bytes: Bytes) -> (ResidentBatch, impl Fn() -> usize) {
    let accepted = Arc::new(std::sync::Mutex::new(AcceptedPrefix::default()));
    let probe = Arc::clone(&accepted);
    (
        ResidentBatch {
            reads: [ResidentRead {
                bytes,
                elapsed: Duration::ZERO,
            }]
            .into(),
            accepted,
            changed: Arc::new(tokio::sync::Notify::new()),
            progress_started: tokio::time::Instant::now(),
        },
        move || {
            probe
                .lock()
                .expect("acceptance probe lock")
                .pieces
                .iter()
                .map(|(bytes, _)| bytes)
                .sum()
        },
    )
}

pub(super) enum LocalDeliveryEvent {
    Accepted(u64),
    StorageRead(u64, Duration),
    Failed(std::io::Error, &'static str),
    Finished,
}

/// One owner drives both local HLS paths. A batch is a bounded set of ready
/// storage reads, not an acknowledgement: the body records each small frame
/// independently, and this owner reconciles that ordered prefix on every exit.
pub(super) async fn pump_local_media<S, D, C>(
    mut reader: S,
    sender: tokio::sync::mpsc::Sender<ResidentBatch>,
    terminal: StreamedBodyTerminal,
    body_deadline: tokio::time::Instant,
    len: u64,
    mut delivery: D,
    complete: C,
) where
    S: futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
    D: FnMut(LocalDeliveryEvent) -> bool,
    C: FnOnce(),
{
    use std::task::Poll;
    let mut delivered = 0_u64;
    let mut last_progress = tokio::time::Instant::now();
    let mut complete = Some(complete);
    let fail = |delivery: &mut D, error: std::io::Error, class| {
        terminal.fail(error.kind(), error.to_string());
        delivery(LocalDeliveryEvent::Failed(error, class));
    };
    loop {
        let mut reads = std::collections::VecDeque::new();
        let mut source_end = None;
        let mut read_started = Instant::now();
        let gathering = futures_util::future::poll_fn(|cx| {
            loop {
                match std::pin::Pin::new(&mut reader).poll_next(cx) {
                    Poll::Ready(Some(Ok(bytes))) => {
                        if bytes.is_empty() {
                            continue;
                        }
                        // Production readers reserve exactly this capacity.
                        // Reject an oversized read rather than silently
                        // weakening the retained-data bound for another caller.
                        if bytes.len() > MEDIA_BODY_READ_BUFFER {
                            source_end =
                                Some(Some(std::io::Error::other("oversized media storage read")));
                            return Poll::Ready(());
                        }
                        reads.push_back(ResidentRead {
                            bytes,
                            elapsed: read_started.elapsed(),
                        });
                        read_started = Instant::now();
                        if reads.len() == LOCAL_MEDIA_BATCH_READS {
                            return Poll::Ready(());
                        }
                    }
                    Poll::Ready(item) => {
                        source_end =
                            Some(item.map(|result| result.expect_err("data handled above")));
                        return Poll::Ready(());
                    }
                    Poll::Pending if reads.is_empty() => return Poll::Pending,
                    Poll::Pending => return Poll::Ready(()),
                }
            }
        });
        let progress_deadline = (last_progress + MEDIA_BODY_NO_PROGRESS_TIMEOUT).min(body_deadline);
        tokio::select! {
            biased;
            () = sender.closed() => {
                if let Some(class) = terminal.take_body_timeout() {
                    fail(&mut delivery, local_body_timeout(class), class);
                }
                return;
            },
            () = terminal.signal.cancelled() => {
                if let Some(class) = terminal.take_body_timeout() {
                    fail(&mut delivery, local_body_timeout(class), class);
                }
                return;
            },
            _ = tokio::time::sleep_until(body_deadline) => {
                fail(&mut delivery, std::io::Error::new(std::io::ErrorKind::TimedOut,
                    "media response exceeded its maximum admitted body lifetime"), "body_lifetime_exceeded");
                return;
            }
            _ = tokio::time::sleep_until(progress_deadline) => {
                fail(&mut delivery, std::io::Error::new(std::io::ErrorKind::TimedOut,
                    "media response made no storage progress before its body deadline"), "storage_no_progress");
                return;
            }
            () = gathering => {}
        }
        if !reads.is_empty() {
            let provenance: Vec<_> = reads
                .iter()
                .map(|read| (read.bytes.len(), read.elapsed))
                .collect();
            let total: usize = provenance.iter().map(|(bytes, _)| bytes).sum();
            let accepted = Arc::new(std::sync::Mutex::new(AcceptedPrefix::default()));
            let changed = Arc::new(tokio::sync::Notify::new());
            // Storage just made progress: the downstream phase of this batch
            // gets its own no-progress window from the moment it is handed
            // over, instead of inheriting what the storage read already spent
            // since the previous batch's last acceptance.
            last_progress = tokio::time::Instant::now();
            let batch = ResidentBatch {
                reads,
                accepted: Arc::clone(&accepted),
                changed: Arc::clone(&changed),
                progress_started: last_progress,
            };
            // No other batch is outstanding: the owner waits for this exact
            // prefix before reading again, so channel(1) cannot double payload.
            if sender.send(batch).await.is_err() {
                if let Some(class) = terminal.take_body_timeout() {
                    fail(&mut delivery, local_body_timeout(class), class);
                }
                return;
            }
            let mut reconciled = 0;
            let mut batch_delivered = 0;
            let mut read_index = 0;
            let mut read_delivered = 0;
            loop {
                let downstream_deadline;
                {
                    // Holding the prefix lock through deadline selection makes an
                    // acceptance race settle before failure; body polls use the
                    // same lock for their fence and prefix recording.
                    let prefix = accepted
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    for &(bytes, at) in &prefix.pieces[reconciled..] {
                        delivery(LocalDeliveryEvent::Accepted(bytes as u64));
                        if read_delivered == 0 {
                            let (bytes, elapsed) = provenance[read_index];
                            delivery(LocalDeliveryEvent::StorageRead(bytes as u64, elapsed));
                        }
                        delivered += bytes as u64;
                        batch_delivered += bytes;
                        read_delivered += bytes;
                        last_progress = at;
                        if read_delivered == provenance[read_index].0 {
                            read_index += 1;
                            read_delivered = 0;
                        }
                    }
                    reconciled = prefix.pieces.len();
                    if delivered == len {
                        if delivery(LocalDeliveryEvent::Finished) {
                            complete.take().expect("one completion")();
                        }
                        return;
                    }
                    // The consumer may observe its timeout and drop before
                    // this owner wakes. Reconcile its prefix first, then emit
                    // the retained classification before ordinary drop/cancel.
                    if let Some(class) = terminal.take_body_timeout() {
                        fail(&mut delivery, local_body_timeout(class), class);
                        return;
                    }
                    if sender.is_closed() {
                        return;
                    }
                    if terminal.signal.is_cancelled() {
                        return;
                    }
                    if tokio::time::Instant::now() >= body_deadline {
                        fail(
                            &mut delivery,
                            std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "media response exceeded its maximum admitted body lifetime",
                            ),
                            "body_lifetime_exceeded",
                        );
                        return;
                    }
                    downstream_deadline =
                        (last_progress + MEDIA_BODY_NO_PROGRESS_TIMEOUT).min(body_deadline);
                    if tokio::time::Instant::now() >= downstream_deadline {
                        fail(&mut delivery, std::io::Error::new(std::io::ErrorKind::TimedOut,
                        "media response made no downstream progress before its body deadline"), "downstream_no_progress");
                        return;
                    }
                    if batch_delivered == total {
                        break;
                    }
                    // The vector is bounded by one read / 4 KiB (short reads
                    // add at most one entry each), never a channel of per-frame acks.
                }
                tokio::select! {
                    biased;
                    () = changed.notified() => {}
                    () = sender.closed() => {}
                    () = terminal.signal.cancelled() => {}
                    _ = tokio::time::sleep_until(downstream_deadline) => {}
                }
            }
        }
        if let Some(end) = source_end {
            let error =
                end.unwrap_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::UnexpectedEof,
                format!("media response reached EOF after {delivered} of {len} advertised bytes"))
                });
            fail(&mut delivery, error, "storage_error");
            return;
        }
    }
}

pub(super) fn resident_local_body(
    receiver: tokio::sync::mpsc::Receiver<ResidentBatch>,
    terminal: StreamedBodyTerminal,
    body_deadline: tokio::time::Instant,
) -> Body {
    Body::from_stream(futures_util::stream::unfold(
        (receiver, None::<ResidentBatch>, terminal, false),
        move |(mut receiver, mut batch, terminal, finished)| async move {
            if finished {
                return None;
            }
            if batch.as_ref().is_none_or(|batch| batch.reads.is_empty()) {
                batch = tokio::select! {
                    biased;
                    _ = tokio::time::sleep_until(body_deadline) => None,
                    () = terminal.signal.cancelled() => None,
                    batch = receiver.recv() => batch,
                };
            }
            if let Some(current) = &mut batch {
                let mut prefix = current
                    .accepted
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let last_progress = prefix
                    .pieces
                    .last()
                    .map_or(current.progress_started, |(_, at)| *at);
                if !terminal.signal.is_cancelled() && tokio::time::Instant::now() >= body_deadline {
                    let error = terminal.observe_body_timeout("body_lifetime_exceeded");
                    return Some((Err(error), (receiver, None, terminal, true)));
                }
                if !terminal.signal.is_cancelled()
                    && tokio::time::Instant::now() >= last_progress + MEDIA_BODY_NO_PROGRESS_TIMEOUT
                {
                    return Some((
                        Err(terminal.observe_body_timeout("downstream_no_progress")),
                        (receiver, None, terminal, true),
                    ));
                }
                if tokio::time::Instant::now() < body_deadline && !terminal.signal.is_cancelled() {
                    let read = current.reads.front_mut().expect("resident data");
                    let piece = read
                        .bytes
                        .split_to(read.bytes.len().min(MEDIA_BODY_ACK_GRANULARITY));
                    prefix
                        .pieces
                        .push((piece.len(), tokio::time::Instant::now()));
                    if read.bytes.is_empty() {
                        current.reads.pop_front();
                    }
                    current.changed.notify_one();
                    drop(prefix);
                    return Some((Ok(piece), (receiver, batch, terminal, false)));
                }
            }
            let error = terminal.take_error().or_else(|| {
                (tokio::time::Instant::now() >= body_deadline)
                    .then(|| terminal.observe_body_timeout("body_lifetime_exceeded"))
            });
            error.map(|error| (Err(error), (receiver, batch, terminal, true)))
        },
    ))
}

fn local_body_timeout(class: &'static str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        if class == "body_lifetime_exceeded" {
            "media response exceeded its maximum admitted body lifetime"
        } else {
            "media response made no downstream progress before its body deadline"
        },
    )
}

#[derive(Clone)]
pub(super) struct StreamedBodyTerminal {
    failure: Arc<std::sync::Mutex<Option<(std::io::ErrorKind, String)>>>,
    body_timeout: Arc<std::sync::Mutex<Option<&'static str>>>,
    signal: tokio_util::sync::CancellationToken,
}

impl StreamedBodyTerminal {
    pub(super) fn new() -> Self {
        Self {
            failure: Arc::new(std::sync::Mutex::new(None)),
            body_timeout: Arc::new(std::sync::Mutex::new(None)),
            signal: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn observe_body_timeout(&self, class: &'static str) -> std::io::Error {
        *self
            .body_timeout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(class);
        self.signal.cancel();
        local_body_timeout(class)
    }

    fn take_body_timeout(&self) -> Option<&'static str> {
        self.body_timeout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
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

#[cfg(test)]
mod batching_tests {
    use super::*;
    use http_body_util::BodyExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Receipt {
        accepted: Vec<u64>,
        reads: Vec<(u64, Duration)>,
        failures: Vec<(std::io::ErrorKind, &'static str)>,
        finished: usize,
    }

    fn fixture<S>(
        reader: S,
        advertised: u64,
        lifetime: Duration,
    ) -> (
        Body,
        tokio::task::JoinHandle<()>,
        Arc<std::sync::Mutex<Receipt>>,
        Arc<AtomicUsize>,
    )
    where
        S: futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Unpin + Send + 'static,
    {
        let (sender, receiver) = tokio::sync::mpsc::channel(LOCAL_MEDIA_BODY_CHANNEL_CAPACITY);
        let terminal = StreamedBodyTerminal::new();
        let deadline = tokio::time::Instant::now() + lifetime;
        let receipt = Arc::new(std::sync::Mutex::new(Receipt::default()));
        let completed = Arc::new(AtomicUsize::new(0));
        let notes = Arc::clone(&receipt);
        let completions = Arc::clone(&completed);
        let pump = tokio::spawn(pump_local_media(
            reader,
            sender,
            terminal.clone(),
            deadline,
            advertised,
            move |event| {
                let mut notes = notes.lock().expect("receipt lock");
                match event {
                    LocalDeliveryEvent::Accepted(bytes) => notes.accepted.push(bytes),
                    LocalDeliveryEvent::StorageRead(bytes, elapsed) => {
                        notes.reads.push((bytes, elapsed))
                    }
                    LocalDeliveryEvent::Failed(error, class) => {
                        notes.failures.push((error.kind(), class))
                    }
                    LocalDeliveryEvent::Finished => notes.finished += 1,
                }
                true
            },
            move || {
                completions.fetch_add(1, Ordering::SeqCst);
            },
        ));
        (
            resident_local_body(receiver, terminal, deadline),
            pump,
            receipt,
            completed,
        )
    }

    fn cursor(
        bytes: usize,
    ) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Unpin {
        tokio_util::io::ReaderStream::with_capacity(
            std::io::Cursor::new(vec![7; bytes]),
            MEDIA_BODY_READ_BUFFER,
        )
    }

    #[tokio::test]
    async fn a_batch_counts_only_its_accepted_prefix() {
        let (mut body, pump, receipt, completed) =
            fixture(cursor(512 * 1024), 512 * 1024, Duration::from_secs(300));
        for _ in 0..3 {
            assert_eq!(
                body.frame()
                    .await
                    .expect("body frame value")
                    .expect("body frame value")
                    .into_data()
                    .expect("body frame value")
                    .len(),
                4096
            );
        }
        drop(body);
        pump.await.expect("pump task");
        let notes = receipt.lock().expect("receipt lock");
        assert_eq!(notes.accepted, vec![4096; 3]);
        assert_eq!(notes.reads.len(), 1);
        assert_eq!(notes.reads[0].0, 128 * 1024);
        assert_eq!(completed.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_final_batch_completes_exactly_once() {
        let bytes = 3 * 1024 * 1024;
        let (mut body, pump, receipt, completed) =
            fixture(cursor(bytes), bytes as u64, Duration::from_secs(300));
        let mut taken = 0;
        while taken < bytes {
            let frame = body
                .frame()
                .await
                .expect("body frame value")
                .expect("body frame value")
                .into_data()
                .expect("body frame value");
            assert!(frame.len() <= 4096);
            taken += frame.len();
        }
        // No extra poll at EOF and no consumer remains to keep commit alive.
        drop(body);
        pump.await.expect("pump task");
        let notes = receipt.lock().expect("receipt lock");
        assert_eq!(notes.accepted.iter().sum::<u64>(), bytes as u64);
        assert_eq!(notes.reads.len(), 24);
        assert_eq!(notes.finished, 1);
        assert_eq!(completed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_short_source_is_still_unexpected_eof() {
        let bytes = 5 * 1024 * 1024 / 2;
        let (mut body, pump, receipt, completed) =
            fixture(cursor(bytes), 3 * 1024 * 1024, Duration::from_secs(300));
        let mut taken = 0;
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => taken += frame.into_data().expect("body frame value").len(),
                Err(_) => break,
            }
        }
        pump.await.expect("pump task");
        let notes = receipt.lock().expect("receipt lock");
        assert_eq!(taken, bytes);
        assert_eq!(notes.accepted.iter().sum::<u64>(), bytes as u64);
        assert_eq!(
            notes.failures,
            vec![(std::io::ErrorKind::UnexpectedEof, "storage_error")]
        );
        assert_eq!(completed.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_dropped_receiver_ends_the_pump_without_a_failure() {
        let (mut body, pump, receipt, _) =
            fixture(cursor(1024 * 1024), 1024 * 1024, Duration::from_secs(300));
        let frame = body
            .frame()
            .await
            .expect("body frame value")
            .expect("body frame value")
            .into_data()
            .expect("body frame value");
        // Simulate a socket retaining a partially written frame. Neither
        // keeping it nor retrying its suffix polls/accepts another body frame.
        let suffix = frame.slice(100..);
        drop(body);
        pump.await.expect("pump task");
        assert_eq!(suffix.len(), 3996);
        let notes = receipt.lock().expect("receipt lock");
        assert_eq!(notes.accepted, vec![4096]);
        assert!(notes.failures.is_empty());
        assert_eq!(notes.finished, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_downstream_deadline_still_fires_across_a_batch() {
        let (mut body, pump, receipt, _) =
            fixture(cursor(128 * 1024), 128 * 1024, Duration::from_secs(300));
        body.frame()
            .await
            .expect("body frame value")
            .expect("body frame value");
        tokio::time::advance(Duration::from_secs(30)).await;
        pump.await.expect("pump task");
        assert!(body.frame().await.expect("body frame value").is_err());
        {
            let notes = receipt.lock().expect("receipt lock");
            assert_eq!(notes.accepted, vec![4096]);
            assert_eq!(notes.failures[0].1, "downstream_no_progress");
        }
        let (mut body, pump, receipt, _) = fixture(
            cursor(128 * 1024),
            128 * 1024,
            MAX_ADMITTED_MEDIA_BODY_LIFETIME,
        );
        body.frame()
            .await
            .expect("body frame value")
            .expect("body frame value");
        for _ in 0..10 {
            tokio::time::advance(Duration::from_secs(29)).await;
            body.frame()
                .await
                .expect("body frame value")
                .expect("body frame value");
        }
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(body.frame().await.expect("body frame value").is_err());
        pump.await.expect("pump task");
        let notes = receipt.lock().expect("receipt lock");
        assert_eq!(notes.accepted, vec![4096; 11]);
        assert_eq!(notes.failures[0].1, "body_lifetime_exceeded");
    }

    /// A slow storage read and a slow downstream acceptance are separate
    /// phases. Twenty seconds of each must not add up to one 30 s
    /// no-progress failure on the next batch.
    #[tokio::test(start_paused = true)]
    async fn storage_and_downstream_each_get_their_own_no_progress_window() {
        let reader = Box::pin(futures_util::stream::unfold(0u8, |step| async move {
            match step {
                0 => Some((Ok(Bytes::from(vec![1; 4096])), 1)),
                1 => {
                    tokio::time::sleep(Duration::from_secs(20)).await;
                    Some((Ok(Bytes::from(vec![2; 4096])), 2))
                }
                _ => None,
            }
        }));
        let (mut body, pump, receipt, completed) = fixture(reader, 8192, Duration::from_secs(300));
        assert_eq!(
            body.frame()
                .await
                .expect("first frame")
                .expect("first data")
                .into_data()
                .expect("first data")
                .len(),
            4096
        );
        // The second storage read takes 20 s, then the consumer waits 20 s
        // more before accepting anything from the new batch. A paused-clock
        // sleep (not `advance` plus yields) lets the pump finish that read and
        // hand the batch over before the consumer's own wait starts.
        tokio::time::sleep(Duration::from_secs(20)).await;
        tokio::time::advance(Duration::from_secs(20)).await;
        assert_eq!(
            body.frame()
                .await
                .expect("second frame")
                .expect("downstream still inside its own window")
                .into_data()
                .expect("second data")
                .len(),
            4096
        );
        drop(body);
        pump.await.expect("pump task");
        let notes = receipt.lock().expect("receipt lock");
        assert!(notes.failures.is_empty(), "{:?}", notes.failures);
        assert_eq!(notes.accepted, vec![4096, 4096]);
        assert_eq!(notes.finished, 1);
        assert_eq!(completed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_batch_never_waits_for_more_bytes_than_the_reader_has() {
        use std::task::Poll;
        let mut supplied = false;
        let reader = futures_util::stream::poll_fn(move |_cx| {
            if supplied {
                Poll::Pending
            } else {
                supplied = true;
                Poll::Ready(Some(Ok(Bytes::from(vec![1; 100 * 1024]))))
            }
        });
        let (mut body, pump, receipt, _) = fixture(reader, 200 * 1024, Duration::from_secs(300));
        tokio::task::yield_now().await;
        // now_or_never polls once: no producer yield is allowed between the
        // resident frames, and a real Pending starts only after the prefix.
        use futures_util::FutureExt;
        for _ in 0..25 {
            assert_eq!(
                body.frame()
                    .now_or_never()
                    .expect("body frame value")
                    .expect("body frame value")
                    .expect("body frame value")
                    .into_data()
                    .expect("body frame value")
                    .len(),
                4096
            );
        }
        assert!(body.frame().now_or_never().is_none());
        drop(body);
        pump.await.expect("pump task");
        assert_eq!(
            receipt
                .lock()
                .expect("receipt lock")
                .accepted
                .iter()
                .sum::<u64>(),
            100 * 1024
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_first_timeout_survives_drop_before_the_pump_wakes() {
        use futures_util::FutureExt;
        for absolute in [false, true] {
            let lifetime = MAX_ADMITTED_MEDIA_BODY_LIFETIME;
            let (sender, receiver) = tokio::sync::mpsc::channel(1);
            let terminal = StreamedBodyTerminal::new();
            let deadline = tokio::time::Instant::now() + lifetime;
            let receipt = Arc::new(std::sync::Mutex::new(Receipt::default()));
            let notes = Arc::clone(&receipt);
            let completed = Arc::new(AtomicUsize::new(0));
            let completions = Arc::clone(&completed);
            let pump = pump_local_media(
                cursor(128 * 1024),
                sender,
                terminal.clone(),
                deadline,
                128 * 1024,
                move |event| {
                    let mut notes = notes.lock().expect("receipt lock");
                    match event {
                        LocalDeliveryEvent::Accepted(bytes) => notes.accepted.push(bytes),
                        LocalDeliveryEvent::Failed(error, class) => {
                            notes.failures.push((error.kind(), class))
                        }
                        LocalDeliveryEvent::Finished => notes.finished += 1,
                        LocalDeliveryEvent::StorageRead(..) => {}
                    }
                    true
                },
                move || {
                    completions.fetch_add(1, Ordering::SeqCst);
                },
            );
            tokio::pin!(pump);
            assert!(
                pump.as_mut().now_or_never().is_none(),
                "queue one backing, then park the pump"
            );
            let mut body = resident_local_body(receiver, terminal, deadline);
            assert_eq!(
                body.frame()
                    .await
                    .expect("frame")
                    .expect("data")
                    .into_data()
                    .expect("data")
                    .len(),
                4096
            );
            if absolute {
                for _ in 0..10 {
                    tokio::time::advance(Duration::from_secs(29)).await;
                    body.frame().await.expect("frame").expect("data");
                }
                tokio::time::advance(Duration::from_secs(10)).await;
            } else {
                tokio::time::advance(MEDIA_BODY_NO_PROGRESS_TIMEOUT).await;
            }
            assert!(body.frame().await.expect("timeout frame").is_err());
            drop(body);
            pump.await;
            let notes = receipt.lock().expect("receipt lock");
            assert_eq!(notes.accepted, vec![4096; if absolute { 11 } else { 1 }]);
            assert_eq!(
                notes.failures,
                [(
                    std::io::ErrorKind::TimedOut,
                    if absolute {
                        "body_lifetime_exceeded"
                    } else {
                        "downstream_no_progress"
                    }
                )]
            );
            assert_eq!(notes.finished, 0);
            assert_eq!(completed.load(Ordering::SeqCst), 0);
        }
    }
}
