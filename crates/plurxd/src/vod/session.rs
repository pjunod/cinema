use super::*;

/// The rendition's init identity and where it came from — `from_disk` marks a
/// resurrection adoption, which is the only case §5's mismatch arm may purge
/// and re-establish rather than fail typed.
#[derive(Debug, Default)]
pub(super) struct IdentityState {
    pub(super) identity: Option<InitIdentity>,
    pub(super) from_disk: bool,
}

/// One live rendition: a directory, a manifest, a producer, and its readers.
pub(super) struct Rendition {
    pub(super) key: String,
    pub(super) dir: RenditionDir,
    pub(super) recipe: Recipe,
    /// One opened source object for the rendition's whole life. Every ffmpeg
    /// generation inherits this descriptor and every materialization/serve
    /// checks its object version, so pathname replacement or in-place mutation
    /// cannot mix two source revisions under one immutable playlist.
    pub(super) source: Option<crate::fragment_index_cluster::SourceFence>,
    /// The stored plan — normative, immutable, the source of everything below.
    pub(super) plan: SegmentPlan,
    /// Rendered ONCE from the plan at attach; identical for the rendition's
    /// whole life (plan §2.1).
    pub(super) playlist: Vec<u8>,
    pub(super) timescale: u32,
    pub(super) seconds_per_segment: f64,
    pub(super) index: Option<FragmentIndex>,
    pub(super) policy: CutPolicy,
    pub(super) working_set_budget: u64,
    pub(super) completed_cache_budget: u64,
    pub(super) materialize_budget: Duration,
    pub(super) manifest: Mutex<Manifest>,
    pub(super) identity: Mutex<IdentityState>,
    pub(super) slot: ProducerSlot,
    pub(super) readers: Mutex<HashMap<String, Reader>>,
    /// Monotonic identity of each successful segment publication. The ledger
    /// stores this beside an entry index so eviction followed by ordinary
    /// rematerialization cannot inherit stale prewarm credit.
    pub(super) publication_serial: AtomicU64,
    pub(super) publication_versions: StdMutex<Vec<Option<u64>>>,
    /// Attribution for work already dispatched to one producer generation.
    /// Control may disable future speculation while that generation has the
    /// landing fragment in flight; the dispatch survives just long enough to
    /// credit that publication to the playback that requested it.
    pub(super) marker_prewarm_dispatch: StdMutex<Option<MarkerPrewarmDispatch>>,
    /// Keeps the ordinary producer publication path O(1) when no playback is
    /// currently attributing work to marker prewarm.
    pub(super) active_marker_prewarms: AtomicU32,
    /// The generation that was started or repurposed for speculative marker
    /// work, encoded as epoch + 1 so zero means absent. Attribution may end as
    /// soon as the requested window publishes, but the producer can still
    /// have output queued; capacity work must fence that generation until it
    /// physically retires.
    pub(super) marker_prewarm_generation: AtomicU64,
    /// A recorded producer failure: subsequent planned-segment GETs answer
    /// `ProducerFailed` until a new create replaces the rendition.
    ///
    /// The class travels with the prose. The prose is a sentence for an
    /// operator reading a log; the class is the only part a client can act on,
    /// because `producer_state` flattens every failure to the word `failed`
    /// and a client that cannot tell "try again" from "this will never work"
    /// guesses toward retry — reopening, getting the same verdict, reopening
    /// again.
    pub(super) failed: StdMutex<Option<RenditionFailure>>,
    /// The capacity hold this rendition is under, if any, retained across the
    /// producer that was terminated for it.
    ///
    /// A hold that cannot clear on its own terminates the producer — a stopped
    /// one goes on holding everything a running one held — which leaves the
    /// belief `Absent` and takes the reason with it. So the status said
    /// `waiting` with no hold: the viewer's control plane saw a producer that
    /// had stopped and never saw *why*, which for a capacity stall is the only
    /// fact that explains why nothing is arriving. Recorded from each pass's
    /// own decision and cleared by the first pass that decides anything else,
    /// so it cannot outlive the condition.
    pub(super) capacity_hold: StdMutex<Option<crate::prodsched::Hold>>,
    /// Survives a yielded process so it cannot re-admit one segment after
    /// giving its permit to a live waiter.
    pub(super) ahead_hold: AtomicBool,
    /// Woken when `init.mp4` lands, for GETs waiting on the identity.
    pub(super) init_notify: Notify,
    /// The driver's kick: wait registration, segment GETs, attach/detach,
    /// maintain ticks.
    pub(super) wake: Notify,
    /// Deterministic observation point for tests that must prove the driver's
    /// stopped-encoder poll, rather than a direct `driver_pass`, caused work.
    #[cfg(test)]
    pub(super) stopped_poll_armed: Notify,
    #[cfg(test)]
    pub(super) stopped_poll_fired: Notify,
    /// Bumped whenever the driver kills or replaces the producer, so a
    /// generation that ends can tell "I died on my own" from "I was told to".
    pub(super) gen_epoch: AtomicU64,
    /// The most recent generation child's pid, for diagnostics and tests.
    pub(super) last_child_pid: AtomicU32,
    pub(super) dormant_since: StdMutex<Option<Instant>>,
    /// Set when the rendition is purged or replaced; the driver exits and the
    /// sink refuses further writes as the quiet `NotFound` teardown.
    pub(super) closed: AtomicBool,
    /// One admission-refusal line per fill, not one per materialize.
    pub(super) warned_admission: AtomicBool,
    /// One "waiting for an encoder permit" line per wait episode, not one per
    /// driver pass. Cleared when a permit is granted or demand goes away.
    pub(super) permit_wait_logged: AtomicBool,
    /// While live, this rendition's producer must give its encoder permit to
    /// the prepared successor named here (and not take one back).
    pub(super) handoff: StdMutex<Option<HandoffRequest>>,
    /// One expiry wake-up task per parked episode.
    pub(super) handoff_expiry_armed: AtomicBool,
    /// First blocked demand per plan entry, retained across HTTP 503 retries.
    pub(super) demand_since: StdMutex<HashMap<u32, MaterializeClock>>,
}

