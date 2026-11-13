//! A missing exact quality receipt may finish while the ordinary play is live.
//! Accepted control exchanges are the only retry events: no timer, repeated
//! measurement, client protocol, or persisted executable authority is added.
use super::*;
use crate::playback_control::{
    ClientSelection, ControlRequestV1, DynamicCapabilities, EffectiveSelection,
};
use crate::transcode::PendingFelQualification;

pub(super) const RETENTION: std::time::Duration = std::time::Duration::from_secs(86_400);
const CHECK_BUDGET: std::time::Duration = std::time::Duration::from_secs(4);

pub(super) struct AwaitingQualification {
    pub(super) intent: Box<UpgradeIntent>,
    pub(super) wait: PendingFelQualification,
    pub(super) report: ProcessingSkip,
    pub(super) recorded: std::time::Instant,
    pub(super) cancelled: tokio_util::sync::CancellationToken,
    presented_at: std::time::Instant,
    selection: Option<ClientSelection>,
    checking: Option<u64>,
}

pub(super) fn retain(
    intent: &UpgradeIntent,
    run: &UpgradeRun,
    wait: PendingFelQualification,
) -> bool {
    if !wait.valid() || wait.file_id != intent.request.file_id || run.cancelled().is_cancelled() {
        return false;
    }
    let mut entries = lock();
    if !matches!(entries.get(&intent.session_id), Some(Entry::Running { claim, report: None, .. }) if *claim == run.claim)
    {
        return false;
    }
    let report = ProcessingSkip {
        generation: intent.incarnation_id.clone(),
        reason: wait.reason().into(),
    };
    entries.insert(
        intent.session_id.clone(),
        Entry::AwaitingQualification(Box::new(AwaitingQualification {
            intent: Box::new(intent.clone()),
            wait,
            report,
            recorded: std::time::Instant::now(),
            cancelled: run.cancelled.clone(),
            presented_at: run.presented_at,
            selection: None,
            checking: None,
        })),
    );
    true
}

#[derive(Clone)]
pub(super) struct ControlInputs {
    pub(super) route: MediaSessionRoute,
    pub(super) recipe: RemoteStartRequest,
    pub(super) selection: ClientSelection,
    pub(super) delivered: EffectiveSelection,
    pub(super) capabilities: Option<DynamicCapabilities>,
    pub(super) previous_target_height: u32,
}

struct Check {
    session: String,
    claim: u64,
    intent: Box<UpgradeIntent>,
    wait: PendingFelQualification,
    cancelled: tokio_util::sync::CancellationToken,
    presented_at: std::time::Instant,
}

fn same_accepted_selection(before: &ClientSelection, after: &ClientSelection, target: u32) -> bool {
    use crate::playback_control::QualitySelection;
    let normalized = |selection: &ClientSelection| {
        let mut selection = selection.clone();
        if let QualitySelection::Auto { height, .. } = selection.quality {
            selection.quality = QualitySelection::Auto {
                height: Some(height.unwrap_or(i64::from(target))),
                candidate_id: None,
            };
        }
        selection
    };
    normalized(before) == normalized(after)
}

impl Check {
    fn take(
        session: &str,
        generation: &str,
        selection: &ClientSelection,
        eligible: bool,
    ) -> Option<Self> {
        let mut entries = lock();
        let Some(Entry::AwaitingQualification(wait)) = entries.get_mut(session) else {
            return None;
        };
        if wait.intent.incarnation_id != generation {
            return None;
        }
        let reason = if wait.recorded.elapsed() >= RETENTION {
            Some("the qualification wait expired; this session keeps its ordinary picture")
        } else if wait.selection.as_ref().is_some_and(|held| {
            !same_accepted_selection(
                held,
                selection,
                wait.intent
                    .request
                    .candidate_context
                    .as_ref()
                    .map(|context| context.selected_candidate.target_height)
                    .unwrap_or(wait.wait.request.output.height),
            )
        }) {
            Some("the playback selection changed while reconstruction was being qualified")
        } else {
            None
        };
        if let Some(reason) = reason {
            wait.cancelled.cancel();
            let playback_id = wait.intent.playback_id.clone();
            let generation = wait.intent.incarnation_id.clone();
            entries.insert(
                session.to_owned(),
                Entry::Skipped {
                    playback_id,
                    report: ProcessingSkip {
                        generation,
                        reason: reason.into(),
                    },
                    recorded: std::time::Instant::now(),
                },
            );
            return None;
        }
        if !eligible || wait.checking.is_some() || wait.cancelled.is_cancelled() {
            return None;
        }
        wait.selection.get_or_insert_with(|| selection.clone());
        let claim = NEXT_CLAIM.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        wait.checking = Some(claim);
        Some(Self {
            session: session.to_owned(),
            claim,
            intent: wait.intent.clone(),
            wait: wait.wait.clone(),
            cancelled: wait.cancelled.clone(),
            presented_at: wait.presented_at,
        })
    }

