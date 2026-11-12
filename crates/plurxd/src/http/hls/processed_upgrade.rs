//! Processed Dolby Vision / HDR10-E as an upgrade after the first frame.
//!
//! Plan ruling 3 (Paul, 2026-11-07): processed output is never work before the
//! viewer's first frame. A create resolves the ordinary plan only; when that
//! plan could be processed it leaves an [`UpgradeIntent`] here, on the node
//! that owns the session, and so does the activation of a locally owned
//! ordinary prepared successor. The first `ttff` beacon naming the session
//! (or, for a prepared successor, its commit, which the client sends only
//! after showing the successor's frame) takes the intent and runs one bounded
//! trial
//! ([`crate::transcode::TranscodeManager::prepare_processed_upgrade`]). A
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
    let mut entries = lock();
    let Some(Entry::Waiting { intent, registered }) = entries.get(session_id) else {
        return None;
    };
    if intent.user_id != user_id {
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
        crate::playback_control::record_processed_upgrade(outcome);
        drop(run);
    });
}

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
fn projected_processed_candidate(
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

/// Where a processed upgrade's successor begins: the trial's verified window,
/// already produced, but never more than [`SUCCESSOR_MAX_HEAD_START_MS`]
/// ahead of the viewer, and never behind the viewer.
pub(in crate::http) fn successor_start_ms(viewer_ms: i64, verified_ms: i64) -> i64 {
    verified_ms
        .min(viewer_ms.saturating_add(SUCCESSOR_MAX_HEAD_START_MS))
        .max(viewer_ms)
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
    F: FnOnce(crate::transcode::SessionRequest, crate::transcode::SessionRecoveryIdentity) -> Fut,
    Fut: std::future::Future<Output = TrialOutcome>,
{
    run_upgrade_with_claim(state, intent, run, None, run_trial).await
}

async fn run_upgrade_with_claim<F, Fut>(
    state: &AppState,
    intent: &UpgradeIntent,
    run: &UpgradeRun,
    pending: Option<super::preparation::PendingCandidateGuard>,
    run_trial: F,
) -> &'static str
where
    F: FnOnce(crate::transcode::SessionRequest, crate::transcode::SessionRecoveryIdentity) -> Fut,
    Fut: std::future::Future<Output = TrialOutcome>,
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
            "processed upgrade skipped: a viewer-directed preparation holds the playback's slot");
        return "not_applicable";
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
    // described the processed recipe for this query, the evidence kept under
    // its digest and bitrate decides now, and a refused recipe costs no
    // trial. A read that misses its deadline is not a refusal: the trial runs
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
    let mut trial_request = intent.request.clone();
    trial_request.start_seconds = verified_ms as f64 / 1_000.0;
    let superseded = pending.cancellation_token();
    let trial = tokio::select! {
        () = superseded.cancelled() => return "cancelled",
        trial = run_trial(trial_request, recovery) => trial,
    };
    let trial = match trial {
        Ok(Some(trial)) => trial,
        Ok(None) => return "not_applicable",
        Err(reason) => {
            if let Some(wait) = crate::transcode::pending_fel_qualification(&reason) {
                if qualification::retain(intent, run, wait) {
                    return "qualification_pending";
                }
                if run.cancelled().is_cancelled() {
                    return "cancelled";
                }
                run.skipped("the current reconstruction recipe no longer matches its completed qualification");
                return "declined";
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
    } = trial;
    // The recipe the trial actually froze is admitted unless it is exactly
    // the one admitted before the trial: same digest, same bitrate.
    if post_trial_admission_needed(proven_before, projected.as_ref(), &candidate) {
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
    // Where the successor begins: at the window the trial verified, which
    // is already produced, within the head start the clients' rendezvous
    // can absorb, and never behind the viewer. A successor
    // begun where the viewer is now leaves its producer to render, from the
    // viewer's position, everything before that window, while the viewer
    // and the client's rendezvous lead and runway move away from it; unless
    // the render runs well above realtime it does not catch up within the
    // client's overlap bound (Avatar on the Google TV: runway_unproven after
    // 12 s). The clients hold the incumbent until it reaches the
    // successor's own start, so beginning ahead of the viewer skips no film.
    // The ordinary request a dispatch falls back to begins there too, or a
    // degraded successor would restart the film at the create's start.
    let film_time_ms = successor_start_ms(
        bounded_film_ms(
            viewer_film_time_ms(state, intent, run).await,
            source.duration_ms,
        ),
        verified_ms,
    );
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
    // Held until staging returns: the successor's first preparation takes it
    // by its incarnation, and whatever it did not take is freed on drop.
    let _root = state
        .transcode
        .hold_processed_upgrade_root(&ids.incarnation_id, context, root);
    let primes = state.hls_route_hooks.get().primes_prepared_successor();
    let staged = super::preparation::stage_prepared_successor_as(
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
    )
    .await;
    if staged {
        tracing::info!(target: "plurxd::http::hls", session = %session,
            "processed upgrade staged as a prepared successor");
        "staged"
    } else {
        run.skipped("the verified reconstruction could not be staged for the current playback");
        "not_staged"
    }
}

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
    if super::create::processed_candidate_is_admitted(&admitted, candidate) {
        Ok(admission)
    } else {
        Err("Auto's link evidence for this playback does not admit the processed candidate's bitrate".into())
    }
}

#[cfg(test)]
#[path = "processed_upgrade_tests.rs"]
pub(in crate::http) mod tests;