impl Rendition {
    pub(super) fn kick(&self) {
        self.wake.notify_one();
    }

    /// Ask (or keep asking) this producer to hand its permit to `successor`.
    /// `true` when this starts a new handoff rather than refreshing one.
    pub(super) fn request_handoff(
        &self,
        incarnation: &str,
        successor: &Arc<Rendition>,
        wanted: crate::admission::TranscodeResourceEstimate,
    ) -> bool {
        let now = Instant::now();
        let mut slot = self.handoff.lock().expect("handoff lock");
        let fresh = !slot
            .as_ref()
            .is_some_and(|request| request.until > now && request.incarnation == incarnation);
        *slot = Some(HandoffRequest {
            until: now + HANDOFF_REQUEST_TTL,
            incarnation: incarnation.to_owned(),
            successor: Arc::downgrade(successor),
            wanted,
        });
        fresh
    }

    pub(super) fn live_handoff(&self) -> Option<HandoffRequest> {
        self.handoff
            .lock()
            .expect("handoff lock")
            .as_ref()
            .filter(|request| request.until > Instant::now())
            .cloned()
    }

    #[cfg(test)]
    pub(super) fn handoff_requested(&self) -> bool {
        self.live_handoff().is_some()
    }

    /// Drop a handoff request unless it names `live_incarnation`, and wake
    /// the producer so a parked viewer is served at once rather than at the
    /// request's expiry or its next GET.
    pub(super) fn release_handoff_unless(&self, live_incarnation: Option<&str>) {
        let released = {
            let mut slot = self.handoff.lock().expect("handoff lock");
            match slot.as_ref() {
                Some(request) if Some(request.incarnation.as_str()) != live_incarnation => {
                    *slot = None;
                    true
                }
                _ => false,
            }
        };
        if released {
            self.kick();
        }
    }

    pub(super) fn failure(&self) -> Option<RenditionFailure> {
        self.failed.lock().expect("failed lock").clone()
    }