    fn terminal(&self, reason: &str) {
        let mut entries = lock();
        let Some(Entry::AwaitingQualification(wait)) = entries.get(&self.session) else {
            return;
        };
        if wait.checking != Some(self.claim) || wait.cancelled.is_cancelled() {
            return;
        }
        let playback_id = wait.intent.playback_id.clone();
        let generation = wait.intent.incarnation_id.clone();
        let reason = reason
            .chars()
            .filter(|c| !c.is_control())
            .take(512)
            .collect();
        entries.insert(
            self.session.clone(),
            Entry::Skipped {
                playback_id,
                report: ProcessingSkip { generation, reason },
                recorded: std::time::Instant::now(),
            },
        );
    }

    fn ready(
        &self,
        request: crate::transcode::SessionRequest,
        owner_epoch: i64,
    ) -> Option<(Box<UpgradeIntent>, UpgradeRun)> {
        let mut entries = lock();
        let Some(Entry::AwaitingQualification(wait)) = entries.get(&self.session) else {
            return None;
        };
        if wait.checking != Some(self.claim) || wait.cancelled.is_cancelled() {
            return None;
        }
        let mut intent = wait.intent.clone();
        let mut request = request;
        request
            .candidate_context
            .as_mut()?
            .required_fel_qualification = Some(wait.wait.clone());
        intent.auto_policy = request.automatic;
        intent.request = request;
        let report = ProcessingSkip { generation: intent.incarnation_id.clone(),
            reason: "reconstruction qualification completed; this session still uses the ordinary conversion until replacement".into() };
        entries.insert(
            self.session.clone(),
            Entry::Running {
                playback_id: intent.playback_id.clone(),
                incarnation_id: intent.incarnation_id.clone(),
                claim: self.claim,
                cancelled: self.cancelled.clone(),
                report: Some(report),
            },
        );
        Some((
            intent,
            UpgradeRun {
                session_id: self.session.clone(),
                claim: self.claim,
                cancelled: self.cancelled.clone(),
                presented_at: self.presented_at,
                expected_owner_epoch: Some(owner_epoch),
            },
        ))
    }
}

impl Drop for Check {
    fn drop(&mut self) {
        if let Some(Entry::AwaitingQualification(wait)) = lock().get_mut(&self.session) {
            if wait.checking == Some(self.claim) {
                wait.checking = None;
            }
        }
    }
}

/// Called only after this exchange was accepted. A changed ask cancels a
/// pending continuation even when that exchange is not eligible for an upgrade.
#[allow(clippy::too_many_arguments)]
pub(in crate::http::hls) fn note_accepted_control(
    state: &AppState,
    route: &MediaSessionRoute,
    recipe: &RemoteStartRequest,
    request: &ControlRequestV1,
    delivered: &EffectiveSelection,
    capabilities: Option<&DynamicCapabilities>,
    eligible: bool,
) {
    if route.owner_node_id != state.node_id
        || route.state != "active"
        || request.generation != route.incarnation_id
    {
        return;
    }
    let Some(check) = Check::take(
        &route.session_id,
        &route.incarnation_id,
        &request.selection,
        eligible,
    ) else {
        return;
    };
    let state = state.clone();
    let previous_target_height = check
        .intent
        .request
        .candidate_context
        .as_ref()
        .map(|context| context.selected_candidate.target_height)
        .unwrap_or(0);
    let inputs = ControlInputs {
        route: route.clone(),
        recipe: recipe.clone(),
        selection: request.selection.clone(),
        delivered: delivered.clone(),
        capabilities: capabilities.cloned(),
        previous_target_height,
    };
    tokio::spawn(async move {
        check_with(
            &state,
            check,
            inputs,
            |state, wait, inputs| async move { resolve_current(&state, &wait, &inputs).await },
            |state, intent, run, pending| start_upgrade_claim(state, intent, run, Some(pending)),
        )
        .await;
    });
}

