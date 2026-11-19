//! Processed Dolby Vision / HDR10-E as an upgrade after the first frame.
//!
//! Plan ruling 3 (Paul, 2026-11-07): processed output is never work before the
//! viewer's first frame. A create resolves the ordinary plan only; when that
//! plan could be processed it leaves an [`UpgradeIntent`] here, on the node
//! that owns the session, and so does the activation of a locally owned
//! ordinary prepared successor. The first proof that the viewer is presenting
//! the session takes the intent and runs one bounded trial: its `ttff`
//! beacon, an accepted rendering control exchange from its own generation
//! (the only proof for a resume or a stall recovery, which send no `ttff`),
//! or, for a prepared successor, its commit, which the client sends only
//! after showing the successor's frame. The trial is
//! [`crate::transcode::TranscodeManager::prepare_processed_upgrade`]. A
//! verified first window is staged as an M6 prepared successor
//! ([`super::preparation::stage_prepared_successor_as`]) and the control
//! exchange offers it like any other. Every refusal leaves the ordinary
//! session exactly as it was: nothing here touches it.
//!
//! Cancellation: a release of the session ([`cancel_for_session`]), a newer
//! session for the playback ([`cancel_for_playback`], from
//! `cancel_preparations_for_superseded_predecessor`), or a viewer's own
//! quality change, which takes the playback's one pending-candidate slot and
//! so cancels the upgrade's claim on it.

use super::*;

#[path = "processed_upgrade_qualification.rs"]
mod qualification;
#[cfg(test)]
pub(super) use qualification::exercise_check;
pub(super) use qualification::note_accepted_control;
#[cfg(test)]
pub(super) use qualification::tests::pending_qualification_fixture;

/// How long an intent waits for its first frame. A start whose picture has
/// not appeared by then is not one an upgrade should be spent on; the entry is
/// dropped at the next registration, not by a timer.
const FIRST_FRAME_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);
/// Bounds the registry against a node that starts many sessions no client
/// ever reports a frame for.
const MAX_INTENTS: usize = 512;

/// What the create knew that the upgrade needs, captured once, on the owner.
#[derive(Clone)]
pub(in crate::http) struct UpgradeIntent {
    pub(in crate::http) user_id: i64,
    pub(in crate::http) playback_id: String,
    pub(in crate::http) session_id: String,
    pub(in crate::http) incarnation_id: String,
    /// The ordinary request the session was started with, including its
    /// process-private candidate context (catalog query, canonical caps,
    /// planning binding), which no durable reconstruction can restore.
    pub(in crate::http) request: crate::transcode::SessionRequest,
    pub(in crate::http) hdr10_intent: bool,
    pub(in crate::http) auto_policy: bool,
    pub(in crate::http) network: Option<crate::telemetry::NetworkIdentity>,
    pub(in crate::http) network_prior: Option<plurx_core::domain::NetworkPrior>,
    pub(in crate::http) incumbent_receipt: Option<String>,
}

enum Entry {
    Waiting {
        intent: Box<UpgradeIntent>,
        registered: std::time::Instant,
    },
    AwaitingQualification(Box<qualification::AwaitingQualification>),
    Skipped {
        playback_id: String,
        report: ProcessingSkip,
        recorded: std::time::Instant,
    },
    Running {
        playback_id: String,
        incarnation_id: String,
        claim: u64,
        cancelled: tokio_util::sync::CancellationToken,
        report: Option<ProcessingSkip>,
    },
}

impl Entry {
    fn playback_and_incarnation(&self) -> (&str, &str) {
        match self {
            Self::Waiting { intent, .. } => (&intent.playback_id, &intent.incarnation_id),
            Self::AwaitingQualification(wait) => {
                (&wait.intent.playback_id, &wait.intent.incarnation_id)
            }
            Self::Skipped {
                playback_id,
                report,
                ..
            } => (playback_id, &report.generation),
            Self::Running {
                playback_id,
                incarnation_id,
                ..
            } => (playback_id, incarnation_id),
        }
    }
}

fn registry() -> &'static std::sync::Mutex<std::collections::HashMap<String, Entry>> {
    static REGISTRY: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Entry>>,
    > = std::sync::OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, Entry>> {
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// An explanation from the owner of this exact session generation. It is
/// informational: a refused optional upgrade never changes its ordinary delivery.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProcessingSkip {
    pub(in crate::http) generation: String,
    pub(in crate::http) reason: String,
}