    /// Just the operator-facing sentence, for the paths that answer a typed
    /// HTTP refusal whose body is prose.
    pub(super) fn failure_cause(&self) -> Option<String> {
        self.failure().map(|failure| failure.cause)
    }

    pub(super) fn identity_path(&self) -> PathBuf {
        self.dir.path().join(IDENTITY_NAME)
    }

    pub(super) fn clear_demand(&self, index: u32) {
        self.demand_since
            .lock()
            .expect("demand lock")
            .remove(&index);
    }

    #[cfg(test)]
    pub(super) async fn attach_reader(&self, session_id: &str, frontier: u32) {
        self.readers
            .lock()
            .await
            .insert(session_id.to_string(), Reader::new(frontier));
        *self.dormant_since.lock().expect("dormant lock") = None;
    }

    pub(super) async fn detach_reader(&self, pool: &crate::waitpool::WaitPool, session_id: &str) {
        {
            let mut readers = self.readers.lock().await;
            readers.remove(session_id);
            if readers.is_empty() {
                *self.dormant_since.lock().expect("dormant lock") = Some(Instant::now());
            }
        }
        // Retire this session's parked GETs with its reader, not after them.
        // `playback_demands` ranks a blocked request against the reader that
        // asked for it, and with no reader to rank against it falls back to
        // marking the session's oldest wait foreground — so a departed
        // viewer's abandoned request outranks a present viewer's and aims the
        // producer at media nobody is watching until its deadline expires.
        pool.retire_session(&self.key, session_id);
        // Every VOD session *ending* converges here — terminal and idle reap
        // alike — so this is the one place a departing viewer's subtitle
        // window is released. Reattachment is deliberately not one of them:
        // it is not an ending, the id it reuses belongs to a viewer who is
        // still watching, and releasing fences that id for half a minute. It
        // stops its own obsolete flight by name instead, without the fence,
        // which the comment here used to claim it did through this function.
        // The readers guard is deliberately
        // dropped first: releasing waits for a real ffmpeg to settle, and
        // holding a rendition-wide lifecycle lock across that await would let
        // one leaving viewer stall every other reader of the same rendition.
        crate::subtitles::release_session_window(session_id).await;
    }

    /// Eviction windows for every attached reader (plan §2.4's reader guard).
    pub(super) async fn reader_windows(&self) -> Vec<ReaderWindow> {
        let readers = self.readers.lock().await;
        readers
            .values()
            .map(|reader| reader_window(reader, self.seconds_per_segment))
            .collect()
    }
}

pub(super) struct TerminalCleanup {
    finished: AtomicBool,
    notify: Notify,
    completed_at: std::sync::OnceLock<tokio::time::Instant>,
    #[cfg(test)]
    pub(super) wait_enabled_pause: StdMutex<Option<Arc<tokio::sync::Barrier>>>,
}

impl TerminalCleanup {
    pub(super) fn new() -> Self {
        Self {
            finished: AtomicBool::new(false),
            notify: Notify::new(),
            completed_at: std::sync::OnceLock::new(),
            #[cfg(test)]
            wait_enabled_pause: StdMutex::new(None),
        }
    }

    pub(super) fn complete(&self) {
        // The late-response window starts only after reader detach, lifecycle
        // emission, and any terminal commit settlement owned by the cleanup
        // task have actually finished. Starting it at tombstone creation made
        // a slow cleanup immediately evictable.
        let _ = self.completed_at.set(tokio::time::Instant::now());
        self.finished.store(true, Release);
        self.notify.notify_waiters();
    }

    pub(super) fn is_finished(&self) -> bool {
        self.finished.load(Acquire)
    }

    pub(super) fn retention_expired(&self) -> bool {
        self.completed_at.get().is_some_and(|completed_at| {
            tokio::time::Instant::now() >= *completed_at + TERMINAL_TOMBSTONE_RETENTION
        })
    }

    pub(super) async fn wait(&self) {
        while !self.is_finished() {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // `notify_waiters` stores no permit for a future that has only
            // been constructed. Register it before the second state read so
            // completion can occur on either side of that read without being
            // lost.
            notified.as_mut().enable();
            #[cfg(test)]
            {
                let pause = self
                    .wait_enabled_pause
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if let Some(pause) = pause {
                    pause.wait().await;
                    pause.wait().await;
                }
            }
            if self.is_finished() {
                return;
            }
            notified.await;
        }
    }
}

