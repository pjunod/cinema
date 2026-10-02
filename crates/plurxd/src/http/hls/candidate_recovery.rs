//! Candidate failure observations are authenticated against the incumbent,
//! never reconstructed from a caller's height or recovery-cause string.
use super::StartResponse;
use crate::{media_sessions::RemoteStartRequest, state::AppState, telemetry::NetworkIdentity};
use plurx_core::{
    domain::{MediaFile, MediaSessionRoute},
    playback::candidate::QualityCandidate,
    store::CandidateRecoveryScope,
};
use std::time::Duration;

pub(super) struct AcceptedDecoderProof {
    scope: CandidateRecoveryScope,
    session: String,
    incarnation: String,
    owner_epoch: i64,
    owner: String,
    recipe: [u8; 32],
    observed: std::time::Instant,
}
impl AcceptedDecoderProof {
    pub(super) fn session(&self) -> &str {
        &self.session
    }
    pub(super) fn fresh(&self) -> bool {
        std::time::Instant::now()
            .checked_duration_since(self.observed)
            .is_some_and(|age| age <= Duration::from_secs(15))
    }
    pub(super) fn matches(&self, bound: &BoundRecovery) -> bool {
        self.fresh()
            && self.scope == bound.scope
            && self.session == bound.route.session_id
            && self.incarnation == bound.route.incarnation_id
            && self.owner_epoch == bound.route.owner_epoch
            && self.owner == bound.route.owner_node_id
            && self.recipe == bound.candidate.recipe_digest
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientRecoverySample {
    pub cause: plurx_core::store::CandidateRecoveryCause,
    pub event_id: String,
    pub candidate_id: plurx_core::playback::candidate::CandidateId,
    pub recipe_digest: [u8; 32],
    pub age_ms: u32,
    pub decoder_failed: bool,
    pub rendered_elapsed_ms: u32,
    pub position_progress_ms: u32,
    pub dropped_frames: u32,
    pub runway_ms: u32,
}

/// Best-effort fault recording cannot consume a quality response or block media.
pub(crate) async fn accept_sample(
    state: &AppState,
    network: &NetworkIdentity,
    session: Option<&str>,
    sample: &ClientRecoverySample,
) -> Option<String> {
    use plurx_core::store::{CandidateRecoveryCause, CandidateRecoveryObservation};
    if sample.age_ms > 15_000
        || uuid::Uuid::parse_str(&sample.event_id)
            .ok()
            .map(|id| id.to_string())
            .as_deref()
            != Some(sample.event_id.as_str())
        || sample.cause != CandidateRecoveryCause::Decode
        || !(sample.decoder_failed
            || (sample.rendered_elapsed_ms >= 4_000
                && sample.position_progress_ms >= 2_000
                && sample.dropped_frames >= 6
                && sample.runway_ms >= 10_000))
    {
        return None;
    }
    let observed =
        std::time::Instant::now().checked_sub(Duration::from_millis(u64::from(sample.age_ms)))?;
    tokio::time::timeout(Duration::from_millis(100), async {
        let session = session?;
        let route = state.store.media_session_route(session).await.ok()??;
        if route.user_id != network.user_id? || route.recipe_json.len() > 64 * 1024 {
            return None;
        }
        let recipe: RemoteStartRequest = serde_json::from_str(&route.recipe_json).ok()?;
        let file = state.store.get_file(recipe.request.file_id).await.ok()??;
        let bound = incumbent(state, network, &file, &route.playback_id, Some(session)).await?;
        if bound.candidate.id != sample.candidate_id
            || bound.candidate.recipe_digest != sample.recipe_digest
        {
            return None;
        }
        state
            .store
            .observe_candidate_recovery(
                &CandidateRecoveryObservation {
                    scope: bound.scope.clone(),
                    route: bound.route.clone(),
                    recipe_digest: sample.recipe_digest,
                    event_id: sample.event_id.clone(),
                    cause: sample.cause,
                    quality_step: false,
                },
                super::unix_ms(),
            )
            .await
            .ok()??;
        // Reconstruct again after the durable await; stale accepted writes
        // remain memory, never a live admission proof for another attachment.
        let current = incumbent(state, network, &file, &route.playback_id, Some(session)).await?;
        let proof = AcceptedDecoderProof {
            scope: bound.scope,
            session: bound.route.session_id,
            incarnation: bound.route.incarnation_id,
            owner_epoch: bound.route.owner_epoch,
            owner: bound.route.owner_node_id,
            recipe: sample.recipe_digest,
            observed,
        };
        if !proof.matches(&current) {
            return None;
        }
        state.link_receipts.record_decoder(proof)?;
        Some(sample.event_id.clone())
    })
    .await
    .ok()
    .flatten()
}

/// Consume a typed cause only after the create's durable request was acquired.
/// Replays cannot spend the single decoder-quality response again.
pub(super) async fn observe(
    state: &AppState,
    network: Option<&NetworkIdentity>,
    file: Option<&MediaFile>,
    request: &crate::transcode::SessionRequest,
    nonce: Option<&str>,
    event: &str,
) -> Result<(), String> {
    use crate::transcode::ReopenReason;
    use plurx_core::store::{CandidateRecoveryCause as Cause, CandidateRecoveryObservation};
    let Some(reason) = request.reopen_reason else {
        return Ok(());
    };
    if reason == ReopenReason::Stall {
        return Ok(());
    }
    let context = request
        .candidate_context
        .as_ref()
        .ok_or("typed recovery requires a full candidate")?;
    let network = network.ok_or("typed recovery has no authenticated client namespace")?;
    let file = file.ok_or("typed recovery has no current source")?;
    let previous = request
        .previous_session_id
        .as_deref()
        .ok_or("typed recovery has no incumbent")?;
    let bound = incumbent(state, network, file, &request.playback_id, Some(previous))
        .await
        .ok_or("typed recovery incumbent proof is unavailable")?;
    let changed = context.recipe_digest != bound.candidate.recipe_digest;
    let cause = match reason {
        ReopenReason::Link => {
            let proof = state
                .link_receipts
                .current_negative(state, network, file, nonce, &request.playback_id)
                .await
                .ok_or("Link recovery has no fresh incumbent negative proof")?;
            if proof.incumbent_session() != previous
                || proof.incumbent_recipe().0 != bound.candidate.recipe_digest
            {
                return Err("Link recovery proof belongs to another candidate".into());
            }
            Cause::Link
        }
        ReopenReason::Encode => {
            if !state
                .transcode
                .candidate_production_proof(bound.candidate.recipe_digest)
                .is_some_and(|speed| speed > 0 && speed < 1000)
            {
                return Err("Encode recovery has no fresh active producer-pressure proof".into());
            }
            Cause::Encode
        }
        ReopenReason::Decode => {
            if !state.link_receipts.decoder_current(&bound) {
                return Err("Decode recovery has no accepted current decoder evidence".into());
            }
            let memory = tokio::time::timeout(
                Duration::from_millis(100),
                state.store.candidate_recovery_memory(&bound.scope),
            )
            .await
            .map_err(|_| "decoder memory read timed out")?
            .map_err(|_| "decoder memory read failed")?;
            if changed && memory.decode_step_recipe.is_some() {
                return Err(
                    "decoder quality response already belongs to the compatibility owner".into(),
                );
            }
            Cause::Decode
        }
        ReopenReason::Hold => {
            if changed {
                return Err("Hold cannot change candidate".into());
            }
            Cause::Hold
        }
        ReopenReason::Authority => {
            if changed {
                return Err("Authority cannot change candidate".into());
            }
            Cause::Authority
        }
        ReopenReason::Stall => unreachable!(),
    };
    if cause != Cause::Decode {
        // Link and producer evidence have their own live proof owners. Hold and
        // authority are not failure ceilings. Never spend decoder memory merely
        // to record another observational recovery with the same recipe.
        let current = incumbent(state, network, file, &request.playback_id, Some(previous))
            .await
            .ok_or("typed recovery incumbent changed during proof validation")?;
        if current.scope != bound.scope
            || current.route.incarnation_id != bound.route.incarnation_id
            || current.route.owner_epoch != bound.route.owner_epoch
            || current.candidate.id != bound.candidate.id
        {
            return Err("typed recovery incumbent changed during proof validation".into());
        }
        return Ok(());
    }
    let now = super::unix_ms();
    let current = incumbent(state, network, file, &request.playback_id, Some(previous))
        .await
        .ok_or("decoder incumbent changed before response admission")?;
    if !state.link_receipts.decoder_current(&current)
        || current.scope != bound.scope
        || current.route.incarnation_id != bound.route.incarnation_id
        || current.route.owner_epoch != bound.route.owner_epoch
    {
        return Err("decoder evidence changed before response admission".into());
    }
    let observation = CandidateRecoveryObservation {
        scope: bound.scope,
        route: bound.route,
        recipe_digest: bound.candidate.recipe_digest,
        event_id: event.to_owned(),
        cause,
        quality_step: cause == Cause::Decode && changed,
    };
    let recorded = tokio::time::timeout(
        Duration::from_millis(100),
        state.store.observe_candidate_recovery(&observation, now),
    )
    .await
    .map_err(|_| "candidate recovery write timed out")?
    .map_err(|_| "candidate recovery write failed")?;
    recorded.ok_or_else(|| "candidate recovery was duplicate, stale or unavailable".to_owned())?;
    Ok(())
}

pub(super) struct BoundRecovery {
    pub(super) scope: CandidateRecoveryScope,
    pub(super) route: MediaSessionRoute,
    pub(super) candidate: QualityCandidate,
}

pub(crate) async fn auto_catalog(
    state: &AppState,
    network: Option<&NetworkIdentity>,
    file: &MediaFile,
    playback: &str,
    catalog: Vec<QualityCandidate>,
) -> Vec<QualityCandidate> {
    let Some(network) = network else {
        return catalog;
    };
    let Some(bound) = incumbent(state, network, file, playback, None).await else {
        return catalog;
    };
    let memory = tokio::time::timeout(
        Duration::from_millis(100),
        state.store.candidate_recovery_memory(&bound.scope),
    )
    .await;
    let Ok(Ok(memory)) = memory else {
        return catalog;
    };
    // A read cannot transfer a failure ceiling to a replacement attachment.
    let current = incumbent(
        state,
        network,
        file,
        playback,
        Some(&bound.route.session_id),
    )
    .await;
    if !current.is_some_and(|current| {
        current.scope == bound.scope
            && current.route.incarnation_id == bound.route.incarnation_id
            && current.candidate.id == bound.candidate.id
    }) {
        return catalog;
    }
    catalog
        .into_iter()
        .filter(|candidate| !memory.rejected_recipes.contains(&candidate.recipe_digest))
        .collect()
}

/// A Decision has no playback identity in its public intent. Only its exact
/// installed-incumbent receipt may identify the lifetime to read; absent live
/// proof keeps the ordinary catalog, never borrows a namespace's latest player.
pub(crate) async fn decision_catalog(
    state: &AppState,
    network: Option<&NetworkIdentity>,
    file: &MediaFile,
    nonce: Option<&str>,
    catalog: Vec<QualityCandidate>,
) -> Vec<QualityCandidate> {
    let Some(network) = network else {
        return catalog;
    };
    let Some(proof) = state
        .link_receipts
        .current_positive(state, network, file, nonce, None, Some(&state.node_id))
        .await
    else {
        return catalog;
    };
    let Ok(Ok(Some(route))) = tokio::time::timeout(
        Duration::from_millis(100),
        state.store.media_session_route(proof.incumbent_session()),
    )
    .await
    else {
        return catalog;
    };
    if proof.transfer().is_none() {
        return catalog;
    }
    let filtered = auto_catalog(
        state,
        Some(network),
        file,
        &route.playback_id,
        catalog.clone(),
    )
    .await;
    let Some(current) = state
        .link_receipts
        .current_positive(
            state,
            network,
            file,
            nonce,
            Some(&route.playback_id),
            Some(&state.node_id),
        )
        .await
    else {
        return catalog;
    };
    if current.incumbent_session() != proof.incumbent_session() {
        return catalog;
    }
    filtered
}

/// Missing/legacy/remote facts are Unknown, not an ordinary playback refusal.
pub(super) async fn incumbent(
    state: &AppState,
    network: &NetworkIdentity,
    file: &MediaFile,
    playback: &str,
    previous: Option<&str>,
) -> Option<BoundRecovery> {
    tokio::time::timeout(Duration::from_millis(100), async {
        let user = network.user_id?;
        let route = state
            .store
            .media_session_route_for_playback(user, playback)
            .await
            .ok()??;
        if route.owner_node_id != state.node_id
            || route.state != "active"
            || route.publication_ready_at_ms != 0
            || route.lease_expires_at_ms <= super::unix_ms()
            || previous.is_some_and(|id| id != route.session_id)
            || route.recipe_json.len() > 64 * 1024
            || route.response_json.len() > 1024 * 1024
        {
            return None;
        }
        let recipe: RemoteStartRequest = serde_json::from_str(&route.recipe_json).ok()?;
        if recipe.user_id != user
            || recipe.request.file_id != file.id
            || recipe.source_size != file.size
            || recipe.source_mtime != file.mtime
            || recipe.incarnation_id != route.incarnation_id
            || recipe.request.playback_id != playback
        {
            return None;
        }
        let response: StartResponse = serde_json::from_str(&route.response_json).ok()?;
        let id = response.quality_candidate_id?;
        if recipe.candidate_id != Some(id) || response.session_id != route.session_id {
            return None;
        }
        let candidates = response.quality_candidates?;
        if candidates.len() > 64 {
            return None;
        }
        let mut matches = candidates
            .into_iter()
            .filter(|candidate| candidate.id == id);
        let candidate = matches.next()?;
        if matches.next().is_some() || !candidate.identity_matches() {
            return None;
        }
        let source =
            super::link_receipts::binding(network, file, candidate.recipe_digest, candidate.route)
                .await?;
        // Source work may await. A retired/replaced incumbent cannot mint memory.
        let current = state
            .store
            .media_session_route_for_playback(user, playback)
            .await
            .ok()??;
        if current.incarnation_id != route.incarnation_id
            || current.session_id != route.session_id
            || current.owner_node_id != route.owner_node_id
            || current.owner_epoch != route.owner_epoch
            || current.recovery_epoch != route.recovery_epoch
            || current.recipe_json != route.recipe_json
            || current.state != "active"
            || current.publication_ready_at_ms != 0
            || current.lease_expires_at_ms <= super::unix_ms()
        {
            return None;
        }
        let scope = CandidateRecoveryScope {
            user_id: user,
            playback_id: playback.to_owned(),
            recovery_epoch: route.recovery_epoch.clone(),
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            source_object_version: source.source_object_version,
            credential_generation: source.credential_generation,
            client_class: source.client_class,
        };
        scope.valid().then_some(BoundRecovery {
            scope,
            route,
            candidate,
        })
    })
    .await
    .ok()
    .flatten()
}
