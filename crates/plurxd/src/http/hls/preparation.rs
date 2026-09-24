use super::*;

/// Detached preparation candidates in flight, bounding production fan-out.
///
/// The gate is *this selection differs from the last accepted one*, so a
/// client alternating between two selections trips it on every exchange —
/// four a second, against the server's own 250 ms floor. That is adversarial
/// rather than likely, but a detached task per exchange per client with a
/// store read inside it is not a shape to leave unbounded. Over the cap the
/// candidate fails safe and no successor is staged; the accepted exchange is
/// already complete and remains valid.
static PREPARATION_CANDIDATES_IN_FLIGHT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
/// Enough for every node in the fleet to evaluate several clients at once,
/// and far below the point where detached Store work becomes competing load.
const MAX_PREPARATION_CANDIDATES: usize = 32;

/// One process-local owner for every reserved but uncommitted successor.
///
/// The durable row remains the authority. This registry supplies the missing
/// cancellation edge: settings disable, incumbent wait, the fixed deadline,
/// and a planned-outage cancellation all name the same exact reservation and
/// only one of them can take cleanup ownership.
#[derive(Clone)]
pub(super) struct ActivePreparedSuccessor {
    state: AppState,
    executor: crate::playback_control::PreparationExecutor,
    pub(super) preparation: plurx_core::domain::MediaSessionPreparation,
    pub(super) purpose: PreparationPurpose,
    cancelled: tokio_util::sync::CancellationToken,
}

fn active_prepared_successors(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, ActivePreparedSuccessor>> {
    static ACTIVE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, ActivePreparedSuccessor>>,
    > = std::sync::OnceLock::new();
    ACTIVE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn prepared_handoff_transition() -> Arc<tokio::sync::RwLock<()>> {
    static TRANSITION: std::sync::OnceLock<Arc<tokio::sync::RwLock<()>>> =
        std::sync::OnceLock::new();
    Arc::clone(TRANSITION.get_or_init(|| Arc::new(tokio::sync::RwLock::new(()))))
}

pub(in crate::http) async fn prepared_handoff_write_guard() -> tokio::sync::OwnedRwLockWriteGuard<()>
{
    prepared_handoff_transition().write_owned().await
}

/// Every playback whose ask is currently being turned into a successor, from
/// the moment the exchange spawns the candidate until that task exits by any
/// path — refused, unplannable, disabled, cancelled, or registered as an
/// `ActivePreparedSuccessor`, at which point the registry above takes over.
///
/// This exists because the work between the dispatch exchange and registration
/// is otherwise invisible. `process_preparation_candidate` reads a setting, a
/// source row, and runs `plan_preparation_candidate` — several Store awaits —
/// before anything is registered. An exchange landing inside that window has
/// neither a new dispatch nor a registry entry, and a server that answered
/// `none` there would be telling a client that is doing exactly the right
/// thing to stop waiting and reopen, which is the reopen this whole milestone
/// exists to retire.
///
/// Keyed by playback id, because that is the identity that survives the
/// session change a directed quality ask produces. The value carries the
/// desired digest the candidate is for, so a superseding ask is visible as
/// "pending, but for a different ask"; a `claim` that only this task's guard
/// matches, so a guard dropping late cannot evict a *newer* candidate's entry;
/// and a cancelled flag, because removal is the wrong signal — the task must
/// still run its own teardown, and the entry must keep naming the playback
/// until it does.
struct PendingPreparationCandidate {
    claim: u64,
    desired_digest: String,
    cancelled: bool,
}

fn pending_preparation_candidates(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, PendingPreparationCandidate>> {
    static PENDING: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, PendingPreparationCandidate>>,
    > = std::sync::OnceLock::new();
    PENDING.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

static NEXT_PENDING_CANDIDATE_CLAIM: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// RAII: a destructor rather than a call at each `return`, because
/// `process_preparation_candidate` already has nine early exits and will grow
/// more. A guard also covers the two returns no `return` statement can —
/// a panic inside the detached task, and a runtime shutdown that drops it.
pub(super) struct PendingCandidateGuard {
    playback_id: String,
    claim: u64,
}

impl PendingCandidateGuard {
    /// Claim the slot for `playback_id`. A newer ask deliberately overwrites an
    /// older one: there is one preparation slot per playback, so the older
    /// candidate is already doomed, and leaving its digest installed would make
    /// the emit rule answer `staging` about work nobody asked for any more.
    pub(super) fn begin(playback_id: &str, desired_digest: &str) -> Self {
        let claim = NEXT_PENDING_CANDIDATE_CLAIM.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        pending_preparation_candidates()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                playback_id.to_owned(),
                PendingPreparationCandidate {
                    claim,
                    desired_digest: desired_digest.to_owned(),
                    cancelled: false,
                },
            );
        Self {
            playback_id: playback_id.to_owned(),
            claim,
        }
    }

    /// Whether this candidate has been superseded since it was spawned.
    ///
    /// A guard whose entry is no longer its own reads as cancelled too: the
    /// slot belongs to a newer ask, so continuing to build this one would
    /// spend an admission slot on a selection the viewer has already left.
    pub(super) fn cancelled(&self) -> bool {
        pending_preparation_candidates()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&self.playback_id)
            .is_none_or(|pending| pending.claim != self.claim || pending.cancelled)
    }
}

impl Drop for PendingCandidateGuard {
    fn drop(&mut self) {
        let mut pending = pending_preparation_candidates()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending
            .get(&self.playback_id)
            .is_some_and(|entry| entry.claim == self.claim)
        {
            pending.remove(&self.playback_id);
        }
    }
}

/// The desired digest a pending candidate is being built for, if one is.
pub(super) fn pending_candidate_for_playback(playback_id: &str) -> Option<String> {
    pending_preparation_candidates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(playback_id)
        .filter(|pending| !pending.cancelled)
        .map(|pending| pending.desired_digest.clone())
}

/// Mark a pending candidate superseded. The task observes this at its next
/// await boundary and exits through its own accounting; the entry stays until
/// its guard drops, so nothing can observe a gap in which neither this map nor
/// the active registry names the playback.
fn cancel_pending_candidate(playback_id: &str) {
    if let Some(pending) = pending_preparation_candidates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_mut(playback_id)
    {
        pending.cancelled = true;
    }
}

/// Whether the pending candidate for this playback has been superseded, from
/// the point of view of a task building `desired_digest`.
///
/// Read once more immediately after registration: that is the one window the
/// guard's own checks cannot cover, because between a task's last await and
/// its `register_active_preparation` there is no suspension point at which it
/// could have noticed.
///
/// **Two ways to be superseded, and the flag is only one of them.** The other
/// is the ordinary one: a second quality tap on the same session activates
/// nothing, so `cancel_preparations_for_superseded_predecessor` never runs and
/// no flag is ever set — the next exchange simply dispatches again and
/// `PendingCandidateGuard::begin` *overwrites* the entry, with `cancelled`
/// false, for a different ask. A reader that looked only at the flag would let
/// that task register, arm its watch, reserve and prime a full encoder for a
/// selection the viewer had already left, and would leave two entries naming
/// one playback. This is the same "the slot is no longer mine" test
/// `PendingCandidateGuard::cancelled` makes, expressed against the digest
/// rather than the claim, because the caller here holds a `preparation` rather
/// than the guard.
///
/// An *absent* entry is not superseded: the test-only staging paths run with no
/// guard at all, and a task whose guard has already dropped has nothing left to
/// supersede. `None` for `desired_digest` is the staging path with no observed
/// selection — no evidence either way, so only the flag counts.
pub(super) fn pending_candidate_superseded(
    playback_id: &str,
    desired_digest: Option<&str>,
) -> bool {
    pending_preparation_candidates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(playback_id)
        .is_some_and(|pending| {
            pending.cancelled || desired_digest.is_some_and(|asked| pending.desired_digest != asked)
        })
}

/// Whether a successor is registered and priming **for this ask**.
///
/// The digest comparison is the same one the pending-marker clause makes, and
/// for the same reason. Without it one leftover entry — a stale ask the viewer
/// left, a planned relocation, a successor whose commit never comes — answers
/// `staging` on every exchange for the rest of the playback, and a client that
/// believes it burns its whole wait and *then* reopens. That is strictly worse
/// than the `none` it would have been told without the entry: the reopen still
/// happens, just several seconds later.
///
/// An executor with no recorded ask matches. Those are the staging paths that
/// ran without an observed selection, and an absent record is no evidence
/// either way — reading it as a mismatch would answer `none` while a real
/// successor primes.
pub(super) fn has_active_preparation_for_ask(playback_id: &str, desired_digest: &str) -> bool {
    active_prepared_successors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .any(|active| {
            active.preparation.playback_id == playback_id
                && active
                    .executor
                    .asked()
                    .is_none_or(|asked| asked == desired_digest)
        })
}

fn register_active_preparation(active: ActivePreparedSuccessor) {
    active_prepared_successors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(active.preparation.incarnation_id.clone(), active);
}

/// Register an `ActivePreparedSuccessor` without standing up a real encoder.
///
/// The stage-only path (`stage_prepared_successor`) deliberately does not
/// register: registration lives inside the prime branch, because cancellation
/// ownership only means anything once a worker exists to cancel. A test that
/// used the stage-only path to stand in for registration would be asserting
/// against an empty registry — the cancel would "succeed" because there was
/// nothing there, which is exactly the vacuous-guard shape this programme's
/// reviews keep finding. This builds the entry the real path builds and inserts
/// it the same way, so the edge under test is the real one.
#[cfg(test)]
pub(super) fn register_test_preparation(
    state: AppState,
    executor: crate::playback_control::PreparationExecutor,
    preparation: plurx_core::domain::MediaSessionPreparation,
    purpose: PreparationPurpose,
) -> tokio_util::sync::CancellationToken {
    let cancelled = tokio_util::sync::CancellationToken::new();
    register_active_preparation(ActivePreparedSuccessor {
        state,
        executor,
        preparation,
        purpose,
        cancelled: cancelled.clone(),
    });
    cancelled
}

fn arm_preparation_foreground_watch(active: ActivePreparedSuccessor) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = active.cancelled.cancelled() => return,
                () = tokio::time::sleep(Duration::from_millis(25)) => {}
            }
            if active_preparation(&active.preparation.incarnation_id).is_none() {
                return;
            }
            if active.state.transcode.foreground_media_waiting() {
                if let Some(active) = take_active_preparation(&active.preparation.incarnation_id) {
                    settle_cancelled_preparation(
                        active,
                        "foreground playback claimed prepared capacity",
                    )
                    .await;
                }
                return;
            }
        }
    });
}

