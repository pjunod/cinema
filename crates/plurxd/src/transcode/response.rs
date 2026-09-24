use super::*;

/// A segment, open and ready to stream.
pub struct SegmentFile {
    pub file: tokio::fs::File,
    pub len: u64,
    pub(crate) delivery: SegmentDelivery,
}

impl SegmentFile {
    pub(crate) fn response_owner(&self) -> MediaResponseOwner {
        MediaResponseOwner(MediaResponseOwnerKind::Rolling {
            session: Arc::clone(&self.delivery.session),
            producer_attempt: self.delivery.producer_attempt,
        })
    }
}

/// Opaque engine/incarnation identity carried from resource resolution to the
/// HTTP response commit. Session ids are durable routing keys; they are not
/// sufficient proof that the bytes still belong to the currently live
/// rolling actor or resurrected VOD attachment.
#[derive(Clone)]
pub(crate) struct MediaResponseOwner(pub(super) MediaResponseOwnerKind);

/// One VOD lookup result plus the exact attachment/tombstone snapshot that
/// produced it. Keeping the owner beside errors and misses lets HTTP admit a
/// bodyless status without reconstructing authority from a reusable id.
pub(crate) struct VodResponsePublication<T> {
    pub(crate) result: Result<T, crate::vodserve::VodError>,
    pub(crate) owner: MediaResponseOwner,
}

impl MediaResponseOwner {
    /// AVC is init-derived only for fMP4 sessions. Rolling full transcodes use
    /// MPEG-TS and have no initialization object to inspect.
    pub(crate) fn avc_master_uses_init(&self) -> bool {
        match &self.0 {
            MediaResponseOwnerKind::Rolling { session, .. } => {
                matches!(session.kind, SessionKind::Copy { .. })
            }
            MediaResponseOwnerKind::Vod(_) => true,
        }
    }

    /// A strong validator for one rolling object. VOD supplies its own
    /// artifact digest; rolling scratch needs both the process-local Session
    /// incarnation and exact producer attempt so an ABA reuse can never turn
    /// different bytes with the same length into a false 304.
    pub(crate) fn rolling_etag(
        &self,
        session_id: &str,
        object_name: &str,
        len: u64,
    ) -> Option<String> {
        let MediaResponseOwnerKind::Rolling {
            session,
            producer_attempt,
        } = &self.0
        else {
            return None;
        };
        let identity = format!(
            "{session_id}\0{}\0{producer_attempt}\0{object_name}\0{len}",
            session.response_incarnation
        );
        Some(format!(
            "\"{}\"",
            hex::encode(Sha256::digest(identity.as_bytes()))
        ))
    }
}

/// Why exact response publication could not linearize. HTTP must distinguish
/// a reusable capability whose owner disappeared from a live incarnation that
/// merely changed attempt/decision state while the response was prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MediaResponsePublicationRejection {
    OwnerGone,
    StateChanged,
    ProducerEnded(String),
}

/// Frozen presentation facts resolved without erasing the distinction between
/// a missing capability, a still-current generation in transition, and a
/// terminal producer verdict. HTTP must publish the latter two as typed
/// responses rather than passing through the live-only facade and inventing a
/// fatal 404.
// The Ready payload is intentionally returned by value to preserve the
// existing cross-module response contract; boxing it would force every HTTP
// caller to unwrap an owned presentation before publication admission.
#[allow(clippy::large_enum_variant)]
pub(crate) enum HlsPresentationResolution {
    Ready(
        HlsContext,
        plurx_core::domain::MediaFile,
        MediaResponseOwner,
    ),
    Failed(PlaylistPublicationError),
    StateChanged,
    Gone,
}

/// A typed playlist refusal plus the exact rolling incarnation that produced
/// it. `SessionGone` deliberately carries no owner; live startup and immutable
/// failure responses must revalidate this owner before HTTP exposes them.
pub(crate) struct PlaylistPublicationError {
    pub(crate) error: PlaylistError,
    pub(crate) owner: Option<MediaResponseOwner>,
}

impl PlaylistPublicationError {
    pub(super) fn gone() -> Self {
        Self {
            error: PlaylistError::SessionGone,
            owner: None,
        }
    }

    pub(super) fn for_session(error: PlaylistError, session: &Arc<Session>) -> Self {
        Self {
            error,
            owner: Some(MediaResponseOwner(MediaResponseOwnerKind::Rolling {
                session: Arc::clone(session),
                producer_attempt: session.control.current_producer_attempt(),
            })),
        }
    }