pub(super) struct TerminalCleanupGuard(pub(super) Arc<TerminalCleanup>);

impl Drop for TerminalCleanupGuard {
    fn drop(&mut self) {
        self.0.complete();
    }
}

/// One detached ffmpeg whose Drop path still owns SIGKILL plus a confirmed
/// wait. The reaper task is intentionally independent of the request future:
/// dropping a resurrection/build cannot drop the only process owner.
pub(super) struct HeadChildOwner {
    child: Option<tokio::process::Child>,
    child_job: Option<crate::process_control::ChildJob>,
    pub(super) permit: Option<crate::vodencode::EncodePermit>,
    #[cfg(test)]
    reap_pause: Option<Arc<tokio::sync::Barrier>>,
}

impl HeadChildOwner {
    pub(super) fn new(child: tokio::process::Child) -> Self {
        Self {
            child: Some(child),
            child_job: None,
            permit: None,
            #[cfg(test)]
            reap_pause: None,
        }
    }

    pub(super) fn new_job_owned(
        child: tokio::process::Child,
        child_job: crate::process_control::ChildJob,
    ) -> Self {
        Self {
            child: Some(child),
            child_job: Some(child_job),
            permit: None,
            #[cfg(test)]
            reap_pause: None,
        }
    }

    #[cfg(test)]
    pub(super) fn with_reap_pause(
        child: tokio::process::Child,
        reap_pause: Arc<tokio::sync::Barrier>,
    ) -> Self {
        Self {
            child: Some(child),
            child_job: None,
            permit: None,
            reap_pause: Some(reap_pause),
        }
    }

    fn begin_reap(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let mut child = self.child.take()?;
        let child_job = self.child_job.take();
        let permit = self.permit.take();
        let _ = child.start_kill();
        #[cfg(test)]
        let reap_pause = self.reap_pause.take();
        Some(tokio::spawn(async move {
            let _child_job = child_job;
            let _permit = permit;
            #[cfg(test)]
            if let Some(pause) = reap_pause {
                pause.wait().await;
                pause.wait().await;
            }
            let _ = child.wait().await;
        }))
    }

    pub(super) async fn terminate_and_reap(&mut self) {
        if let Some(reaper) = self.begin_reap() {
            // Cancelling this await only detaches the already-owned reaper.
            let _ = reaper.await;
        }
    }
}

impl From<tokio::process::Child> for HeadChildOwner {
    fn from(child: tokio::process::Child) -> Self {
        Self::new(child)
    }
}

impl Drop for HeadChildOwner {
    fn drop(&mut self) {
        let _ = self.begin_reap();
    }
}

#[derive(Debug)]
pub(super) enum HeadRegenerationError {
    Busy,
    Oversize,
    Failed(String),
}

impl std::fmt::Display for HeadRegenerationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("head-regeneration capacity is full"),
            Self::Oversize => write!(
                formatter,
                "head regeneration exceeded the {} byte init limit",
                HEAD_REGENERATION_MAX_BYTES
            ),
            Self::Failed(reason) => formatter.write_str(reason),
        }
    }
}

