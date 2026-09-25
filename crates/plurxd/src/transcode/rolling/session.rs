use super::*;

pub(super) struct Session {
    pub(super) dir: PathBuf,
    /// Process-local response incarnation. The bearer session id can be reused
    /// by a durable reattachment, while segment names and lengths can repeat
    /// across producer attempts. Strong HTTP validators therefore need this
    /// additional immutable coordinate.
    pub(super) response_incarnation: uuid::Uuid,
    pub(super) frozen_presentation: Option<FrozenHlsPresentation>,
    /// True for every rolling generation whose response publication is
    /// admitted by the actor. Producer recovery may still remain compatibility
    /// owned; response ownership does not imply prepublication retry policy.
    pub(super) actor_managed_response_publication: bool,
    /// Immutable scope bit for the one actor-managed prepublication process
    /// cut. The mutable projection below clears at actor acceptance, before the
    /// detached waiter can install its successor owner, so retirement uses this
    /// bit plus `first_media_handoff_applied` to close that handoff interval.
    pub(super) actor_managed_prepublication_process: bool,
    /// This bit is only a projection of the actor's exact first-media
    /// authorization. It begins true for every actor-managed producer cut and is
    /// cleared by that authorization, never by a compatibility observation.
    pub(super) actor_prepublication_producer: Arc<AtomicBool>,
    /// Serializes actor response authorization through the exact first-media
    /// ownership transfer. It is per Session, so one slow generation cannot
    /// block unrelated registry traffic.
    pub(super) response_publication_transition: Mutex<()>,
    /// Set only after the actor-owned published lifetime is active.
    /// Concurrent/cancelled response requests wait on this shared completion,
    /// not a request-local handoff future.
    pub(super) first_media_handoff_applied: AtomicBool,
    pub(super) first_media_handoff_notify: tokio::sync::Notify,
    /// Exactly one cancellation-safe task owns actor-managed prepublication
    /// reap and resource settlement, whether entered by failure or routine
    /// retirement. It remains true while an unconfirmed child keeps the
    /// Session and its admission resources retained for repair.
    pub(super) prepublication_cleanup_active: AtomicBool,
    /// Monotonic claim made only after actor Terminal admission is
    /// irreversible. Exactly one detached owner may then remove this exact
    /// Arc, confirm physical reap, and release admissions. Unique scratch is
    /// transferred afterward to its own retrying lifecycle owner.
    /// It deliberately remains set after convergence so a late reaper cannot
    /// recreate a second cleanup owner for the same generation.
    pub(super) retirement_cleanup_started: AtomicBool,
    /// Published only after registry removal, confirmed child reap, and
    /// admission release have all completed. Supersession
    /// followers use it to join an already-claimed exact cleanup owner.
    pub(super) retirement_cleanup_finished: AtomicBool,
    /// Exact shared physical-settlement receipt. The first cause wins; every
    /// admin, cache, prepublication, reaper, or supersession follower joins
    /// this same result rather than starting or mislabeling another cleanup.
    pub(super) retirement_settlement: std::sync::Mutex<Option<Arc<RollingRetirementSettlement>>>,
    /// Exact once-only claim for this Session's unique scratch directory.
    /// Both legacy prepublication fallback and universal retirement converge
    /// here; after the finite attempt budget, maintenance owns the orphan.
    pub(super) scratch_cleanup_started: AtomicBool,
    /// Weak registry ownership lets detached prepublication failure paths join
    /// manager retirement without forming Session -> registry -> Session.
    pub(super) retirement_context: Option<RollingRetirementContext>,
    /// Monotonic ownership latch for a cache generation that failed byte
    /// verification after it had already been admitted for serving. The first
    /// observer publishes the typed failure synchronously, then one detached
    /// owner invalidates the location and retires this exact Session. Keeping
    /// the latch set also lets the move-only response owner authenticate that
    /// failure after retirement removes the process-local registry entry.
    pub(super) cache_integrity_cleanup_started: AtomicBool,
    /// The ffmpeg producing this session's segments — `None` for a cache hit,
    /// where the segments already exist and there is nothing to run, watch,
    /// suspend or kill.
    pub(super) child: Mutex<Option<AttemptChild>>,
    /// Serializes an actor-authorized kill-to-successor interval with terminal,
    /// flow, retention, and response-path lifecycle work.
    pub(super) child_transition: Mutex<()>,
    /// True only while an actor-approved fallback deliberately replaces one
    /// live producer with another inside this same session. The predecessor's
    /// non-zero kill status belongs to the failed attempt, never its successor.
    pub(super) replacing_child: AtomicBool,
    #[cfg(test)]
    pub(super) replacement_pause: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
    /// Test-only rendezvous after the actor snapshot for a per-session
    /// activity read. It proves the manager registry lock was released before
    /// telemetry can wait and makes attempt replacement interleavings exact.
    #[cfg(test)]
    pub(super) activity_detail_pause: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
    /// Test-only seam after the control actor has accepted and ticketed a
    /// command while the HTTP-owned transition guard is still held.
    #[cfg(test)]
    pub(super) control_applied_pause: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
    /// Set synchronously by the actor's accepted-End callback and cleared only
    /// after the replicated terminal commit settles. The reaper observes it
    /// under `child_transition`, so no actor-to-continuation reply gap exists.
    pub(super) terminal_response_pending: Arc<AtomicBool>,
    /// One actor-installed terminal operation shared by the original request
    /// and every exact retry. It owns physical convergence, response
    /// projection and the durable acknowledgement receipt.
    pub(super) terminal_control: std::sync::Mutex<Option<RollingTerminalOperation>>,
    /// Test-only seam after producer policy is applied but before the flow
    /// ticket is completed back to a waiting control response.
    #[cfg(test)]
    pub(super) flow_completion_pause: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
    /// Test-only rendezvous immediately before first-playlist publication.
    #[cfg(test)]
    pub(super) playlist_publication_pause: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
    /// Test-only rendezvous after actor authorization and before the exact
    /// producer fence is reacquired for child assignment.
    #[cfg(test)]
    pub(super) producer_install_pause: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
    /// Test-only rendezvous after a refresh reads its playlist and before it
    /// merges the prepared observation.
    #[cfg(test)]
    pub(super) refresh_after_read_pause: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
    /// Test-only rendezvous after a path reader's first replacement-marker
    /// check and before it samples compatibility/actor attempt ownership.
    #[cfg(test)]
    pub(super) path_owner_sample_pause: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
    /// Test-only rendezvous after retention resolves its doomed names while
    /// holding the path-ownership transition.
    #[cfg(test)]
    pub(super) retention_delete_pause: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
    #[cfg(test)]
    pub(super) response_projection_pause: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
    /// Test-only seam after actor settlement cleared prepublication and before
    /// the detached waiter publishes that actor lifetime ownership is active.
    #[cfg(test)]
    pub(super) first_media_owner_claim_pause: std::sync::Mutex<Option<Arc<LifecycleTestPause>>>,
    /// Test-only proof that teardown reached the shared transition before a
    /// paused replacement is released.
    #[cfg(test)]
    pub(super) retirement_started: AtomicBool,
    /// Test-only seam after actor Terminal has handed exact cleanup to the
    /// universal detached owner but before registry removal. It proves caller
    /// cancellation cannot revoke either pre- or post-publication ownership.
    #[cfg(test)]
    pub(super) retirement_cleanup_handoff_pause: std::sync::Mutex<Option<Arc<LifecycleTestPause>>>,
    /// Test-only seam owned by the detached scratch lifecycle. Physical
    /// process settlement and replacement admission must finish before this
    /// independently retrying filesystem work is allowed to proceed.
    #[cfg(test)]
    pub(super) scratch_cleanup_pause: std::sync::Mutex<Option<Arc<LifecycleTestPause>>>,
    /// Served from the pre-transcode cache.
    ///
    /// A cached session has no process, and its directory is a finished asset
    /// this session did not produce and other viewers will want. Deleting it on
    /// the way out, which is what rolling sessions do with unique scratch,
    /// would destroy the cache one playback at a time.
    pub(super) cached: bool,
    /// Process-local read ownership for a finished cache entry. Kept for the
    /// session's full lifetime so the budget sweep cannot remove its playlist
    /// or segments while an HTTP response can still reach them.
    pub(super) _cache_reader: Option<crate::cachekeep::CacheReadGuard>,
    /// Exact bounded text-subtitle inode inherited by ffmpeg as `/dev/fd/5`.
    /// Keeping it for the session lifetime also lets a fallback child inherit
    /// the same bytes without reopening a replaceable pathname.
    pub(super) subtitle_handle: Option<std::fs::File>,
    /// Windows cannot hand stock ffmpeg a seekable kernel handle. Keep the
    /// authorized source and output directory alive so every initial or retry
    /// launch can revalidate its pathname against the exact held object.
    #[cfg(windows)]
    source_handle: Option<std::fs::File>,
    #[cfg(windows)]
    output_handle: Option<plurx_core::fs_secure::SecureDirectory>,
    /// Small authenticated inventory loaded once at offer time. Media objects
    /// are verified only when requested, not walked before playback starts.
    pub(super) cache_manifest: Option<Arc<plurx_core::transcode::manifest::GenerationManifest>>,
    /// Exact durable identity used to invalidate this location if a requested
    /// object later fails the generation manifest.
    pub(super) cache_location: Option<CachedLocationIdentity>,
    /// The one owner of control sequencing, demand, media renewal, expiry, and
    /// retirement ordering for this rolling generation. Legacy clients begin
    /// in compatibility lease mode; the first accepted explicit exchange
    /// binds the full demand snapshot without creating a second liveness
    /// clock.
    pub(super) control: crate::playback_control::RollingControlHandle,
    /// Raw writer revisions are private staged inventory. Only the immutable
    /// snapshot in this actor-authorized clock may cross the HTTP boundary.
    pub(super) publication: Mutex<RollingPublicationClock>,
    pub(super) publication_worker_started: AtomicBool,
    /// One detached, cancellation-safe consumer applies every actor-owned
    /// producer-policy wake. HTTP response futures may disappear after the
    /// actor accepts a command; this worker must not disappear with them.
    pub(super) flow_worker_started: AtomicBool,
    // -- metadata for the activity page --
    pub(super) file_id: i64,
    pub(super) item_id: i64,
    pub(super) item_title: String,
    pub(super) user_name: String,
    /// Namespaced immutable user id for clustered sessions, or the legacy
    /// username scope for process-local callers.
    pub(super) supersession_user: String,
    /// The player instance that owns this session — the supersession key.
    pub(super) playback_id: String,
    /// The durable identity this session reserves its recovery budget against.
    ///
    /// `None` for a cached serve and for a test fixture: neither has a
    /// producer, so neither can fault, so neither can spend a budget.
    pub(super) recovery: Option<SessionRecoveryIdentity>,
    /// Whether this session's height came from server Auto policy. A manual
    /// height is sticky across a stall reopen; only Auto sessions may move
    /// down the ladder without another viewer choice.
    pub(super) automatic: bool,
    /// The normalized delivery kind that created this session. A stall reopen
    /// bound to a manual session repeats this exact route instead of silently
    /// turning an Original/copy delivery into a transcode.
    pub(super) kind: SessionKind,
    /// Re-encoding the picture, or only repackaging it. Immutable, unlike
    /// `encoder_label`: what this session *is* does not change when the
    /// encoder behind it does, and the activity page must not relabel a copy
    /// as a transcode because a fallback fired or a cache hit answered.
    pub(super) method: crate::delivery::Method,
    /// Where this session's timeline begins in the source, so a recovered
    /// (idempotent) create reports the same offset the first one did.
    pub(super) start_seconds: f64,
    /// Where the session's media *actually* begins in the source, which is
    /// not always what was asked for. A transcode seeks accurately and starts
    /// exactly at `start_seconds`; a copy session seeks with
    /// `-noaccurate_seek` and therefore begins at the keyframe before it, up
    /// to a full GOP earlier. Anything that maps a source timestamp onto this
    /// session's timeline has to subtract THIS, not the request — subtitle
    /// cues did the latter and led the picture by up to six seconds on 4K
    /// film GOPs.
    pub(super) media_origin_seconds: f64,
    /// RFC 6381 sample types present in the primary HLS rendition. A native
    /// subtitle master must advertise these; omitting or abbreviating the
    /// referenced formats makes AVPlayer reject an otherwise playable copy.
    /// The dynamic range this session's bytes actually carry. Recorded on the
    /// session, not recomputed from the request, because an idempotent
    /// recovery must report what the running encoder is producing rather than
    /// what a later reader would decide.
    pub(super) grade: OutputGrade,
    /// Backward-compatible enhancement carried by the video samples, such as
    /// Dolby Vision Profile 8.1 over an HDR10 HEVC base layer. Apple requires
    /// this outside CODECS so clients that only understand the base can still
    /// select the variant.
    pub(super) target_height: i64,
    /// The explicit CPU tone-map input and its bounded provenance. Absent for
    /// copy delivery and GPU graphs, which do not consume this CPU-chain fact.
    pub(super) tone_map_peak_nits: Option<u32>,
    pub(super) tone_map_peak_source: Option<&'static str>,
    /// The encoder actually running *now*. Mutable because the
    /// hardware->software fallback replaces the process inside one session,
    /// and an activity page still naming the hardware encoder after that is
    /// reporting a pipeline that no longer exists.
    pub(super) encoder_label: Mutex<&'static str>,
    pub(super) started_unix: i64,
    /// Set when the session can never produce output (hardware and software
    /// both failed to emit a first segment). Playlist/segment reads then fail
    /// fast so the player shows an error instead of waiting on a gray screen.
    pub(super) failed: Arc<AtomicBool>,
    /// *Why* [`Self::failed`] was set, recorded at the moment of the verdict.
    ///
    /// The flag alone is what made every terminal cause anonymous downstream:
    /// whoever failed the session knew the exit status or the stall detail,
    /// and the next playlist reader had only a bool. Written under the same
    /// action that sets the flag, so a later reader reports the first — and
    /// therefore the real — cause rather than whichever one it can still
    /// observe. A `std::sync::Mutex` because it is a one-line store read on a
    /// path that never awaits while holding it.
    pub(super) failure: std::sync::Mutex<Option<PlaylistError>>,
    /// True once the first playlist response is allowed out. Cached assets and
    /// copy sessions start true: VOD is already complete, and the copy
    /// segmenter owns its separate 12-second publication gate. A live
    /// transcode flips this exactly once when its small startup cushion exists.
    pub(super) playlist_published: AtomicBool,
    /// Highest segment index the client has fetched (-1 before the first).
    /// Kept for logs and for resolving the frontier against the index; the
    /// accounting itself works in media time.
    pub(super) high_segment: Arc<AtomicI64>,
    /// Exact producer attempt represented by the compatibility frontier
    /// atomics. The short synchronous gate makes an accepted predecessor EOF
    /// and a successor reset order without blocking response EOF on process
    /// transition I/O.
    pub(super) compatibility_attempt: Arc<std::sync::Mutex<u64>>,
    /// The client's DOWNLOAD frontier in session-relative ms: the end of the
    /// furthest segment served, from that segment's own `EXTINF`. Not the
    /// playhead — a client fetches its whole forward buffer ahead of the
    /// picture — and every name and log line here says so.
    pub(super) fetched_end_ms: Arc<AtomicI64>,
    /// What the playlist says is published, refreshed as segments complete.
    pub(super) segments: Mutex<SegmentIndex>,
    /// Published bytes past the client's frontier, cached from the last
    /// refresh. This is the PACING number: how much reserve the client has.
    /// It is not a disk number — retention keeps [`RETENTION_SECS`] of
    /// media *behind* the frontier too, and those bytes are just as much on
    /// the disk (review §2.7).
    pub(super) ahead_bytes: AtomicI64,
    /// Everything this session has on disk that retention has not deleted —
    /// ahead of the frontier and behind it alike. This is the BUDGET number:
    /// what the global scratch cap sums. The two used to be one figure, and
    /// the cap it produced was not a bound: several healthy sessions could
    /// exceed the documented ceiling by their whole retention windows.
    pub(super) live_bytes: Arc<AtomicI64>,
    /// Admission charged before the producer starts. The permit names this
    /// exact incarnation's entry in the scratch ledger and stays with it from
    /// provisional start through retirement; dropping it is a backstop, not
    /// the event that returns capacity. Cleanup releases the entry once the
    /// names are gone and every accepted read has closed.
    pub(super) scratch: Option<crate::scratch_ledger::ScratchPermit>,
    /// The retired object promise, shared with the cleanup owner so an exact
    /// same-viewer release can pull it in once and wake the sleeper.
    pub(super) retired_release: Arc<RetiredRelease>,
    /// How far past its measured bytes this producer stays authorized between
    /// flow evaluations. Zero for a session admitted with its whole ceiling,
    /// which has nothing to grow into.
    pub(super) scratch_envelope: i64,
    /// Where FFmpeg's HLS muxer writes for this session: every object it
    /// uploads passes the scratch grant before it reaches the disk
    /// ([`crate::scratch_put`]). `None` where no muxer attempt can run.
    pub(super) upload: Option<crate::scratch_put::PutSink>,
    /// Bytes renamed out of served segment paths but not yet physically
    /// unlinked. Hidden garbage still consumes the same scratch budget.
    pub(super) retention_garbage_bytes: Arc<AtomicI64>,
    /// One coalesced cleanup queue/owner per session. A slow NAS unlink cannot
    /// create an unbounded stack of detached filesystem workers.
    pub(super) retention_cleanup_queue: Arc<std::sync::Mutex<Vec<(PathBuf, i64)>>>,
    pub(super) retention_cleanup_active: Arc<AtomicBool>,
    /// Live encode telemetry (see [`Progress`]).
    pub(super) progress: Arc<Progress>,
    /// What kind of work this is, for the admission record (see
    /// [`crate::admission::Workload::class`]). Kept on the session because the
    /// speed that matters is measured while it runs, long after the file that
    /// described it went out of scope. Mutable because the hardware→software
    /// fallback replaces the encoder inside one session — speeds measured
    /// after that are software speeds, and recording them under the hardware
    /// class would poison the very measurements admission decides by.
    pub(super) class: std::sync::Mutex<String>,
    /// The hardware slot this session holds.
    ///
    /// Released two ways, on purpose. `release_hardware` hands it back the
    /// moment the session ends, which is what makes the cap *prompt*: actor
    /// and executor tasks can retain an `Arc` while physical cleanup settles,
    /// so waiting for the last reference would unnecessarily retain a slot.
    /// Dropping the session returns it too, which makes the cap *complete* —
    /// every way a session can end, including paths with no explicit branch.
    pub(super) hw_slot: std::sync::Mutex<Option<HwSlot>>,
    /// This session's reservation from the software CPU pool, when it runs
    /// (or fell back to) a software encoder. Held for the session's life and
    /// released the same two ways as the hardware slot, for the same two
    /// reasons: promptly via `release_software`, completely via drop.
    pub(super) sw_permit: std::sync::Mutex<Option<crate::admission::SwPermit>>,
    /// Additional CPU this session reserved *after* admission, when a recovery
    /// moved more of its pipeline onto the CPU. Held separately rather than
    /// replacing `sw_permit`, because replacing it drops the original — the
    /// session would pay for both and end up owning only the second.
    pub(super) sw_delta_permit: std::sync::Mutex<Option<crate::admission::SwPermit>>,
    /// Bytes of segment actually handed to this client, and how fast.
    ///
    /// The player cannot measure this for itself on every transport: native
    /// HLS (Safari's HEVC path) exposes no such number, and hls.js's estimator
    /// exists only when hls.js is the one fetching. The server serves every
    /// segment on every path, so it is the one place the answer always exists.
    pub(super) delivery: Meter,
    /// Media requests currently parked waiting for publication; see
    /// [`HttpWaitLedger`].
    pub(super) http_waits: HttpWaitLedger,
    /// Effective input pace for this session; 0 means unpaced.
    pub(super) readrate: f64,
    /// True while the child is SIGSTOPped for running too far ahead of the
    /// playhead. Everything that judges a session's health has to know: a
    /// suspended encoder makes no progress *on purpose*.
    pub(super) suspended: AtomicBool,
    /// When the current held interval began, for the resume event's duration.
    pub(super) suspended_at: Mutex<Option<SuspendedAt>>,
    /// Successful running→held transitions during this session. A counter,
    /// rather than only the current boolean, exposes flapping after it has
    /// already resumed.
    pub(super) suspend_count: AtomicU64,
    /// Fenced successor coordinates for URI and playlist continuity.
    pub(super) takeover: Option<SessionTakeoverStart>,
    /// The first retained-prefix advance gets one operational log line. A
    /// playlist reload may observe that state hundreds of times; only the
    /// transition records retention starting to advance the visible window.
    pub(super) first_slide_logged: AtomicBool,
}

