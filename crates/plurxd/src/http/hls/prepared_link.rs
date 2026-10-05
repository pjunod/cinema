//! Private HTTP observations; capability control remains a separate authority.
use super::*;
use crate::playback_control::{AcceptedControlFence, AcceptedControlIdentity, PreparationGate};
use crate::telemetry::NetworkIdentity;
use axum::extract::FromRequestParts;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub(super) struct HttpObservation {
    network: NetworkIdentity,
    nonce: String,
}

#[cfg(test)]
impl HttpObservation {
    pub(super) fn network_fingerprint(&self) -> &str {
        &self.network.network_fingerprint
    }
}

#[derive(Clone)]
pub(super) struct AcceptedObservation {
    pub(super) gate: Arc<dyn PreparationGate>,
    pub(super) fence: AcceptedControlFence,
    http: HttpObservation,
    session: String,
    playback: String,
}

pub(super) async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    remote: Option<std::net::SocketAddr>,
    deadline: Instant,
) -> Option<HttpObservation> {
    if Instant::now() >= deadline {
        return None;
    }
    let nonce = super::link_receipts::requested_receipt(headers)?.to_owned();
    let mut request = axum::http::Request::new(());
    *request.headers_mut() = headers.clone();
    let (mut parts, _) = request.into_parts();
    // The observation is optional advisory evidence riding on the control
    // exchange: its credential read takes the exchange's advisory share, so
    // a slow read can never spend the exchange the control answer needs.
    let user = tokio::time::timeout_at(
        crate::media_pool::advisory_share(
            deadline.into(),
            super::link_receipts::ADVISORY_EVIDENCE_STAGE,
        ),
        AuthUser::from_request_parts(&mut parts, state),
    )
    .await
    .ok()?
    .ok()?
    .0;
    if Instant::now() >= deadline {
        return None;
    }
    // This optional capability ingress has no trusted-proxy proof. Do not
    // promote caller-controlled forwarding headers into link authority.
    let mut ingress_headers = headers.clone();
    ingress_headers.remove("forwarded");
    ingress_headers.remove("x-forwarded-for");
    ingress_headers.remove("x-real-ip");
    let mut network = super::super::network::identity(&ingress_headers, remote)?;
    network.user_id = Some(user.id);
    network.credential_generation = Some(plurx_core::domain::CredentialGeneration::derive(
        user.id,
        user.created_at,
        &user.password_hash,
    ));
    Some(HttpObservation { network, nonce })
}

pub(super) async fn capture(
    state: &AppState,
    route: &MediaSessionRoute,
    request: &crate::playback_control::ControlRequestV1,
    accepted: crate::playback_control::ControlDisposition,
    http: Option<HttpObservation>,
) -> Option<AcceptedObservation> {
    if accepted != crate::playback_control::ControlDisposition::Accepted {
        return None;
    }
    let http = http?;
    if http.network.user_id != Some(route.user_id) || route.owner_node_id != state.node_id {
        return None;
    }
    let gate = state
        .transcode
        .session_preparation_gate(&route.session_id)
        .await?;
    let fence = gate
        .accepted_observation(AcceptedControlIdentity {
            generation: request.generation.clone(),
            owner_epoch: request.control_epoch,
            client_instance_id: request.client_instance_id.clone(),
            sequence: request.sequence,
            fingerprint: request.fingerprint()?,
            desired_digest: request.selection.desired().digest(),
        })
        .await?;
    Some(AcceptedObservation {
        gate,
        fence,
        http,
        session: route.session_id.clone(),
        playback: route.playback_id.clone(),
    })
}

impl AcceptedObservation {
    pub(super) async fn current_link(
        &self,
        state: &AppState,
        file: &plurx_core::domain::MediaFile,
        owner: &str,
        deadline: tokio::time::Instant,
    ) -> Option<super::link_receipts::LiveLinkProof> {
        if !self.gate.observation_is_current(self.fence.clone()).await {
            return None;
        }
        let proof = state
            .link_receipts
            .current_positive_until(
                state,
                &self.http.network,
                file,
                Some(&self.http.nonce),
                Some(&self.playback),
                Some(owner),
                deadline,
            )
            .await?;
        if proof.incumbent_session() != self.session
            || !self.gate.observation_is_current(self.fence.clone()).await
        {
            return None;
        }
        proof.transfer()?;
        Some(proof)
    }

