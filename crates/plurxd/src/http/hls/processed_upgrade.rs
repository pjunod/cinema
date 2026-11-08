//! Processed Dolby Vision / HDR10-E as an upgrade after the first frame.
//!
//! Plan ruling 3 (Paul, 2026-11-07): processed output is never work before the
//! viewer's first frame. A create resolves the ordinary plan only; when that
//! plan could be processed it leaves an [`UpgradeIntent`] here, on the node
//! that owns the session. The first `ttff` beacon naming the session takes the
//! intent and runs one bounded trial
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

/// How long an intent waits for its first frame. A start whose picture has
/// not appeared by then is not one an upgrade should be spent on; the entry is
/// dropped at the next registration, not by a timer.
const FIRST_FRAME_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);
/// Bounds the registry against a node that starts many sessions no client
/// ever reports a frame for.
const MAX_INTENTS: usize = 512;

/// What the create knew that the upgrade needs, captured once, on the owner.
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
    Running {
        playback_id: String,
        incarnation_id: String,
        claim: u64,
        cancelled: tokio_util::sync::CancellationToken,
    },
}

impl Entry {
    fn playback_and_incarnation(&self) -> (&str, &str) {
        match self {
            Self::Waiting { intent, .. } => (&intent.playback_id, &intent.incarnation_id),
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

static NEXT_CLAIM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Leave an upgrade waiting for `intent.session_id`'s first frame.
pub(in crate::http) fn register(intent: UpgradeIntent) {
    let now = std::time::Instant::now();
    let mut entries = lock();
    entries.retain(|_, entry| match entry {
        Entry::Waiting { registered, .. } => now.duration_since(*registered) < FIRST_FRAME_WINDOW,
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
        },
    );
    Some((
        intent,
        UpgradeRun {
            session_id: session_id.to_owned(),
            claim,
            cancelled,
        },
    ))
}

/// The running upgrade's own claim on its entry. Dropping it (the task ended,
/// however it ended) removes the entry only if it is still this run's.
pub(in crate::http) struct UpgradeRun {
    session_id: String,
    claim: u64,
    cancelled: tokio_util::sync::CancellationToken,
}

impl UpgradeRun {
    pub(in crate::http) fn cancelled(&self) -> &tokio_util::sync::CancellationToken {
        &self.cancelled
    }
}

impl Drop for UpgradeRun {
    fn drop(&mut self) {
        let mut entries = lock();
        if matches!(entries.get(&self.session_id),
            Some(Entry::Running { claim, .. }) if *claim == self.claim)
        {
            entries.remove(&self.session_id);
        }
    }
}

fn cancel_entry(entry: Entry) {
    if let Entry::Running { cancelled, .. } = entry {
        cancelled.cancel();
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
    crate::playback_control::record_processed_upgrade("scheduled");
    let state = state.clone();
    tokio::spawn(async move {
        let outcome = run_upgrade(&state, &intent, &run).await;
        crate::playback_control::record_processed_upgrade(outcome);
        drop(run);
    });
}

/// One upgrade, start to staged. Returns its outcome label.
async fn run_upgrade(state: &AppState, intent: &UpgradeIntent, run: &UpgradeRun) -> &'static str {
    let session = crate::transcode::session_log_id(&intent.session_id);
    // One preparation slot per playback: a viewer's own change in flight wins,
    // and a viewer's change arriving during the trial cancels this one.
    let Some(pending) =
        super::preparation::begin_processed_upgrade(&intent.playback_id, run.cancelled())
    else {
        if run.cancelled().is_cancelled() {
            return "cancelled";
        }
        tracing::info!(target: "plurxd::http::hls", session = %session,
            "processed upgrade skipped: a viewer-directed preparation holds the playback's slot");
        return "not_applicable";
    };
    let route = match state
        .store
        .media_session_route_by_incarnation(&intent.incarnation_id)
        .await
    {
        Ok(Some(route))
            if route.session_id == intent.session_id
                && route.owner_node_id == state.node_id
                && route.state == "active" =>
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
    let ids = super::preparation::StagedSuccessorIds::mint();
    let recovery = crate::transcode::SessionRecoveryIdentity {
        principal: route.principal.clone(),
        incarnation_id: ids.incarnation_id.clone(),
        recovery_epoch: route.recovery_epoch.clone(),
    };
    let superseded = pending.cancellation_token();
    let trial = tokio::select! {
        () = superseded.cancelled() => return "cancelled",
        trial = state.transcode.prepare_processed_upgrade(
            &intent.session_id,
            &intent.request,
            &recovery,
            intent.hdr10_intent,
        ) => trial,
    };
    let trial = match trial {
        Ok(Some(trial)) => trial,
        Ok(None) => return "not_applicable",
        Err(reason) => {
            tracing::warn!(target: "plurxd::http::hls", session = %session, %reason,
                "processed upgrade declined; the ordinary session keeps playing");
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
    let Ok(Some(source)) = state.store.get_file(intent.request.file_id).await else {
        return "not_applicable";
    };
    let crate::transcode::ProcessedUpgradeTrial {
        request: mut processed,
        candidate,
        root,
    } = trial;
    if let Err(reason) = admit_processed_candidate(state, intent, &source, &route, &candidate).await
    {
        tracing::warn!(target: "plurxd::http::hls", session = %session, %reason,
            "processed upgrade not admitted; the ordinary session keeps playing");
        return "admission_refused";
    }
    let Some(context) = processed.candidate_context.as_deref().cloned() else {
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
    let film_time_ms = (processed.start_seconds * 1_000.0).round() as i64;
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
            film_time_ms: film_time_ms.max(0),
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
        "not_staged"
    }
}

/// The admission the in-create qualification applied to a processed
/// candidate, against the session it now upgrades: this playback's recovery
/// evidence, and for Auto the link evidence for its bitrate.
async fn admit_processed_candidate(
    state: &AppState,
    intent: &UpgradeIntent,
    source: &plurx_core::domain::MediaFile,
    route: &MediaSessionRoute,
    candidate: &plurx_core::playback::candidate::QualityCandidate,
) -> Result<(), String> {
    let advisory = crate::media_pool::create_stage_deadline(std::time::Duration::from_millis(500));
    let network = intent.network.as_ref();
    // Recovery memory is keyed by the viewer's network identity. Without one
    // there is no memory to consult, and the in-create qualification of a
    // fresh start did not consult one either.
    let recovered = network.is_none()
        || super::candidate_recovery::processed_candidate_admissible(
            state,
            network,
            source,
            &intent.playback_id,
            candidate,
            Some(route),
            advisory,
        )
        .await;
    if !recovered {
        return Err("this playback's recovery evidence rejects the processed recipe".into());
    }
    if !intent.auto_policy {
        return Ok(());
    }
    let linked = super::link_receipts::processed_candidate_admissible(
        state, network, source, candidate, advisory,
    )
    .await;
    let mut admitted = if linked {
        vec![candidate.clone()]
    } else {
        Vec::new()
    };
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
        Ok(())
    } else {
        Err("Auto's link evidence for this playback does not admit the processed candidate's bitrate".into())
    }
}

#[cfg(test)]
#[path = "processed_upgrade_tests.rs"]
mod tests;