pub(super) fn active_preparation(staged_incarnation_id: &str) -> Option<ActivePreparedSuccessor> {
    active_prepared_successors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(staged_incarnation_id)
        .cloned()
}

pub(super) fn take_active_preparation(
    staged_incarnation_id: &str,
) -> Option<ActivePreparedSuccessor> {
    active_prepared_successors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(staged_incarnation_id)
}

fn take_active_preparations_for_playback(playback_id: &str) -> Vec<ActivePreparedSuccessor> {
    let mut active = active_prepared_successors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let incarnation_ids = active
        .iter()
        .filter(|(_, preparation)| preparation.preparation.playback_id == playback_id)
        .map(|(incarnation_id, _)| incarnation_id.clone())
        .collect::<Vec<_>>();
    incarnation_ids
        .into_iter()
        .filter_map(|incarnation_id| active.remove(&incarnation_id))
        .collect()
}

pub(super) async fn settle_cancelled_preparation(
    active: ActivePreparedSuccessor,
    reason: &'static str,
) {
    let _ = take_active_preparation(&active.preparation.incarnation_id);
    crate::playback_control::record_preparation_cancelled(reason);
    active.cancelled.cancel();
    let deadline = tokio::time::Instant::now() + PREPARATION_SETTLEMENT_RETRY_BUDGET;
    let mut delay = PREPARATION_SETTLEMENT_RETRY_MIN;
    loop {
        let now_ms = unix_ms();
        let settled = match active
            .executor
            .abort(&active.preparation.incarnation_id, now_ms)
            .await
        {
            Ok(true) => true,
            Ok(false) => active
                .executor
                .discard_reserved(&active.preparation.incarnation_id, now_ms)
                .await
                .is_ok_and(|discarded| discarded),
            Err(_) => false,
        };
        if settled || tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(delay).await;
        delay = delay
            .saturating_mul(2)
            .min(PREPARATION_SETTLEMENT_RETRY_MAX);
    }
    retire_prepared_worker(
        &active.state,
        &active.preparation.owner_node_id,
        &active.preparation.incarnation_id,
        &active.preparation.session_id,
        reason,
    )
    .await;
}

fn spawn_cancelled_preparation(active: ActivePreparedSuccessor, reason: &'static str) {
    active.cancelled.cancel();
    tokio::spawn(settle_cancelled_preparation(active, reason));
}

pub(super) fn cancel_preparations_for_incumbent_wait(playback_id: &str) {
    for active in take_active_preparations_for_playback(playback_id) {
        spawn_cancelled_preparation(active, "incumbent playback needed prepared capacity");
    }
}

/// The client went around the handoff — it created a new session for this
/// playback instead of committing the one being prepared — so the successor
/// has no viewer. Free the worker now rather than at the 330 s deadline, and
/// mark a pending candidate that has not reached the registry yet.
///
/// Two outcomes were being paid for before this existed, both bad: on a node
/// with encoder headroom the orphan encodes for the full deadline for nobody;
/// on a node at its hardware session cap the reopen's own live create tears the
/// successor down as "foreground playback claimed prepared capacity" and counts
/// it as a refusal, which is what the fleet's `staged_total{refused}` has
/// actually been measuring.
///
/// `keep` is the activating route's incarnation id. The one successor that must
/// survive its own predecessor's retirement is the committed one. Commit
/// already removes that registry entry before this can run, so today the guard
/// is never the only thing standing between a committed successor and
/// cancellation — but ordering is not a contract, and the guard is what makes
/// it structural.
pub(super) fn cancel_preparations_for_superseded_predecessor(
    playback_id: &str,
    keep: Option<&str>,
) {
    cancel_pending_candidate(playback_id);
    for active in take_active_preparations_for_playback(playback_id) {
        if keep.is_some_and(|keep| keep == active.preparation.incarnation_id) {
            register_active_preparation(active);
            continue;
        }
        spawn_cancelled_preparation(active, "predecessor superseded by a new session");
    }
}

pub(in crate::http) async fn abort_disabled_prepared_handoffs() {
    let active = {
        let mut active = active_prepared_successors()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        active
            .drain()
            .map(|(_, preparation)| preparation)
            .collect::<Vec<_>>()
    };
    for preparation in active {
        settle_cancelled_preparation(preparation, "prepared handoff disabled").await;
    }
}

pub(in crate::http) fn cancel_prepared_handoff_work() {
    let active = {
        let mut active = active_prepared_successors()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        active
            .drain()
            .map(|(_, preparation)| preparation)
            .collect::<Vec<_>>()
    };
    for preparation in active {
        spawn_cancelled_preparation(preparation, "prepared handoff disabled");
    }
}

struct ReservedPreparationGuard {
    active: Option<ActivePreparedSuccessor>,
}

impl ReservedPreparationGuard {
    fn new(active: ActivePreparedSuccessor) -> Self {
        Self {
            active: Some(active),
        }
    }

    fn active(&self) -> &ActivePreparedSuccessor {
        self.active.as_ref().expect("armed preparation guard")
    }

    fn disarm(mut self) -> ActivePreparedSuccessor {
        self.active.take().expect("armed preparation guard")
    }
}

impl Drop for ReservedPreparationGuard {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            let _ = take_active_preparation(&active.preparation.incarnation_id);
            spawn_cancelled_preparation(active, "prepared successor ownership was cancelled");
        }
    }
}

async fn reserve_preparation_before(
    active: ActivePreparedSuccessor,
) -> Option<ReservedPreparationGuard> {
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if active
            .executor
            .reserve(&active.preparation)
            .await
            .is_ok_and(|reserved| reserved)
        {
            let _ = send.send(ReservedPreparationGuard::new(active));
        } else {
            settle_cancelled_preparation(active, "prepared successor reservation refused").await;
        }
    });
    tokio::time::timeout(PREPARATION_STORE_BUDGET, receive)
        .await
        .ok()
        .and_then(Result::ok)
}