    /// Every read of the proof (prior, cost, live link, source fence) shares
    /// the request's advisory `deadline`; a miss is Unknown and stages no
    /// proof.
    pub(super) async fn proposed_proof(
        &self,
        state: &AppState,
        file: &plurx_core::domain::MediaFile,
        request: &mut crate::transcode::SessionRequest,
        candidate: &plurx_core::playback::candidate::QualityCandidate,
        deadline: tokio::time::Instant,
    ) -> Option<PreparedProof> {
        if request.file_id != file.id {
            return None;
        }
        let context = request.candidate_context.as_ref()?;
        let owner = context.owner_node_id.as_deref()?;
        if owner != state.node_id {
            return None;
        }
        if candidate.id != context.candidate_id || candidate.recipe_digest != context.recipe_digest
        {
            return None;
        }
        if request.automatic {
            let bound = super::candidate_recovery::incumbent(
                state,
                &self.http.network,
                file,
                &self.playback,
                Some(&self.session),
                deadline,
            )
            .await?;
            let memory = tokio::time::timeout_at(
                deadline,
                state.store.candidate_recovery_memory(&bound.scope),
            )
            .await
            .ok()?
            .ok()?;
            if memory.rejected_recipes.contains(&candidate.recipe_digest) {
                return None;
            }
        }
        let cost = state
            .transcode
            .measured_candidate_cost(candidate, request, None)
            .await;
        let link = self.current_link(state, file, owner, deadline).await?;
        // Fence the source once for this proof: the trial's recorded-negative
        // check and the staged binding both derive from the same identity.
        let identity =
            super::link_receipts::SourceLinkIdentity::capture(&self.http.network, file).await?;
        let admission = match cost {
            Some(cost) => {
                if u128::from(link.transfer()?.usable_bps()?) * 10
                    < u128::from(cost.rfc_peak_bps()) * 18
                {
                    return None;
                }
                PreparedAdmission::QualifiedOutput(cost)
            }
            None if unknown_original_trial(candidate, request) => {
                // A retained negative is per-network history: consulted only
                // while `playback.network_priors` keeps it (D6). The live
                // incumbent proof above needs no such setting.
                if super::link_receipts::network_priors_enabled(state).await
                    && !super::link_receipts::admissible_for(state, &identity, candidate).await
                {
                    return None;
                }
                PreparedAdmission::UnknownOriginalTrial(Instant::now() + Duration::from_secs(15))
            }
            None => return None,
        };
        let source = identity.candidate(candidate.recipe_digest, candidate.route);
        if !self.gate.observation_is_current(self.fence.clone()).await {
            return None;
        }
        link.transfer()?;
        if let PreparedAdmission::QualifiedOutput(cost) = &admission {
            request.candidate_context.as_mut()?.retained_output = Some(cost.artifact_facts());
        }
        Some(PreparedProof {
            accepted: self.clone(),
            source,
            admission,
        })
    }
}

#[derive(Clone)]
pub(super) struct StagedProof {
    pub(super) stage: crate::playback_control::StagedObservationFence,
    pub(super) gate: Arc<dyn PreparationGate>,
    pub(super) fence: AcceptedControlFence,
    pub(super) incarnation: String,
    pub(super) owner_epoch: i64,
    pub(super) deadline_unix_ms: i64,
    pub(super) cancelled: tokio_util::sync::CancellationToken,
    pub(super) trial_deadline: Option<Instant>,
}

pub(super) struct PreparedProof {
    accepted: AcceptedObservation,
    source: plurx_core::domain::CandidateLinkBinding,
    admission: PreparedAdmission,
}

enum PreparedAdmission {
    // Keeps the exact qualified output live across staging and dispatch.
    QualifiedOutput(crate::vodserve::retained::MeasuredCandidateCostProof),
    // Stage-owned empirical observations cannot qualify a complete output.
    UnknownOriginalTrial(Instant),
}