impl ProcessingSkip {
    pub(crate) fn owner_header(&self) -> Option<String> {
        let json = serde_json::to_string(self).ok()?;
        let mut ascii = String::new();
        for ch in json.chars() {
            if ch.is_ascii() {
                ascii.push(ch);
            } else {
                for unit in ch.encode_utf16(&mut [0; 2]) {
                    ascii.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
        (ascii.len() <= 4096).then_some(ascii)
    }

    pub(crate) fn from_owner_wire(bytes: &[u8], generation: &str) -> Option<Self> {
        if bytes.len() > 4096 {
            return None;
        }
        let report: Self = serde_json::from_slice(bytes).ok()?;
        (report.generation == generation
            && !report.reason.trim().is_empty()
            && report.reason.chars().count() <= 512
            && !report.reason.chars().any(char::is_control))
        .then_some(report)
    }
}

pub(in crate::http) fn processing_skip(session: &str, generation: &str) -> Option<ProcessingSkip> {
    match lock().get(session) {
        Some(
            Entry::Skipped { report, .. }
            | Entry::Running {
                report: Some(report),
                ..
            },
        ) if report.generation == generation => Some(report.clone()),
        Some(Entry::AwaitingQualification(wait)) if wait.report.generation == generation => {
            Some(wait.report.clone())
        }
        _ => None,
    }
}

static NEXT_CLAIM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Leave an upgrade waiting for `intent.session_id`'s first frame.
pub(in crate::http) fn register(intent: UpgradeIntent) {
    let now = std::time::Instant::now();
    let mut entries = lock();
    entries.retain(|_, entry| match entry {
        Entry::Waiting { registered, .. } => now.duration_since(*registered) < FIRST_FRAME_WINDOW,
        Entry::AwaitingQualification(wait) => wait.recorded.elapsed() < qualification::RETENTION,
        Entry::Skipped { recorded, .. } => {
            now.duration_since(*recorded) < std::time::Duration::from_secs(86_400)
        }
        Entry::Running { .. } => true,
    });
    if entries.len() >= MAX_INTENTS {
        return;
    }
    entries.insert(
        intent.session_id.clone(),
        Entry::Waiting {
            intent: Box::new(intent),
            registered: now,
        },
    );
}

/// A first frame was reported for `session_id` by `user_id`. Returns the
/// intent to run, once: the entry becomes `Running` and later frames (a seek's
/// picture, a stall's recovery) find nothing to take.
pub(in crate::http) fn take_on_first_frame(
    user_id: i64,
    session_id: &str,
) -> Option<(Box<UpgradeIntent>, UpgradeRun)> {
    take_waiting(user_id, session_id, None)
}

/// A rendering control exchange for `incarnation_id` was accepted on this
/// node: the viewer is presenting that session's frames. Like
/// [`take_on_first_frame`], but fenced to the generation the exchange named,
/// so a stale exchange cannot start the upgrade of a later incarnation.
pub(in crate::http) fn take_on_rendering(
    user_id: i64,
    session_id: &str,
    incarnation_id: &str,
) -> Option<(Box<UpgradeIntent>, UpgradeRun)> {
    // Another preparation holds the playback's slot: leave the upgrade
    // waiting for a later exchange rather than spend it against that slot.
    if matches!(lock().get(session_id), Some(Entry::Waiting { intent, .. })
        if super::preparation::preparation_slot_held(&intent.playback_id))
    {
        return None;
    }
    take_waiting(user_id, session_id, Some(incarnation_id))
}

fn take_waiting(
    user_id: i64,
    session_id: &str,
    incarnation_id: Option<&str>,
) -> Option<(Box<UpgradeIntent>, UpgradeRun)> {
    let mut entries = lock();
    let Some(Entry::Waiting { intent, registered }) = entries.get(session_id) else {
        return None;
    };
    if intent.user_id != user_id
        || incarnation_id.is_some_and(|incarnation| incarnation != intent.incarnation_id)
    {
        return None;
    }
    if registered.elapsed() >= FIRST_FRAME_WINDOW {
        entries.remove(session_id);
        return None;
    }
    let Some(Entry::Waiting { intent, .. }) = entries.remove(session_id) else {
        return None;
    };
    let claim = NEXT_CLAIM.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let cancelled = tokio_util::sync::CancellationToken::new();
    entries.insert(
        session_id.to_owned(),
        Entry::Running {
            playback_id: intent.playback_id.clone(),
            incarnation_id: intent.incarnation_id.clone(),
            claim,
            cancelled: cancelled.clone(),
            report: None,
        },
    );
    Some((
        intent,
        UpgradeRun {
            session_id: session_id.to_owned(),
            claim,
            cancelled,
            presented_at: std::time::Instant::now(),
            expected_owner_epoch: None,
        },
    ))
}

/// The running upgrade's own claim on its entry. Dropping it (the task ended,
/// however it ended) removes the entry only if it is still this run's.
pub(in crate::http) struct UpgradeRun {
    session_id: String,
    claim: u64,
    cancelled: tokio_util::sync::CancellationToken,
    /// When the picture this run answers was reported. Before the first
    /// control exchange is accepted, the film has played from the session's
    /// start for this long; see [`viewer_film_time_ms`].
    presented_at: std::time::Instant,
    expected_owner_epoch: Option<i64>,
}

impl UpgradeRun {
    pub(in crate::http) fn cancelled(&self) -> &tokio_util::sync::CancellationToken {
        &self.cancelled
    }

    /// Whether `outcome` only lost the playback's preparation slot to another
    /// preparation: the run itself (its session's release or supersession)
    /// was not cancelled, so the session may still be upgraded.
    fn displaced(&self, outcome: &str) -> bool {
        (outcome == "cancelled" || outcome == SLOT_HELD) && !self.cancelled.is_cancelled()
    }

    /// Only displacement is retryable. A real staging refusal remains a
    /// terminal skip even if another task cancels the token after that refusal.
    pub(super) fn finish_staging(
        &self,
        staged: super::preparation::PreparationStageOutcome,
    ) -> &'static str {
        use super::preparation::PreparationStageOutcome;
        match staged {
            PreparationStageOutcome::Staged => "staged",
            PreparationStageOutcome::Cancelled => "cancelled",
            PreparationStageOutcome::Refused(_) => {
                self.skipped(
                    "the verified reconstruction could not be staged for the current playback",
                );
                "not_staged"
            }
        }
    }

    /// Put the upgrade back to wait for the session's next rendering exchange,
    /// if the entry is still this run's.
    fn wait_again(&self, intent: Box<UpgradeIntent>) {
        let mut entries = lock();
        if matches!(entries.get(&self.session_id),
            Some(Entry::Running { claim, report: None, .. }) if *claim == self.claim)
        {
            entries.insert(
                self.session_id.clone(),
                Entry::Waiting {
                    intent,
                    registered: std::time::Instant::now(),
                },
            );
        }
    }

    fn skipped(&self, reason: &str) {
        let reason: String = reason
            .chars()
            .filter(|c| !c.is_control())
            .take(512)
            .collect();
        if reason.trim().is_empty() || self.cancelled.is_cancelled() {
            return;
        }
        let mut entries = lock();
        let Some(Entry::Running {
            playback_id,
            incarnation_id,
            claim,
            ..
        }) = entries.get(&self.session_id)
        else {
            return;
        };
        if *claim != self.claim {
            return;
        }
        let (playback_id, generation) = (playback_id.clone(), incarnation_id.clone());
        entries.insert(
            self.session_id.clone(),
            Entry::Skipped {
                playback_id,
                report: ProcessingSkip { generation, reason },
                recorded: std::time::Instant::now(),
            },
        );
    }
}

impl Drop for UpgradeRun {
    fn drop(&mut self) {
        let mut entries = lock();
        if matches!(entries.get(&self.session_id),
            Some(Entry::Running { claim, .. }) if *claim == self.claim)
        {
            if let Some(Entry::Running {
                playback_id,
                report: Some(report),
                ..
            }) = entries.remove(&self.session_id)
            {
                entries.insert(
                    self.session_id.clone(),
                    Entry::Skipped {
                        playback_id,
                        report,
                        recorded: std::time::Instant::now(),
                    },
                );
            }
        }
    }
}

fn cancel_entry(entry: Entry) {
    match entry {
        Entry::Running { cancelled, .. } => cancelled.cancel(),
        Entry::AwaitingQualification(wait) => wait.cancelled.cancel(),
        _ => {}
    }
}

/// The session was released: no upgrade waits for it or keeps running for it.
pub(in crate::http) fn cancel_for_session(session_id: &str) {
    let entry = lock().remove(session_id);
    if let Some(entry) = entry {
        cancel_entry(entry);
    }
}

/// A new session activated for `playback_id`. Every upgrade for an older
/// session of that playback is superseded; `keep` is the activating
/// incarnation, whose own intent (registered after this) is not touched.
pub(in crate::http) fn cancel_for_playback(playback_id: &str, keep: Option<&str>) {
    let superseded = {
        let mut entries = lock();
        let sessions: Vec<String> = entries
            .iter()
            .filter(|(_, entry)| {
                let (playback, incarnation) = entry.playback_and_incarnation();
                playback == playback_id && keep != Some(incarnation)
            })
            .map(|(session, _)| session.clone())
            .collect();
        sessions
            .into_iter()
            .filter_map(|session| entries.remove(&session))
            .collect::<Vec<_>>()
    };
    superseded.into_iter().for_each(cancel_entry);
}

#[cfg(test)]
pub(in crate::http) fn is_waiting(session_id: &str) -> bool {
    matches!(lock().get(session_id), Some(Entry::Waiting { .. }))
}

#[cfg(test)]
pub(in crate::http) fn is_running(session_id: &str) -> bool {
    matches!(lock().get(session_id), Some(Entry::Running { .. }))
}

/// The client-log seam: a `ttff` beacon naming a session on this node starts
/// that session's upgrade, if one is waiting.
pub(in crate::http) fn note_first_frame(state: &AppState, user_id: i64, session_id: &str) {
    let Some((intent, run)) = take_on_first_frame(user_id, session_id) else {
        return;
    };
    start_upgrade(state, intent, run);
}

/// The control-exchange seam: an accepted exchange from the session's own
/// generation, with active demand and a rendering picture, proves the viewer
/// is presenting this session. The `ttff` beacon is a start measurement and
/// is never sent for a resume, a seek, or a stall recovery, so a session that
/// began any of those ways would otherwise never be offered its upgrade.
pub(in crate::http) fn note_rendering(state: &AppState, route: &MediaSessionRoute) {
    if route.owner_node_id != state.node_id || route.state != "active" {
        return;
    }
    // A viewer still waiting on its title's measurement keeps that scan's
    // live demand while it is watching.
    qualification::hold_viewer_demand(state, &route.session_id, &route.incarnation_id);
    let Some(user_id) = route.principal.local_user_id() else {
        return;
    };
    let Some((intent, run)) = take_on_rendering(user_id, &route.session_id, &route.incarnation_id)
    else {
        return;
    };
    start_upgrade(state, intent, run);
}

/// A prepared successor this node owns was committed for `user_id`: the
/// client commits only after it has shown the successor's own frame, so the
/// commit is that session's first presented frame, and the upgrade the
/// successor's activation left waiting starts here
/// ([`super::preparation::stage_prepared_successor_as`]). VOD sessions keep no
/// presentation phase of their own; the commit is their presented proof.
pub(in crate::http) fn note_presented(state: &AppState, user_id: i64, session_id: &str) {
    note_first_frame(state, user_id, session_id);
}

/// Run one taken upgrade to its outcome on a task of its own.
///
/// The run is awaited to completion, never raced against its cancellation: a
/// future dropped between the successor's registration and its deadline arm
/// would strand that successor. Cancellation instead reaches every phase
/// through the playback slot's token, a child of the run's
/// ([`super::preparation::begin_processed_upgrade`]): the trial races it, and
/// the staged successor registers under it and exits on it by the same
/// cleanup an ordinary preparation's cancellation takes.
fn start_upgrade(state: &AppState, intent: Box<UpgradeIntent>, run: UpgradeRun) {
    start_upgrade_claim(state, intent, run, None);
}

fn start_upgrade_claim(
    state: &AppState,
    intent: Box<UpgradeIntent>,
    run: UpgradeRun,
    pending: Option<super::preparation::PendingCandidateGuard>,
) {
    crate::playback_control::record_processed_upgrade("scheduled");
    let state = state.clone();
    tokio::spawn(async move {
        let outcome = run_upgrade(&state, &intent, &run, pending).await;
        finish_upgrade_run(intent, run, outcome);
    });
}

/// Finish through the same displacement lifecycle regardless of whether the
/// pending claim was cancelled before the trial or inside successor staging.
pub(super) fn finish_upgrade_run(
    intent: Box<UpgradeIntent>,
    run: UpgradeRun,
    mut outcome: &'static str,
) -> &'static str {
    if run.displaced(outcome) {
        outcome = "displaced";
        run.wait_again(intent);
    }
    crate::playback_control::record_processed_upgrade(outcome);
    drop(run);
    outcome
}

/// The run found another preparation holding the playback's slot before it
/// claimed it; [`start_upgrade_claim`] puts the upgrade back to wait.
const SLOT_HELD: &str = "slot_held";

/// What the trial step answers; see [`run_upgrade_with`].
pub(in crate::http) type TrialOutcome =
    Result<Option<crate::transcode::ProcessedUpgradeTrial>, String>;

/// One upgrade, start to staged. Returns its outcome label.
async fn run_upgrade(
    state: &AppState,
    intent: &UpgradeIntent,
    run: &UpgradeRun,
    pending: Option<super::preparation::PendingCandidateGuard>,
) -> &'static str {
    run_upgrade_with_claim(
        state,
        intent,
        run,
        pending,
        |request, recovery| async move {
            state
                .transcode
                .prepare_processed_upgrade(
                    &intent.session_id,
                    &request,
                    &recovery,
                    intent.hdr10_intent,
                )
                .await
        },
        |incarnation, needed| async move {
            state
                .transcode
                .claim_incumbent_permit(&intent.session_id, &incarnation, needed)
                .await
        },
    )
    .await
}