    pub(super) fn without_owner(error: PlaylistError) -> Self {
        Self { error, owner: None }
    }
}

#[derive(Clone)]
pub(super) enum MediaResponseOwnerKind {
    Rolling {
        session: Arc<Session>,
        producer_attempt: u64,
    },
    Vod(crate::vodserve::ResponseOwner),
}

#[derive(Clone, Copy)]
pub(super) enum MediaResponsePublicationBinding {
    GenerationMetadata,
    AttemptMedia,
    AttemptStatus,
    ProtocolOnly,
}

/// The response class requested by HTTP before any bytes become visible.
/// Object names are retained only for exact EOF/frontier accounting; actor
/// admission is derived from this typed class, never from URL parsing.
pub(crate) struct MediaResponsePublication {
    pub(super) kind: &'static str,
    pub(super) object_name: Option<String>,
    pub(super) binding: MediaResponsePublicationBinding,
}

impl MediaResponsePublication {
    pub(crate) fn generation_metadata(kind: &'static str) -> Self {
        Self {
            kind,
            object_name: None,
            binding: MediaResponsePublicationBinding::GenerationMetadata,
        }
    }

    pub(crate) fn attempt_media(kind: &'static str, object_name: Option<&str>) -> Self {
        Self {
            kind,
            object_name: object_name.map(str::to_owned),
            binding: MediaResponsePublicationBinding::AttemptMedia,
        }
    }

    /// A bodyless status derived from one exact producer attempt. Unlike media
    /// publication this validates ownership without closing prepublication or
    /// advancing any delivery frontier.
    pub(crate) fn attempt_status(kind: &'static str, object_name: Option<&str>) -> Self {
        Self {
            kind,
            object_name: object_name.map(str::to_owned),
            binding: MediaResponsePublicationBinding::AttemptStatus,
        }
    }

    #[allow(dead_code)] // Reserved for generation-scoped control/redirect responses.
    pub(crate) fn protocol_only(kind: &'static str) -> Self {
        Self {
            kind,
            object_name: None,
            binding: MediaResponsePublicationBinding::ProtocolOnly,
        }
    }

    pub(super) fn rolling_object(&self) -> crate::playback_control::RollingResponseObject {
        use crate::playback_control::RollingResponseObject as Object;
        match self.kind {
            "master-playlist" => Object::MasterPlaylist,
            "playlist" => Object::VideoMediaPlaylist,
            "subtitle-playlist" => Object::SubtitleMediaPlaylist,
            "subtitle-segment" => Object::SubtitleSegment,
            "init-segment" => Object::InitializationSegment,
            "media-segment" => Object::MediaSegment,
            "segment-not-modified" => Object::NotModified,
            "segment-range-not-satisfiable" => Object::RangeNotSatisfiable,
            "segment-range" => Object::ByteRange,
            "status" => Object::SessionStatus,
            _ => Object::ProtocolResponse,
        }
    }

    pub(super) fn rolling_media_segment_index(
        &self,
        object: crate::playback_control::RollingResponseObject,
    ) -> Option<i64> {
        use crate::playback_control::RollingResponseObject as Object;
        match object {
            Object::MediaSegment | Object::ByteRange | Object::NotModified => {
                self.object_name.as_deref().and_then(segment_index)
            }
            _ => None,
        }
    }
}

/// Move-only authorization for one prepared HTTP response. Dropping it is a
/// non-commit (including 416); consuming it at exact EOF is the only path that
/// renews demand or advances a rolling/VOD frontier.
pub(crate) struct MediaResponseAuthorization {
    pub(super) session_id: String,
    pub(super) owner: MediaResponseOwner,
    pub(super) release_gate: Arc<SessionReleaseGate>,
    pub(super) admitted_serving_generation: u64,
    pub(super) kind: &'static str,
    pub(super) object_name: Option<String>,
    pub(super) rolling_generation_metadata_fingerprint: Option<String>,
    /// Keeps this exact object's bytes charged while the body streams, even
    /// after cleanup removes its name. A directory scan cannot see an
    /// unlinked-but-open file; the filesystem still owes the space. Dropping
    /// the authorization — at EOF, on cancellation, on a transport error —
    /// settles the pin exactly once.
    pub(super) scratch_pin: Option<crate::scratch_ledger::ScratchPin>,
}