fn unknown_original_trial(
    candidate: &plurx_core::playback::candidate::QualityCandidate,
    request: &crate::transcode::SessionRequest,
) -> bool {
    super::link_receipts::unknown_stageable_original(candidate, &[])
        && matches!(request.kind, crate::transcode::SessionKind::Copy { .. })
        && request.presentation == crate::transcode::Presentation::Vod
        && request.subtitle_burn.is_none()
        && request.candidate_context.as_ref().is_some_and(|context| {
            context.candidate_id == candidate.id
                && context.recipe_digest == candidate.recipe_digest
                && context.grade == candidate.grade
                && context.normalized_geometry == candidate.normalized_geometry
                && context.retained_output.is_none()
        })
}

impl PreparedProof {
    pub(super) async fn register(
        &self,
        state: &AppState,
        session: &str,
        incarnation: &str,
        deadline_unix_ms: i64,
        cancelled: tokio_util::sync::CancellationToken,
    ) {
        let trial_deadline = match &self.admission {
            PreparedAdmission::QualifiedOutput(_) => None,
            PreparedAdmission::UnknownOriginalTrial(deadline) => Some(*deadline),
        };
        if trial_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return;
        }
        let Ok(Some(route)) = state.store.media_session_route(session).await else {
            return;
        };
        if route.incarnation_id != incarnation
            || route.owner_node_id != state.node_id
            || route.user_id != self.source.user_id
            || route.state != "active"
            || route.publication_ready_at_ms
                != plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
        {
            return;
        }
        let owner_epoch = route.owner_epoch;
        if cancelled.is_cancelled()
            || deadline_unix_ms <= crate::media_sessions::unix_ms()
            || trial_deadline.is_some_and(|deadline| Instant::now() >= deadline)
            || !self
                .accepted
                .gate
                .observation_is_current(self.accepted.fence.clone())
                .await
            || self
                .accepted
                .gate
                .staged_observation_is_current(
                    self.accepted.fence.clone(),
                    incarnation.to_owned(),
                    deadline_unix_ms,
                )
                .await
                .is_none()
        {
            return;
        }
        let Ok(Some(file)) = state.store.get_file(self.source.file_id).await else {
            return;
        };
        let Some(source) = super::link_receipts::binding(
            &self.accepted.http.network,
            &file,
            self.source.recipe_digest,
            self.source.route,
        )
        .await
        else {
            return;
        };
        if source != self.source
            || cancelled.is_cancelled()
            || !self
                .accepted
                .gate
                .observation_is_current(self.accepted.fence.clone())
                .await
        {
            return;
        }
        let Some(stage) = self
            .accepted
            .gate
            .staged_observation_is_current(
                self.accepted.fence.clone(),
                incarnation.to_owned(),
                deadline_unix_ms,
            )
            .await
        else {
            return;
        };
        let Ok(Some(current_route)) = state.store.media_session_route(session).await else {
            return;
        };
        if current_route.incarnation_id != incarnation
            || current_route.owner_epoch != owner_epoch
            || current_route.owner_node_id != state.node_id
            || current_route.user_id != source.user_id
            || current_route.state != "active"
            || current_route.publication_ready_at_ms
                != plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
            || cancelled.is_cancelled()
            || !self.accepted.fence.still_live()
            || !stage.still_live()
            || deadline_unix_ms <= crate::media_sessions::unix_ms()
            || trial_deadline.is_some_and(|deadline| Instant::now() >= deadline)
        {
            return;
        }
        if let PreparedAdmission::QualifiedOutput(cost) = &self.admission {
            let _held = cost.artifact_facts();
        }
        state.link_receipts.register_staged(
            super::link_receipts::SessionBinding {
                source,
                session: session.to_owned(),
                incarnation: incarnation.to_owned(),
                owner_epoch,
            },
            StagedProof {
                stage,
                gate: Arc::clone(&self.accepted.gate),
                fence: self.accepted.fence.clone(),
                incarnation: incarnation.to_owned(),
                owner_epoch,
                deadline_unix_ms,
                cancelled,
                trial_deadline,
            },
        );
    }
}