async fn activate_preparation_before(
    guard: ReservedPreparationGuard,
) -> Option<ReservedPreparationGuard> {
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let active = guard.active();
        if active
            .executor
            .activate_reserved(&active.preparation)
            .await
            .is_ok_and(|activated| activated)
        {
            let _ = send.send(guard);
        }
    });
    tokio::time::timeout(PREPARATION_STORE_BUDGET, receive)
        .await
        .ok()
        .and_then(Result::ok)
}

#[cfg(test)]
fn completed_preparation_candidates() -> &'static std::sync::Mutex<std::collections::HashSet<String>>
{
    static COMPLETED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    COMPLETED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

#[cfg(test)]
pub(super) fn take_preparation_candidate_completion(incarnation_id: &str) -> bool {
    completed_preparation_candidates()
        .lock()
        .expect("preparation candidate completions")
        .remove(incarnation_id)
}

/// Last delivered selection of each *playback*, so a session that replaces
/// another can be measured against the one it replaced.
///
/// This exists because of what lab6 measured on 2026-09-02: 949 accepted
/// exchanges and one recorded decision. `ControlState::last_selection` sees a
/// selection change only *within* one session, and Apple does not change a
/// selection within a session — `PlayerController.selectQuality` calls
/// `reopen`, whose own comment says the replacement "intentionally removes the
/// older session". So a viewer's quality change arrives as a brand-new session
/// with no predecessor to differ from, and the gate is false by construction.
///
/// The seam M6 is actually about is therefore the replacement, not the
/// exchange — which is the whole point of the milestone: today a quality
/// change tears the session down, and M6 exists so that it does not.
///
/// **Keyed by playback id, not by `previous_session_id`.** That field is set
/// only on a stall reopen — `PlayerController.swift` builds it exclusively
/// from a `StallReopenTicket` — so a viewer's quality change carries none, and
/// a gate that required one alongside "not a stall" could never fire at all.
/// The playback id is what actually survives the replacement: the reopen
/// comment says the two sessions "share a playback ID".
pub(super) static DELIVERED_SELECTIONS: std::sync::Mutex<Option<DeliveredSelections>> =
    std::sync::Mutex::new(None);

/// Bounded FIFO of `playback id -> the session serving it and what it was last
/// delivering`.
///
/// Process-local and best-effort by construction: a replacement served by a
/// different node measures nothing, which understates the count and never
/// misreports one. Bounded because a long-lived node serves unboundedly many
/// sessions; the oldest is dropped, and dropping one costs a measurement.
#[derive(Default)]
pub(super) struct DeliveredSelections {
    pub(super) by_playback: std::collections::HashMap<String, RememberedDelivery>,
    pub(super) order: std::collections::VecDeque<String>,
}

/// What one playback was last seen delivering, and by which session.
#[derive(Clone)]
pub(super) struct RememberedDelivery {
    session: String,
    /// The file the predecessor was serving.
    ///
    /// A playback id outlives the film. Apple's `PlayerController` mints one
    /// per `PlayerView` and autoplay-next is a `stop()`-then-`start()` on the
    /// same controller, so episode N and N+1 share it — and
    /// `EffectiveSelection` carries no file identity, so without this an
    /// episode boundary is indistinguishable from a viewer changing quality.
    /// On a series binge that would be the *dominant* source of replacement
    /// measurements, and a later slice acting on this seam would stage a
    /// "seamless handoff" across a film boundary.
    file_id: i64,
    /// What the viewer had asked for, as distinct from what was delivered.
    ///
    /// The client reopens for its own reasons that are not viewer intent: a
    /// Dolby Vision fallback to the HDR10 base or to a compatibility
    /// transcode, a burn-in retry, a failure retry, an out-of-window seek.
    /// Those change the *delivered* recipe on exactly the two axes whose
    /// counts are the evidence for or against widening `PREPARED_AXIS`, while
    /// the viewer asked for nothing. Only `reopen_reason: Some(Stall)` is
    /// declared on the wire, so the honest discriminator is this: M6 prepares
    /// for a change the client *asked for*, and an unchanged ask is not one.
    asked: crate::playback_control::ClientSelection,
    selection: crate::playback_control::EffectiveSelection,
    grade: crate::playback_control::GradeIntent,
}

/// Enough for every playback a node serves in the window a viewer might change
/// quality in, and small enough to be invisible.
pub(super) const MAX_REMEMBERED_SELECTIONS: usize = 512;

/// Remember what this playback is delivering; answer what it was delivering
/// under the session this one replaced.
///
/// One lock, one pass. Answers `Some` exactly when the playback is already
/// known **and a different session is now serving it** — which is a
/// replacement, and is the only shape a viewer's quality change takes. Later
/// exchanges of the same session re-record and answer `None`, so one change is
/// counted once rather than for the life of the session.
pub(super) fn remember_delivered_selection(
    playback: &str,
    session: &str,
    file_id: i64,
    asked: &crate::playback_control::ClientSelection,
    delivered: &crate::playback_control::EffectiveSelection,
    grade: crate::playback_control::GradeIntent,
) -> Option<(
    crate::playback_control::EffectiveSelection,
    crate::playback_control::GradeIntent,
)> {
    let Ok(mut guard) = DELIVERED_SELECTIONS.lock() else {
        // A poisoned lock costs measurements, never an exchange.
        return None;
    };
    let table = guard.get_or_insert_with(DeliveredSelections::default);
    let replaced = match table.by_playback.get(playback) {
        // A replacement worth measuring is a *different session*, serving the
        // *same film*, because the viewer *asked for something else*. Drop any
        // one of those three and the count fills with things no viewer did:
        // an episode boundary, or the client's own recovery reopen.
        Some(held)
            if held.session != session && held.file_id == file_id && &held.asked != asked =>
        {
            Some((held.selection.clone(), held.grade))
        }
        Some(_) => None,
        None => {
            table.order.push_back(playback.to_owned());
            None
        }
    };
    // Most-recently-touched, not first-seen. Insertion order would evict the
    // two-hour film first, and mid-film is exactly when a viewer changes
    // quality — the entry most worth keeping would be the first one dropped.
    table.order.retain(|held| held != playback);
    table.order.push_back(playback.to_owned());
    while table.order.len() > MAX_REMEMBERED_SELECTIONS {
        if let Some(evicted) = table.order.pop_front() {
            table.by_playback.remove(&evicted);
        }
    }
    table.by_playback.insert(
        playback.to_owned(),
        RememberedDelivery {
            session: session.to_owned(),
            file_id,
            asked: asked.clone(),
            selection: delivered.clone(),
            grade,
        },
    );
    replaced
}

/// Everything one exchange said, gathered for the detached preparation path.
///
/// A struct rather than eight parameters: these are all *one exchange's*
/// answer, they are always passed together, and the spawned task has no reason
/// to be able to take them from different exchanges.
#[derive(Clone, Copy, Debug)]
pub(super) enum PreparationPurpose {
    SelectionChange,
    PlannedRelocation(crate::serving_fence::PlannedOutageFenceToken),
}

pub(super) struct PreparationCandidateInputs {
    /// The live session this exchange belongs to. Staging needs it twice: to
    /// reach the actor that owns the one successor slot, and to name the
    /// predecessor the commit CAS will fence against.
    pub(super) session_id: String,
    pub(super) route: MediaSessionRoute,
    pub(super) recipe: RemoteStartRequest,
    pub(super) planning_caps: Option<plurx_core::playback::DeviceCaps>,
    pub(super) planning_overrides: Option<CreateOverrides>,
    pub(super) selection: crate::playback_control::ClientSelection,
    pub(super) observed_download_bps: Option<u64>,
    pub(super) delivered: crate::playback_control::EffectiveSelection,
    pub(super) delivered_bps: Option<i64>,
    pub(super) capabilities: Option<crate::playback_control::DynamicCapabilities>,
    pub(super) platform: crate::playback_control::ClientPlatform,
    pub(super) purpose: PreparationPurpose,
    /// Where the viewer actually is, in absolute film time, according to the
    /// envelope this exchange **accepted**.
    ///
    /// Captured here rather than read from the route because only the envelope
    /// knows it. A route carries `fetched_through_ms`, which is a high-water
    /// fetch frontier: it advances to the *end* of a segment the moment the
    /// client asks for it, clients prefetch and retry, and after a backward
    /// seek it stays far ahead of the playhead. Staging from it hands the
    /// viewer a successor that begins past film they have not watched.
    ///
    /// `seek_target_ms` wins over `position_ms` for the same reason
    /// [`ControlRequestV1::validate`] anchors the buffer on it: while a seek is
    /// in flight the reported position is still the old one, and the target is
    /// where this viewer is going.
    ///
    /// Already absolute — the control plane's positions are source-timeline
    /// values, not offsets into this session — so nothing downstream may add
    /// `media_origin_ms` to it.
    ///
    /// Bounded at the seam by the film's own duration, not by `validate`:
    /// validation allows a target duration of slack past the end so a client
    /// reporting the final segment's end is not refused, and that is not a
    /// place a successor may begin.
    pub(super) accepted_film_time_ms: i64,
}

/// Re-run the same capability-aware planning used by ordinary create.
///
/// A prepared successor is a new playable recipe, not an edit to the current
/// encoder. In particular, Original may move a transcode back to direct/remux,
/// and a compound quality/track/subtitle request must be resolved once as a
/// whole. Recipes written before the durable response sidecar was retained use
/// the legacy edit path so an upgrade never guesses capabilities that the
/// client did not provide.
pub(super) async fn plan_preparation_candidate(
    state: &AppState,
    predecessor: &RemoteStartRequest,
    planning_caps: Option<&plurx_core::playback::DeviceCaps>,
    planning_overrides: Option<&CreateOverrides>,
    selection: &crate::playback_control::ClientSelection,
    source: &MediaFile,
    delivered_height: i64,
) -> Result<crate::transcode::SessionRequest, ApiError> {
    if let Some(caps) = planning_caps {
        super::super::stream::validate_device_caps(caps)?;
    }
    let Some(caps) = planning_caps
        .filter(|caps| caps.v == plurx_core::playback::DeviceCaps::VERSION && !caps.is_empty())
    else {
        let height = match selection.quality {
            // A named Auto rung is an explicit ask to `resolve_height`, which
            // snaps it to the ladder and never binds above the capability
            // ceiling — the same treatment a manual rung gets. Unnamed Auto
            // still means "whatever is being delivered".
            crate::playback_control::QualitySelection::Auto {
                height: Some(height),
            } => {
                resolve_height(
                    state,
                    Some(source),
                    None,
                    predecessor.request.hdr10,
                    Some(height),
                )
                .await
            }
            crate::playback_control::QualitySelection::Auto { height: None } => delivered_height,
            crate::playback_control::QualitySelection::Original => {
                source.height.unwrap_or(delivered_height)
            }
            crate::playback_control::QualitySelection::Manual { height } => {
                resolve_height(
                    state,
                    Some(source),
                    None,
                    predecessor.request.hdr10,
                    Some(height),
                )
                .await
            }
        };
        // The legacy branch builds a candidate too, and it carries the
        // viewer's `subtitle_burn` just as the caps-v2 branch does — so it
        // needs the same guard. Without it a client that sends no caps
        // document (or one this build cannot parse) can still have a
        // successor staged as a burn that tone-maps the picture at the moment
        // it is committed, which is the failure this guard exists for.
        let candidate = crate::playback_control::candidate_request(
            &predecessor.request,
            selection,
            height,
            source.height,
        );
        if burn_would_discard_this_session_hdr(
            state,
            Some(source),
            &candidate,
            predecessor.request.hdr10,
            height,
        )
        .await
        {
            return Err(ApiError::Unprocessable(serde_json::json!({
                "code": "hdr_subtitle_burn_refused",
                "error": HDR_SUBTITLE_BURN_REFUSAL,
            })));
        }
        return Ok(candidate);
    };

    use plurx_core::playback::{DeviceProfile, Force, PlaybackMethod};
    use plurx_core::transcode::OutputGrade;

    let quality_force = match selection.quality {
        // Auto stays Auto with or without a named rung. The rung is carried on
        // the create body below, not by switching the server's own policy off.
        crate::playback_control::QualitySelection::Auto { .. } => Force::Auto,
        crate::playback_control::QualitySelection::Original => Force::Original,
        crate::playback_control::QualitySelection::Manual { .. } => Force::Transcode,
    };
    let unsupported = |message| {
        ApiError::typed(
            StatusCode::UNPROCESSABLE_ENTITY,
            "prepared_output_unsupported",
            message,
        )
    };
    let source_codec = source
        .video_codec
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let source_is_h264 = matches!(source_codec.as_str(), "h264" | "avc" | "avc1");
    let source_is_hevc = matches!(source_codec.as_str(), "hevc" | "h265" | "hev1" | "hvc1");
    let source_range = source.hdr.as_deref().unwrap_or("sdr");
    let codec_satisfied = match selection.codec {
        crate::playback_control::CodecPolicy::Auto => true,
        crate::playback_control::CodecPolicy::H264 => source_is_h264,
        crate::playback_control::CodecPolicy::Hevc => source_is_hevc,
        crate::playback_control::CodecPolicy::Av1 => {
            return Err(ApiError::typed(
                StatusCode::UNPROCESSABLE_ENTITY,
                "prepared_output_unsupported",
                "the ordinary planner has no AV1 successor output",
            ));
        }
    };
    let range_satisfied = match selection.dynamic_range {
        crate::playback_control::DynamicRangePolicy::Auto => true,
        crate::playback_control::DynamicRangePolicy::DolbyVision => source_range == "dolby_vision",
        crate::playback_control::DynamicRangePolicy::Hdr10 => source_range == "hdr10",
        crate::playback_control::DynamicRangePolicy::Hlg => source_range == "hlg",
        crate::playback_control::DynamicRangePolicy::Sdr => source_range == "sdr",
    };
    if matches!(
        (selection.codec, selection.dynamic_range),
        (
            crate::playback_control::CodecPolicy::H264,
            crate::playback_control::DynamicRangePolicy::Hdr10
                | crate::playback_control::DynamicRangePolicy::DolbyVision
                | crate::playback_control::DynamicRangePolicy::Hlg
        )
    ) {
        return Err(unsupported(
            "the ordinary planner cannot produce the requested HDR grade in H.264",
        ));
    }
    if matches!(
        selection.dynamic_range,
        crate::playback_control::DynamicRangePolicy::DolbyVision
            | crate::playback_control::DynamicRangePolicy::Hlg
    ) && (!range_satisfied
        || matches!(
            selection.quality,
            crate::playback_control::QualitySelection::Manual { .. }
        ))
    {
        return Err(unsupported(
            "the ordinary planner can preserve this requested grade but cannot synthesize it",
        ));
    }
    if matches!(
        selection.dynamic_range,
        crate::playback_control::DynamicRangePolicy::Hdr10
    ) && !range_satisfied
        && plurx_core::playback::hdr_route(source).is_none()
    {
        return Err(unsupported(
            "the ordinary planner cannot synthesize HDR10 from this source grade",
        ));
    }
    if matches!(selection.codec, crate::playback_control::CodecPolicy::Hevc)
        && !codec_satisfied
        && !matches!(
            selection.dynamic_range,
            crate::playback_control::DynamicRangePolicy::Hdr10
        )
    {
        return Err(unsupported(
            "the ordinary planner emits HEVC only for an HDR10 successor",
        ));
    }
    // Original controls the quality rung; it does not override an explicit
    // codec or grade request. A source that already satisfies the compound
    // policy may still copy/remux, while a mismatch goes through the ordinary
    // transcode planner and unsupported output was refused above.
    let force = if !codec_satisfied || !range_satisfied {
        Force::Transcode
    } else {
        quality_force
    };
    let force_name = match force {
        Force::Auto => "auto",
        Force::Original => "original",
        Force::Transcode => "transcode",
    };
    let mut overrides = planning_overrides.cloned().unwrap_or_default();
    overrides.force = Some(force_name.to_owned());
    let mut profile = DeviceProfile::from_caps_v2(caps);
    profile.retain_applicable_learned_limits(unix_ms());
    let node = super::super::stream::render_caps(state).await;
    let decision = plurx_core::playback::decide_forced(source, &profile, force, &node);
    let copy = decision.method != PlaybackMethod::Transcode;
    let requested_height = match selection.quality {
        crate::playback_control::QualitySelection::Auto { height } => height,
        crate::playback_control::QualitySelection::Original => source.height,
        crate::playback_control::QualitySelection::Manual { height } => Some(height),
    };
    let requested_hdr10 = decision.transcode_grade == OutputGrade::Hdr10
        && !matches!(
            selection.dynamic_range,
            crate::playback_control::DynamicRangePolicy::Sdr
        )
        && !matches!(selection.codec, crate::playback_control::CodecPolicy::H264);
    let delivered_codec_satisfied = match selection.codec {
        crate::playback_control::CodecPolicy::Auto => true,
        crate::playback_control::CodecPolicy::H264 => {
            if copy {
                source_is_h264
            } else {
                !requested_hdr10
            }
        }
        crate::playback_control::CodecPolicy::Hevc => {
            if copy {
                source_is_hevc
            } else {
                requested_hdr10
            }
        }
        crate::playback_control::CodecPolicy::Av1 => false,
    };
    let delivered_range_satisfied = match selection.dynamic_range {
        crate::playback_control::DynamicRangePolicy::Auto => true,
        crate::playback_control::DynamicRangePolicy::DolbyVision => {
            copy && decision.delivered_dynamic_range == "dolby_vision"
        }
        crate::playback_control::DynamicRangePolicy::Hdr10 => {
            (copy && decision.delivered_dynamic_range == "hdr10") || (!copy && requested_hdr10)
        }
        crate::playback_control::DynamicRangePolicy::Hlg => {
            copy && decision.delivered_dynamic_range == "hlg"
        }
        crate::playback_control::DynamicRangePolicy::Sdr => {
            decision.delivered_dynamic_range == "sdr" && !requested_hdr10
        }
    };
    if !delivered_codec_satisfied || !delivered_range_satisfied {
        return Err(unsupported(
            "the ordinary planner cannot satisfy the requested codec and dynamic range together",
        ));
    }
    let native_subtitle = matches!(
        selection.subtitle.mode,
        crate::playback_control::SubtitleMode::Native
    )
    .then_some(selection.subtitle.track)
    .flatten();
    let body = CreateSession {
        playback_id: predecessor.request.playback_id.clone(),
        request_id: None,
        control_sequence: None,
        previous_session_id: None,
        reopen_reason: None,
        height: requested_height,
        quality_auto: Some(matches!(
            selection.quality,
            crate::playback_control::QualitySelection::Auto { .. }
        )),
        subtitle_burn: matches!(
            selection.subtitle.mode,
            crate::playback_control::SubtitleMode::Burn
        )
        .then_some(selection.subtitle.track)
        .flatten(),
        // No acknowledgement. This path calls `resolve_plan` directly, so the
        // create handler's guard never ran on it and `Some(!requested_hdr10)`
        // had no reader at all — it was an answer to a question nobody asked.
        // The candidate is judged by the same guard below instead.
        subtitle_burn_sdr: None,
        native_subtitles: Some(native_subtitle.is_some()),
        subtitle: native_subtitle,
        start: Some(predecessor.request.start_seconds),
        audio: selection.audio_track,
        copy: Some(copy),
        aac: Some(decision.transcode_audio),
        preserve_dolby_vision: Some(decision.preserve_dolby_vision),
        hdr10: Some(requested_hdr10),
        audio_offset_ms: Some(selection.audio_offset_ms),
        caps: Some(caps.clone()),
        overrides: Some(overrides.clone()),
        presentation: Some("vod".to_owned()),
        block_budget_secs: predecessor.request.block_budget_secs,
        transport: predecessor.request.transport.clone(),
        intent: None,
    };
    let review = review_client_plan_inner(
        caps,
        Some(&overrides),
        source,
        &node,
        decision.preserve_dolby_vision,
        requested_hdr10,
        unix_ms(),
        false,
    );
    let plan = resolve_plan(
        PlanInputs {
            state,
            user_id: predecessor.user_id,
            file_id: predecessor.request.file_id,
            source: Some(source),
            network_prior: None,
        },
        Some(review),
        body,
    )
    .await?;
    let plan_height = plan.height;
    let mut resolved = plan.request;
    validate_hevc_copy_transport(state, source, caps, &resolved).await?;
    // The same guard ordinary create runs, on the same function. This path
    // reaches `resolve_plan` directly and therefore skipped it entirely: a
    // successor prepared for an HDR delivery could be staged as a burn that
    // silently tone-maps the picture at the moment it is committed, with the
    // viewer given no notice and no choice. Refusing the candidate is right
    // here — the incumbent keeps playing, which is what a refused preparation
    // means everywhere else.
    // `resolved.hdr10`, not the pre-review `requested_hdr10`: the review may
    // clamp the ask against the device profile, and the request this guard is
    // judging carries the clamped value. Create reads its post-review value
    // for the same reason.
    if burn_would_discard_this_session_hdr(
        state,
        Some(source),
        &resolved,
        resolved.hdr10,
        plan_height,
    )
    .await
    {
        return Err(ApiError::Unprocessable(serde_json::json!({
            "code": "hdr_subtitle_burn_refused",
            "error": HDR_SUBTITLE_BURN_REFUSAL,
        })));
    }
    // Planning chooses the codec/container recipe. It must not silently turn
    // a retained rolling fallback into VOD: the source prerequisite that made
    // the incumbent use rolling has not changed merely because its quality or
    // tracks did. VOD stays VOD and rolling stays rolling.
    resolved.presentation = predecessor.request.presentation;
    Ok(resolved)
}

async fn prepared_relocation_owner(
    state: &AppState,
    source: &MediaFile,
    candidate: &crate::transcode::SessionRequest,
    accepted_film_time_ms: i64,
) -> Option<String> {
    let target_height = match candidate.kind {
        crate::transcode::SessionKind::Transcode { height } => height,
        crate::transcode::SessionKind::Copy { .. } => source.height?,
    }
    .clamp(crate::transcode::MIN_HEIGHT, crate::transcode::MAX_HEIGHT);
    let request = MediaOfferRequest::new(
        source,
        target_height,
        accepted_film_time_ms,
        candidate.audio_index,
        candidate.subtitle_burn,
        candidate.hdr10,
    )
    .ok()?;
    state
        .media_pool
        .offers(state, request)
        .await
        .offers
        .into_iter()
        .find(|offer| offer.eligible && offer.node_id != state.node_id)
        .map(|offer| offer.node_id)
}

/// Evaluate and, when admitted, durably stage this selection change.
///
/// The response never waits for this work: the accepted exchange has already
/// been built, and a later exchange observes the durable successor. The
/// decision is nevertheless production authority, not shadow measurement;
/// its actor slot and durable ledger are the only way this path can stage.
///
/// Called only when the engine reports the selection moved, and spawned rather
/// than awaited: the exchange is under an absolute deadline it has *already
/// spent* by this point, the response is fully built, and a store read that
/// ran long would turn a completed exchange into a 503 after the accepted
/// transaction had already been recorded.
///
/// Decided against **the response this exchange actually sent**: the client
/// was told a height and a rate, so staging from different values would build
/// a transition the client never requested.
pub(super) async fn process_preparation_candidate(
    state: AppState,
    pending: PendingCandidateGuard,
    exchange: PreparationCandidateInputs,
) {
    let PreparationCandidateInputs {
        session_id,
        route,
        recipe,
        planning_caps,
        planning_overrides,
        selection,
        observed_download_bps,
        delivered,
        delivered_bps,
        capabilities,
        platform,
        accepted_film_time_ms,
        purpose,
    } = exchange;
    #[cfg(test)]
    struct Completion(String);
    #[cfg(test)]
    impl Drop for Completion {
        fn drop(&mut self) {
            completed_preparation_candidates()
                .lock()
                .expect("preparation candidate completions")
                .insert(self.0.clone());
        }
    }
    #[cfg(test)]
    let _completion = Completion(route.incarnation_id.clone());
    // Released on every exit below, including the early one.
    struct InFlight;
    impl Drop for InFlight {
        fn drop(&mut self) {
            PREPARATION_CANDIDATES_IN_FLIGHT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if PREPARATION_CANDIDATES_IN_FLIGHT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        >= MAX_PREPARATION_CANDIDATES
    {
        PREPARATION_CANDIDATES_IN_FLIGHT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let _in_flight = InFlight;
    let enabled = match state
        .store
        .get_setting(plurx_core::store::keys::PREPARED_QUALITY_HANDOFF)
        .await
    {
        Ok(value) => plurx_core::store::stored_switch(value.as_deref(), true),
        Err(error) => {
            tracing::warn!(%error, "prepared-handoff setting could not be read");
            return;
        }
    };
    if !enabled {
        return;
    }
    if pending.cancelled() {
        crate::playback_control::record_preparation_staged(false);
        return;
    }
    let Ok(source) = state.store.get_file(recipe.request.file_id).await else {
        // Detached failure is fail-safe: no candidate is staged, and the
        // already-completed exchange remains valid.
        return;
    };
    let Some(source) = source.as_ref() else {
        return;
    };
    if pending.cancelled() {
        crate::playback_control::record_preparation_staged(false);
        return;
    }
    // Test-only. The refusal arm takes the same exit as the `Err` arm below,
    // so a forced refusal is indistinguishable from an unplannable candidate.
    #[cfg(test)]
    if let Some((delay, refuse)) = take_preparation_planning_fault(&route.playback_id) {
        tokio::time::sleep(delay).await;
        if refuse {
            crate::playback_control::record_preparation_staged(false);
            return;
        }
    }
    let candidate = match plan_preparation_candidate(
        &state,
        &recipe,
        planning_caps.as_ref(),
        planning_overrides.as_ref(),
        &selection,
        source,
        delivered.height,
    )
    .await
    {
        Ok(candidate) => candidate,
        Err(error) => {
            tracing::warn!(?error, "prepared successor could not be planned");
            crate::playback_control::record_preparation_staged(false);
            return;
        }
    };
    if pending.cancelled() {
        crate::playback_control::record_preparation_staged(false);
        return;
    }
    let proposed = crate::playback_control::EffectiveSelection::from_request(
        &candidate,
        match candidate.kind {
            crate::transcode::SessionKind::Transcode { height } => height,
            // A copy is not a rung, so it keeps the height it is already
            // delivering rather than one the ladder would have picked.
            crate::transcode::SessionKind::Copy { .. } => delivered.height,
        },
        None,
    );
    // Built once and passed to both decisions, so the two counters can never
    // disagree about what the transition was — only about the client gate.
    let delivered_view = crate::playback_control::RecipeView {
        selection: &delivered,
        grade: crate::playback_control::GradeIntent::from_request(&recipe.request),
    };
    let proposed_view = crate::playback_control::RecipeView {
        selection: &proposed,
        grade: crate::playback_control::GradeIntent::from_request(&candidate),
    };
    let conditions = crate::playback_control::PreparationConditions {
        observed_download_bps,
        delivered_bps,
    };
    let decision = crate::playback_control::decide_preparation(
        delivered_view,
        proposed_view,
        capabilities.as_ref(),
        conditions,
    );
    crate::playback_control::record_preparation_decision(platform, decision);
    // Keep the counterfactual beside the production decision as advisory
    // rollout telemetry. All three clients may advertise the explicit runtime
    // capability; neither a missing receipt nor a link estimate changes the
    // viewer's saved choice.
    crate::playback_control::record_preparation_counterfactual(
        platform,
        crate::playback_control::decide_preparation_after_client_release(
            delivered_view,
            proposed_view,
            conditions,
        ),
    );
    let successor_owner = match purpose {
        PreparationPurpose::SelectionChange => matches!(
            decision,
            crate::playback_control::PreparationDecision::Prepare { .. }
        )
        .then(|| state.node_id.clone()),
        PreparationPurpose::PlannedRelocation(fence)
            if capabilities
                .as_ref()
                .is_some_and(|caps| caps.dual_player_preparation)
                && state.serving.planned_outage_is_current(fence).await =>
        {
            prepared_relocation_owner(&state, source, &candidate, accepted_film_time_ms).await
        }
        PreparationPurpose::PlannedRelocation(_) => None,
    };
    if let Some(successor_owner) = successor_owner {
        #[cfg(test)]
        let _ = &successor_owner;
        #[cfg(not(test))]
        stage_and_prime_prepared_successor(
            &state,
            &session_id,
            &route,
            &recipe,
            &candidate,
            Some(source),
            &successor_owner,
            purpose,
            AcceptedAsk {
                film_time_ms: accepted_film_time_ms,
                desired_digest: Some(selection.desired().digest()),
            },
        )
        .await;
        // These decision-boundary tests use synthetic media rows and exercise
        // durable preparation semantics without launching ffmpeg. Production
        // always takes the reserve-and-prime call above.
        #[cfg(test)]
        stage_prepared_successor(
            &state,
            &session_id,
            &route,
            &recipe,
            &candidate,
            Some(source),
            AcceptedAsk {
                film_time_ms: accepted_film_time_ms,
                desired_digest: Some(selection.desired().digest()),
            },
        )
        .await;
    }
}

/// How long a staged successor may sit unclaimed.
///
/// Keyed off `VOD_LEASE_TIMEOUT_MS`, the longest control lease this can inherit.
/// A client is allowed that long between exchanges before it is even nominally
/// late, so a shorter window would expire VOD successors for clients behaving
/// exactly as the protocol permits. One longest lease plus a margin covers a
/// slow client without covering a gone one; rolling successors still advertise
/// and renew on their shorter rolling lease after commit.
///
/// **Correction, 2026-09-08: the store does enforce this bound, and this
/// paragraph used to say it did not.** It read *"This bound is enforced here,
/// not by the store. Maintenance retires an expired row only
/// `TAKEOVER_RECOVERY_MS` after its lease lapses…"*, and that was
/// **already false when it was committed**: `1d55c976`, "a preparation expires
/// on its own deadline", landed at 02:47:33 and `27e77824`, which added this
/// paragraph, at 04:14:45 the same morning — eighty-seven minutes later. The
/// belief was true when its author formed it; the tree moved underneath them
/// before they wrote it down, which is the ordinary way a comment is born
/// wrong rather than a way it goes stale.
///
/// `sqlite/sessions.rs` ends a staged row keyed on `deadline_ms` directly, and
/// `hiqlite_sessions.rs` mirrors it — *"A preparation expires on **its own
/// deadline**"* — and the lease loop refuses
/// to renew any incarnation carrying a preparation row, so the deadline cannot
/// be postponed.
///
/// So what the timer below is actually for is narrower than it looks: it frees
/// **the actor's in-memory slot**, which nothing in the store path settles,
/// and it shaves up to one maintenance tick (five minutes) off the durable
/// reap. Both are worth having. Neither is "the only thing enforcing the
/// bound", and building on that belief is how a reader concludes the durable
/// side is unprotected.
const PREPARATION_DEADLINE_MS: i64 = crate::playback_control::VOD_LEASE_TIMEOUT_MS as i64 + 30_000;
pub(super) const PREPARATION_STORE_BUDGET: Duration = Duration::from_secs(5);
const PREPARATION_PRIME_BUDGET: Duration = Duration::from_secs(45);

/// M6 §3.4 — stage the successor the decision just admitted.
///
/// This joins stage with reserve-and-prime: the durable row is written first,
/// the VOD worker attaches behind its unpublished capability, and only then
/// may the actor expose its one successor slot. The pointer remains untouched
/// by construction — `prepare_media_session` is the entry point that neither
/// runs the supersession reap nor advances `media_playback_pointers`, which is
/// why a preparation cannot be built on `activate_media_session`.
///
/// Best-effort from the viewer's perspective. A staged successor that
/// fails to appear costs the viewer nothing: the fallback replacement they
/// would have taken before M6 is still exactly what happens. A staged
/// successor that appears when it should not costs a saturated user real
/// resources, which is why every refusal below returns rather than retries.
/// What the accepted control exchange said the viewer wants.
///
/// The two travel together because they are facts about the same exchange and
/// are both wrong if taken from different ones: staging at the film time from
/// one exchange under the ask from another builds a successor for a moment and
/// a selection that never coexisted.
pub(super) struct AcceptedAsk {
    /// The absolute film time the accepted envelope settled on — the viewer's
    /// seek target where they asked for one, their playhead otherwise.
    pub(super) film_time_ms: i64,
    /// The normalized ask, recorded on the preparation slot so an
    /// acknowledgement arriving later is judged against the ask that is
    /// current then. `None` where the caller stages without an observed
    /// selection.
    pub(super) desired_digest: Option<String>,
}

#[cfg(test)]
pub(super) async fn stage_prepared_successor(
    state: &AppState,
    session_id: &str,
    route: &MediaSessionRoute,
    predecessor: &RemoteStartRequest,
    candidate: &crate::transcode::SessionRequest,
    source: Option<&plurx_core::domain::MediaFile>,
    accepted: AcceptedAsk,
) {
    stage_prepared_successor_with_prime(
        state,
        session_id,
        route,
        predecessor,
        candidate,
        source,
        &state.node_id,
        PreparationPurpose::SelectionChange,
        accepted,
        false,
    )
    .await;
}

#[cfg(not(test))]
#[allow(clippy::too_many_arguments)]
async fn stage_and_prime_prepared_successor(
    state: &AppState,
    session_id: &str,
    route: &MediaSessionRoute,
    predecessor: &RemoteStartRequest,
    candidate: &crate::transcode::SessionRequest,
    source: Option<&plurx_core::domain::MediaFile>,
    successor_owner: &str,
    purpose: PreparationPurpose,
    accepted: AcceptedAsk,
) {
    stage_prepared_successor_with_prime(
        state,
        session_id,
        route,
        predecessor,
        candidate,
        source,
        successor_owner,
        purpose,
        accepted,
        true,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn stage_prepared_successor_with_prime(
    state: &AppState,
    session_id: &str,
    route: &MediaSessionRoute,
    predecessor: &RemoteStartRequest,
    candidate: &crate::transcode::SessionRequest,
    source: Option<&plurx_core::domain::MediaFile>,
    successor_owner: &str,
    purpose: PreparationPurpose,
    accepted: AcceptedAsk,
    prime_worker: bool,
) {
    // Saving the switch off takes the write side, persists the choice, and
    // settles every registered successor before releasing it. Holding this
    // read side through the final setting read and stage closes the race where
    // an older detached candidate reserved after the administrator disabled
    // preparation.
    let _handoff_transition = prepared_handoff_transition().read_owned().await;
    let enabled = state
        .store
        .get_setting(plurx_core::store::keys::PREPARED_QUALITY_HANDOFF)
        .await
        .ok()
        .is_some_and(|value| plurx_core::store::stored_switch(value.as_deref(), true));
    if !enabled {
        crate::playback_control::record_preparation_staged(false);
        return;
    }
    let AcceptedAsk {
        film_time_ms: accepted_film_time_ms,
        desired_digest,
    } = accepted;
    if let PreparationPurpose::PlannedRelocation(fence) = purpose {
        if !state.serving.planned_outage_is_current(fence).await {
            crate::playback_control::record_preparation_staged(false);
            return;
        }
    }
    // The actor owns the slot. No live local worker means no slot to take, and
    // an expired, remote-owned or retired playback stages nothing.
    let Some(gate) = state.transcode.session_preparation_gate(session_id).await else {
        // Counted, not silent: a decision that never reached a gate is
        // exactly what the first version of this did on every production
        // session, and an uncounted return made that indistinguishable from
        // "nothing was admitted".
        crate::playback_control::record_preparation_staged(false);
        return;
    };
    // The source snapshot has to be the file's real one. `0/0` is this
    // codebase's documented "this placement never read the file" sentinel, and
    // `takeover_source_matches` refuses a takeover on it forever — so writing
    // it as a placeholder would poison the successor the moment it committed.
    let Some(source) = source else {
        return;
    };
    let now_ms = crate::media_sessions::unix_ms();
    let staged_incarnation_id = uuid::Uuid::new_v4().to_string();
    let staged_session_id = uuid::Uuid::new_v4().to_string();
    // Where the successor actually begins: **where the viewer is**, not where
    // the predecessor was created and not how far it has fetched.
    //
    // `candidate` is the predecessor's request with only the selection fields
    // overwritten, so its `start_seconds` is the *original* start — committing
    // that would restart the film for a viewer forty minutes in. That much was
    // always true. What this previously did instead was
    // `media_origin_ms + fetched_through_ms`, and that is the other error:
    // the fetched frontier is a high-water mark, not a presentation time. It
    // reaches the end of a segment as soon as the client requests that segment,
    // it survives prefetch and retry, and after a backward seek it stays where
    // the client had already reached. Resuming there hands the viewer a
    // successor that starts past film they have never been shown — the very
    // discontinuity a prepared handoff exists to avoid. `OwnerLossResume`
    // documents the same hazard and pulls its frontier back by a whole segment
    // precisely because that path has no envelope to ask; this one does.
    //
    // `accepted_film_time_ms` is already absolute source-timeline film time, so
    // `media_origin_ms` must **not** be added back: the predecessor's origin is
    // baked into the position the client reported against it, and adding it
    // again would push a viewer thirty seconds into a film that started at
    // 00:01:30 out to 00:02:30.
    let resume_ms = accepted_film_time_ms;
    let staged_request = crate::transcode::SessionRequest {
        request_id: Some(staged_incarnation_id.clone()),
        start_seconds: resume_ms as f64 / 1_000.0,
        // A successor is its own generation, not a reopen of the one it
        // replaces. Carrying these across would stage a row claiming to be a
        // stall reopen of a session that is still playing.
        previous_session_id: None,
        reopen_reason: None,
        control_sequence: None,
        ..candidate.clone()
    };
    let staged_recipe = RemoteStartRequest {
        protocol_version: crate::media_pool::PROTOCOL_VERSION,
        incarnation_id: staged_incarnation_id.clone(),
        user_id: route.user_id,
        source_size: source.size,
        source_mtime: source.mtime,
        // Read from the predecessor rather than assumed: this decides whether
        // anything may ever take the successor over.
        typeless_playlist: predecessor.typeless_playlist,
        library_channel: predecessor.library_channel.clone(),
        request: staged_request.clone(),
    };
    let Ok(recipe_json) = serde_json::to_string(&staged_recipe) else {
        return;
    };
    // A real `StartResponse`, because every reader of a route's
    // `response_json` parses it as one — and `control_start_response` filters
    // on the bootstrap being present, so a row without it answers 404
    // `session_gone` on the successor's first exchange after commit.
    let response = StartResponse {
        session_id: staged_session_id.clone(),
        playlist_url: format!("/api/v1/hls/{staged_session_id}/index.m3u8"),
        duration_ms: source.duration_ms,
        start_seconds: resume_ms as f64 / 1_000.0,
        // VOD playlists retain the film's absolute zero origin. A rolling
        // successor instead begins its local timeline at this film boundary.
        // Copy may internally pull back to a keyframe, but the requested
        // boundary remains the client-visible handoff target just as it does
        // for an ordinary rolling create.
        media_origin_ms: Some(
            if staged_request.presentation == crate::transcode::Presentation::Vod {
                0
            } else {
                resume_ms
            },
        ),
        height: match staged_request.kind {
            crate::transcode::SessionKind::Transcode { height } => height,
            crate::transcode::SessionKind::Copy { .. } => source.height.unwrap_or_default(),
        },
        encoder: if staged_request.presentation == crate::transcode::Presentation::Vod {
            "vod"
        } else if matches!(
            staged_request.kind,
            crate::transcode::SessionKind::Copy { .. }
        ) {
            "copy"
        } else {
            "transcode"
        }
        .to_owned(),
        vod: staged_request.presentation == crate::transcode::Presentation::Vod,
        ladder: Vec::new(),
        prior_kbps: None,
        delivered_dynamic_range: None,
        delivered_dolby_vision_profile: None,
        control: crate::playback_control::ControlBootstrap::new(
            &staged_session_id,
            &staged_incarnation_id,
            1,
            if staged_request.presentation == crate::transcode::Presentation::Vod {
                crate::playback_control::VOD_LEASE_TIMEOUT_MS
            } else {
                crate::playback_control::ROLLING_LEASE_TIMEOUT_MS
            },
        ),
        plan_notes: vec![PREPARED_SUCCESSOR_PLAN_NOTE.to_owned()],
    };
    let mut response_value = match serde_json::to_value(response) {
        Ok(value) => value,
        Err(_) => return,
    };
    if let Some(caps) = retained_planning_caps(&route.response_json) {
        response_value
            .as_object_mut()
            .expect("StartResponse serializes as an object")
            .insert(
                PLANNING_CAPS_RESPONSE_FIELD.to_owned(),
                match serde_json::to_value(caps) {
                    Ok(value) => value,
                    Err(_) => return,
                },
            );
    }
    if let Some(overrides) = retained_planning_overrides(&route.response_json) {
        response_value
            .as_object_mut()
            .expect("StartResponse serializes as an object")
            .insert(
                PLANNING_OVERRIDES_RESPONSE_FIELD.to_owned(),
                match serde_json::to_value(overrides) {
                    Ok(value) => value,
                    Err(_) => return,
                },
            );
    }
    if let PreparationPurpose::PlannedRelocation(fence) = purpose {
        response_value
            .as_object_mut()
            .expect("StartResponse serializes as an object")
            .insert(
                PREPARATION_REASON_RESPONSE_FIELD.to_owned(),
                serde_json::Value::String(format!("planned_relocation:{}", fence.identity())),
            );
    }
    let Ok(response_json) = serde_json::to_string(&response_value) else {
        return;
    };
    let executor = crate::playback_control::PreparationExecutor::new(
        Arc::clone(&state.store),
        gate,
        route.user_id,
        route.playback_id.clone(),
        route.owner_node_id.clone(),
        route.owner_epoch,
    )
    // The ask this successor is being built for, recorded on the slot so an
    // acknowledgement arriving later is judged against the ask that is current
    // then rather than against the one that started the work.
    .asking(desired_digest.clone());
    let preparation = plurx_core::domain::MediaSessionPreparation {
        incarnation_id: staged_incarnation_id,
        session_id: staged_session_id,
        user_id: route.user_id,
        playback_id: route.playback_id.clone(),
        // Recorded now rather than read fresh at commit, so a lost CAS aborts
        // this successor and never reaps a newer player generation.
        expected_predecessor_incarnation_id: route.incarnation_id.clone(),
        expected_predecessor_owner_node_id: route.owner_node_id.clone(),
        expected_predecessor_owner_epoch: route.owner_epoch,
        // The successor's own intent, not the predecessor's: the
        // fingerprint encodes `kind` and `start_seconds`, both of which this
        // row deliberately changes.
        request_fingerprint: staged_request.durable_intent_fingerprint(route.user_id),
        owner_node_id: successor_owner.to_owned(),
        recipe_json,
        response_json,
        media_origin_ms: if staged_request.presentation == crate::transcode::Presentation::Vod {
            0
        } else {
            resume_ms
        },
        now_ms,
        // Read here rather than carried from the exchange that triggered this,
        // and re-compared inside the admission transaction. The awaits between
        // that exchange and this line — the source file read, the height
        // resolution — are exactly where a newer ask lands, and a value
        // captured before them would prove only that the ask had not changed
        // before the work started.
        expected_desired_revision: state
            .store
            .desired_selection(route.user_id, &route.playback_id)
            .await
            .ok()
            .flatten()
            .map(|desired| desired.revision),
        deadline_ms: now_ms.saturating_add(PREPARATION_DEADLINE_MS),
    };
    if prime_worker {
        if let PreparationPurpose::PlannedRelocation(fence) = purpose {
            if !state.serving.planned_outage_is_current(fence).await {
                crate::playback_control::record_preparation_staged(false);
                return;
            }
        }
        let active = ActivePreparedSuccessor {
            state: state.clone(),
            executor: executor.clone(),
            preparation: preparation.clone(),
            purpose,
            cancelled: tokio_util::sync::CancellationToken::new(),
        };
        // Test-only: see `preparation_registration_delays`. This is the only
        // way to put a supersession into the window the comment below names,
        // because in production that window contains no await at all.
        #[cfg(test)]
        if let Some(delay) = take_preparation_registration_delay(&preparation.playback_id) {
            tokio::time::sleep(delay).await;
        }
        // Publish cancellation ownership before the Store future is polled.
        // A wait or settings disable may then cancel an in-flight reservation;
        // the detached reservation owner reconciles a late commit exactly.
        register_active_preparation(active.clone());
        // Between this task's last await and the line above there is no
        // suspension point, so a supersession landing in that window is
        // invisible to the guard's own checks. Reading the flag once more here,
        // after the registry names the successor, is what closes it: from this
        // point on `cancel_preparations_for_superseded_predecessor` can see the
        // entry itself, and before it the guard could.
        if pending_candidate_superseded(&preparation.playback_id, desired_digest.as_deref()) {
            if let Some(active) = take_active_preparation(&preparation.incarnation_id) {
                spawn_cancelled_preparation(active, "predecessor superseded by a new session");
            }
            crate::playback_control::record_preparation_staged(false);
            return;
        }
        arm_preparation_foreground_watch(active.clone());
        let Some(reservation) = reserve_preparation_before(active.clone()).await else {
            crate::playback_control::record_preparation_staged(false);
            return;
        };
        if let PreparationPurpose::PlannedRelocation(fence) = purpose {
            if !state.serving.planned_outage_is_current(fence).await {
                crate::playback_control::record_preparation_staged(false);
                return;
            }
        }
        let prime_deadline = Instant::now() + PREPARATION_PRIME_BUDGET;
        let prime = async {
            if successor_owner == state.node_id {
                if staged_request.presentation == crate::transcode::Presentation::Live {
                    prime_live_prepared_session(
                        state,
                        &staged_recipe,
                        &preparation.session_id,
                        &route.recovery_epoch,
                        1,
                        prime_deadline.into(),
                    )
                    .await
                } else {
                    match state
                        .transcode
                        .session_adoption_token(&preparation.session_id)
                    {
                        Some(adoption) => {
                            state
                                .transcode
                                .vod_resurrect_before(
                                    &preparation.recipe_json,
                                    &preparation.session_id,
                                    preparation.user_id,
                                    adoption,
                                    prime_deadline,
                                    true,
                                )
                                .await
                        }
                        None => false,
                    }
                }
            } else {
                state
                    .media_sessions
                    .prepare_remote(
                        successor_owner,
                        &RemotePrepareRequest {
                            protocol_version: crate::media_pool::PROTOCOL_VERSION,
                            incarnation_id: preparation.incarnation_id.clone(),
                            session_id: preparation.session_id.clone(),
                            user_id: preparation.user_id,
                            expected_owner_epoch: 1,
                        },
                        prime_deadline.into(),
                    )
                    .await
                    .is_ok()
            }
        };
        let primed = tokio::select! {
            () = active.cancelled.cancelled() => false,
            primed = prime => primed,
        };
        if !primed {
            crate::playback_control::record_preparation_staged(false);
            return;
        }
        if let PreparationPurpose::PlannedRelocation(fence) = purpose {
            if !state.serving.planned_outage_is_current(fence).await {
                crate::playback_control::record_preparation_staged(false);
                return;
            }
        }
        let activated = tokio::select! {
            () = active.cancelled.cancelled() => None,
            activated = activate_preparation_before(reservation) => activated,
        };
        if let Some(guard) = activated {
            let active = guard.disarm();
            crate::playback_control::record_preparation_staged(true);
            arm_preparation_deadline(active);
        } else {
            crate::playback_control::record_preparation_staged(false);
        }
        return;
    }

    let staged = tokio::time::timeout(PREPARATION_STORE_BUDGET, executor.stage(&preparation)).await;
    match staged {
        Ok(Ok(true)) => {
            crate::playback_control::record_preparation_staged(true);
        }
        Ok(Ok(false)) | Ok(Err(_)) | Err(_) => {
            crate::playback_control::record_preparation_staged(false);
        }
    }
}

pub(super) async fn retire_prepared_worker(
    state: &AppState,
    owner_node_id: &str,
    incarnation_id: &str,
    session_id: &str,
    reason: &'static str,
) {
    if owner_node_id == state.node_id {
        state
            .transcode
            .begin_session_terminal(session_id, crate::vodserve::Terminal::Replaced, reason)
            .await;
        state.transcode.complete_session_release(session_id);
    } else if let Err(error) = state
        .media_sessions
        .abort_remote(
            owner_node_id,
            &RemoteAbortRequest {
                incarnation_id: incarnation_id.to_owned(),
                session_id: session_id.to_owned(),
                expected_owner_epoch: 1,
                reason: None,
            },
        )
        .await
    {
        tracing::warn!(?error, owner = %owner_node_id, %reason, "remote prepared worker cleanup did not settle");
    }
}

/// Free the actor slot, durable row, and exact worker when nobody claims the
/// successor before its fixed deadline. The registry transfer means a commit,
/// explicit abort, settings disable, or incumbent wait can take ownership
/// first; the timer then observes no entry and cannot retire the winner.
fn arm_preparation_deadline(active: ActivePreparedSuccessor) {
    let deadline_ms = active.preparation.deadline_ms.saturating_sub(unix_ms());
    let staged_incarnation_id = active.preparation.incarnation_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(deadline_ms.max(0) as u64)).await;
        if let Some(active) = take_active_preparation(&staged_incarnation_id) {
            settle_cancelled_preparation(active, "prepared successor expired").await;
        }
    });
}