impl MediaResponseAuthorization {
    /// Charge the exact object this response opened to its session's ledger
    /// entry for the lifetime of the body. Concurrent range readers of the
    /// same object share one charge.
    ///
    /// An unrelated `Arc<Session>` held by a status reader is not a pin and
    /// never was; only an accepted read is.
    pub(crate) fn pin_scratch_object(&mut self, bytes: u64) {
        if self.scratch_pin.is_some() {
            return;
        }
        let MediaResponseOwnerKind::Rolling { session, .. } = &self.owner.0 else {
            return;
        };
        let Some(name) = self.object_name.as_deref() else {
            return;
        };
        let Some(permit) = session.scratch.as_ref() else {
            return;
        };
        self.scratch_pin = permit.ledger().acquire_pin(
            permit.key(),
            name,
            i64::try_from(bytes).unwrap_or(i64::MAX),
        );
    }
}

/// Prepare a concrete, non-reentrant first-media transfer before asking the
/// actor to authorize a response. The detached waiter survives cancellation
/// of the HTTP request. Its acknowledgement lets the successful caller wait
/// until manager ownership has changed before publishing response bytes.
#[cfg(test)]
pub(super) async fn begin_first_media_publication_handoff(
    session: &Arc<Session>,
    session_id: &str,
) -> Option<(
    crate::playback_control::RollingFirstMediaPublicationHandoff,
    tokio::sync::oneshot::Receiver<bool>,
)> {
    let deadline = tokio::time::Instant::now().into_std() + Duration::from_secs(5);
    begin_first_media_publication_handoff_before(session, session_id, deadline).await
}

pub(super) async fn begin_first_media_publication_handoff_before(
    session: &Arc<Session>,
    _session_id: &str,
    deadline: Instant,
) -> Option<(
    crate::playback_control::RollingFirstMediaPublicationHandoff,
    tokio::sync::oneshot::Receiver<bool>,
)> {
    let permit = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        Arc::clone(first_media_settlement_slots()).acquire_owned(),
    )
    .await
    .ok()?
    .ok()?;
    let handoff = crate::playback_control::RollingFirstMediaPublicationHandoff::with_prepublication_projection(
        Arc::clone(&session.actor_prepublication_producer),
        permit,
    );
    let waiter = handoff.waiter();
    let session = Arc::clone(session);
    let (applied, applied_response) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let accepted =
            match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), waiter.wait())
                .await
            {
                Ok(accepted) => accepted,
                Err(_) => session.control.settle_first_media_at_deadline(&waiter),
            };
        if accepted {
            #[cfg(test)]
            let pause = session
                .first_media_owner_claim_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            #[cfg(test)]
            if let Some(pause) = pause {
                pause.reached.notify_one();
                pause.release.notified().await;
            }
            // Attempt-media authorization is the exact linearization point at
            // which the rolling actor becomes the sole transcode lifetime
            // owner. The decision executor remains registered across this
            // handoff; no detached compatibility watcher is elected here.
            session.first_media_handoff_applied.store(true, Release);
            session.first_media_handoff_notify.notify_waiters();
            session.first_media_handoff_notify.notify_one();
        }
        let _ = applied.send(accepted);
    });
    Some((handoff, applied_response))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentOpenError {
    /// Authenticated snapshot memory is still owned by earlier response
    /// bodies. This is an admission outcome, not evidence that the immutable
    /// cache generation is corrupt.
    Capacity,
}

/// Exact outcome of resolving one rolling media object. Negative results keep
/// the Session/attempt owner that produced the verdict so HTTP can fence a
/// bodyless 404/502/503 before it becomes visible.
pub(crate) enum SegmentPublication {
    Ready(Box<SegmentFile>),
    Missing(Option<MediaResponseOwner>),
    /// The object was found for this exact owner, but its metadata could not
    /// be inspected. Absence and corrupt bytes are both stronger claims than
    /// the storage layer can make in this state.
    Unavailable(MediaResponseOwner),
    Pending(MediaResponseOwner),
    Failed(PlaylistPublicationError),
}

/// Who is reading the segment behind a [`SegmentDelivery`].
///
/// Almost every open is a client fetch, but `exact_hls_context` opens
/// `init.mp4` during master-playlist generation to read the exact HEVC tier
/// out of `hvcC`. No response body exists on that path, so its bytes are not
/// delivery and a stall on it is not a client-visible freeze. Folding the two
/// together is exactly the availability/delivery conflation this tracker
/// exists to remove, so the purpose travels with the tracker: it decides
/// whether the session's delivery meter moves, and it is stamped on every
/// event so an operator reading a freeze can tell a playlist-time codec sniff
/// from a segment the player was waiting on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryPurpose {
    /// Bytes on their way to a client response body.
    ClientResponse,
    /// A server-internal read with no response behind it.
    InternalProbe,
}