pub(super) enum SessionReapVerdict {
    Live(String, Arc<Session>),
    CleanupOwned,
    Expired {
        id: String,
        session: Arc<Session>,
        idle_seconds: u64,
        last_request: &'static str,
        cleanup_reason: &'static str,
    },
}

/// Keeps the replacement marker true across every await between actor
/// admission and publishing (or terminally failing) its successor.
///
/// Before admission, cancellation is harmless and releases the marker. After
/// admission, the actor and compatibility projections already name a new
/// attempt. Dropping that transaction must fail closed: reopening old paths
/// would serve predecessor bytes under successor ownership.
pub(super) struct ChildReplacement<'a> {
    session: &'a Session,
    _transition: tokio::sync::MutexGuard<'a, ()>,
    state: ChildReplacementState,
    cancellation_failure: Option<PlaylistError>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChildReplacementState {
    PreAdmission,
    AdmissionPendingOrAccepted,
    Completed,
    TerminallySettled,
}

impl ChildReplacement<'_> {
    /// The actor can commit an attempt before its oneshot reply is delivered.
    /// Mark that cancellation-sensitive interval before awaiting the command.
    pub(super) fn mark_admission_pending(&mut self) {
        self.state = ChildReplacementState::AdmissionPendingOrAccepted;
    }

    pub(super) fn mark_actor_decision_pending(&mut self, failure: PlaylistError) {
        self.cancellation_failure = Some(failure);
        self.mark_admission_pending();
    }

    /// A typed rejection proves the actor did not change path ownership.
    #[cfg(test)]
    fn admission_rejected(&mut self) {
        self.state = ChildReplacementState::PreAdmission;
    }

    /// Publish completion and release the transition when this call returns.
    /// A completed transaction must not remain a hidden mutex owner merely
    /// because its caller keeps the now-useless local binding in scope.
    pub(super) fn complete(mut self) {
        self.state = ChildReplacementState::Completed;
        self.session.replacing_child.store(false, Release);
    }

    /// Final installation can lose to an actor-owned terminal verdict after
    /// the candidate was spawned. Keep path ownership fenced without turning
    /// that ordinary race into a synthetic "cancelled" session failure.
    pub(super) fn settle_terminal_rejection(&mut self) {
        self.state = ChildReplacementState::TerminallySettled;
        self.request_terminal_fence();
    }

    fn request_terminal_fence(&self) {
        if self.session.control.is_retired() {
            return;
        }
        // Drop cannot await and Session is borrowed by the transition guard.
        // Retire through the cloneable actor handle; the ordinary repair loop
        // owns child/scratch cleanup. Keep `replacing_child` true until then so
        // no request can reopen predecessor paths in the successor attempt.
        let control = self.session.control.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if control.end().await.is_err() || !control.is_retired() {
                    control.fence_unavailable();
                }
            });
        } else {
            control.fence_unavailable();
        }
    }
}

impl Drop for ChildReplacement<'_> {
    fn drop(&mut self) {
        match self.state {
            ChildReplacementState::PreAdmission | ChildReplacementState::Completed => {
                self.session.replacing_child.store(false, Release);
            }
            ChildReplacementState::TerminallySettled => {}
            ChildReplacementState::AdmissionPendingOrAccepted => {
                self.session
                    .fail(self.cancellation_failure.clone().unwrap_or_else(|| {
                        PlaylistError::SessionFailed(
                            "producer replacement was cancelled before publication".into(),
                        )
                    }));
                self.request_terminal_fence();
            }
        }
    }
}

#[derive(Default)]
pub(super) struct SessionEventFields<'a> {
    pub(super) reason: Option<&'a str>,
    pub(super) extra: Option<String>,
    pub(super) hold_reason: Option<AheadHoldReason>,
    pub(super) ms: Option<i64>,
}