/// A film position bounded by the film: a successor may not begin past its
/// end, and never before its start.
fn bounded_film_ms(ms: i64, duration_ms: Option<i64>) -> i64 {
    match duration_ms {
        Some(duration) if duration > 0 => ms.clamp(0, duration),
        _ => ms.max(0),
    }
}

/// Where the viewer of the upgraded session is now, on the source timeline.
///
/// The successor has to begin where the viewer is, as an ordinary prepared
/// successor does from its control ask's accepted position — not at the
/// create's `start_seconds`, which a viewer reaches the first frame of and
/// then leaves behind for as long as the trial and the staging take. This is
/// the predecessor's latest accepted demand advanced by its age, the playhead
/// estimate the producers already read
/// ([`crate::playback_control::rolling_estimated_position_ms`]). Before any
/// exchange has been accepted, the first frame was at the start position and
/// the film has played since.
pub(in crate::http) async fn viewer_film_time_ms(
    state: &AppState,
    intent: &UpgradeIntent,
    run: &UpgradeRun,
) -> i64 {
    if let Some(estimated) = state
        .transcode
        .session_playhead_estimate_ms(&intent.session_id)
        .await
    {
        return estimated.max(0);
    }
    let start_ms = (intent.request.start_seconds.max(0.0) * 1_000.0).round() as i64;
    let played_ms = i64::try_from(run.presented_at.elapsed().as_millis()).unwrap_or(i64::MAX);
    start_ms.saturating_add(played_ms)
}

/// The processed candidate this upgrade's trial would produce, when this node
/// has already described it for the session's catalog query: a processed row
/// of the ordinary catalog, on this node, at the height the trial renders.
/// Its recipe digest and bitrate are what recovery and link evidence are kept
/// under, so admission can refuse it before the trial spends the GPU.
pub(in crate::http) fn projected_processed_candidate(
    state: &AppState,
    intent: &UpgradeIntent,
    source: &plurx_core::domain::MediaFile,
) -> Option<plurx_core::playback::candidate::QualityCandidate> {
    let ordinary = intent.request.candidate_context.as_ref()?;
    let source_height = source.height.filter(|height| *height > 0)?;
    let height = source_height.min(i64::from(ordinary.selected_candidate.target_height)) & !1;
    intent
        .request
        .quality_catalog
        .as_deref()?
        .candidates
        .iter()
        .find(|entry| {
            entry.node_id == state.node_id
                && entry.candidate.processed_dv
                && i64::from(entry.candidate.target_height) == height
        })
        .map(|entry| entry.candidate.clone())
}

/// How far ahead of the viewer a processed upgrade's successor may begin.
/// The incumbent keeps playing until it reaches the successor's start, and
/// both pipelines run meanwhile: the Apple client plans its rendezvous within
/// its overlap's commit lead (`PreparedCommitRendezvous.planLoaded`, at most
/// 8 s and less as priming spends the overlap), and Android within its 12 s
/// overlap bound. Four seconds at staging leaves both room after priming.
const SUCCESSOR_MAX_HEAD_START_MS: i64 = 4_000;