impl DeliveryPurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::ClientResponse => "client_response",
            Self::InternalProbe => "internal_probe",
        }
    }
}

/// Per-response HLS delivery accounting.
///
/// Segment availability ends when the file opens; storage and transport can
/// still stall while the response body is being read. Keeping this tracker in
/// the body stream makes `delivered_bps` describe bytes actually read from the
/// segment, and gives slow/error reads their own telemetry instead of folding
/// them into producer starvation.
pub(crate) struct SegmentDelivery {
    store: Arc<dyn Store>,
    session: Arc<Session>,
    producer_attempt: u64,
    session_id: String,
    segment: String,
    segment_start_ms: Option<i64>,
    segment_duration_ms: Option<i64>,
    method: &'static str,
    encoder: String,
    purpose: DeliveryPurpose,
    expected_bytes: u64,
    delivered_bytes: u64,
    started_at: Instant,
    slow_read_reported: bool,
    terminal: bool,
    // Keeps the global authenticated-snapshot memory permit for exactly the
    // lifetime of the response or internal probe consuming that snapshot.
    _snapshot_lease: Option<plurx_core::transcode::manifest::VerifiedObjectLease>,
}

pub(super) struct SegmentDeliveryContext {
    pub(super) store: Arc<dyn Store>,
    pub(super) session: Arc<Session>,
    pub(super) producer_attempt: u64,
    pub(super) session_id: String,
    pub(super) segment: String,
    pub(super) segment_start_ms: Option<i64>,
    pub(super) segment_duration_ms: Option<i64>,
    pub(super) encoder: String,
}

impl SegmentDelivery {
    pub(super) fn new(
        context: SegmentDeliveryContext,
        expected_bytes: u64,
        snapshot_lease: Option<plurx_core::transcode::manifest::VerifiedObjectLease>,
    ) -> Self {
        let SegmentDeliveryContext {
            store,
            session,
            producer_attempt,
            session_id,
            segment,
            segment_start_ms,
            segment_duration_ms,
            encoder,
        } = context;
        let method = match session.method {
            crate::delivery::Method::Direct => "direct_play",
            crate::delivery::Method::Remux | crate::delivery::Method::HlsCopy => "remux",
            crate::delivery::Method::Transcode => "transcode",
        };
        Self {
            store,
            session,
            producer_attempt,
            session_id,
            segment,
            segment_start_ms,
            segment_duration_ms,
            method,
            encoder,
            purpose: DeliveryPurpose::ClientResponse,
            expected_bytes,
            delivered_bytes: 0,
            started_at: Instant::now(),
            slow_read_reported: false,
            terminal: false,
            _snapshot_lease: snapshot_lease,
        }
    }

    /// Re-label this tracker as a server-internal read.
    ///
    /// The bytes stop feeding the session's delivery meter — nothing is being
    /// delivered — and every event this tracker emits carries the purpose.
    pub(crate) fn into_internal_probe(mut self) -> Self {
        self.purpose = DeliveryPurpose::InternalProbe;
        self
    }

    /// Lower the completion expectation to the bytes the caller will actually
    /// ask storage for.
    ///
    /// A reader bounded below the segment's real length (an `init.mp4` past
    /// the inspection bound, say) returns every byte it was asked for and then
    /// stops. Without this, `finish()` compares that short read against the
    /// full file length and reports `storage_unexpected_eof` for a read that
    /// completed exactly as requested.
    pub(crate) fn expect_at_most(&mut self, bytes: u64) {
        self.expected_bytes = self.expected_bytes.min(bytes);
    }

    /// Mark a conditional or unsatisfiable response that intentionally has no
    /// media body. Opening the authenticated object proved the capability;
    /// the HTTP contract owes zero bytes and must not look like abandonment.
    pub(crate) fn finish_without_body(&mut self) {
        self.expected_bytes = 0;
        self.finish();
    }