pub(super) fn deferred_terminal_commit(
    committer: Arc<dyn crate::playback_control::TerminalControlCommitter>,
    result: &mut crate::playback_control::LocalControlResult,
) -> crate::playback_control::TerminalCommitReceipt {
    // Install immutable prepared data before the outer receipt is attached.
    // Keeping that receipt out of the closure's captured result avoids the
    // outer -> closure -> prepared result -> outer Arc cycle.
    let prepared = Arc::new(StdMutex::new(Some(result.clone())));
    let inner = Arc::new(StdMutex::new(
        None::<crate::playback_control::TerminalCommitReceipt>,
    ));
    let receipt = crate::playback_control::TerminalCommitReceipt::deferred_retryable({
        let prepared = Arc::clone(&prepared);
        let inner = Arc::clone(&inner);
        move |attempt| {
            let (receipt, retry) = {
                let mut inner = inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(receipt) = inner.as_ref() {
                    (receipt.clone(), true)
                } else {
                    let receipt = {
                        let prepared = prepared
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        committer.start(
                            prepared
                                .as_ref()
                                .expect("deferred VOD terminal result must be installed"),
                        )
                    };
                    *inner = Some(receipt.clone());
                    (receipt, false)
                }
            };
            if let Some(expires_at_unix_ms) = receipt.expires_at_unix_ms() {
                attempt.set_expires_at_unix_ms(expires_at_unix_ms);
            }
            if retry {
                receipt.retry();
            }
            tokio::spawn(async move {
                attempt.complete(receipt.wait().await);
            });
        }
    });
    result.terminal_commit = Some(receipt.clone());
    receipt
}

pub(super) fn terminal_reason(cause: Terminal) -> &'static str {
    match cause {
        Terminal::Deleted => "client_released",
        Terminal::Superseded => "superseded",
        Terminal::AdminStop => "killed",
        Terminal::Revoked => "revoked",
        Terminal::Replaced => "file_replaced",
    }
}

/// One session handle (plan §2.5): auth attribution, sliding TTL, reader
/// window, and — once it ends for good — a tombstone.
pub(super) struct Session {
    /// Cleared synchronously by the detached terminal cleanup owner before it
    /// publishes completion. The compact fields below retain exact 410/replay
    /// identity without retaining manifests, source handles, readers, or the
    /// producer graph until the next maintenance tick.
    pub(super) rendition: Option<Arc<Rendition>>,
    pub(super) rendition_key: String,
    pub(super) file: Arc<MediaFile>,
    pub(super) playback_id: String,
    pub(super) user_name: String,
    pub(super) item_title: String,
    pub(super) started_unix: i64,
    /// Echoed on an idempotent create replay (`request_id` recovery).
    pub(super) target_height: i64,
    /// Echoed on an idempotent create replay.
    pub(super) kind: SessionKind,
    /// The same user scope the legacy supersession sweep filters by, so one
    /// viewer's `playback_id` can never end another viewer's session.
    pub(super) supersession_user: String,
    pub(super) block_budget: Duration,
    /// Serializes authority-checked control with terminal/reap transitions for
    /// this session only. Keeping the gate on the session avoids making an
    /// unrelated rolling or VOD control wait behind node-wide Store I/O.
    pub(super) lifecycle: Arc<Mutex<()>>,
    /// Unique identity of this attachment while `lifecycle` is deliberately
    /// stable across same-id idle reap and resurrection.
    pub(super) incarnation: Arc<()>,
    pub(super) last_touch: StdMutex<Instant>,
    /// Bytes this viewer has actually been handed, over monotonic time.
    ///
    /// Per session rather than per rendition: two viewers of the same
    /// immutable rendition have two different links, and a rate that averaged
    /// them would describe neither. This is the VOD half of what
    /// `transcode::Session::delivery` already is for rolling delivery. The
    /// measurement remains useful for fleet advice and incident diagnosis,
    /// while missing or low values no longer veto prepared handoff.
    ///
    /// `Arc` because the response body outlives the registry lock: bytes are
    /// counted on the pump task, long after `segment` has returned and the
    /// `sessions` map has been unlocked.
    pub(super) delivery: Arc<crate::meter::Meter>,
    /// Owner-local sequence fence kept separate from media-object touches.
    pub(super) control: StdMutex<crate::playback_control::ControlState>,
    /// Stored, source-fenced marker boundaries projected onto this rendition's
    /// immutable plan. No request-path probing or detector runs here.
    pub(super) marker_destinations: Vec<MarkerDestination>,
    /// Latest accepted snapshot, used to correlate a session-less shipped
    /// marker beacon and to settle a seek whose transient state was coalesced.
    /// Replays never replace it or emit a second outcome.
    pub(super) last_control_snapshot: Option<crate::playback_control::PlaybackDemandSnapshot>,
    /// Exact terminal acknowledgement retained after a client `demand=end`
    /// tombstones the attachment. Other lifecycle causes never populate it.
    pub(super) control_end: Option<crate::playback_control::LocalControlResult>,
    /// Complete accepted End payload. Reusing its sequence with different
    /// observations is a conflict, not an idempotent terminal replay.
    pub(super) control_end_snapshot: Option<crate::playback_control::PlaybackDemandSnapshot>,
    /// Session-owned, idempotent reader detach. It continues if the request
    /// that committed the tombstone is cancelled and is joined by every later
    /// replay/end/maintenance path.
    pub(super) terminal_cleanup: Option<Arc<TerminalCleanup>>,
    pub(super) tombstone: Option<Terminal>,
    /// The durable incarnation this session was primed as, when it is a
    /// prepared successor. A predecessor hands its encoder permit only to the
    /// session whose incarnation its own preparation slot names.
    pub(super) prepared_incarnation: Option<String>,
}