/// A future prepared entry is met at its actual start. If it is still too
/// far ahead, schedule staging later, before the client's overlap starts;
/// clamping backward would put the producer on unprepared media again.
/// A viewer already past the entry is never sent backward.
pub(in crate::http) fn successor_start_ms(viewer_ms: i64, verified_ms: i64) -> Option<i64> {
    (verified_ms <= viewer_ms.saturating_add(SUCCESSOR_MAX_HEAD_START_MS))
        .then_some(verified_ms.max(viewer_ms))
}

pub(super) async fn await_successor_start(
    state: &AppState,
    intent: &UpgradeIntent,
    run: &UpgradeRun,
    pending: &super::preparation::PendingCandidateGuard,
    prepared_window: Option<&std::ops::Range<i64>>,
    duration_ms: Option<i64>,
) -> Result<(i64, bool), &'static str> {
    let cancelled = pending.cancellation_token();
    let mut waited = false;
    loop {
        if cancelled.is_cancelled() {
            return Err("cancelled");
        }
        if state
            .transcode
            .session_preparation_gate(&intent.session_id)
            .await
            .is_none()
        {
            return Err("not_applicable");
        }
        let viewer = bounded_film_ms(viewer_film_time_ms(state, intent, run).await, duration_ms);
        let Some(window) = prepared_window else {
            // A reused producer owns its live coverage; there is no new
            // private trial window to wait for or to claim as cached media.
            return Ok((viewer, waited));
        };
        if let Some(start) = successor_start_ms(viewer, window.start) {
            return Ok((start, waited));
        }
        waited = true;
        // This schedules a handoff, not a recovery timer. A paused incumbent
        // retains the admitted root until it resumes, is superseded, or its
        // session ends; the existing cancellation/instance bounds still own it.
        tokio::select! {
            () = cancelled.cancelled() => return Err("cancelled"),
            () = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
        }
    }
}

/// [`run_upgrade`] with its trial supplied: production passes
/// `prepare_processed_upgrade`, and tests pass a trial of their own, so the
/// upgrade's decisions around the trial are exercised without a renderer.
/// The trial is given the request to verify (positioned at the viewer) and
/// the identity of the successor it is verified for.
#[cfg(test)]
pub(in crate::http) async fn run_upgrade_with<F, Fut>(
    state: &AppState,
    intent: &UpgradeIntent,
    run: &UpgradeRun,
    run_trial: F,
) -> &'static str
where
    F: FnMut(crate::transcode::SessionRequest, crate::transcode::SessionRecoveryIdentity) -> Fut,
    Fut: std::future::Future<Output = TrialOutcome>,
{
    run_upgrade_with_claim(
        state,
        intent,
        run,
        None,
        run_trial,
        |incarnation, needed| async move {
            state
                .transcode
                .claim_incumbent_permit(&intent.session_id, &incarnation, needed)
                .await
        },
    )
    .await
}

/// [`run_upgrade_with`] with the incumbent claim supplied too, so the
/// refusal → claim → retry path is exercised without a vod incumbent.
#[cfg(test)]
pub(in crate::http) async fn run_upgrade_with_incumbent<F, Fut, C, CFut>(
    state: &AppState,
    intent: &UpgradeIntent,
    run: &UpgradeRun,
    run_trial: F,
    claim_incumbent: C,
) -> &'static str
where
    F: FnMut(crate::transcode::SessionRequest, crate::transcode::SessionRecoveryIdentity) -> Fut,
    Fut: std::future::Future<Output = TrialOutcome>,
    C: FnOnce(String, crate::admission::TranscodeResourceEstimate) -> CFut,
    CFut: std::future::Future<Output = Option<crate::vodserve::IncumbentPermitClaim>>,
{
    run_upgrade_with_claim(state, intent, run, None, run_trial, claim_incumbent).await
}