    fn emit(&self, event: &str, reason: &str, ms: i64, mut extra: serde_json::Value) {
        // Stamped centrally rather than at each call site: an untagged
        // `segment_delivery_*` row is indistinguishable from a client fetch,
        // which is the whole failure this field exists to prevent.
        if let Some(fields) = extra.as_object_mut() {
            fields.insert(
                "purpose".to_owned(),
                serde_json::Value::from(self.purpose.as_str()),
            );
            fields.insert(
                "producer_attempt".to_owned(),
                serde_json::Value::from(self.producer_attempt),
            );
            fields.insert(
                "response_incarnation".to_owned(),
                serde_json::Value::from(self.session.response_incarnation.to_string()),
            );
            fields.insert(
                "segment_start_ms".to_owned(),
                self.segment_start_ms
                    .map_or(serde_json::Value::Null, serde_json::Value::from),
            );
            fields.insert(
                "segment_duration_ms".to_owned(),
                self.segment_duration_ms
                    .map_or(serde_json::Value::Null, serde_json::Value::from),
            );
            let producer_superseded = self.session.replacing_child.load(Acquire)
                || self.session.control.current_producer_attempt() != self.producer_attempt;
            fields.insert(
                "producer_superseded".to_owned(),
                serde_json::Value::from(producer_superseded),
            );
            // A dropped body proves only that this response ended before EOF.
            // A new browser attachment can abandon an old Session without
            // changing that Session's producer attempt, so the server must not
            // manufacture "client_cancelled" or "client_superseded" here.
            fields.insert(
                "client_disposition".to_owned(),
                serde_json::Value::from("unknown"),
            );
            fields.insert(
                "cut_class".to_owned(),
                serde_json::Value::from(match reason {
                    "storage_unexpected_eof" => "source_eof",
                    "storage_read_error" => "storage_error",
                    "body_lifetime_exceeded" => "transport_lifetime",
                    "downstream_no_progress" => "transport_stall",
                    "response_dropped" if producer_superseded => "producer_superseded",
                    "response_dropped" => "unclassified_drop",
                    _ => "none",
                }),
            );
        }
        crate::telemetry::emit(
            Arc::clone(&self.store),
            PlaybackEvent {
                at_unix_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
                    .unwrap_or(0),
                session_id: Some(session_log_id(&self.session_id)),
                file_id: Some(self.session.file_id),
                event: event.to_owned(),
                method: Some(self.method.to_owned()),
                encoder: Some(self.encoder.clone()),
                height: Some(self.session.target_height),
                ms: Some(ms),
                speed_recent: self.session.progress.recent_speed(),
                suspended: Some(self.session.suspended.load(Relaxed)),
                delivered_bps: self.session.delivery.recent_bps().map(|bytes| bytes * 8),
                readrate: Some(self.session.readrate),
                reason: Some(reason.to_owned()),
                extra: Some(extra.to_string()),
                ..PlaybackEvent::default()
            },
        );
    }

    /// Credit bytes the consumer has acknowledged taking. The caller owns the
    /// acknowledgement fence; this is the accounting that follows it.
    pub(crate) fn note_delivered(&mut self, bytes: u64) {
        self.delivered_bytes = self.delivered_bytes.saturating_add(bytes);
        if self.purpose == DeliveryPurpose::ClientResponse {
            self.session.delivery.note(bytes);
        }
    }

    /// Report one storage read's size and duration for the stall signal.
    ///
    /// Separate from `note_delivered` because the two have different units: a
    /// body is delivered in `MEDIA_BODY_ACK_GRANULARITY` pieces but read from
    /// storage in `MEDIA_BODY_READ_BUFFER` ones, and a rate computed from the
    /// acknowledgement size against the whole read's duration would report
    /// every healthy large read as a stall.
    pub(crate) fn note_storage_read(&mut self, bytes: u64, elapsed: Duration) {
        if !storage_read_is_slow(bytes, elapsed) || self.slow_read_reported {
            return;
        }
        self.slow_read_reported = true;
        let waited_ms = elapsed.as_millis().min(i64::MAX as u128) as i64;
        tracing::warn!(
            session = %session_log_id(&self.session_id),
            segment = %self.segment,
            waited_ms,
            delivered_bytes = self.delivered_bytes,
            expected_bytes = self.expected_bytes,
            "HLS segment body read stalled on storage"
        );
        self.emit(
            "segment_delivery_wait",
            "storage_read_slow",
            waited_ms,
            serde_json::json!({
                "segment": self.segment,
                "delivered_bytes": self.delivered_bytes,
                "expected_bytes": self.expected_bytes
            }),
        );
    }