impl Session {
    /// Release this session's rendition from any handoff its preparation no
    /// longer backs (abort, rejection, settlement, tombstone, or a newer
    /// successor staged in its place).
    fn release_stale_handoff(&self) {
        let live = self
            .control
            .lock()
            .expect("control lock")
            .live_preparation_incarnation()
            .map(str::to_owned);
        if let Some(rendition) = self.rendition.as_ref() {
            rendition.release_handoff_unless(live.as_deref());
        }
    }

    /// Move any staged M6 successor to aborting, because this session is over.
    ///
    /// Called wherever a tombstone is written, under the same registry lock
    /// that writes it, so the slot and the session's liveness can never
    /// disagree. That is the rolling actor's shape — its `terminate` aborts
    /// the slot it is holding — and it is what lets
    /// `VodPreparationGate::may_commit_preparation` trust the slot instead of
    /// layering a second refusal on top of an untouched one.
    ///
    /// The durable row is not torn down here and is not this path's to reap:
    /// the executor's commit finds `may_commit` false and stops, and the
    /// preparation's own `deadline_ms` is the backstop for an owner that died
    /// holding a successor. What this does buy is that nothing afterwards
    /// believes the successor is still wanted.
    pub(super) fn abort_staged_preparation(&self) {
        {
            let mut control = self.control.lock().expect("control lock");
            if let Some(staged) = control.staged_incarnation_id().map(str::to_owned) {
                control.abort_preparation(&staged);
            }
        }
        self.release_stale_handoff();
    }

    pub(super) fn live_rendition(&self) -> Option<&Arc<Rendition>> {
        self.rendition.as_ref().filter(|_| self.tombstone.is_none())
    }

    pub(super) fn response_owner(&self) -> ResponseOwner {
        ResponseOwner {
            lifecycle: Arc::clone(&self.lifecycle),
            incarnation: Arc::clone(&self.incarnation),
            rendition: self.rendition.as_ref().map(Arc::clone),
            rendition_key: self.rendition_key.clone(),
            file: Arc::clone(&self.file),
            tombstone: self.tombstone,
        }
    }

    /// A terminal control response remains part of the exact-owner replay
    /// contract until its commit receipt expires, even if the independent
    /// cleanup-retention window has already elapsed.
    pub(super) fn terminal_replay_expired(&self) -> bool {
        self.control_end
            .as_ref()
            .and_then(|result| result.terminal_commit.as_ref())
            .is_none_or(crate::playback_control::TerminalCommitReceipt::is_expired)
    }
}