async fn run_upgrade_with_claim<F, Fut, C, CFut>(
    state: &AppState,
    intent: &UpgradeIntent,
    run: &UpgradeRun,
    pending: Option<super::preparation::PendingCandidateGuard>,
    mut run_trial: F,
    claim_incumbent: C,
) -> &'static str
where
    F: FnMut(crate::transcode::SessionRequest, crate::transcode::SessionRecoveryIdentity) -> Fut,
    Fut: std::future::Future<Output = TrialOutcome>,
    C: FnOnce(String, crate::admission::TranscodeResourceEstimate) -> CFut,
    CFut: std::future::Future<Output = Option<crate::vodserve::IncumbentPermitClaim>>,
{
    let session = crate::transcode::session_log_id(&intent.session_id);
    // One preparation slot per playback: a viewer's own change in flight wins,
    // and a viewer's change arriving during the trial cancels this one.
    let Some(pending) = pending.or_else(|| {
        super::preparation::begin_processed_upgrade(&intent.playback_id, run.cancelled())
    }) else {
        if run.cancelled().is_cancelled() {
            return "cancelled";
        }
        tracing::info!(target: "plurxd::http::hls", session = %session,
            "processed upgrade waits: another preparation holds the playback's slot");
        return SLOT_HELD;
    };
    if pending.cancelled() {
        return "cancelled";
    }
    let route = match state
        .store
        .media_session_route_by_incarnation(&intent.incarnation_id)
        .await
    {
        Ok(Some(route))
            if route.session_id == intent.session_id
                && route.owner_node_id == state.node_id
                && route.state == "active"
                && run
                    .expected_owner_epoch
                    .is_none_or(|epoch| route.owner_epoch == epoch) =>
        {
            route
        }
        _ => return "not_applicable",
    };
    let Ok(predecessor) = serde_json::from_str::<RemoteStartRequest>(&route.recipe_json) else {
        return "not_applicable";
    };
    if state
        .transcode
        .session_preparation_gate(&intent.session_id)
        .await
        .is_none()
    {
        return "not_applicable";
    }
    let Ok(Some(source)) = state.store.get_file(intent.request.file_id).await else {
        return "not_applicable";
    };
    // Admission before the GPU (review 3 S6): when this node has already
    // described the processed recipe for this query, the recovery and link
    // evidence kept under its digest and bitrate decides now, and a refused
    // recipe costs no trial; its production is proven by the trial itself.
    // A read that misses its deadline is not a refusal: the trial runs
    // unproven and the check after it is the admission. Without a description
    // there is no evidence about the recipe yet (recovery and link memory are
    // kept under its digest, which only exists once it has been rendered), and
    // the check after the trial is the admission.
    let projected = projected_processed_candidate(state, intent, &source);
    let mut proven_before = true;
    if let Some(projected) = projected.as_ref() {
        match admit_processed_candidate(
            state,
            intent,
            &source,
            &route,
            projected,
            AdmissionStage::BeforeTrial,
        )
        .await
        {
            Ok(Admission::Proven) => {}
            Ok(Admission::Unproven) => {
                tracing::info!(target: "plurxd::http::hls", session = %session,
                    "processed upgrade trial runs without readable link memory; the admission after it decides");
                proven_before = false;
            }
            Err(reason) => {
                tracing::warn!(target: "plurxd::http::hls", session = %session, %reason,
                    "processed upgrade not admitted before its trial; the ordinary session keeps playing");
                run.skipped(&reason);
                return "admission_refused";
            }
        }
    }
    let ids = super::preparation::StagedSuccessorIds::mint();
    let recovery = crate::transcode::SessionRecoveryIdentity {
        principal: route.principal.clone(),
        incarnation_id: ids.incarnation_id.clone(),
        recovery_epoch: route.recovery_epoch.clone(),
    };
    // The trial verifies its first window where the viewer will be when it
    // ends: at most its own budget past where the viewer is now. The
    // successor begins at that window (see `successor_start_ms` below), so
    // its producer starts from media the trial already produced.
    let trial_budget_ms =
        i64::try_from(crate::transcode::PROCESSED_UPGRADE_TRIAL_BUDGET.as_millis())
            .unwrap_or(i64::MAX);
    let verified_ms = bounded_film_ms(
        viewer_film_time_ms(state, intent, run)
            .await
            .saturating_add(trial_budget_ms),
        source.duration_ms,
    );
    #[cfg(target_os = "linux")]
    let peer_budget = crate::media_pool::CreateStartupBudget::new_with_clock(
        trial_budget_ms as u64,
        &state.membership.clock_guard(),
    );
    let mut trial_request = intent.request.clone();
    trial_request.start_seconds = verified_ms as f64 / 1_000.0;
    let superseded = pending.cancellation_token();
    let admission = Arc::new(crate::vodencode::ProcessedTrialAdmission::default());
    let trial = tokio::select! {
        () = superseded.cancelled() => return "cancelled",
        trial = crate::vodencode::PROCESSED_TRIAL_ADMISSION
            .scope(Arc::clone(&admission), run_trial(trial_request, recovery.clone())) => trial,
    };
    // Refused for capacity this viewer's own incumbent holds: claim that
    // permit through the predecessor-yield handoff, and only once it is in
    // hand run the trial again, so the wait never comes out of the window's
    // own budget. Any other refusal, or an incumbent that cannot yield
    // (another viewer reads it, no runway, it would still not fit), keeps
    // today's decline.
    let mut incumbent_claim = None;
    let trial = match (trial, admission.needed()) {
        (Err(reason), Some(needed)) => {
            match claim_incumbent(ids.incarnation_id.clone(), needed).await {
                None => Err(reason),
                Some(claim) => {
                    let granted = tokio::select! {
                        () = superseded.cancelled() => return "cancelled",
                        token = claim.claim().wait_ready(
                            tokio::time::Instant::now() + crate::admission::QUEUE_WAIT) => token,
                    };
                    if granted.is_none() {
                        Err(reason)
                    } else {
                        let verified_ms = bounded_film_ms(
                            viewer_film_time_ms(state, intent, run)
                                .await
                                .saturating_add(trial_budget_ms),
                            source.duration_ms,
                        );
                        let mut retry = intent.request.clone();
                        retry.start_seconds = verified_ms as f64 / 1_000.0;
                        let scoped =
                            Arc::new(crate::vodencode::ProcessedTrialAdmission::with_claim(
                                Arc::clone(claim.claim()),
                            ));
                        // A claim closed mid-trial (a seek past the runway)
                        // means the incumbent must produce now: the trial is
                        // dropped at once rather than holding its capacity
                        // to its own deadline.
                        let trial = tokio::select! {
                            () = superseded.cancelled() => return "cancelled",
                            () = claim.claim().closed() => {
                                run.skipped(INCUMBENT_RUNWAY_SPENT);
                                return "declined";
                            }
                            trial = crate::vodencode::PROCESSED_TRIAL_ADMISSION
                                .scope(scoped, run_trial(retry, recovery.clone())) => trial,
                        };
                        incumbent_claim = Some(claim);
                        trial
                    }
                }
            }
        }
        (trial, _) => trial,
    };
    let trial = match trial {
        Ok(Some(trial)) => trial,
        Ok(None) => return "not_applicable",
        Err(reason) => {
            if let Some(wait) = crate::transcode::pending_fel_qualification(&reason) {
                if qualification::retain(intent, run, wait) {
                    qualification::hold_viewer_demand(
                        state,
                        &intent.session_id,
                        &intent.incarnation_id,
                    );
                    return "qualification_pending";
                }
                if run.cancelled().is_cancelled() {
                    return "cancelled";
                }
                run.skipped("the current reconstruction recipe no longer matches its completed qualification");
                return "declined";
            }
            #[cfg(target_os = "linux")]
            if intent.auto_policy
                && crate::transcode::vod_refusal(&reason).is_some_and(|(code, _)| {
                    code == crate::media_sessions::ADAPTIVE_PROCESSED_CAPACITY_REFUSED
                })
            {
                // The remote successor owns no local yield. Return any
                // parked producer to the incumbent before considering a peer.
                drop(incumbent_claim.take());
                let remote = peer_budget
                    .scope(stage_remote_fel_after_capacity_refusal(
                        state,
                        intent,
                        run,
                        &pending,
                        &route,
                        &predecessor,
                        &source,
                        projected.as_ref(),
                    ))
                    .await;
                match remote {
                    Ok(outcome) => return outcome,
                    Err(error) => {
                        tracing::info!(target: "plurxd::http::hls", error = %error,
                            "same-quality FEL peer did not stage a successor");
                    }
                }
            }
            tracing::warn!(target: "plurxd::http::hls", session = %session, %reason,
                "processed upgrade declined; the ordinary session keeps playing");
            run.skipped(&reason);
            return "declined";
        }
    };
    if pending.cancelled() {
        return "cancelled";
    }
    // The trial spent up to its budget; the session it upgrades may have
    // ended meanwhile, and a successor for nothing is not staged.
    if state
        .transcode
        .session_preparation_gate(&intent.session_id)
        .await
        .is_none()
    {
        return "not_applicable";
    }
    let crate::transcode::ProcessedUpgradeTrial {
        request: mut processed,
        candidate,
        root,
        prepared_window,
    } = trial;
    // A parked incumbent keeps only its published runway. When that no
    // longer covers a failed hand-back (or the viewer seeks past it), the
    // claim closes and this upgrade gives back what it took, now.
    let claim_closed = async {
        match incumbent_claim.as_ref() {
            Some(claim) => claim.claim().closed().await,
            None => std::future::pending().await,
        }
    };
    let (film_time_ms, waited) = match tokio::select! {
        waited = await_successor_start(
            state,
            intent,
            run,
            &pending,
            prepared_window.as_ref(),
            source.duration_ms,
        ) => waited,
        () = claim_closed => {
            run.skipped(INCUMBENT_RUNWAY_SPENT);
            return "declined";
        }
    } {
        Ok(position) => position,
        Err(outcome) => return outcome,
    };
    // The recipe the trial actually froze is admitted unless it is exactly
    // the one admitted before the trial: same digest, same bitrate.
    if waited || post_trial_admission_needed(proven_before, projected.as_ref(), &candidate) {
        if let Err(reason) = admit_processed_candidate(
            state,
            intent,
            &source,
            &route,
            &candidate,
            AdmissionStage::AfterTrial,
        )
        .await
        {
            tracing::warn!(target: "plurxd::http::hls", session = %session, %reason,
                "processed upgrade not admitted; the ordinary session keeps playing");
            run.skipped(&reason);
            return "admission_refused";
        }
    }
    if let Err(reason) = trial_proved_production(&candidate) {
        tracing::warn!(target: "plurxd::http::hls", session = %session, %reason,
            "processed upgrade not admitted; the ordinary session keeps playing");
        run.skipped(reason);
        return "admission_refused";
    }
    // Admission may await Store reads. Keep its elapsed time out of the
    // successor's start without losing the future prepared-entry floor.
    let film_time_ms = film_time_ms.max(bounded_film_ms(
        viewer_film_time_ms(state, intent, run).await,
        source.duration_ms,
    ));
    // The actual prepared entry, not the requested trial timestamp, decides
    // the start. If playback has already passed that entry, current media must
    // be produced instead; never replay old film to consume a stale window.
    let prepared_window_reused = prepared_window
        .as_ref()
        .is_some_and(|window| window.contains(&film_time_ms));
    if let Some(ordinary) = processed
        .candidate_context
        .as_mut()
        .and_then(|context| context.ordinary_request.as_mut())
    {
        ordinary.start_seconds = film_time_ms as f64 / 1_000.0;
    }
    let Some(context) = processed.candidate_context.as_deref().cloned() else {
        run.skipped("the verified processing recipe has no current candidate context");
        return "declined";
    };
    if let Some(ordinary) = intent.request.quality_catalog.as_deref() {
        let mut catalog = ordinary.clone();
        let upserted = super::create::upsert_processed_worker_candidate(
            &mut catalog,
            crate::media_pool::WorkerQualityCandidate {
                node_id: state.node_id.clone(),
                candidate: candidate.clone(),
                binding: context.planning_binding.clone(),
                partial: false,
                dispatch_supported: true,
            },
        );
        if let Err(reason) = upserted {
            tracing::warn!(target: "plurxd::http::hls", session = %session, reason,
                "processed upgrade not admitted: the quality catalog refused the candidate");
            run.skipped(reason);
            return "admission_refused";
        }
        processed.quality_catalog = Some(Arc::new(catalog));
    }
    state.transcode.remember_processed_candidate(&processed);
    // Capture the admitted output plan while this exact verified root is
    // still in hand. The hold and staging share the minted incarnation below;
    // candidate descriptions and requested destinations do not author delivery.
    let delivery = super::preparation::StagedProcessingDelivery {
        dolby_vision_profile: root.encoding.preserves_processed_dv().then_some(8),
        fel: root.encoding.processed_fel(),
    };
    // Held until staging returns: the successor's first preparation takes it
    // by its incarnation, and whatever it did not take is freed on drop.
    let _root = state
        .transcode
        .hold_processed_upgrade_root(&ids.incarnation_id, context, root);
    if incumbent_claim
        .as_ref()
        .is_some_and(|claim| claim.claim().is_closed())
    {
        run.skipped(INCUMBENT_RUNWAY_SPENT);
        return "declined";
    }
    let primes = state.hls_route_hooks.get().primes_prepared_successor();
    let staged = super::preparation::stage_prepared_successor_with_delivery_as(
        state,
        &intent.session_id,
        &route,
        &predecessor,
        &processed,
        Some(&source),
        &state.node_id,
        super::preparation::PreparationPurpose::SelectionChange,
        super::preparation::AcceptedAsk {
            prepared_proof: None,
            film_time_ms,
            desired_digest: None,
            planning_cancellation: Some(pending.cancellation_token()),
            planning_registration: Some(pending.registration()),
            quality_intent: None,
        },
        primes,
        ids,
        Some(delivery),
        None,
    )
    .await;
    if staged == super::preparation::PreparationStageOutcome::Staged {
        tracing::info!(target: "plurxd::http::hls", session = %session,
            film_time_ms, prepared_window_reused,
            incumbent_yielded = incumbent_claim.is_some(),
            "processed upgrade staged as a prepared successor");
        // The incumbent stays parked under the staged slot's binding, which
        // commit, abort and rejection already settle.
        if let Some(claim) = incumbent_claim.take() {
            claim.hand_to_staging();
        }
    }
    run.finish_staging(staged)
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
async fn stage_remote_fel_after_capacity_refusal(
    state: &AppState,
    intent: &UpgradeIntent,
    run: &UpgradeRun,
    pending: &super::preparation::PendingCandidateGuard,
    route: &MediaSessionRoute,
    predecessor: &RemoteStartRequest,
    source: &MediaFile,
    projected: Option<&plurx_core::playback::candidate::QualityCandidate>,
) -> Result<&'static str, String> {
    use crate::media_sessions::AdaptivePreparePurpose;
    use plurx_core::transcode::dv_processing::DvDestination;

    let deadline = crate::media_pool::create_preparation_deadline()
        .ok_or("the original deferred FEL preparation cutoff is absent")?;
    crate::media_pool::create_remote_grant(deadline).map_err(|error| error.to_string())?;
    let owner = state
        .media_pool
        .adaptive_fel_workers(state, true)
        .await
        .into_iter()
        .next()
        .ok_or("no eligible worker advertises deferred FEL preparation")?;
    let quote = state
        .media_pool
        .adaptive_worker_quote(state, &owner)
        .await
        .ok_or("deferred FEL worker quote is no longer current")?;
    let planning = state
        .store
        .playback_planning_snapshot(source.id, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await
        .map_err(|error| error.to_string())?
        .ok_or("deferred FEL source planning is unavailable")?;
    let original = intent
        .request
        .candidate_context
        .as_deref()
        .ok_or("the deferred FEL request has no selection context")?;
    let incumbent = predecessor
        .candidate_catalog
        .as_ref()
        .filter(|catalog| {
            !catalog.candidate.processed_dv
                && predecessor.candidate_id == Some(catalog.candidate.id)
                && catalog.candidate.id == original.candidate_id
                && catalog.candidate.recipe_digest == original.recipe_digest
        })
        .ok_or("the current incumbent has no matching ordinary candidate")?;
    let requested = projected.unwrap_or(&incumbent.candidate);
    let source_height = source
        .height
        .filter(|height| *height > 0)
        .ok_or("the Profile 7 source has no known picture height")?;
    let height = source_height.min(i64::from(incumbent.candidate.target_height)) & !1;
    if height < 32 || (requested.processed_dv && i64::from(requested.target_height) != height) {
        return Err("the original deferred FEL raster is no longer current".into());
    }
    let query = original
        .catalog_query
        .as_ref()
        .and_then(|query| {
            crate::transcode::TranscodeManager::processed_catalog_query(&planning, query)
        })
        .ok_or("the deferred FEL audio or source contract changed")?;
    if crate::transcode::TranscodeManager::processed_catalog_destination(&planning, &query)
        != Some(DvDestination::Profile81)
        || pending.cancelled()
        || tokio::time::Instant::now() >= deadline
    {
        return Err("the deferred FEL source, settings or original cutoff changed".into());
    }
    let viewer_ms = viewer_film_time_ms(state, intent, run).await;
    let remaining_ms = i64::try_from(
        deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .as_millis(),
    )
    .unwrap_or(i64::MAX);
    let requested_ms = bounded_film_ms(viewer_ms.saturating_add(remaining_ms), source.duration_ms);
    let root_id = uuid::Uuid::new_v4().to_string();
    let claim_id = format!("deferred-fel:{root_id}");
    let mut request = intent.request.clone();
    request.start_seconds = requested_ms as f64 / 1000.0;
    request.kind = crate::transcode::SessionKind::Transcode { height };
    request.hdr10 = true;
    request.dv_processing_destination = Some(DvDestination::Profile81);
    request.dv_refit_digest = None;
    request.audio_index = query.audio_index;
    request.audio_offset_ms = query.audio_offset_ms;
    request.audio_claim = query.audio_claim.clone();
    request.audio_delivery = query.audio_delivery.clone();
    request.request_id = Some(root_id.clone());
    request.previous_session_id = None;
    request.reopen_reason = None;
    request.control_sequence = None;
    request.quality_catalog = None;
    request.candidate_context = None;
    let catalog = crate::media_sessions::CandidateCatalogContext {
        caps: query.caps.clone(),
        candidate: requested.clone(),
        binding: crate::media_pool::PlanningBinding::from_snapshot(&planning),
        catalog_query: Some(query),
    };
    let pristine = RemoteStartRequest {
        retained_output_receiver: Some(1),
        retained_output: None,
        candidate_id: Some(requested.id),
        candidate_catalog: Some(catalog),
        decoder_caps: predecessor.decoder_caps.clone(),
        presentation_target: predecessor.presentation_target,
        protocol_version: crate::media_pool::PROTOCOL_VERSION,
        incarnation_id: root_id.clone(),
        principal: route.principal.clone(),
        source_size: source.size,
        source_mtime: source.mtime,
        typeless_playlist: predecessor.typeless_playlist,
        library_channel: None,
        request,
    };
    let fingerprint = pristine
        .request
        .durable_intent_fingerprint(&pristine.principal);
    let desired_revision = state
        .store
        .desired_selection(&route.principal, &route.playback_id)
        .await
        .map_err(|error| error.to_string())?
        .map(|desired| desired.revision);
    let now_ms = crate::media_sessions::unix_ms();
    let claim_expiry = now_ms.saturating_add(60_000);
    let (mut guard, acquired) = super::session_guard::MediaSessionRequestGuard::claim(
        state.clone(),
        route.principal.clone(),
        claim_id.clone(),
        root_id.clone(),
        async {
            let (outcome, _) = state
                .store
                .claim_media_session_request_recording(
                    &route.principal,
                    &claim_id,
                    &fingerprint,
                    &route.playback_id,
                    &root_id,
                    now_ms,
                    claim_expiry,
                    None,
                )
                .await?;
            Ok(outcome)
        },
    )
    .await
    .map_err(|error| error.to_string())?
    .map_err(|_| "deferred FEL root claim is unavailable")?;
    if acquired != root_id {
        return Err("the deferred FEL root claim selected another incarnation".into());
    }
    let mut context = None;
    let selected = super::create::prepare_adaptive_origin(
        state,
        &mut context,
        &owner,
        quote,
        source,
        &planning,
        &pristine,
        &claim_id,
        &root_id,
        &fingerprint,
        &route.recovery_epoch,
        Some(route),
        Some(&route.incarnation_id),
        desired_revision,
        claim_expiry,
        deadline,
        AdaptivePreparePurpose::DeferredFel,
    )
    .await;
    if context.is_some() {
        guard.disarm();
    }
    let prepared = selected.map_err(|failure| match failure {
        super::create::adaptive_retry::AdaptiveOriginFailure::NextWorker(error)
        | super::create::adaptive_retry::AdaptiveOriginFailure::Hold(error) => format!("{error:?}"),
    })?;
    stage_verified_remote_fel(
        state,
        intent,
        run,
        pending,
        route,
        predecessor,
        source,
        &owner,
        prepared,
        requested_ms,
        deadline,
    )
    .await
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
async fn stage_verified_remote_fel(
    state: &AppState,
    intent: &UpgradeIntent,
    run: &UpgradeRun,
    pending: &super::preparation::PendingCandidateGuard,
    route: &MediaSessionRoute,
    predecessor: &RemoteStartRequest,
    source: &MediaFile,
    owner: &str,
    prepared: super::create::AdaptiveOriginPrepared,
    requested_ms: i64,
    deadline: tokio::time::Instant,
) -> Result<&'static str, String> {
    let ready = async {
        let (start, end) = prepared
            .prepared_window_ms
            .ok_or("the worker has no verified FEL window")?;
        if requested_ms < start
            || requested_ms >= end
            || (prepared.preview.start_seconds * 1000.0).round() as i64 != requested_ms
        {
            return Err(
                "the original verified worker window does not cover the deferred playhead".into(),
            );
        }
        let route_recipe: RemoteStartRequest = serde_json::from_str(&prepared.selected_recipe_json)
            .map_err(|error| error.to_string())?;
        let catalog = route_recipe
            .candidate_catalog
            .as_ref()
            .ok_or("the worker has no selected FEL candidate")?;
        let mut request = route_recipe.request.clone();
        let mut selected =
            crate::transcode::TranscodeManager::candidate_context(&catalog.candidate);
        selected.canonical_caps = Some(catalog.caps.clone());
        selected.catalog_query = catalog.catalog_query.clone();
        selected.planning_binding = Some(catalog.binding.clone());
        selected.owner_node_id = Some(owner.to_owned());
        request.candidate_context = Some(Box::new(selected));
        request.audio_delivery = prepared.preview.audio_delivery.clone();
        admit_processed_candidate(
            state,
            intent,
            source,
            route,
            &catalog.candidate,
            AdmissionStage::AfterTrial,
        )
        .await?;
        let verified_entry = requested_ms..end;
        let (rendezvous_ms, _) = tokio::time::timeout_at(
            deadline,
            await_successor_start(
                state,
                intent,
                run,
                pending,
                Some(&verified_entry),
                source.duration_ms,
            ),
        )
        .await
        .map_err(|_| "the original deferred FEL rendezvous cutoff expired")??;
        if pending.cancelled()
            || tokio::time::Instant::now() >= deadline
            || rendezvous_ms != requested_ms
        {
            return Err("the viewer moved past the original deferred FEL opportunity".into());
        }
        let cancellation_key = crate::media_sessions::bounded_adaptive_digest(&prepared.claim.key)?;
        let duration_ms = prepared
            .preview
            .duration_ms
            .ok_or("the worker has no bound VOD duration")?;
        Ok((request, cancellation_key, duration_ms))
    }
    .await;
    let (request, cancellation_key, duration_ms) = match ready {
        Ok(ready) => ready,
        Err(error) => {
            super::create::cancel_deferred_fel_origin(state, &prepared).await;
            return Err(error);
        }
    };
    let ids = super::preparation::StagedSuccessorIds {
        incarnation_id: prepared.claim.key.route_incarnation_id.to_string(),
        session_id: prepared.claim.key.session_id.to_string(),
    };
    let deferred = super::preparation::DeferredFelStage {
        recipe_json: prepared.selected_recipe_json.clone(),
        request_fingerprint: prepared.claim.binding.request_fingerprint.clone(),
        desired_revision: prepared.claim.binding.expected_desired_revision,
        cancellation_key,
        duration_ms,
        prepared,
        deadline,
    };
    let outcome = super::preparation::stage_prepared_successor_with_delivery_as(
        state,
        &intent.session_id,
        route,
        predecessor,
        &request,
        Some(source),
        owner,
        super::preparation::PreparationPurpose::SelectionChange,
        super::preparation::AcceptedAsk {
            prepared_proof: None,
            film_time_ms: requested_ms,
            desired_digest: None,
            planning_cancellation: Some(pending.cancellation_token()),
            planning_registration: Some(pending.registration()),
            quality_intent: None,
        },
        true,
        ids,
        Some(super::preparation::StagedProcessingDelivery {
            dolby_vision_profile: Some(8),
            fel: Some(true),
        }),
        Some(&deferred),
    )
    .await;
    if outcome != super::preparation::PreparationStageOutcome::Staged {
        super::create::cancel_deferred_fel_origin(state, &deferred.prepared).await;
    }
    Ok(run.finish_staging(outcome))
}

/// Skip reason when a parked incumbent's published runway (or a seek past it)
/// ended the claim before the processed successor was staged.
pub(in crate::http) const INCUMBENT_RUNWAY_SPENT: &str =
    "the incumbent's published runway no longer covers a processed upgrade hand-back";

/// When an admission is asked: before the GPU trial, or after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::http) enum AdmissionStage {
    BeforeTrial,
    AfterTrial,
}