pub(super) async fn emit_session_event_to_store(
    store: Arc<dyn Store>,
    session_id: &str,
    session: &Session,
    event: &str,
    fields: SessionEventFields<'_>,
) {
    let method = match session.method {
        crate::delivery::Method::Direct => "direct_play",
        crate::delivery::Method::Remux | crate::delivery::Method::HlsCopy => "remux",
        crate::delivery::Method::Transcode => "transcode",
    };
    let hold_reason = if let Some(reason) = fields.hold_reason {
        Some(reason)
    } else if session.suspended.load(Relaxed) {
        (*session.suspended_at.lock().await).map(|held| held.hold.reason)
    } else {
        None
    }
    .map(|reason| {
        match reason {
            AheadHoldReason::Demand => "demand",
            AheadHoldReason::Time => "time",
            AheadHoldReason::Bytes => "bytes",
            AheadHoldReason::Global => "global",
        }
        .to_owned()
    });
    crate::telemetry::emit(
        store,
        PlaybackEvent {
            at_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
                .unwrap_or(0),
            session_id: Some(session_log_id(session_id)),
            file_id: Some(session.file_id),
            event: event.to_owned(),
            method: Some(method.to_owned()),
            encoder: Some((*session.encoder_label.lock().await).to_owned()),
            height: Some(session.target_height),
            ms: fields.ms,
            speed_recent: session.progress.recent_speed(),
            ahead_seconds: session.ahead().await.map(|ahead| ahead.seconds),
            suspended: Some(session.suspended.load(Relaxed)),
            hold_reason,
            delivered_bps: session.delivery.recent_bps().map(|bytes| bytes * 8),
            readrate: Some(session.readrate),
            reason: fields.reason.map(str::to_owned),
            extra: fields.extra,
            ..PlaybackEvent::default()
        },
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionRegistrationRejection {
    ServingFence,
    SessionRelease,
    AdoptionCapacity,
    Producer(crate::playback_control::ProducerAttemptRejection),
}

/// How far a session's published media runs ahead of the client's download
/// frontier, in seconds and in bytes.
///
/// Both terms are session-relative: ffmpeg's input seek restarts output
/// timestamps at zero and segment numbering restarts with the session, so the
/// subtraction needs no absolute timeline.
pub(super) fn ahead_of(index: &SegmentIndex, fetched_end_ms: i64) -> Option<Ahead> {
    let produced_end = index.produced_playable_end_ms()?;
    Some(Ahead {
        seconds: (produced_end - fetched_end_ms) / 1000,
        bytes: index.bytes_after_ms(fetched_end_ms),
    })
}

impl Session {
    /// Measure everything this session has in its scratch directory, and say
    /// whether the measurement is complete.
    ///
    /// Session scratch is one flat directory: `init.mp4`, `index.m3u8`, the
    /// segment objects, their `*.tmp` staging names and the
    /// `.plurx-retention-*` renames are all regular files at its top level.
    /// Nothing writes a subdirectory here, and nothing writes a symlink —
    /// `DirEntry::metadata` does not traverse one, so `is_file()` is false
    /// for a link and it is skipped rather than charged for its target. That
    /// undercounts a thing this directory never contains; it is recorded
    /// because a reader deserves to know which way the scanner is wrong.
    ///
    /// The sum is `metadata.len()` — apparent length in this namespace, not
    /// `st_blocks` and not a claim about physical reclamation.
    ///
    /// A read or stat failure keeps the previous charge, because a transient
    /// metadata error must never make capacity reappear. The caller is told
    /// which happened: retirement may only collapse a reservation onto a
    /// `Complete` inventory.
    pub(super) async fn measure_scratch_bytes(&self) -> ScratchMeasurement {
        if self.cached {
            return ScratchMeasurement::NotScratch;
        }
        let mut entries = match tokio::fs::read_dir(&self.dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return ScratchMeasurement::Absent;
            }
            Err(_) => return ScratchMeasurement::Incomplete,
        };
        let mut bytes = 0_i64;
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(_) => return ScratchMeasurement::Incomplete,
            };
            let metadata = match entry.metadata().await {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return ScratchMeasurement::Incomplete,
            };
            if metadata.is_file() {
                bytes = bytes.saturating_add(i64::try_from(metadata.len()).unwrap_or(i64::MAX));
            }
        }
        ScratchMeasurement::Complete(bytes)
    }

    /// The periodic sample. It publishes a measurement to the cached figure
    /// and to the ledger, and it can only be an observation: while a producer
    /// runs, a scan that happens to find few bytes is not evidence that the
    /// future capacity it was admitted with is no longer needed.
    pub(super) async fn refresh_scratch_bytes(&self) -> ScratchMeasurement {
        // What had landed before the walk began is all the walk can be
        // trusted to have seen.
        let written_before = self
            .scratch
            .as_ref()
            .map_or(0, |permit| permit.ledger().written_of(permit.key()));
        let measurement = self.measure_scratch_bytes().await;
        match measurement {
            ScratchMeasurement::Complete(bytes) => {
                self.live_bytes.store(bytes, Release);
                self.observe_scratch_bytes(bytes, written_before);
            }
            ScratchMeasurement::Absent => {
                self.live_bytes.store(0, Release);
                self.observe_scratch_bytes(0, written_before);
            }
            ScratchMeasurement::Incomplete | ScratchMeasurement::NotScratch => {}
        }
        measurement
    }

    pub(super) fn scratch_key(&self) -> Option<crate::scratch_ledger::ScratchKey> {
        self.scratch
            .as_ref()
            .map(crate::scratch_ledger::ScratchPermit::key)
    }

    pub(super) fn observe_scratch_bytes(&self, bytes: i64, written_before: i64) {
        if let Some(permit) = self.scratch.as_ref() {
            permit
                .ledger()
                .observe_walk(permit.key(), bytes, written_before);
        }
    }

    pub(super) fn ensure_publication_worker(self: &Arc<Self>, session_id: &str) {
        if self.cached || self.publication_worker_started.swap(true, AcqRel) {
            return;
        }
        let session = Arc::clone(self);
        let session_id = session_id.to_owned();
        tokio::spawn(async move {
            while !session.control.is_retired() {
                if let Err(reason) = session.publication_cycle(&session_id).await {
                    tracing::error!(
                        session = %session_log_id(&session_id),
                        %reason,
                        "rolling publication clock retired an invalid presentation"
                    );
                    session.fail(if reason.starts_with("rolling_insufficient_capacity:") {
                        PlaylistError::InsufficientCapacity(reason)
                    } else {
                        PlaylistError::SessionFailed(reason)
                    });
                    if let Some(ticket) =
                        spawn_context_retirement_owner(&session, None, "publication_clock")
                    {
                        let _ = ticket.wait().await;
                    }
                    break;
                }
                tokio::time::sleep(ROLLING_PUBLICATION_POLL).await;
            }
            session.publication_worker_started.store(false, Release);
        });
    }

    pub(super) async fn publication_cycle(&self, session_id: &str) -> Result<(), String> {
        self.publication_cycle_at(session_id, Instant::now()).await
    }

    pub(super) async fn publication_cycle_at(
        &self,
        session_id: &str,
        now: Instant,
    ) -> Result<(), String> {
        // A write the muxer's upload needed failed. FFmpeg does not read the
        // reply, so it would keep producing objects no playlist can name.
        if let Some(reason) = self
            .upload
            .as_ref()
            .and_then(crate::scratch_put::PutSink::failure)
        {
            return Err(reason);
        }
        let _ = self.refresh_scratch_bytes().await;
        if self.replacing_child.load(Acquire) {
            return Ok(());
        }
        let producer_attempt = self.control.current_producer_attempt();
        if self.compatibility_producer_attempt() != producer_attempt {
            return Ok(());
        }
        let Some(raw) =
            plurx_core::transcode::manifest::read_bounded_playlist(&self.dir, "index.m3u8")
                .await
                .map_err(|error| format!("reading rolling writer playlist: {error}"))?
        else {
            return Ok(());
        };
        if raw.is_empty() {
            return Ok(());
        }
        let text = std::str::from_utf8(&raw)
            .map_err(|_| "rolling writer playlist was not UTF-8".to_owned())?;
        if !text.lines().any(|line| line.trim().starts_with("#EXTINF:")) {
            return Ok(());
        }
        validate_rolling_target(&raw)?;
        let index = SegmentIndex {
            segs: parse_playlist(text),
            revision: 0,
        };
        let Some(last_segment) = index.segs.last().map(|segment| segment.index) else {
            return Ok(());
        };
        let Some(end_ms) = index.produced_playable_end_ms() else {
            return Ok(());
        };
        let end_list = text.lines().any(|line| line.trim() == "#EXT-X-ENDLIST");
        let lease = self.control.snapshot().await;
        let demand = lease.as_ref().and_then(|lease| lease.demand.as_ref());
        let playback_rate = rolling_playback_rate(demand);
        let initial_runway_ms = rolling_initial_runway_ms(playback_rate);
        let producer_speed = self.progress.recent_speed();
        let media_origin_ms = (self.media_origin_seconds * 1_000.0).round() as i64;
        let wants_early_publication = lease.as_ref().is_some_and(|lease| {
            lease.demand_observation_age.is_some_and(|age| {
                age <= Duration::from_millis(
                    crate::playback_control::ROLLING_EXPLICIT_LEASE_TIMEOUT_MS as u64,
                )
            }) && lease.demand.as_ref().is_some_and(|demand| {
                demand.demand == crate::playback_control::PlaybackDemand::Active
                    && demand.render_state == crate::playback_control::RenderState::Rendering
                    && demand.runway_ms() <= ROLLING_PUBLICATION_GUARD_MS
            })
        });
        let (publish, expired, insufficient, retention_first_segment, budget) = {
            let mut clock = self.publication.lock().await;
            clock.reset_for_attempt(producer_attempt);
            if clock
                .staged_last_segment
                .is_none_or(|current| last_segment >= current)
            {
                clock.staged_last_segment = Some(last_segment);
                clock.staged_end_ms =
                    Some(clock.staged_end_ms.map_or(end_ms, |old| old.max(end_ms)));
            }
            let budget =
                clock.publication_budget_at(now, producer_attempt, lease.as_ref(), media_origin_ms);
            let publish = match clock.served.as_ref() {
                None => end_list || end_ms >= budget.desired_end_ms,
                Some(served) if served.producer_attempt != producer_attempt => false,
                Some(served)
                    if end_list && (last_segment > served.last_segment || !served.end_list) =>
                {
                    true
                }
                Some(served) => {
                    clock.next_publish_at.is_some_and(|target| now >= target)
                        || (wants_early_publication
                            && now >= served.available_at + ROLLING_PUBLICATION_EARLIEST)
                }
            };
            let expired = !end_list
                && clock.served.as_ref().is_some_and(|served| {
                    clock.hard_deadline.is_some_and(|deadline| now >= deadline)
                        && end_ms <= served.end_ms
                });
            let insufficient = !end_list
                && rolling_insufficient_capacity(playback_rate, producer_speed)
                && match clock.served.as_ref() {
                    None => end_ms >= initial_runway_ms,
                    Some(served) => {
                        clock.hard_deadline.is_some_and(|deadline| now >= deadline)
                            && end_ms <= served.end_ms
                    }
                };
            (
                publish,
                expired,
                insufficient,
                clock.retention_first_segment,
                budget,
            )
        };
        if insufficient {
            return Err(format!(
                "rolling_insufficient_capacity: playback rate {playback_rate:.2}x requires at least {playback_rate:.2}x production; measured {}x",
                producer_speed.unwrap_or(0.0)
            ));
        }
        if expired {
            return Err(format!(
                "rolling publication missed its {}s hard deadline without a completed segment",
                ROLLING_PUBLICATION_HARD.as_secs()
            ));
        }
        if !publish {
            return Ok(());
        }

        let previous_served = self.publication.lock().await.served.clone();
        let first_new_segment = previous_served
            .as_ref()
            .map_or(i64::MIN, |served| served.last_segment.saturating_add(1));
        let selected_last = if end_list {
            last_segment
        } else if budget.demand_sequence.is_none() {
            // A legacy viewer has no accepted playback position with which to
            // authorize a rounded-up segment.  Its fixed wall/download clock
            // therefore exposes only the last complete endpoint *inside* the
            // allowance.  Requiring an endpoint at or beyond the desired
            // frontier deadlocks variable-duration startup whenever one
            // segment ends just below the boundary and the next ends above it.
            let Some(selected) = index.segs.iter().rev().find(|segment| {
                segment.index >= first_new_segment && segment.end_ms <= budget.allowed_end_ms
            }) else {
                return Ok(());
            };
            selected.index
        } else {
            let earned = index.segs.iter().find(|segment| {
                segment.index >= first_new_segment
                    && segment.end_ms >= budget.desired_end_ms
                    && segment.end_ms <= budget.allowed_end_ms
            });
            let floor = earned.or_else(|| {
                budget.demand_sequence.and_then(|_| {
                    index.segs.iter().find(|segment| {
                        segment.index >= first_new_segment
                            && segment.end_ms.saturating_sub(budget.consumed_end_ms)
                                <= ROLLING_RESERVE_MAX_MS
                    })
                })
            });
            let Some(selected) = floor else {
                if budget.demand_sequence.is_some()
                    && index
                        .segs
                        .iter()
                        .any(|segment| segment.index >= first_new_segment)
                {
                    return Err("rolling_window_budget_exhausted: next completed segment exceeds the active publication safety floor".to_owned());
                }
                return Ok(());
            };
            selected.index
        };
        if previous_served.as_ref().is_some_and(|served| {
            selected_last <= served.last_segment && (served.end_list || !end_list)
        }) {
            return Ok(());
        }
        let selected_end_ms = index
            .end_ms_of(selected_last)
            .ok_or_else(|| "rolling budget selected a missing segment".to_owned())?;
        let window_floor_ms = selected_end_ms.saturating_sub(ROLLING_SERVED_WINDOW_MS);
        let window_first_segment = index
            .segs
            .iter()
            .find(|segment| segment.end_ms > window_floor_ms)
            .map(|segment| segment.index)
            .unwrap_or(selected_last);
        let protected_first_segment = index
            .segs
            .iter()
            .find(|segment| segment.end_ms > budget.protected_position_ms)
            .map(|segment| segment.index)
            .unwrap_or(selected_last);
        if !end_list && window_first_segment > protected_first_segment {
            return Err("rolling_window_budget_exhausted: protected playback segment no longer fits the served window".to_owned());
        }
        let requested_first_segment = retention_first_segment
            .map_or(window_first_segment, |retained| {
                retained.max(window_first_segment)
            });
        let first_segment = requested_first_segment.min(protected_first_segment);
        let served_raw = served_live_playlist(
            raw,
            Some(first_segment),
            (selected_last < last_segment).then_some(selected_last),
            self.takeover.as_ref(),
        )
        .ok_or_else(|| "rolling snapshot could not retain a complete media window".to_owned())?;
        let served_index = SegmentIndex {
            segs: parse_playlist(&String::from_utf8_lossy(&served_raw)),
            revision: 0,
        };
        let served_first = served_index
            .segs
            .first()
            .map(|segment| segment.index)
            .ok_or_else(|| "rolling snapshot contained no served media".to_owned())?;
        let served_last = served_index
            .segs
            .last()
            .map(|segment| segment.index)
            .ok_or_else(|| "rolling snapshot contained no served media".to_owned())?;
        let served_end_ms = index
            .end_ms_of(served_last)
            .ok_or_else(|| "rolling snapshot had no playable end".to_owned())?;
        let served_start_ms = index
            .segs
            .iter()
            .find(|segment| segment.index == served_first)
            .map(|segment| segment.start_ms)
            .ok_or_else(|| "rolling snapshot had no playable start".to_owned())?;
        let served_duration_ms = served_end_ms.saturating_sub(served_start_ms);

        // Snapshot admission and publication share the same producer
        // transition as replacement and signaling. Actor acceptance happens
        // first; only its exact attempt may then make these bytes retrievable.
        let _transition = self.child_transition.lock().await;
        if self.replacing_child.load(Acquire)
            || self.control.current_producer_attempt() != producer_attempt
            || self.compatibility_producer_attempt() != producer_attempt
        {
            return Ok(());
        }
        let accepted = self
            .control
            .observe_publication(crate::playback_control::RollingPublicationObservation {
                producer_attempt,
                publication_commit: true,
                demand_sequence: budget.demand_sequence,
                produced_segment: Some(last_segment),
                produced_end_ms: Some(end_ms),
                playlist_ready: true,
                published_segment: Some(served_last),
                published_end_ms: Some(served_end_ms),
                published_first_segment: Some(served_first),
                published_start_ms: Some(served_start_ms),
                media_origin_ms,
                next_media_sequence: served_last.saturating_add(1),
                resolved_fetched_segment: None,
                resolved_fetched_end_ms: None,
            })
            .await;
        if !accepted {
            return Ok(());
        }
        // `now` is an injectable policy clock used to decide whether this
        // cycle is due.  Publication may have awaited disk and actor work
        // since then, so availability, object grace and the next deadlines
        // begin at the actual commit point rather than being backdated to the
        // policy observation.
        let available_at = Instant::now();
        let mut clock = self.publication.lock().await;
        if self.control.current_producer_attempt() != producer_attempt
            || clock.budget_attempt != Some(producer_attempt)
            || clock.budget_anchor_sequence != budget.demand_sequence
        {
            return Ok(());
        }
        let revision = clock
            .served
            .as_ref()
            .map_or(1, |served| served.revision.saturating_add(1));
        let previous_duration_ms = clock
            .served
            .as_ref()
            .map_or(served_duration_ms, |served| served.duration_ms);
        {
            let mut segments = self.segments.lock().await;
            for segment in segments.segs.iter_mut().filter(|segment| {
                segment.visibility.is_advertised() && segment.index < served_first
            }) {
                let segment_duration_ms = segment.end_ms.saturating_sub(segment.start_ms);
                let promise_ms = segment_duration_ms.saturating_add(previous_duration_ms);
                let promise =
                    Duration::from_millis(u64::try_from(promise_ms.max(0)).unwrap_or(u64::MAX));
                segment.visibility = SegmentVisibility::Grace {
                    removed_at: available_at,
                    serve_until: available_at.checked_add(promise).unwrap_or(available_at),
                };
            }
            segments.revision = segments.revision.wrapping_add(1);
        }
        clock.served = Some(ServedPlaylistSnapshot {
            raw: Arc::from(served_raw),
            producer_attempt,
            revision,
            last_segment: served_last,
            first_segment: served_first,
            end_ms: served_end_ms,
            duration_ms: served_duration_ms,
            end_list,
            available_at,
        });
        clock.carried_surplus_ms = served_end_ms.saturating_sub(budget.desired_end_ms).max(0);
        clock.next_publish_at = (!end_list).then(|| available_at + ROLLING_PUBLICATION_TARGET);
        clock.hard_deadline = (!end_list).then(|| available_at + ROLLING_PUBLICATION_HARD);
        let carried_surplus_ms = clock.carried_surplus_ms;
        drop(clock);
        self.control.request_flow();
        tracing::debug!(
            session = %session_log_id(session_id),
            producer_attempt,
            revision,
            last_segment,
            end_ms,
            budget_anchor_sequence = ?budget.demand_sequence,
            allowed_end_ms = budget.allowed_end_ms,
            carried_surplus_ms,
            demand_observation_age_ms = ?budget.observation_age_ms,
            end_list,
            "rolling playlist snapshot became available"
        );
        Ok(())
    }

    pub(super) async fn served_playlist(&self, producer_attempt: u64) -> Option<Arc<[u8]>> {
        self.publication
            .lock()
            .await
            .served
            .as_ref()
            .filter(|snapshot| snapshot.producer_attempt == producer_attempt)
            .map(|snapshot| Arc::clone(&snapshot.raw))
    }

    pub(super) async fn staged_publication_seconds(&self, producer_attempt: u64) -> Option<i64> {
        let clock = self.publication.lock().await;
        clock
            .served
            .as_ref()
            .filter(|snapshot| snapshot.producer_attempt == producer_attempt)
            .map(|_| clock.staged_seconds())
    }

    /// Convert the final advertised window into read-only object promises.
    /// The producer is already terminal when this runs; only the original
    /// paths, byte accounting, and immutable response incarnation survive.
    pub(super) async fn prepare_retired_object_promise(&self) -> Instant {
        let now = Instant::now();
        let playlist_duration_ms = self
            .publication
            .lock()
            .await
            .served
            .as_ref()
            .map_or(0, |served| served.duration_ms.max(0));
        let mut serve_until = now
            .checked_add(Duration::from_millis(
                u64::try_from(playlist_duration_ms).unwrap_or(u64::MAX),
            ))
            .unwrap_or(now);
        let mut segments = self.segments.lock().await;
        for segment in &mut segments.segs {
            match segment.visibility {
                SegmentVisibility::Advertised => {
                    let promise_ms = playlist_duration_ms
                        .saturating_add(segment.end_ms.saturating_sub(segment.start_ms))
                        .max(0);
                    let deadline = now
                        .checked_add(Duration::from_millis(
                            u64::try_from(promise_ms).unwrap_or(u64::MAX),
                        ))
                        .unwrap_or(now);
                    segment.visibility = SegmentVisibility::Grace {
                        removed_at: now,
                        serve_until: deadline,
                    };
                    serve_until = serve_until.max(deadline);
                }
                SegmentVisibility::Grace {
                    serve_until: existing,
                    ..
                } => serve_until = serve_until.max(existing),
                SegmentVisibility::Deleted => {}
            }
        }
        segments.revision = segments.revision.wrapping_add(1);
        // An eligible viewer released this session before it finished
        // retiring. Fold that in here rather than racing the cleanup owner's
        // sleep: the promise is published once, already shortened.
        let serve_until = match self.retired_release.released() {
            Some((released_at, allowance)) => serve_until.min(
                released_at
                    .checked_add(allowance)
                    .unwrap_or(serve_until)
                    .max(now),
            ),
            None => serve_until,
        };
        self.retired_release.publish(serve_until);
        serve_until
    }

    /// Record one accepted exact-incarnation release.
    ///
    /// Returns the shortened deadline when this release actually moved it.
    /// Non-renewing by construction: the first accepted release wins, a
    /// duplicate DELETE finds the latch taken, and the deadline can only move
    /// in. A conservative class records nothing at all.
    pub(super) fn accept_exact_release(
        &self,
        now: Instant,
        class: ReleaseClass,
    ) -> Option<Instant> {
        let allowance = class.allowance()?;
        if !self.retired_release.accept_release(now, allowance) {
            return None;
        }
        let deadline = now.checked_add(allowance)?;
        // Already retired: pull the published promise in and wake cleanup.
        // Not yet retired: `prepare_retired_object_promise` reads the
        // recorded release time above when it publishes.
        self.retired_release.shorten_to(deadline)
    }

    /// Bring every per-object grace deadline in with the promise.
    ///
    /// Without this the cleanup owner deletes at the shortened deadline while
    /// the segment index still advertises the old one, so a read admitted in
    /// between opens a file that is already gone and answers a bare miss
    /// instead of the retired/gone answer the contract specifies. Reads
    /// already accepted are untouched: they hold an open descriptor and a
    /// ledger pin, and neither is a deadline.
    pub(super) async fn shorten_object_grace(&self, deadline: Instant) {
        let mut segments = self.segments.lock().await;
        let mut moved = false;
        for segment in &mut segments.segs {
            if let SegmentVisibility::Grace {
                removed_at,
                serve_until,
            } = segment.visibility
            {
                if serve_until > deadline {
                    segment.visibility = SegmentVisibility::Grace {
                        removed_at,
                        serve_until: deadline,
                    };
                    moved = true;
                }
            }
        }
        if moved {
            segments.revision = segments.revision.wrapping_add(1);
        }
    }

    /// The identity a reservation against the recovery ledger is keyed by,
    /// when this session has one.
    ///
    /// `None` for a cached serve and a test fixture, which have no producer.
    /// `None` too for a legacy process-local start and a relayed worker start:
    /// both reach the daemon without a server-minted epoch, so both mean *no
    /// budget* rather than *an unspent budget* — a distinction a caller must
    /// not collapse, because reading "no epoch" as "a fresh one" grants an
    /// automatic recovery per attempt on exactly the paths that have no
    /// durable bound.
    ///
    /// The filter checks all three fields rather than the epoch alone. The
    /// store refuses a non-positive `user_id`, an empty incarnation and an
    /// empty epoch, and it refuses them separately — so a `Some` this method
    /// returned on the strength of the epoch alone would be a reservation the
    /// store rejects at the moment it is needed. Today the three legacy sites
    /// zero all three together and the epoch check happens to catch them, but
    /// "happens to" is not a contract, and this method is the one place a
    /// caller is entitled to trust.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn recovery_identity(&self) -> Option<&SessionRecoveryIdentity> {
        self.recovery.as_ref().filter(|recovery| {
            recovery.user_id > 0
                && !recovery.incarnation_id.is_empty()
                && !recovery.recovery_epoch.is_empty()
        })
    }

    pub(super) fn compatibility_producer_attempt(&self) -> u64 {
        *self
            .compatibility_attempt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Capture one producer attempt that coherently owns the compatibility
    /// paths. Sampling the compatibility tag before the actor attempt and
    /// checking the replacement marker on both sides closes the interval in
    /// which actor admission has advanced but predecessor paths still exist.
    pub(super) async fn coherent_path_producer_attempt(&self) -> Option<u64> {
        if self.replacing_child.load(Acquire) {
            return None;
        }
        #[cfg(test)]
        {
            let pause = self
                .path_owner_sample_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(pause) = pause {
                pause.wait().await;
                pause.wait().await;
            }
        }
        let compatibility_attempt = self.compatibility_producer_attempt();
        let producer_attempt = self.control.current_producer_attempt();
        (!self.replacing_child.load(Acquire) && compatibility_attempt == producer_attempt)
            .then_some(producer_attempt)
    }

    pub(super) fn compatibility_playlist_published(&self, producer_attempt: u64) -> Option<bool> {
        let projected_attempt = self
            .compatibility_attempt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (*projected_attempt == producer_attempt).then(|| self.playlist_published.load(Relaxed))
    }

    #[cfg(test)]
    pub(super) async fn pause_playlist_publication_for_test(&self) {
        let pause = self
            .playlist_publication_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(pause) = pause {
            pause.wait().await;
            pause.wait().await;
        }
    }

    pub(super) async fn publish_compatibility_playlist(
        &self,
        producer_attempt: u64,
        deadline: Instant,
    ) -> bool {
        if tokio::time::Instant::now().into_std() >= deadline {
            return false;
        }
        #[cfg(test)]
        self.pause_playlist_publication_for_test().await;
        if tokio::time::Instant::now().into_std() >= deadline {
            return false;
        }
        let projected_attempt = self
            .compatibility_attempt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *projected_attempt != producer_attempt {
            return false;
        }
        self.playlist_published.store(true, Relaxed);
        true
    }

    /// Reset every compatibility projection immediately after the actor
    /// allocates a successor attempt and before the predecessor is touched.
    /// The actor remains authoritative; this short gate exists only until M4
    /// removes the atomics that legacy pacing and pruning still consume.
    #[cfg(test)]
    pub(super) async fn reset_compatibility_delivery(&self, producer_attempt: u64) {
        {
            let mut projected_attempt = self
                .compatibility_attempt
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *projected_attempt = producer_attempt;
            self.high_segment.store(-1, Relaxed);
            self.fetched_end_ms.store(0, Relaxed);
            self.ahead_bytes.store(0, Relaxed);
        }
        *self.segments.lock().await = SegmentIndex::default();
        *self.publication.lock().await = RollingPublicationClock::default();
        if matches!(&self.kind, SessionKind::Transcode { .. }) {
            self.playlist_published.store(false, Relaxed);
        }
        self.progress.begin_fenced_attempt(producer_attempt);
    }

    /// Clear all legacy catalog/frontier/accounting projections only after
    /// predecessor scratch was verified empty and before actor retry
    /// admission. The attempt tag deliberately remains on the predecessor
    /// until the actor returns the exact admitted successor id; path serving
    /// is fenced for this whole interval by `replacing_child`.
    pub(super) async fn clear_compatibility_before_retry(&self) {
        self.high_segment.store(-1, Relaxed);
        self.fetched_end_ms.store(0, Relaxed);
        self.ahead_bytes.store(0, Relaxed);
        *self.segments.lock().await = SegmentIndex::default();
        *self.publication.lock().await = RollingPublicationClock::default();
        self.playlist_published.store(false, Relaxed);
        self.suspended.store(false, Release);
        *self.suspended_at.lock().await = None;
    }

    pub(super) async fn bind_retry_compatibility_attempt(&self, producer_attempt: u64) {
        *self
            .compatibility_attempt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = producer_attempt;
        self.progress.begin_fenced_attempt(producer_attempt);
    }

    /// Release predecessor scratch accounting only after the verified clear
    /// has removed every old served path. Admission resets the compatibility
    /// catalog earlier, but rename/removal has not freed physical bytes then.
    pub(super) fn confirm_predecessor_scratch_cleared(&self) {
        let mut cleanup_queue = self
            .retention_cleanup_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Verified-empty scratch means every queued hidden path is already
        // absent. Removing those identities under the same accounting lock
        // makes a detached cleaner's later NotFound a no-op instead of a
        // subtraction from the successor's totals.
        cleanup_queue.clear();
        self.retention_garbage_bytes.store(0, Release);
        self.live_bytes.store(0, Release);
    }

    /// The actor clears its compatibility prepublication projection before the
    /// detached first-media waiter can synchronously claim the retained
    /// lifetime owner. Until that second publication is visible, retirement
    /// must keep using the cancellation-safe confirmed-reap path.
    pub(super) fn prepublication_process_cleanup_required(&self) -> bool {
        self.actor_managed_prepublication_process && !self.first_media_handoff_applied.load(Acquire)
    }

    /// Fence every later renewal in the control actor before publishing the
    /// process-local serving verdict. Actor ordering replaces the old
    /// check-plus-two-lock activity-clock protocol.
    #[cfg(test)]
    pub(super) async fn end_activity(&self) {
        let _ = self.end_activity_until(None).await;
    }

    /// Preserve a replacement request's absolute deadline while waiting for
    /// the actor End round trip. Once the command has entered the actor queue,
    /// dropping this waiter cannot revoke it: Terminal applies independently
    /// of reply delivery and the ordinary reaper observes the retired actor.
    pub(super) async fn end_activity_until(
        &self,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<crate::playback_control::RollingTerminalCause, String> {
        let outcome = match deadline {
            Some(deadline) => match self.control.end_before(deadline.into_std()).await {
                Ok(outcome) => Ok(outcome),
                Err(crate::playback_control::RollingTerminalRequestError::AdmissionDeadline) => {
                    return Err(replacement_deadline_error());
                }
                Err(crate::playback_control::RollingTerminalRequestError::ControlUnavailable) => {
                    Err(())
                }
            },
            None => self.control.end().await.map_err(|_| ()),
        };
        match outcome {
            Ok(outcome) => {
                tracing::trace!(terminal = ?outcome.cause(), "rolling session end observed");
                Ok(outcome.cause())
            }
            Err(_) => {
                self.control.fence_unavailable();
                Ok(crate::playback_control::RollingTerminalCause::AuthorityFence)
            }
        }
    }

    /// Fence this generation because the node can no longer prove durable or
    /// cluster serving authority. This cause must survive later cleanup so
    /// failover is never misreported as an ordinary lifecycle end.
    pub(super) fn project_authority_fence(&self) {
        // This synchronous projection uses the same producer transition as
        // actor response admission. The actor imports the shared retirement
        // bit as typed AuthorityFence before applying its next queued command.
        self.control.fence_unavailable();
    }

    pub(super) async fn settle_authority_fence(&self) {
        match self.control.authority_fence().await {
            Ok(outcome) => {
                tracing::trace!(terminal = ?outcome.cause(), "rolling authority fence observed");
            }
            Err(_) => self.control.fence_unavailable(),
        }
    }

    /// Renew the actor-owned playback lease only if no serving fence
    /// linearized first.
    #[cfg(test)]
    pub(super) async fn touch_attempt_if_active(
        &self,
        kind: &'static str,
        producer_attempt: u64,
    ) -> bool {
        self.control
            .commit_media(
                kind,
                producer_attempt,
                None,
                None,
                None,
                tokio::time::Instant::now().into_std() + Duration::from_secs(5),
            )
            .await
    }

    /// Submit sequence acceptance and lease renewal as one actor command.
    /// Equal/stale controls return without mutating either fact.
    pub(super) async fn accept_control(
        &self,
        request: crate::playback_control::LocalControlRequest<'_>,
        deadline_unix_ms: i64,
        terminal_admission: Option<Arc<dyn crate::playback_control::RollingTerminalAdmission>>,
        preparation_admission: Option<
            Arc<dyn crate::playback_control::PreparationSettlementAdmission>,
        >,
    ) -> Option<
        Result<
            (
                crate::playback_control::ControlDisposition,
                u64,
                crate::playback_control::ControlAction,
                bool,
                Option<crate::playback_control::PreparationDirective>,
                crate::playback_control::ClientPlatform,
                // What the viewer changed and what the device can do about
                // it. A distinct type rather than a bare `bool`, which also
                // removes any chance of transposing it with
                // `acknowledged_end` at the tail of this tuple.
                crate::playback_control::SelectionObservation,
                i64,
                u32,
                u64,
                &'static str,
                bool,
            ),
            crate::playback_control::ControlStateError,
        >,
    > {
        let outcome = match self
            .control
            .control_before(
                request,
                deadline_unix_ms,
                terminal_admission,
                preparation_admission,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => return Some(Err(error)),
        };
        let acknowledged_end = outcome.lease.terminal
            == Some(crate::playback_control::RollingTerminalCause::End)
            && outcome.lease.demand.as_ref().is_some_and(|demand| {
                demand.demand == crate::playback_control::PlaybackDemand::End
            });
        let lease_state = outcome.lease.terminal.map_or(
            "active",
            crate::playback_control::RollingTerminalCause::status,
        );
        Some(Ok((
            outcome.disposition,
            outcome.accepted_sequence,
            outcome.action,
            outcome.action_suppressed,
            outcome.preparation_directive,
            outcome.platform,
            outcome.selection,
            outcome.lease.expires_at_unix_ms(),
            outcome.lease.timeout_ms(),
            outcome.flow_ticket,
            lease_state,
            acknowledged_end,
        )))
    }

    /// Where this generation's session-relative zero sits on the durable
    /// incarnation timeline.
    ///
    /// Derived, never stored, and derived from `media_origin_seconds` — the
    /// origin this session *achieved*. A remux cannot start anywhere but a
    /// keyframe, so it begins at or before the position it was asked for;
    /// recording the requested offset instead would report a frontier ahead
    /// of the media actually produced, and the next successor would resume
    /// past a span no generation ever fills. Because there is one field to
    /// read, no construction site can pick the wrong one.
    pub(super) fn frontier_offset_ms(&self) -> i64 {
        self.takeover.as_ref().map_or(0, |takeover| {
            ((self.media_origin_seconds * 1_000.0).round() as i64)
                .saturating_sub(takeover.origin_base_ms)
        })
    }

    /// Fail this session *and say why*, in one step.
    ///
    /// The pairing is the point: a bare `failed.store(true)` is how a cause
    /// that was known at the verdict became an anonymous 404 three layers
    /// later. First writer wins — the initial verdict is the real one, and a
    /// later reader that merely notices the flag must not overwrite it with a
    /// vaguer restatement.
    pub(super) fn fail(&self, reason: PlaylistError) {
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert(reason);
        // The actor-owned response contract reads this with Acquire while
        // authorizing copy/cache responses. This Release is the synchronous
        // half of that ordering: an earlier failure rejects publication,
        // while an authorization that sampled false linearized first.
        self.failed.store(true, Release);
    }

    /// The recorded cause, for a reader that has already seen `failed`.
    ///
    /// Falls back to an unnamed verdict rather than to "no failure": the flag
    /// is the authority on *whether* the session is dead, and a store this
    /// path cannot see is still a dead session.
    pub(super) fn failure_reason(&self) -> PlaylistError {
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| {
                PlaylistError::SessionFailed("the transcode stopped before it could start".into())
            })
    }

    /// Actor-owned post-publication producer failure, if this exact attempt has
    /// one. This is deliberately not projected into `Session::failed`: that
    /// compatibility fence would reject the playlist and segments which were
    /// already admitted before the producer ended.
    pub(super) async fn published_producer_ended_before(
        &self,
        producer_attempt: u64,
        deadline: Instant,
    ) -> Option<(PlaylistError, Option<i64>)> {
        if !self.actor_managed_prepublication_process {
            return None;
        }
        let snapshot = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.control.snapshot(),
        )
        .await
        .ok()
        .flatten()?;
        if snapshot.terminal.is_some()
            || snapshot.delivery.producer_attempt != producer_attempt
            || !snapshot.producer_control.producer_media_published
            || !snapshot.producer_control.producer_ended_with_proposal
        {
            return None;
        }
        let reason = snapshot.producer_control.decision_reason?;
        Some((
            PlaylistError::ProducerEnded(reason.to_owned()),
            snapshot.delivery.published_segment,
        ))
    }

    /// A cache-integrity failure owner remains an exact response capability
    /// after its detached cleanup removes this Session from the live registry.
    /// The monotonic cleanup latch and first-writer failure cell together make
    /// that exception specific to this Arc and this typed verdict; an ordinary
    /// stale owner still cannot authorize through an absent or reused id.
    pub(super) fn owns_retired_cache_integrity_failure(&self, error: &PlaylistError) -> bool {
        self.cached
            && self.cache_integrity_cleanup_started.load(Acquire)
            && self.failed.load(Relaxed)
            && matches!(
                error,
                PlaylistError::SessionFailed(reason)
                    if reason == CACHED_MEDIA_INTEGRITY_FAILURE
            )
            && self.failure_reason() == *error
    }

    pub(super) async fn begin_child_replacement(&self) -> ChildReplacement<'_> {
        let transition = self.child_transition.lock().await;
        self.replacing_child.store(true, Release);
        ChildReplacement {
            session: self,
            _transition: transition,
            state: ChildReplacementState::PreAdmission,
            cancellation_failure: None,
        }
    }

    #[cfg(test)]
    pub(super) async fn kill_child_for_replacement(
        &self,
    ) -> Result<(ChildReplacement<'_>, u64), crate::playback_control::ProducerAttemptRejection>
    {
        let mut replacement = self.begin_child_replacement().await;
        // Retirement uses the same transition. If it won first, this
        // previously scheduled fallback is stale and must not resurrect an
        // encoder after the manager removed the session. Holding the gate
        // through the check and successor publication also prevents
        // retirement from starting between this verdict and the caller's
        // install.
        if self.control.is_retired() {
            return Err(crate::playback_control::ProducerAttemptRejection::SessionEnded);
        }
        replacement.mark_admission_pending();
        let producer_attempt = match self.control.begin_producer_attempt().await {
            Ok(producer_attempt) => producer_attempt,
            Err(crate::playback_control::ProducerAttemptRejection::ControlUnavailable) => {
                replacement.settle_terminal_rejection();
                return Err(crate::playback_control::ProducerAttemptRejection::ControlUnavailable);
            }
            Err(rejection) => {
                replacement.admission_rejected();
                return Err(rejection);
            }
        };
        self.reset_compatibility_delivery(producer_attempt).await;
        self.kill_child().await;
        #[cfg(test)]
        {
            let pause = self
                .replacement_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(pause) = pause {
                pause.wait().await;
                pause.wait().await;
            }
        }
        Ok((replacement, producer_attempt))
    }

    /// Install a spawned replacement only if the actor still authorizes this
    /// exact attempt at the install linearization point. Actor expiry can win
    /// while teardown waits behind `child_transition`; staging the supervised
    /// process must never make it serveable merely because its earlier
    /// admission succeeded.
    ///
    /// The prepublication variant acquires the Session slot *before* spawning
    /// and places the new process under AttemptChild supervision without an
    /// intervening await. Cancellation can therefore never strand a raw Child
    /// outside the cleanup owner's confirmed-reap path.
    pub(super) async fn spawn_and_install_prepublication_child<F>(
        &self,
        producer_attempt: u64,
        spawn: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<ObservedFfmpeg, String>,
    {
        let mut slot = self.child.lock().await;
        if let Some(child) = slot.as_ref() {
            return Err(format!(
                "producer attempt {producer_attempt} cannot stage over installed attempt {}",
                child.producer_attempt
            ));
        }
        let (candidate, child_job, diagnostics) = spawn()?.into_parts();
        *slot = Some(AttemptChild::new_with_job(
            producer_attempt,
            candidate,
            Some(child_job),
            self.control.clone(),
            Some(diagnostics),
        ));
        drop(slot);

        let install_authorization = match self
            .control
            .authorize_producer_install(producer_attempt)
            .await
        {
            Ok(authorization) => authorization,
            Err(reason) => {
                let cleanup = terminate_exact_prepublication_child(self, producer_attempt).await;
                return Err(match cleanup {
                    Ok(()) => format!("actor rejected exact producer installation: {reason:?}"),
                    Err(error) => format!(
                        "actor rejected exact producer installation ({reason:?}); staged candidate reap failed: {error}"
                    ),
                });
            }
        };
        #[cfg(test)]
        {
            let pause = self
                .producer_install_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(pause) = pause {
                pause.wait().await;
                pause.wait().await;
            }
        }
        let final_rejection = {
            let slot = self.child.lock().await;
            if !slot
                .as_ref()
                .is_some_and(|child| child.producer_attempt == producer_attempt)
            {
                Some(crate::playback_control::ProducerAttemptRejection::SessionEnded)
            } else {
                self.control
                    .lock_authorized_producer_install(install_authorization)
                    .err()
            }
        };
        let Some(reason) = final_rejection else {
            return Ok(());
        };
        let cleanup = terminate_exact_prepublication_child(self, producer_attempt).await;
        Err(match cleanup {
            Ok(()) => format!("actor rejected final producer installation fence: {reason:?}"),
            Err(error) => format!(
                "actor rejected final producer installation fence ({reason:?}); staged candidate reap failed: {error}"
            ),
        })
    }

    /// Pipe-producing counterpart to `spawn_and_install_prepublication_child`.
    /// The raw Child enters exact-attempt supervision synchronously before the
    /// stdout reader is returned, so cancellation cannot detach either half.
    pub(super) async fn spawn_and_install_prepublication_pipe_child<F>(
        &self,
        producer_attempt: u64,
        spawn: F,
    ) -> Result<tokio::process::ChildStdout, String>
    where
        F: FnOnce() -> Result<(ObservedFfmpeg, tokio::process::ChildStdout), String>,
    {
        let mut slot = self.child.lock().await;
        if let Some(child) = slot.as_ref() {
            return Err(format!(
                "producer attempt {producer_attempt} cannot stage over installed attempt {}",
                child.producer_attempt
            ));
        }
        let (observed, stdout) = spawn()?;
        let (candidate, child_job, diagnostics) = observed.into_parts();
        *slot = Some(AttemptChild::new_with_job(
            producer_attempt,
            candidate,
            Some(child_job),
            self.control.clone(),
            Some(diagnostics),
        ));
        drop(slot);

        let install_authorization = match self
            .control
            .authorize_producer_install(producer_attempt)
            .await
        {
            Ok(authorization) => authorization,
            Err(reason) => {
                drop(stdout);
                let cleanup = terminate_exact_prepublication_child(self, producer_attempt).await;
                return Err(match cleanup {
                    Ok(()) => {
                        format!("actor rejected exact pipe producer installation: {reason:?}")
                    }
                    Err(error) => format!(
                        "actor rejected exact pipe producer installation ({reason:?}); staged candidate reap failed: {error}"
                    ),
                });
            }
        };
        #[cfg(test)]
        {
            let pause = self
                .producer_install_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(pause) = pause {
                pause.wait().await;
                pause.wait().await;
            }
        }
        let final_rejection = {
            let slot = self.child.lock().await;
            if !slot
                .as_ref()
                .is_some_and(|child| child.producer_attempt == producer_attempt)
            {
                Some(crate::playback_control::ProducerAttemptRejection::SessionEnded)
            } else {
                self.control
                    .lock_authorized_producer_install(install_authorization)
                    .err()
            }
        };
        let Some(reason) = final_rejection else {
            return Ok(stdout);
        };
        drop(stdout);
        let cleanup = terminate_exact_prepublication_child(self, producer_attempt).await;
        Err(match cleanup {
            Ok(()) => format!("actor rejected final pipe producer installation fence: {reason:?}"),
            Err(error) => format!(
                "actor rejected final pipe producer installation fence ({reason:?}); staged candidate reap failed: {error}"
            ),
        })
    }

    /// Install a spawned replacement only if the actor still authorizes this
    /// exact attempt at the install linearization point. Legacy copy callers
    /// still pass an already-spawned child; actor-owned transcodes use the
    /// cancellation-safe prepublication variant above.
    #[cfg(test)]
    async fn terminate_rejected_candidate(&self, mut candidate: Child, producer_attempt: u64) {
        let mut first_error = None;
        for signal_attempt in 1..=2 {
            match candidate.kill().await {
                Ok(()) => return,
                Err(error) => match candidate.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) if signal_attempt == 1 => {
                        first_error = Some(error);
                        tokio::task::yield_now().await;
                    }
                    Ok(None) => {
                        let first_error = first_error.as_ref().unwrap_or(&error);
                        tracing::error!(
                            producer_attempt,
                            first_error = %first_error,
                            retry_error = %error,
                            "rejected replacement remained live after two termination attempts"
                        );
                        self.fail(PlaylistError::SessionFailed(format!(
                            "rejected producer {producer_attempt} could not be terminated: {error}"
                        )));
                        return;
                    }
                    Err(status_error) => {
                        tracing::error!(
                            producer_attempt,
                            signal_error = %error,
                            %status_error,
                            "rejected replacement termination status could not be observed"
                        );
                        self.fail(PlaylistError::SessionFailed(format!(
                            "rejected producer {producer_attempt} termination could not be observed: {status_error}"
                        )));
                        return;
                    }
                },
            }
        }
    }

    #[cfg(test)]
    pub(super) async fn install_replacement_child(
        &self,
        producer_attempt: u64,
        candidate: Child,
    ) -> Result<(), crate::playback_control::ProducerAttemptRejection> {
        // Acquire the async slot before authorization. The exact actor fence
        // below is synchronous and must never be held across an await.
        let mut child = self.child.lock().await;
        let install_authorization = match self
            .control
            .authorize_producer_install(producer_attempt)
            .await
        {
            Ok(authorization) => authorization,
            Err(reason) => {
                drop(child);
                self.terminate_rejected_candidate(candidate, producer_attempt)
                    .await;
                return Err(reason);
            }
        };
        #[cfg(test)]
        {
            let pause = self
                .producer_install_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(pause) = pause {
                pause.wait().await;
                pause.wait().await;
            }
        }
        let rejected = {
            match self
                .control
                .lock_authorized_producer_install(install_authorization)
            {
                Ok(install) => {
                    *child = Some(AttemptChild::new(
                        producer_attempt,
                        candidate,
                        self.control.clone(),
                        None,
                    ));
                    drop(install);
                    None
                }
                Err(reason) => Some((candidate, reason)),
            }
        };
        drop(child);
        let Some((candidate, reason)) = rejected else {
            return Ok(());
        };
        // The std MutexGuard Result is gone before this await. Rejection owns
        // the candidate until SIGKILL is confirmed and the process is reaped;
        // no detached task or session slot can lose that lifecycle.
        self.terminate_rejected_candidate(candidate, producer_attempt)
            .await;
        Err(reason)
    }

    /// Stop the encoder, if there is one. A cache hit has no process; a
    /// session whose ffmpeg already exited has one that is already reaped.
    /// Keep the handle in the slot after waiting so exact-attempt lifecycle
    /// tests can inspect the supervisor's terminal record.
    #[cfg(test)]
    pub(super) async fn kill_child(&self) {
        if let Some(child) = self.child.lock().await.as_mut() {
            let _ = child.kill().await;
            let _ = child.try_wait_observed(&self.control);
        }
    }

    /// Bypass the prepublication-reap retention fence only when the caller has
    /// either confirmed physical reap or is intentionally transferring a live
    /// hardware producer to its admitted software successor.
    pub(super) fn release_hardware_after_confirmed_reap(&self) {
        let _ = self.hw_slot.lock().expect("hw slot mutex").take();
    }

    pub(super) fn release_software_after_confirmed_reap(&self) {
        let _ = self.sw_permit.lock().expect("sw permit mutex").take();
        let _ = self
            .sw_delta_permit
            .lock()
            .expect("sw delta permit mutex")
            .take();
    }

    /// The bookkeeping half of the hardware→software fallback, split out so a
    /// test can prove it. Two things must happen at the transition, not at
    /// teardown: the hardware slot goes back (a software session holding a
    /// GPU slot parks the next hardware start in the admission queue for as
    /// long as this session lives — potentially a whole film), and the
    /// admission class flips to software, so the speeds measured from here on
    /// are recorded as what they are rather than poisoning the hardware
    /// class's record with a software encoder's numbers.
    /// The mixed transition: keep the encoder and its hardware slot, add the
    /// CPU the pipeline has started spending.
    ///
    /// Deliberately not [`Self::demote_to_software`]. Releasing the hardware
    /// slot here would leave a live hardware encoder running with nothing
    /// reserved for it, and the next hardware start would be admitted onto the
    /// same block — one slot authorizing two encoders, which is the exact
    /// contention the cap exists to prevent. The class becomes a mixed one for
    /// the same reason the demotion changes class: measurements recorded under
    /// the all-hardware class would make every later hardware admission
    /// decision from numbers a software decode produced.
    pub(super) fn add_cpu_decode_reservation(&self, permit: crate::admission::SwPermit) {
        *self.sw_delta_permit.lock().expect("sw delta permit mutex") = Some(permit);
    }

    /// CPU threads this session has reserved, across both slots.
    ///
    /// A recovery that moves more of the pipeline onto the CPU owes the
    /// *difference*, not the whole estimate: the session is already paying for
    /// what it reserved at admission, and asking for the full amount again
    /// makes it pay twice to end up owning once.
    pub(super) fn software_threads_held(&self) -> usize {
        let base = self
            .sw_permit
            .lock()
            .expect("sw permit mutex")
            .as_ref()
            .map_or(0, crate::admission::SwPermit::threads);
        let delta = self
            .sw_delta_permit
            .lock()
            .expect("sw delta permit mutex")
            .as_ref()
            .map_or(0, crate::admission::SwPermit::threads);
        base + delta
    }

    pub(super) fn demote_to_software(
        &self,
        work: Workload<'_>,
        permit: crate::admission::SwPermit,
    ) {
        self.release_hardware_after_confirmed_reap();
        *self.class.lock().expect("class mutex") = work.software_class();
        // Forced, not negotiated — the viewer is already watching — but on
        // the books: the pool runs over budget and every later admission
        // sees it (review §2.4).
        *self.sw_permit.lock().expect("sw permit mutex") = Some(permit);
    }

    /// Re-read the playlist, take in what is newly published, and measure
    /// only that.
    ///
    /// Parsing and metadata reads are prepared outside the producer-transition
    /// gate. The final in-memory merge is attempt-revalidated under that gate,
    /// so slow storage cannot block stop/replacement/control while an old
    /// playlist still cannot enter its successor's compatibility index.
    pub(super) async fn refresh_segments(&self) {
        let _ = self.refresh_scratch_bytes().await;
        // Actor admission deliberately precedes predecessor teardown. During
        // that interval the current attempt already names the successor while
        // the directory can still contain predecessor bytes. Never prepare an
        // observation in that mixed state: otherwise it could wait on the
        // transition and merge the old index after successor publication.
        let Some(producer_attempt) = self.coherent_path_producer_attempt().await else {
            return;
        };
        // Capture the catalog generation before starting any storage work.
        // Capturing it after the playlist read gives an old read a new token
        // when another refresh lands during that read, allowing stale bytes
        // to overwrite the newer observation.
        let previous = self.segments.lock().await.clone();
        let base_revision = previous.revision;
        let pending_fetched_segment = self
            .control
            .snapshot()
            .await
            .and_then(|snapshot| snapshot.delivery.pending_fetched_segment);
        let Ok(raw) = tokio::fs::read(self.dir.join("index.m3u8")).await else {
            return;
        };
        #[cfg(test)]
        {
            let pause = self
                .refresh_after_read_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(pause) = pause {
                pause.wait().await;
                pause.wait().await;
            }
        }
        let mut observed = SegmentIndex {
            segs: parse_playlist(&String::from_utf8_lossy(&raw)),
            revision: 0,
        };
        // Preserve known sizes and visibility promises only when this observation agrees
        // with the snapshot it extends. A rewrite describes different files
        // even when it reused their names, so all of those sizes are measured
        // again.
        let compatible_prefix =
            previous
                .segs
                .iter()
                .zip(&observed.segs)
                .all(|(current, observed)| {
                    current.index == observed.index
                        && current.name == observed.name
                        && current.start_ms == observed.start_ms
                        && current.end_ms == observed.end_ms
                });
        if compatible_prefix {
            for (observed, previous) in observed.segs.iter_mut().zip(&previous.segs) {
                observed.bytes = previous.bytes;
                observed.visibility = previous.visibility;
            }
        }
        for segment in observed
            .segs
            .iter_mut()
            .filter(|segment| segment.bytes == 0 && segment.visibility.is_advertised())
        {
            if let Ok(meta) = tokio::fs::metadata(self.dir.join(&segment.name)).await {
                segment.bytes = meta.len() as i64;
            }
        }
        if self.replacing_child.load(Acquire)
            || self.compatibility_producer_attempt() != producer_attempt
        {
            return;
        }

        // Producer replacement owns this gate from actor admission through
        // successor publication. Only the final in-memory projection crosses
        // it; every filesystem operation above remains independently bounded
        // by the storage layer rather than blocking lifecycle actions.
        let (produced_segment, produced_end_ms, resolved_fetched_end_ms) = {
            let producer_transition = self.child_transition.lock().await;
            if self.control.current_producer_attempt() != producer_attempt {
                return;
            }
            let mut index = self.segments.lock().await;
            let projected_attempt = self
                .compatibility_attempt
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *projected_attempt != producer_attempt {
                return;
            }
            let Some(rebuilt) = index.merge_prepared(base_revision, observed) else {
                // Another same-attempt observation (or retention mutation)
                // landed while metadata was prepared. Its newer catalog is
                // authoritative; a later refresh will observe anything this
                // discarded read still has to add.
                return;
            };
            if rebuilt {
                tracing::debug!("segment index rebuilt — the playlist was truncated or replaced");
            }
            // Resolve the frontier against the fresh index: a segment served
            // before its EXTINF was known gets its real end time now.
            let high = self.high_segment.load(Relaxed);
            if high >= 0 {
                if let Some(end) = index.end_ms_of(high) {
                    self.fetched_end_ms.fetch_max(end, Relaxed);
                }
            }
            // A cached asset's bytes are not scratch, and must not be counted
            // as any. The global budget is a sum over every session, and it
            // decides whether live encoders get suspended — so a 6 GB cached
            // 4K title reported here would blow the budget the moment somebody
            // pressed play and hold every real encoder on the box. Those bytes
            // are already accounted for by the cache's own size budget.
            if !self.cached {
                if let Some(ahead) = ahead_of(&index, self.fetched_end_ms.load(Relaxed).max(0)) {
                    self.ahead_bytes.store(ahead.bytes, Relaxed);
                }
            }
            let result = (
                index.segs.last().map(|segment| segment.index),
                index.produced_playable_end_ms(),
                pending_fetched_segment.and_then(|segment| index.end_ms_of(segment)),
            );
            drop(projected_attempt);
            drop(index);
            drop(producer_transition);
            result
        };
        let served = self
            .publication
            .lock()
            .await
            .served
            .as_ref()
            .filter(|snapshot| snapshot.producer_attempt == producer_attempt)
            .cloned();
        // Never hold a filesystem/process or segment-index lock while waiting
        // on the actor mailbox. A replacement that wins after the projection
        // check increments the attempt first, and the actor rejects this stale
        // observation.
        let _ = self
            .control
            .observe_publication(crate::playback_control::RollingPublicationObservation {
                producer_attempt,
                publication_commit: false,
                demand_sequence: None,
                produced_segment,
                produced_end_ms,
                playlist_ready: served.is_some(),
                published_segment: served.as_ref().map(|snapshot| snapshot.last_segment),
                published_end_ms: served.as_ref().map(|snapshot| snapshot.end_ms),
                published_first_segment: None,
                published_start_ms: None,
                media_origin_ms: (self.media_origin_seconds * 1_000.0).round() as i64,
                next_media_sequence: served
                    .as_ref()
                    .map_or(0, |snapshot| snapshot.last_segment.saturating_add(1)),
                resolved_fetched_segment: pending_fetched_segment
                    .filter(|_| resolved_fetched_end_ms.is_some()),
                resolved_fetched_end_ms,
            })
            .await;
    }

    pub(super) async fn ahead(&self) -> Option<Ahead> {
        ahead_of(
            &*self.segments.lock().await,
            self.fetched_end_ms.load(Relaxed).max(0),
        )
    }
}