    /// One read whose whole size is also the delivery unit: the buffered
    /// init/probe paths, which hand the complete body over at once.
    pub(crate) fn note_read(&mut self, bytes: u64, elapsed: Duration) {
        self.note_delivered(bytes);
        self.note_storage_read(bytes, elapsed);
    }

    /// Finish delivery and report whether every advertised byte was read.
    /// This completion bit is the only authorization to move the consumed
    /// frontier for a streamed response.
    pub(crate) fn finish(&mut self) -> bool {
        if self.terminal {
            return self.delivered_bytes >= self.expected_bytes;
        }
        self.terminal = true;
        if self.delivered_bytes >= self.expected_bytes {
            return true;
        }
        let elapsed_ms = self.started_at.elapsed().as_millis().min(i64::MAX as u128) as i64;
        tracing::warn!(
            session = %session_log_id(&self.session_id),
            segment = %self.segment,
            delivered_bytes = self.delivered_bytes,
            expected_bytes = self.expected_bytes,
            "HLS segment response reached EOF before its advertised length"
        );
        self.emit(
            "segment_delivery_incomplete",
            "storage_unexpected_eof",
            elapsed_ms,
            serde_json::json!({
                "segment": self.segment,
                "delivered_bytes": self.delivered_bytes,
                "expected_bytes": self.expected_bytes
            }),
        );
        false
    }

    pub(crate) fn fail(&mut self, error: &std::io::Error) {
        self.fail_with_reason(error, "storage_read_error", "storage");
    }

    /// Record a response-body transport failure without teaching adaptive
    /// control that local storage stalled. The caller supplies one stable,
    /// bounded reason from the HTTP lifecycle (`body_lifetime_exceeded` or
    /// `downstream_no_progress`).
    pub(crate) fn fail_transport(&mut self, error: &std::io::Error, reason: &'static str) {
        debug_assert!(matches!(
            reason,
            "body_lifetime_exceeded" | "downstream_no_progress"
        ));
        self.fail_with_reason(error, reason, "transport");
    }

    fn fail_with_reason(
        &mut self,
        error: &std::io::Error,
        reason: &'static str,
        failure_domain: &'static str,
    ) {
        if self.terminal {
            return;
        }
        self.terminal = true;
        let elapsed_ms = self.started_at.elapsed().as_millis().min(i64::MAX as u128) as i64;
        tracing::error!(
            session = %session_log_id(&self.session_id),
            segment = %self.segment,
            delivered_bytes = self.delivered_bytes,
            expected_bytes = self.expected_bytes,
            failure_domain,
            %error,
            "HLS segment response failed before delivery completed"
        );
        self.emit(
            "segment_delivery_incomplete",
            reason,
            elapsed_ms,
            serde_json::json!({
                "segment": self.segment,
                "delivered_bytes": self.delivered_bytes,
                "expected_bytes": self.expected_bytes,
                "failure_domain": failure_domain,
                "error": error.to_string()
            }),
        );
    }
}

impl Drop for SegmentDelivery {
    fn drop(&mut self) {
        if self.terminal || self.delivered_bytes >= self.expected_bytes {
            return;
        }
        self.terminal = true;
        let elapsed_ms = self.started_at.elapsed().as_millis().min(i64::MAX as u128) as i64;
        tracing::warn!(
            session = %session_log_id(&self.session_id),
            segment = %self.segment,
            delivered_bytes = self.delivered_bytes,
            expected_bytes = self.expected_bytes,
            "HLS segment response was dropped before delivery completed"
        );
        self.emit(
            "segment_delivery_incomplete",
            "response_dropped",
            elapsed_ms,
            serde_json::json!({
                "segment": self.segment,
                "delivered_bytes": self.delivered_bytes,
                "expected_bytes": self.expected_bytes
            }),
        );
    }
}

/// The source timeline behind one capability-authenticated HLS session.
/// Subtitle child requests resolve this instead of accepting a file id from
/// the URL, so one session capability can never be used to read another file.
///
/// `codecs` and `supplemental_codecs` describe the exact formats in the
/// session's primary rendition. The native Apple master uses them only for
/// HDR variants: Apple requires the exact Main10/Dolby declaration alongside
/// `VIDEO-RANGE`, while leaving SDR masters codec-neutral preserves the broad
/// compatibility established by physical-device testing.
#[derive(Debug, Clone, PartialEq)]
pub struct HlsContext {
    pub file_id: i64,
    pub start_seconds: f64,
    /// The source timestamp that this session's media calls t=0. See
    /// `Session::media_origin_seconds` — subtitle cue shifting uses this, and
    /// using `start_seconds` instead is the P0-2 defect.
    pub media_origin_seconds: f64,
    pub codecs: String,
    pub supplemental_codecs: Option<String>,
    /// Maximum video frame rate from the source probe. Apple requires this on
    /// every video variant in a multivariant playlist.
    pub frame_rate: Option<f64>,
}