/// Whether an admission rested on evidence it actually read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::http) enum Admission {
    /// Every evidence read answered and none refused the recipe.
    Proven,
    /// Before the trial, the link memory could not be read in time. Nothing
    /// refused the recipe, so the trial may run, but the check after it is
    /// the admission and must not be skipped.
    Unproven,
}

/// How one link-memory read bears on a processed recipe at one stage.
pub(in crate::http) fn link_admission(
    evidence: super::link_receipts::ProcessedLinkEvidence,
    stage: AdmissionStage,
) -> Result<Admission, String> {
    use super::link_receipts::ProcessedLinkEvidence as Evidence;
    match (evidence, stage) {
        (Evidence::Admissible, _) => Ok(Admission::Proven),
        (Evidence::Negative, _) => Err(
            "this network's link memory holds an active negative for the processed recipe".into(),
        ),
        (Evidence::Unavailable, AdmissionStage::BeforeTrial) => Ok(Admission::Unproven),
        (Evidence::Unavailable, AdmissionStage::AfterTrial) => Err(
            "this network's link memory for the processed recipe could not be read before its deadline"
                .into(),
        ),
    }
}

/// How one recovery-memory read bears on a processed recipe at one stage.
pub(in crate::http) fn recovery_admission(
    evidence: super::candidate_recovery::ProcessedRecoveryEvidence,
    stage: AdmissionStage,
) -> Result<Admission, String> {
    use super::candidate_recovery::ProcessedRecoveryEvidence as Evidence;
    match (evidence, stage) {
        (Evidence::Admissible, _) => Ok(Admission::Proven),
        (Evidence::Rejected, _) => {
            Err("this playback's recovery evidence rejects the processed recipe".into())
        }
        (Evidence::Unavailable, AdmissionStage::BeforeTrial) => Ok(Admission::Unproven),
        (Evidence::Unavailable, AdmissionStage::AfterTrial) => Err(
            "this playback's recovery evidence for the processed recipe could not be read before its deadline"
                .into(),
        ),
    }
}