enum Resolution {
    Pending,
    Terminal(String),
    Ready(Box<crate::transcode::SessionRequest>),
}

enum Receipt {
    Missing,
    Rejected(String),
    Admitted,
}

fn exact_receipt(
    wait: &PendingFelQualification,
    size: i64,
    mtime: i64,
    settings_generation: i64,
    document: Option<&str>,
) -> Receipt {
    if size != wait.request.source_size || mtime != wait.request.source_mtime {
        return Receipt::Rejected(
            "the source changed while reconstruction was being qualified".into(),
        );
    }
    if settings_generation != wait.settings_generation {
        return Receipt::Rejected(
            "playback settings changed while reconstruction was being qualified".into(),
        );
    }
    use plurx_core::transcode::dv_fel_qualification::{verdict_state, VerdictState};
    match verdict_state(document, size, mtime, &wait.request.identity) {
        VerdictState::Missing => Receipt::Missing,
        VerdictState::Current(verdict) if !verdict.admitted => Receipt::Rejected(verdict.refusal()),
        VerdictState::Current(_) => Receipt::Admitted,
    }
}

async fn resolve_current(
    state: &AppState,
    wait: &PendingFelQualification,
    inputs: &ControlInputs,
) -> Result<Resolution, ()> {
    let snapshot = state
        .store
        .playback_planning_snapshot(wait.file_id, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await
        .map_err(|_| ())?;
    let Some(snapshot) = snapshot else {
        return Ok(Resolution::Terminal(
            "the qualified source is no longer available".into(),
        ));
    };
    if snapshot.file.size != wait.request.source_size
        || snapshot.file.mtime != wait.request.source_mtime
        || inputs.recipe.source_size != wait.request.source_size
        || inputs.recipe.source_mtime != wait.request.source_mtime
    {
        return Ok(Resolution::Terminal(
            "the source changed while reconstruction was being qualified".into(),
        ));
    }
    match exact_receipt(
        wait,
        snapshot.file.size,
        snapshot.file.mtime,
        snapshot.generation,
        snapshot.probe_json.as_deref(),
    ) {
        Receipt::Missing => return Ok(Resolution::Pending),
        Receipt::Rejected(reason) => return Ok(Resolution::Terminal(reason)),
        Receipt::Admitted => {}
    }
    match crate::fragment_index_cluster::inspect_source(&snapshot.file).await {
        Ok(version) if version == wait.source_object_version => {}
        Ok(_) => {
            return Ok(Resolution::Terminal(
                "the held source object changed while reconstruction was being qualified".into(),
            ))
        }
        Err(_) => {
            return Ok(Resolution::Terminal(
                "the source object could not be revalidated for reconstruction".into(),
            ))
        }
    }
    if !state.transcode.pending_fel_backend_current(wait).await {
        return Ok(Resolution::Terminal(
            "the reconstruction helper or encoder is unavailable or changed after qualification"
                .into(),
        ));
    }
    let fresh = super::super::preparation::replan_qualified_upgrade(
        state,
        &inputs.route,
        &inputs.recipe,
        &inputs.selection,
        &inputs.delivered,
        inputs.capabilities.as_ref(),
        &snapshot.file,
        inputs.previous_target_height,
    )
    .await;
    let fresh = match fresh {
        Ok(request) => request,
        Err(reason) => return Ok(Resolution::Terminal(reason)),
    };
    let current = state
        .transcode
        .validate_candidate_planning_binding(&fresh, None, None)
        .await
        .map_err(|_| ())?;
    if current.as_ref().is_none_or(|current| {
        current.generation != wait.settings_generation
            || current.file.id != wait.file_id
            || current.file.size != wait.request.source_size
            || current.file.mtime != wait.request.source_mtime
    }) {
        return Ok(Resolution::Terminal(
            "the reconstruction plan changed while refreshing its authority".into(),
        ));
    }
    Ok(Resolution::Ready(Box::new(fresh)))
}

async fn check_with<F, Fut, R>(
    state: &AppState,
    check: Check,
    inputs: ControlInputs,
    resolve: F,
    resume: R,
) where
    R: FnOnce(
        &AppState,
        Box<UpgradeIntent>,
        UpgradeRun,
        super::super::preparation::PendingCandidateGuard,
    ),
    F: FnOnce(AppState, PendingFelQualification, ControlInputs) -> Fut,
    Fut: std::future::Future<Output = Result<Resolution, ()>>,
{
    // Claim first, including the read-only check, so a newer viewer-directed
    // ask cancels it before it can promote an old accepted exchange.
    let Some(pending) = super::super::preparation::begin_processed_upgrade(
        &check.intent.playback_id,
        &check.cancelled,
    ) else {
        return;
    };
    let owner_epoch = inputs.route.owner_epoch;
    let principal = inputs.route.principal.clone();
    let recipe_json = inputs.route.recipe_json.clone();
    let cancelled = pending.cancellation_token();
    let deadline = tokio::time::Instant::now() + CHECK_BUDGET;
    let result = tokio::select! {
        () = cancelled.cancelled() => return,
        result = tokio::time::timeout_at(deadline, resolve(state.clone(), check.wait.clone(), inputs)) => result,
    };
    if cancelled.is_cancelled() {
        return;
    }
    match result {
        Ok(Ok(Resolution::Pending)) | Ok(Err(())) | Err(_) => {}
        Ok(Ok(Resolution::Terminal(reason))) => check.terminal(&reason),
        Ok(Ok(Resolution::Ready(request))) => {
            if cancelled.is_cancelled() {
                return;
            }
            let current = tokio::select! {
                () = cancelled.cancelled() => return,
                current = tokio::time::timeout_at(deadline,
                    state.store.media_session_route_by_incarnation(&check.intent.incarnation_id)) => current,
            };
            if !matches!(current, Ok(Ok(Some(route))) if route.state == "active" && route.owner_node_id == state.node_id
                && route.session_id == check.session && route.playback_id == check.intent.playback_id
                && route.owner_epoch == owner_epoch && route.principal == principal && route.recipe_json == recipe_json)
            {
                return;
            }
            if cancelled.is_cancelled() {
                return;
            }
            if let Some((intent, run)) = check.ready(*request, owner_epoch) {
                resume(state, intent, run, pending);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(in crate::http::hls) async fn exercise_check<F, Fut, R>(
    state: &AppState,
    route: &MediaSessionRoute,
    recipe: &RemoteStartRequest,
    selection: &ClientSelection,
    delivered: &EffectiveSelection,
    resolve: F,
    resume: R,
) where
    F: FnOnce(AppState, PendingFelQualification) -> Fut,
    Fut: std::future::Future<Output = Result<Option<crate::transcode::SessionRequest>, String>>,
    R: FnOnce(Box<UpgradeIntent>, UpgradeRun, super::super::preparation::PendingCandidateGuard),
{
    let Some(check) = Check::take(&route.session_id, &route.incarnation_id, selection, true) else {
        return;
    };
    let inputs = ControlInputs {
        route: route.clone(),
        recipe: recipe.clone(),
        selection: selection.clone(),
        delivered: delivered.clone(),
        capabilities: None,
        previous_target_height: 1080,
    };
    check_with(
        state,
        check,
        inputs,
        |state, wait, _| async move {
            Ok(match resolve(state, wait).await {
                Ok(None) => Resolution::Pending,
                Ok(Some(request)) => Resolution::Ready(Box::new(request)),
                Err(reason) => Resolution::Terminal(reason),
            })
        },
        |_, intent, run, pending| resume(intent, run, pending),
    )
    .await;
}

#[cfg(test)]
#[path = "processed_upgrade_qualification_tests.rs"]
pub(super) mod tests;