/// A client that has completed no delivery for this long, while media it has
/// not fetched is published, is wedged rather than slow. The Apple
/// `DeliveryStarvationDetector`'s idle threshold, so both sides call the same
/// session wedged.
pub(crate) const WEDGE_IDLE_MS: i64 = 16_000;
/// Published media the client has not fetched. The same detector's pending
/// threshold.
pub(crate) const WEDGE_GAP_MS: i64 = 10_000;

/// The server-side signature of a fetch wedge: the client completed no
/// delivery for `WEDGE_IDLE_MS` while `WEDGE_GAP_MS` of published media sat
/// unfetched.
///
/// A slow link fails the first term — it is still completing deliveries,
/// slowly — and so keeps the rung step it deserves. A wedge is not a link
/// verdict at all, and lowering quality for one costs the viewer picture for a
/// fault the link never had.
pub(super) fn delivery_wedge(
    delivered_idle_ms: i64,
    published_end_ms: Option<i64>,
    fetched_end_ms: i64,
) -> bool {
    delivered_idle_ms >= WEDGE_IDLE_MS
        && published_end_ms
            .is_some_and(|published| published.saturating_sub(fetched_end_ms) >= WEDGE_GAP_MS)
}

/// The two frontiers one live session is judged by: how far this client has
/// fetched, and how far media has been published for it.
///
/// One reader, because the rung a stall reopen gets and the numbers the
/// activity page shows are the same facts, and a second copy of these
/// expressions is how they would come to disagree. The lease is authority when
/// there is one; the in-memory index is the answer for a session that has
/// never exchanged control.
///
/// Takes the `segments` mutex. Never call it while holding `sessions`.
async fn delivery_frontier(
    session: &Session,
    lease: Option<&crate::playback_control::RollingLeaseSnapshot>,
) -> (i64, Option<i64>) {
    let fetched_end_ms = lease.map_or_else(
        || session.fetched_end_ms.load(Relaxed),
        |lease| lease.delivery.fetched_end_ms,
    );
    let published_end_ms = match lease {
        Some(lease) => lease.delivery.published_end_ms,
        None => session.segments.lock().await.produced_playable_end_ms(),
    };
    (fetched_end_ms, published_end_ms)
}