/// Whether the recipe a trial froze still needs the admission after the
/// trial. Only a recipe exactly the one admitted on evidence before the trial
/// (same digest, same bitrate) is spared it; an unproven pre-trial admission
/// never is.
pub(in crate::http) fn post_trial_admission_needed(
    proven_before: bool,
    projected: Option<&plurx_core::playback::candidate::QualityCandidate>,
    candidate: &plurx_core::playback::candidate::QualityCandidate,
) -> bool {
    !(proven_before
        && projected.is_some_and(|projected| {
            projected.recipe_digest == candidate.recipe_digest
                && projected.average_bps == candidate.average_bps
                && projected.peak_bps == candidate.peak_bps
        }))
}

/// The admission the in-create qualification applied to a processed
/// candidate, against the session it now upgrades: this playback's recovery
/// evidence, and for Auto the link evidence for its bitrate.
///
/// Before the trial only an exact negative refuses: a read that misses its
/// deadline is not a verdict about the recipe, and refusing on it skipped the
/// upgrade for the whole playback whenever the store was slow right after the
/// first frame. After the trial an unreadable link memory still refuses,
/// because that reserved read is the last word before a grant.
async fn admit_processed_candidate(
    state: &AppState,
    intent: &UpgradeIntent,
    source: &plurx_core::domain::MediaFile,
    route: &MediaSessionRoute,
    candidate: &plurx_core::playback::candidate::QualityCandidate,
    stage: AdmissionStage,
) -> Result<Admission, String> {
    let advisory = crate::media_pool::create_stage_deadline(std::time::Duration::from_millis(500));
    let network = intent.network.as_ref();
    // Recovery memory is keyed by the viewer's network identity. Without one
    // there is no memory to consult, and the in-create qualification of a
    // fresh start did not consult one either.
    let recovery = if network.is_none() {
        super::candidate_recovery::ProcessedRecoveryEvidence::Admissible
    } else {
        super::candidate_recovery::processed_candidate_recovery_evidence(
            state,
            network,
            source,
            &intent.playback_id,
            candidate,
            Some(route),
            advisory,
        )
        .await
    };
    let recovered = recovery_admission(recovery, stage)?;
    if !intent.auto_policy {
        return Ok(recovered);
    }
    let evidence = super::link_receipts::processed_candidate_link_evidence(
        state, network, source, candidate, advisory,
    )
    .await;
    let admission = match (recovered, link_admission(evidence, stage)?) {
        (Admission::Proven, Admission::Proven) => Admission::Proven,
        _ => Admission::Unproven,
    };
    let mut admitted = vec![candidate.clone()];
    admitted = super::link_receipts::link_starved_catalog(
        intent.network_prior.as_ref(),
        admitted,
        unix_ms(),
    );
    if let Some(receipt) = intent.incumbent_receipt.as_deref() {
        admitted = super::link_receipts::positive_catalog(
            state,
            network,
            source,
            Some(receipt),
            Some(&intent.playback_id),
            admitted,
            None,
            advisory,
        )
        .await;
    }
    // Production is not part of this: before the trial the recipe is not
    // producing, so a description of it from an earlier trial has no current
    // proof, and the trial is what proves it (checked on the frozen recipe
    // after the trial, whatever this admission found).
    if super::create::processed_candidate_kept(&admitted, candidate).is_some() {
        Ok(admission)
    } else {
        Err("Auto's link evidence for this playback does not admit the processed candidate's bitrate".into())
    }
}

/// The recipe the trial froze is only staged when the trial proved it
/// sustains production: [`QualityCandidate::playable`] on the trial's own
/// candidate, which carries the trial's production proof.
fn trial_proved_production(
    candidate: &plurx_core::playback::candidate::QualityCandidate,
) -> Result<(), &'static str> {
    if candidate.playable() {
        Ok(())
    } else {
        Err("the processed trial did not prove the recipe sustains realtime production")
    }
}

#[cfg(test)]
#[path = "processed_upgrade_tests.rs"]
pub(in crate::http) mod tests;