/// This engine's half of [`crate::playback_control::PreparationGate`].
///
/// M6's preparation slot lives on `ControlState`, which both delivery engines
/// hold, precisely so it exists here — `into_request` sets `Presentation::Vod`
/// for every create, so this engine serves the sessions a staged successor is
/// actually for. What this type adds is the liveness half the slot cannot
/// answer for itself: a session that has vanished from the registry or carries
/// a tombstone takes no successor.
///
/// Holds `Arc<Shared>` and an id rather than a reference into the registry,
/// the same shape as [`VodPreparationGuard`], because `Session` lives inside
/// the `sessions` map by value and cannot be borrowed across an await. Each
/// call therefore takes the registry lock, holds it for one slot transition,
/// and releases it — never across durable I/O, which is what the executor's
/// three-phase order exists to keep true.
///
/// **The id alone is not the session.** A gate outlives a stage by however
/// long the successor takes to warm up, and in that window an idle reap can
/// remove this session — deliberately without a tombstone — and a reconnect
/// resurrect the same durable id with a fresh `ControlState`. The lifecycle
/// gate is stable across exactly that, on purpose, so it identifies nothing
/// here. So this pins `incarnation`, like every other operation in this file
/// that acts on a session it looked up earlier: without it a stale gate takes
/// the *replacement's* slot for a successor whose predecessor no longer holds
/// the pointer, leaving the store's CAS as the only thing between that and a
/// wrong commit — the second authority over one playback the executor exists
/// to prevent.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct VodPreparationGate {
    pub(super) shared: Arc<Shared>,
    pub(super) session_id: String,
    /// Weak so a held gate cannot keep a dead attachment's identity alive; a
    /// failed upgrade is simply a session that is gone.
    pub(super) incarnation: Weak<()>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl VodPreparationGate {
    /// The live session this gate was made for, or `None` if it has been
    /// removed or replaced by a later incarnation.
    fn bound<'a>(&self, sessions: &'a mut HashMap<String, Session>) -> Option<&'a mut Session> {
        let mine = self.incarnation.upgrade()?;
        let session = sessions.get_mut(&self.session_id)?;
        Arc::ptr_eq(&session.incarnation, &mine).then_some(session)
    }
}

impl crate::playback_control::PreparationGate for VodPreparationGate {
    fn stage_preparation_for_owner<'a>(
        &'a self,
        staged_incarnation_id: String,
        predecessor_incarnation_id: String,
        deadline_ms: i64,
        expected_owner_epoch: i64,
        desired_digest: Option<String>,
    ) -> crate::playback_control::GateAnswer<'a> {
        Box::pin(async move {
            let mut sessions = self.shared.sessions.lock().await;
            let Some(session) = self.bound(&mut sessions) else {
                return false;
            };
            // The liveness half, and the only place this engine asks it: a session
            // that has ended takes no successor. Past this point the slot answers
            // for itself, because `abort_staged_preparation` has already moved it
            // wherever a tombstone put it.
            if session.tombstone.is_some() {
                return false;
            }
            let staged = session
                .control
                .lock()
                .expect("control lock")
                .stage_preparation_for_owner(
                    staged_incarnation_id,
                    predecessor_incarnation_id,
                    deadline_ms,
                    expected_owner_epoch,
                    desired_digest,
                );
            if staged {
                // A successor primed before its slot was staged may already
                // have been refused with nothing to ask; let it ask now.
                let live = session
                    .control
                    .lock()
                    .expect("control lock")
                    .live_preparation_incarnation()
                    .map(str::to_owned);
                for successor in sessions.values() {
                    if successor.prepared_incarnation.is_some()
                        && successor.prepared_incarnation == live
                    {
                        if let Some(rendition) = successor.live_rendition() {
                            rendition.kick();
                        }
                    }
                }
            }
            staged
        })
    }

    fn may_commit_preparation_for_owner<'a>(
        &'a self,
        staged_incarnation_id: &'a str,
        expected_owner_epoch: i64,
    ) -> crate::playback_control::GateAnswer<'a> {
        Box::pin(async move {
            let mut sessions = self.shared.sessions.lock().await;
            let Some(session) = self.bound(&mut sessions) else {
                return false;
            };
            // Deliberately **not** a second tombstone check. The rolling actor
            // enforces liveness here by mutating the slot at termination and then
            // letting the slot answer, and layering a refusal on top of an
            // untouched slot would leave the two disagreeing indefinitely — the
            // gate saying no while `ControlState` still said the successor was
            // committable. This engine terminalizes the same way: every path that
            // writes a tombstone calls `abort_staged_preparation` under the same
            // registry lock.
            let may = session
                .control
                .lock()
                .expect("control lock")
                .may_commit_preparation_for_owner(staged_incarnation_id, expected_owner_epoch);
            may
        })
    }

    fn begin_abort_preparation_for_owner<'a>(
        &'a self,
        staged_incarnation_id: &'a str,
        expected_owner_epoch: i64,
    ) -> crate::playback_control::GateAnswer<'a> {
        Box::pin(async move {
            let mut sessions = self.shared.sessions.lock().await;
            let Some(session) = self.bound(&mut sessions) else {
                return false;
            };
            let reserved = session
                .control
                .lock()
                .expect("control lock")
                .begin_abort_preparation_for_owner(staged_incarnation_id, expected_owner_epoch);
            session.release_stale_handoff();
            reserved
        })
    }

    fn reject_preparation_commit_for_owner<'a>(
        &'a self,
        staged_incarnation_id: &'a str,
        expected_owner_epoch: i64,
    ) -> crate::playback_control::GateAnswer<'a> {
        Box::pin(async move {
            let mut sessions = self.shared.sessions.lock().await;
            let Some(session) = self.bound(&mut sessions) else {
                return false;
            };
            let rejected = session
                .control
                .lock()
                .expect("control lock")
                .reject_preparation_commit_for_owner(staged_incarnation_id, expected_owner_epoch);
            session.release_stale_handoff();
            rejected
        })
    }

    fn settle_preparation_for_owner<'a>(
        &'a self,
        staged_incarnation_id: &'a str,
        committed: bool,
        expected_owner_epoch: i64,
    ) -> crate::playback_control::GateAnswer<'a> {
        Box::pin(async move {
            let mut sessions = self.shared.sessions.lock().await;
            let Some(session) = self.bound(&mut sessions) else {
                // A settle nobody can hear is not a failure. The session is gone,
                // so its slot is gone with it, and the durable outcome the caller
                // is reporting has already been written either way.
                return false;
            };
            let mut control = session.control.lock().expect("control lock");
            if !committed {
                control
                    .begin_abort_preparation_for_owner(staged_incarnation_id, expected_owner_epoch);
            }
            let settled =
                control.settle_preparation_for_owner(staged_incarnation_id, expected_owner_epoch);
            drop(control);
            session.release_stale_handoff();
            settled
        })
    }
}