/// Whether this session is wedged, read from the same two frontiers the
/// activity page publishes.
pub(super) async fn delivery_wedge_signature(session: &Session) -> bool {
    let lease = session.control.snapshot().await;
    let (fetched_end_ms, published_end_ms) = delivery_frontier(session, lease.as_ref()).await;
    delivery_wedge(
        session.delivery.idle_for_ms(),
        published_end_ms,
        fetched_end_ms,
    )
}

/// One live session as the activity page and the stats overlay see it.
pub(super) async fn session_info(
    id: &str,
    s: &Session,
    limits: AheadLimits,
    global_live_bytes: i64,
    global_ahead_bytes: i64,
) -> SessionInfo {
    let observed_at = Instant::now();
    let lease = s.control.snapshot().await;
    let demand = lease.as_ref().and_then(|lease| lease.demand.as_ref());
    let media_origin_ms = (s.media_origin_seconds * 1_000.0).round() as i64;
    let ready_anchor_ms =
        demand.map(crate::playback_control::PlaybackDemandSnapshot::buffer_anchor_ms);
    #[cfg(test)]
    {
        let pause = s
            .activity_detail_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(pause) = pause {
            pause.wait().await;
            pause.wait().await;
        }
    }
    let (fetched_end_ms, published_end_ms) = delivery_frontier(s, lease.as_ref()).await;
    let (
        ahead,
        first_retained_segment,
        server_ready,
        produced_end_ms,
        advertised_bytes,
        grace_bytes,
        produced_segments,
    ) = {
        let index = s.segments.lock().await;
        (
            ahead_of(&index, fetched_end_ms.max(0)),
            index.first_retained_index(),
            ready_anchor_ms.map_or_else(ReadyCoverage::unavailable, |anchor| {
                index.server_ready(media_origin_ms, anchor)
            }),
            index.produced_playable_end_ms(),
            index.advertised_bytes(),
            index.grace_bytes(),
            index
                .segs
                .iter()
                .map(|segment| (segment.start_ms, segment.bytes))
                .collect::<Vec<_>>(),
        )
    };
    let (
        served_end_ms,
        served_revision,
        last_segment_advanced_idle_ms,
        next_publication_in_ms,
        publication_deadline_remaining_ms,
        staged_bytes,
        budget_anchor_sequence,
        allowed_end_ms,
        carried_surplus_ms,
        demand_observation_age_ms,
    ) = {
        let publication = s.publication.lock().await;
        let served_end_ms = publication.served.as_ref().map(|served| served.end_ms);
        let staged_bytes = produced_segments
            .iter()
            .filter(|(start_ms, _)| *start_ms >= served_end_ms.unwrap_or(0))
            .map(|(_, bytes)| *bytes)
            .sum();
        (
            served_end_ms,
            publication.served.as_ref().map(|served| served.revision),
            publication.served.as_ref().map(|served| {
                i64::try_from(
                    observed_at
                        .saturating_duration_since(served.available_at)
                        .as_millis(),
                )
                .unwrap_or(i64::MAX)
            }),
            publication.next_publish_at.map(|deadline| {
                i64::try_from(deadline.saturating_duration_since(observed_at).as_millis())
                    .unwrap_or(i64::MAX)
            }),
            publication.hard_deadline.map(|deadline| {
                i64::try_from(deadline.saturating_duration_since(observed_at).as_millis())
                    .unwrap_or(i64::MAX)
            }),
            staged_bytes,
            publication.budget_anchor_sequence,
            publication.allowed_end_ms,
            publication
                .served
                .as_ref()
                .map(|_| publication.carried_surplus_ms),
            publication.demand_observation_age_ms,
        )
    };
    let idle_seconds = lease
        .as_ref()
        .map_or(SESSION_IDLE_SECS, |lease| lease.idle_for.as_secs());
    let last_request_kind = lease
        .as_ref()
        .map_or("control-unavailable", |lease| lease.last_renewal_kind);
    let lease_mode = match lease.as_ref().map(|lease| lease.mode) {
        Some(crate::playback_control::RollingLeaseMode::Explicit) => "explicit",
        Some(crate::playback_control::RollingLeaseMode::Legacy) => "legacy",
        None => "unavailable",
    };
    let lease_state = match lease.as_ref() {
        Some(lease) => lease.terminal.map_or("active", |cause| cause.status()),
        None => "unavailable",
    };
    let delivery = lease.as_ref().map(|lease| &lease.delivery);
    let control_demand = demand.map(|demand| match demand.demand {
        crate::playback_control::PlaybackDemand::Active => "active",
        crate::playback_control::PlaybackDemand::Hold => "hold",
        crate::playback_control::PlaybackDemand::End => "end",
    });
    let render_state = demand.map(|demand| match demand.render_state {
        crate::playback_control::RenderState::Starting => "starting",
        crate::playback_control::RenderState::Rendering => "rendering",
        crate::playback_control::RenderState::Waiting => "waiting",
        crate::playback_control::RenderState::Stalled => "stalled",
        crate::playback_control::RenderState::Seeking => "seeking",
        crate::playback_control::RenderState::Ended => "ended",
        crate::playback_control::RenderState::Failed => "failed",
    });
    let suspended = s.suspended.load(Relaxed);
    let staged_publication_seconds = s
        .staged_publication_seconds(s.control.current_producer_attempt())
        .await;
    let flow = lease.as_ref().map(|lease| {
        evaluate_flow(FlowInputs {
            physical_ahead: ahead,
            published_end_ms,
            staged_publication_seconds,
            startup_protected: lease.startup.protects_from_time_hold(),
            media_origin_ms,
            lease_mode: lease.mode,
            demand: lease.demand.as_ref(),
            global_live_bytes,
            global_ahead_bytes,
            limits,
            currently_suspended: suspended,
            scratch_grant_exhausted: None,
        })
    });
    let active_hold = if suspended {
        (*s.suspended_at.lock().await).map(|held| held.hold)
    } else {
        None
    };
    let status_owner_attempt = delivery.map_or_else(
        || s.control.current_producer_attempt(),
        |delivery| delivery.producer_attempt,
    );
    let actor_available = delivery.is_some();
    let (child_is_running, fallback_exit) = {
        let child = s.child.lock().await;
        child.as_ref().map_or((false, None), |child| {
            if child.producer_attempt != status_owner_attempt {
                return (false, None);
            }
            (
                child.id().is_some(),
                if actor_available {
                    None
                } else {
                    child.terminal_exit_snapshot()
                },
            )
        })
    };
    let producer_exit = delivery
        .and_then(|delivery| delivery.producer_exit.clone())
        .or(fallback_exit);
    let recent_speed = delivery
        .and_then(|delivery| delivery.producer_recent_speed_milli)
        .map(|speed| speed as f64 / 1_000.0);
    let cumulative_speed = delivery
        .and_then(|delivery| delivery.producer_speed_milli)
        .map(|speed| speed as f64 / 1_000.0);
    let rate_estimate_source = if recent_speed.is_some() {
        "recent_progress"
    } else if cumulative_speed.is_some() {
        "cumulative_progress"
    } else {
        "unavailable"
    };
    let actor_producer = lease.as_ref().map(|lease| &lease.producer_control);
    let http_waits = s.http_waits.snapshot();
    let producer_state = if s.failed.load(Relaxed) {
        "failed"
    } else if s.cached
        || (s.actor_managed_prepublication_process
            && actor_producer.is_some_and(|producer| producer.completion != "incomplete"))
        || (!s.actor_managed_prepublication_process
            && producer_exit.as_ref().is_some_and(|exit| exit.success))
    {
        "complete"
    } else if actor_producer.is_some_and(|producer| producer.producer_ended_with_proposal) {
        "producer_ended_with_proposal"
    } else if producer_exit.is_some() {
        "exited"
    } else if suspended {
        "held"
    } else if child_is_running {
        "running"
    } else {
        "waiting"
    };
    SessionInfo {
        id: id.to_owned(),
        presentation: "live-recovery",
        file_id: s.file_id,
        item_id: s.item_id,
        item_title: s.item_title.clone(),
        user_name: s.user_name.clone(),
        target_height: s.target_height,
        encoder: *s.encoder_label.lock().await,
        tone_map_peak_nits: s.tone_map_peak_nits,
        tone_map_peak_source: s.tone_map_peak_source,
        started_unix: s.started_unix,
        idle_seconds,
        last_request: last_request_kind,
        lease_mode,
        lease_state,
        lease_timeout_ms: lease.as_ref().map(|lease| lease.timeout_ms()),
        startup_state: lease.as_ref().map(|lease| lease.startup.status()),
        startup_remaining_ms: lease.as_ref().and_then(|lease| {
            lease
                .startup
                .remaining
                .map(|remaining| i64::try_from(remaining.as_millis()).unwrap_or(i64::MAX))
        }),
        presentation_progress_seen: lease
            .as_ref()
            .map(|lease| lease.startup.presentation_progress_seen),
        produced_end_ms,
        served_end_ms,
        budget_anchor_sequence,
        allowed_end_ms,
        carried_surplus_ms,
        demand_observation_age_ms,
        staged_bytes,
        playlist_target_ms: Some(
            i64::try_from(ROLLING_PUBLICATION_TARGET.as_millis()).unwrap_or(i64::MAX),
        ),
        served_revision,
        last_segment_advanced_idle_ms,
        next_publication_in_ms,
        publication_deadline_remaining_ms,
        maintenance_state: if s.retention_cleanup_active.load(Acquire) {
            "retention_cleanup"
        } else if s.retention_garbage_bytes.load(Acquire) > 0 {
            "retention_pending"
        } else {
            "idle"
        },
        rate_estimate_source,
        estimate_active_speed: recent_speed.or(cumulative_speed),
        pause_grace_remaining_ms: lease.as_ref().and_then(|lease| {
            lease
                .pause_remaining
                .map(|remaining| i64::try_from(remaining.as_millis()).unwrap_or(i64::MAX))
        }),
        retirement_reason: lease
            .as_ref()
            .and_then(|lease| lease.terminal.map(|cause| cause.status())),
        advertised_bytes,
        grace_bytes,
        reserved_bytes: s
            .scratch
            .as_ref()
            .map(crate::scratch_ledger::ScratchPermit::admitted_bytes)
            .filter(|bytes| *bytes > 0),
        live_bytes: s.live_bytes.load(Acquire),
        control_demand,
        reported_position_ms: demand.map(|demand| demand.position_ms),
        client_runway_ms: demand.map(|demand| demand.runway_ms()),
        render_state,
        server_ready_state: server_ready.state,
        server_ready_anchor_ms: server_ready.anchor_ms,
        server_ready_end_ms: server_ready.end_ms,
        server_ready_seconds: server_ready.seconds,
        server_next_ready_start_ms: server_ready.next_start_ms,
        server_next_ready_end_ms: server_ready.next_end_ms,
        production_policy: flow.map_or("unavailable", |flow| flow.policy),
        production_ahead_seconds: flow.and_then(|flow| flow.production_ahead_seconds),
        production_target_seconds: flow.and_then(|flow| flow.production_target_seconds),
        producer_control: lease.as_ref().map(|lease| lease.producer_control.clone()),
        producer_state,
        producer_attempt: Some(status_owner_attempt),
        playlist_ready: delivery.map(|delivery| delivery.playlist_ready),
        published_segment: delivery.and_then(|delivery| delivery.published_segment),
        next_media_sequence: delivery.map(|delivery| delivery.next_media_sequence),
        pending_fetched_segment: delivery.and_then(|delivery| delivery.pending_fetched_segment),
        speed: cumulative_speed,
        recent_speed,
        out_time_ms: delivery.and_then(|delivery| delivery.producer_out_time_ms),
        progress_idle_ms: delivery.map_or(-1, |delivery| delivery.producer_progress_idle_ms),
        producer_exit_success: producer_exit.as_ref().map(|exit| exit.success),
        producer_exit_code: producer_exit.as_ref().and_then(|exit| exit.code),
        producer_exit_signal: producer_exit.as_ref().and_then(|exit| exit.signal),
        producer_exit_idle_ms: producer_exit.as_ref().map(|exit| exit.observed_idle_ms),
        published_end_ms,
        fetched_end_ms,
        fetched_segment: lease.as_ref().map_or_else(
            || Some(s.high_segment.load(Relaxed)).filter(|index| *index >= 0),
            |lease| lease.delivery.fetched_segment,
        ),
        first_retained_segment,
        playlist_shape: if s.cached { "vod" } else { "sliding" },
        ahead_seconds: ahead.map(|a| a.seconds),
        hold_reason: active_hold.map(|hold| hold.reason),
        resume_below_seconds: active_hold
            .filter(|hold| hold.reason == AheadHoldReason::Time)
            .map(|hold| hold.release_value),
        resume_below_bytes: active_hold
            .filter(|hold| {
                matches!(
                    hold.reason,
                    AheadHoldReason::Bytes | AheadHoldReason::Global
                )
            })
            .map(|hold| hold.release_value),
        ahead_bytes: ahead.map(|a| a.bytes),
        delivered_bytes: s.delivery.total_bytes(),
        delivered_bps: s.delivery.recent_bps().map(|b| b * 8),
        delivered_idle_ms: s.delivery.idle_for_ms(),
        http_wait_count: http_waits.count,
        http_wait_oldest_ms: http_waits.oldest_ms,
        http_wait_segment: http_waits.oldest_segment,
        status_generated_unix_ms: crate::media_sessions::unix_ms(),
        readrate: s.readrate,
        suspended,
        suspend_count: s.suspend_count.load(Relaxed),
    }
}