/// Why a session could not hand back a media playlist.
///
/// This used to be the `None` half of an `Option`, which collapsed every
/// terminal cause into one anonymous 404: a producer that exited non-zero, a
/// watchdog stall verdict, a session superseded by the same viewer's next
/// seek, and a startup that simply ran out of budget all read identically to
/// the client. The operator behind #263 was told "the server couldn't build
/// this stream — see Settings → Logs" for a failure the server had already
/// classified and logged, on a headless box they were not sitting at. The
/// cause is known at the moment it is decided; it is carried from there.
///
/// [`Self::code`] is the stable machine-readable half and [`Self::message`] is
/// the sentence a viewer reads, so a client that wants to choose a recovery
/// action never has to parse prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistError {
    /// No session is registered under this id: its window expired, the same
    /// viewer's next seek superseded it, or it never existed.
    SessionGone,
    /// The producer exited non-zero before publishing a usable playlist. The
    /// string is ffmpeg's rendered exit status — the one fact that separates
    /// "this build refused the source" from "something killed it".
    ProducerExited(String),
    /// An actor-managed producer ended after media from its exact attempt had
    /// already been admitted. Published objects remain readable; this verdict
    /// is returned only when a request needs media beyond the retained
    /// published frontier.
    ProducerEnded(String),
    /// The session was failed after it started: an actor-owned producer
    /// verdict or a fallback that could not spawn. Carries the operator-facing
    /// detail recorded at the moment of the verdict.
    SessionFailed(String),
    /// The requested playback rate drained runway faster than this producer
    /// could replace it.  This is distinct from an opaque encoder failure so
    /// clients and operators can choose a lower rate/rendition.
    InsufficientCapacity(String),
    /// Still starting when [`PLAYLIST_WAIT_BUDGET`] ran out. Not terminal —
    /// the session may yet publish — so clients are told to retry rather than
    /// to give up.
    StartupTimedOut(Duration),
}

impl PlaylistError {
    /// Stable machine-readable cause. Clients branch on this, never on the
    /// message.
    pub fn code(&self) -> &'static str {
        match self {
            PlaylistError::SessionGone => "session_gone",
            PlaylistError::ProducerExited(_) => "producer_failed",
            PlaylistError::ProducerEnded(_) => "producer_ended",
            PlaylistError::SessionFailed(_) => "session_failed",
            PlaylistError::InsufficientCapacity(_) => "rolling_insufficient_capacity",
            PlaylistError::StartupTimedOut(_) => "startup_timeout",
        }
    }

    /// One sentence naming what went wrong, written for the person watching
    /// rather than the person reading the log.
    pub fn message(&self) -> String {
        match self {
            PlaylistError::SessionGone => {
                "this stream is no longer running — it expired, or another play \
                 replaced it"
                    .to_owned()
            }
            PlaylistError::ProducerExited(status) => format!(
                "the server's encoder exited before it produced any video ({status}) — \
                 this build of ffmpeg could not handle this source, audio track or \
                 subtitle burn-in"
            ),
            PlaylistError::ProducerEnded(reason) => format!(
                "the server's encoder ended after publishing part of this stream \
                 ({reason}); media already listed remains available"
            ),
            PlaylistError::SessionFailed(detail) => {
                format!("the server could not build this stream: {detail}")
            }
            PlaylistError::InsufficientCapacity(detail) => {
                format!("this server cannot sustain the requested playback rate: {detail}")
            }
            PlaylistError::StartupTimedOut(waited) => format!(
                "the server is still preparing this stream after {}s — a subtitle \
                 burn-in or 4K re-encode can take this long to start",
                waited.as_secs()
            ),
        }
    }

    /// Terminal causes are worth reporting once; a startup that merely ran out
    /// of budget is the client's cue to ask again.
    pub fn retryable(&self) -> bool {
        matches!(self, PlaylistError::StartupTimedOut(_))
    }
}