/// A prepared rendition plus the exact per-key build gate. The gate remains
/// held through the final reader/session registry transaction, so a dormant
/// purge cannot remove the handle between lookup and attachment. Slow build
/// ownership is cancellation-independent; if its requester disappears, the
/// detached owner finishes any child reap and drops this unused guard only at
/// the transaction boundary.
pub(super) struct RenditionAttachment {
    pub(super) rendition: Arc<Rendition>,
    pub(super) _build_guard: tokio::sync::OwnedMutexGuard<()>,
}

pub(super) struct TerminalEvictionCandidate {
    pub(super) session_id: String,
    pub(super) lifecycle: Arc<Mutex<()>>,
    pub(super) incarnation: Arc<()>,
    pub(super) cleanup: Arc<TerminalCleanup>,
}

/// Exact, cancellation-safe marker for a VOD capability whose rendition is
/// prepared outside the short final attachment transaction. Lease loss must
/// be able to terminalize this stable capability even before it appears in
/// `sessions`; otherwise a request admitted just before the loss can attach
/// after the owner has self-fenced.
pub(crate) struct VodPreparationGuard {
    pub(super) shared: Arc<Shared>,
    pub(super) session_id: String,
}

impl Drop for VodPreparationGuard {
    fn drop(&mut self) {
        let mut preparing = self
            .shared
            .preparing_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(holders) = preparing.get_mut(&self.session_id) {
            *holders = holders.saturating_sub(1);
            if *holders == 0 {
                preparing.remove(&self.session_id);
            }
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(super) enum TerminalRouteTestOutcome {
    Success,
    Timeout,
    Error,
}

pub(super) fn spawn_cancellation_independent<T, F>(future: F) -> tokio::sync::oneshot::Receiver<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result = future.await;
        let _ = result_tx.send(result);
    });
    result_rx
}