pub(super) fn vod_delivery_session_info(info: crate::vodserve::VodDeliveryInfo) -> SessionInfo {
    SessionInfo {
        id: info.id,
        presentation: "vod",
        file_id: info.file_id,
        item_id: info.item_id,
        item_title: info.item_title,
        user_name: info.user_name,
        target_height: info.target_height,
        encoder: "vod",
        tone_map_peak_nits: None,
        tone_map_peak_source: None,
        started_unix: info.started_unix,
        idle_seconds: info.idle_seconds,
        last_request: "vod",
        lease_mode: "vod",
        lease_state: "active",
        lease_timeout_ms: Some(crate::playback_control::VOD_LEASE_TIMEOUT_MS),
        startup_state: None,
        startup_remaining_ms: None,
        presentation_progress_seen: None,
        produced_end_ms: None,
        served_end_ms: None,
        budget_anchor_sequence: None,
        allowed_end_ms: None,
        carried_surplus_ms: None,
        demand_observation_age_ms: None,
        staged_bytes: 0,
        playlist_target_ms: None,
        served_revision: None,
        last_segment_advanced_idle_ms: None,
        next_publication_in_ms: None,
        publication_deadline_remaining_ms: None,
        maintenance_state: "immutable",
        rate_estimate_source: "unavailable",
        estimate_active_speed: None,
        pause_grace_remaining_ms: None,
        retirement_reason: None,
        advertised_bytes: 0,
        grace_bytes: 0,
        reserved_bytes: None,
        live_bytes: 0,
        control_demand: None,
        reported_position_ms: None,
        client_runway_ms: None,
        render_state: None,
        server_ready_state: "unavailable",
        server_ready_anchor_ms: None,
        server_ready_end_ms: None,
        server_ready_seconds: None,
        server_next_ready_start_ms: None,
        server_next_ready_end_ms: None,
        production_policy: "immutable_vod",
        production_ahead_seconds: None,
        production_target_seconds: None,
        producer_control: None,
        producer_state: "vod",
        producer_attempt: None,
        playlist_ready: None,
        published_segment: None,
        next_media_sequence: None,
        pending_fetched_segment: None,
        speed: None,
        recent_speed: None,
        out_time_ms: None,
        progress_idle_ms: 0,
        producer_exit_success: None,
        producer_exit_code: None,
        producer_exit_signal: None,
        producer_exit_idle_ms: None,
        published_end_ms: None,
        fetched_end_ms: 0,
        fetched_segment: None,
        first_retained_segment: None,
        playlist_shape: "vod",
        ahead_seconds: None,
        hold_reason: None,
        resume_below_seconds: None,
        resume_below_bytes: None,
        ahead_bytes: None,
        // Measured, not assumed. The zero and the session-touch age this
        // replaces looked like a reading and were not one: a handle touched a
        // second ago reported a fresh delivery whether or not a byte had moved.
        delivered_bytes: info.delivered_bytes,
        delivered_bps: info.delivered_bps,
        delivered_idle_ms: info.delivered_idle_ms,
        http_wait_count: 0,
        http_wait_oldest_ms: None,
        http_wait_segment: None,
        status_generated_unix_ms: crate::media_sessions::unix_ms(),
        readrate: 0.0,
        suspended: false,
        suspend_count: 0,
    }
}
