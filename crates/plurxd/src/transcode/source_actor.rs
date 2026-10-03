//! Owned Source producer admission and lifetime. HTTP callers only wait.
use super::*;

/// Actual daemon components retained by immutable Source renditions. Only
/// the owned worker factory constructs this gate; there is no wire constructor.
pub(crate) struct SourceProducerAuthority {
    store: Arc<dyn Store>,
    membership: plurx_core::cluster::membership::MembershipManager,
    master: Arc<plurx_core::secrets::CredentialKey>,
}

pub(crate) struct SourceGenerationAuthority {
    proofs: Vec<plurx_core::sharing_source_sessions::SourcePublicationAuthority>,
}

impl SourceGenerationAuthority {
    pub(crate) fn validate_before_spawn(&self) -> Result<(), String> {
        let now = crate::fragment_index_cluster::unix_ms();
        for proof in &self.proofs {
            proof
                .validate_observation_freshness(now)
                .map_err(|_| "Source producer observation expired before spawn".to_owned())?;
        }
        Ok(())
    }
}

impl SourceProducerAuthority {
    pub(crate) async fn authorize_generation(
        &self,
        assignments: &[plurx_core::sharing_source_sessions::SourceDispatchAssignment],
    ) -> Result<SourceGenerationAuthority, String> {
        if assignments.is_empty() || assignments.len() > 8 {
            return Err("Source producer assignment bound exceeded".into());
        }
        use plurx_core::sharing_source_sessions::SourcePublicationAuthorityRead;
        let members = self
            .membership
            .observe_source_admission_members()
            .await
            .map_err(|_| "Source member observation failed".to_owned())?
            .ok_or_else(|| "Source member floor is unavailable".to_owned())?;
        let mut proofs = Vec::with_capacity(assignments.len());
        for assignment in assignments {
            match self
                .store
                .prepare_source_publication_authority(assignment, &self.master, &members)
                .await
                .map_err(|_| "Source producer authority read failed".to_owned())?
            {
                SourcePublicationAuthorityRead::Ready(proof) => proofs.push(*proof),
                SourcePublicationAuthorityRead::Unavailable
                | SourcePublicationAuthorityRead::Capacity => {}
            }
        }
        if proofs.is_empty() {
            return Err("Source producer has no current authorized viewer".to_owned());
        }
        let authority = SourceGenerationAuthority { proofs };
        authority.validate_before_spawn()?;
        Ok(authority)
    }
}

impl TranscodeManager {
    /// The same actual admission/materialization allowance used by the Source
    /// actor and its peer start handler. Transport adds its separate margin.
    /// A response deadline never certifies producer or writer settlement.
    #[allow(dead_code)] // The qualified peer start consumer is being integrated.
    pub(crate) async fn source_worker_start_budget(
        &self,
        prepared: &crate::http::hls::PreparedSourcePlayback,
    ) -> Result<Duration, String> {
        self.source_start_budget_for_request(prepared.request())
            .await
    }

    async fn source_start_budget_for_request(
        &self,
        request: &SessionRequest,
    ) -> Result<Duration, String> {
        let settings = self.vod_settings(request).await?.ok_or_else(|| {
            vod_refusal_error(
                "vod_disabled",
                "VOD presentation is disabled for maintenance",
            )
        })?;
        Ok(crate::admission::QUEUE_WAIT + settings.materialize_budget)
    }
}

#[cfg(test)]
#[path = "tests/source_actor.rs"]
mod tests;

use plurx_core::{
    domain::{
        MediaSessionActivation, MediaSessionEnd, MediaSessionRenewal, MediaSessionRoute,
        MEDIA_SESSION_PUBLICATION_BLOCKED,
    },
    sharing_resources::{SharingHlsResource, SharingHlsResourceKind},
    sharing_source_sessions::{
        SourceDispatchAssignment, SourceOwnedRouteAuthorityRead, SourcePublicationAuthorityRead,
        SourceReleaseOutcome, SourceSessionWriteAuthority, SourceWriteAuthorityRead,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceWorkerError {
    Unavailable,
    Capacity,
    Conflict,
    Deadline,
    Unresolved,
    Unsupported,
}

#[derive(Default)]
pub(super) struct SourceWorkerRegistry {
    entries: std::sync::Mutex<Vec<Arc<SourceViewerInner>>>,
}
struct SourceViewerInner {
    assignment: SourceDispatchAssignment,
    manager: std::sync::Weak<TranscodeManager>,
    gate: Arc<SourceProducerAuthority>,
    state: std::sync::Mutex<SourceViewerState>,
    changed: tokio::sync::Notify,
}
enum SourcePhysicalSettlement {
    // Only this detached task can reach the no-admission branch; Source
    // preparation queues no child and its real permit has already dropped.
    NeverAdmitted,
    Registered(Box<crate::vodserve::SourceProducerAssociationsSettled>),
}
struct SourceViewerState {
    start: Option<Result<crate::http::hls::StartResponse, SourceWorkerError>>,
    retirement_requested: bool,
    settled: Option<Result<(), SourceWorkerError>>,
    bodies: usize,
    planned_session: Option<String>,
}

/// A waiter for one actual registered owner. Dropping this value cannot cancel
/// its detached producer or imply release of its durable dispatch obligation.
#[derive(Clone)]
pub(crate) struct SourceViewerActor(Arc<SourceViewerInner>);

pub(crate) enum SourceResourcePayload {
    Playlist(Vec<u8>),
    File(crate::vodserve::SegmentReady),
}
pub(crate) struct SourceOpenedResource {
    payload: SourceResourcePayload,
    guard: SourceResponseGuard,
}
impl SourceOpenedResource {
    /// The transport must move the guard into its actual Body, including empty
    /// or failed bodies. Header construction alone cannot drop this barrier.
    pub(crate) fn into_parts(self) -> (SourceResourcePayload, SourceResponseGuard) {
        (self.payload, self.guard)
    }
}
pub(crate) struct SourceResponseGuard {
    owner: Arc<SourceViewerInner>,
    source: Option<crate::fragment_index_cluster::SourceFence>,
}
impl Drop for SourceResponseGuard {
    fn drop(&mut self) {
        drop(self.source.take());
        let mut state = self.owner.state.lock().expect("Source worker state");
        state.bodies -= 1;
        drop(state);
        self.owner.changed.notify_waiters();
    }
}
impl SourceResponseGuard {
    pub(crate) async fn cancelled(&self) {
        loop {
            let notification = self.owner.changed.notified();
            tokio::pin!(notification);
            notification.as_mut().enable();
            if self
                .owner
                .state
                .lock()
                .expect("Source worker state")
                .retirement_requested
            {
                return;
            }
            notification.await;
        }
    }
}
impl SourceViewerActor {
    /// Transport bookkeeping only. This observes the real physical/body/SQL
    /// result; a missing actor, lease expiry or acknowledgement cannot mint it.
    pub(crate) fn settlement_status(&self) -> Option<Result<(), SourceWorkerError>> {
        self.0.state.lock().expect("Source worker state").settled
    }
    pub(crate) async fn wait_ready(
        &self,
        deadline: Instant,
    ) -> Result<crate::http::hls::StartResponse, SourceWorkerError> {
        loop {
            let notification = self.0.changed.notified();
            tokio::pin!(notification);
            notification.as_mut().enable();
            {
                let state = self.0.state.lock().expect("Source worker state");
                if let Some(Err(error)) = &state.start {
                    return Err(*error);
                }
                if state.retirement_requested {
                    return Err(SourceWorkerError::Unavailable);
                }
                if let Some(start) = &state.start {
                    return start.clone();
                }
            }
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), notification)
                .await
                .map_err(|_| SourceWorkerError::Deadline)?;
        }
    }
    pub(crate) async fn retire(&self) -> Result<(), SourceWorkerError> {
        self.request_retirement();
        loop {
            let notification = self.0.changed.notified();
            tokio::pin!(notification);
            notification.as_mut().enable();
            if let Some(result) = self.0.state.lock().expect("Source worker state").settled {
                return result;
            }
            notification.await;
        }
    }
    fn request_retirement(&self) {
        self.0
            .state
            .lock()
            .expect("Source worker state")
            .retirement_requested = true;
        self.0.changed.notify_waiters();
    }
    /// Metadata retries have a real Body obligation but do not represent
    /// viewer media activity and therefore never touch the VOD idle clock.
    pub(crate) async fn open_start_response(
        &self,
        deadline: Instant,
    ) -> Result<(crate::http::hls::StartResponse, SourceResponseGuard), SourceWorkerError> {
        let response = self.wait_ready(deadline).await?;
        let manager = self
            .0
            .manager
            .upgrade()
            .ok_or(SourceWorkerError::Unavailable)?;
        {
            let mut state = self.0.state.lock().expect("Source worker state");
            if state.retirement_requested
                || state.bodies >= 64
                || state
                    .start
                    .as_ref()
                    .and_then(|start| start.as_ref().ok())
                    .is_none_or(|start| start.session_id != response.session_id)
            {
                return Err(SourceWorkerError::Unavailable);
            }
            state.bodies += 1;
        }
        let mut guard = SourceResponseGuard {
            owner: Arc::clone(&self.0),
            source: None,
        };
        let facts = manager
            .vod
            .hls_facts(&response.session_id)
            .await
            .ok_or(SourceWorkerError::Unavailable)?;
        guard.source = Some(
            manager
                .vod
                .source_response_physical_fence(&facts.response_owner)
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?,
        );
        let proof = self.0.gate.current_owned(&self.0.assignment).await?;
        let route = self.0.gate.renew_with(&self.0.assignment, &proof).await?;
        if route.session_id != response.session_id
            || route.publication_ready_at_ms != 0
            || route.response_json
                != serde_json::to_string(&response).map_err(|_| SourceWorkerError::Unavailable)?
            || self
                .0
                .state
                .lock()
                .expect("Source worker state")
                .retirement_requested
            || !manager
                .vod
                .response_owner_is_live(&response.session_id, &facts.response_owner)
                .await
            || guard
                .source
                .as_ref()
                .is_none_or(|source| !source.unchanged())
        {
            return Err(SourceWorkerError::Unavailable);
        }
        Ok((response, guard))
    }
    pub(crate) async fn open_resource(
        &self,
        resource: &SharingHlsResource,
        deadline: Instant,
    ) -> Result<SourceOpenedResource, SourceWorkerError> {
        // Native wrappers, subtitles and diagnostics need their own admitted
        // producers/representation builders; this first copy actor is plain VOD.
        if resource.as_str().contains('?')
            || !matches!(
                resource.kind(),
                SharingHlsResourceKind::Index
                    | SharingHlsResourceKind::Init
                    | SharingHlsResourceKind::MediaSegment
            )
        {
            return Err(SourceWorkerError::Unsupported);
        }
        let manager = self
            .0
            .manager
            .upgrade()
            .ok_or(SourceWorkerError::Unavailable)?;
        let session_id = {
            let mut state = self.0.state.lock().expect("Source worker state");
            if state.retirement_requested || state.bodies >= 64 {
                return Err(SourceWorkerError::Unavailable);
            }
            let session_id = state
                .start
                .as_ref()
                .and_then(|start| start.as_ref().ok())
                .ok_or(SourceWorkerError::Unavailable)?
                .session_id
                .clone();
            state.bodies += 1;
            session_id
        };
        let mut guard = SourceResponseGuard {
            owner: Arc::clone(&self.0),
            source: None,
        };
        let initial = self.0.gate.current_owned(&self.0.assignment).await?;
        initial
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| SourceWorkerError::Unavailable)?;
        let (payload, owner) = match resource.kind() {
            SharingHlsResourceKind::Index => {
                let publication = manager
                    .vod
                    .playlist(&session_id)
                    .await
                    .ok_or(SourceWorkerError::Unavailable)?;
                (
                    SourceResourcePayload::Playlist(
                        publication
                            .result
                            .map_err(|_| SourceWorkerError::Unavailable)?,
                    ),
                    publication.owner,
                )
            }
            SharingHlsResourceKind::Init | SharingHlsResourceKind::MediaSegment => {
                let publication = manager
                    .vod
                    .segment_before(&session_id, resource.as_str(), Some(deadline))
                    .await
                    .ok_or(SourceWorkerError::Unavailable)?;
                (
                    SourceResourcePayload::File(
                        publication
                            .result
                            .map_err(|_| SourceWorkerError::Unavailable)?
                            .ok_or(SourceWorkerError::Unavailable)?,
                    ),
                    publication.owner,
                )
            }
            _ => return Err(SourceWorkerError::Unsupported),
        };
        guard.source = Some(
            manager
                .vod
                .source_response_physical_fence(&owner)
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?,
        );
        // A parked segment can outlive the first observation's five-second
        // window. Re-observe real membership/current binding after the wait.
        let proof = self.0.gate.current_owned(&self.0.assignment).await?;
        proof
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| SourceWorkerError::Unavailable)?;
        if !manager
            .vod
            .response_owner_is_live(&session_id, &owner)
            .await
            || self
                .0
                .state
                .lock()
                .expect("Source worker state")
                .retirement_requested
        {
            return Err(SourceWorkerError::Unavailable);
        }
        // Couple delivery liveness to current Source authority; the response
        // token is checked again immediately before its Local VOD touch.
        let route = self.0.gate.renew_with(&self.0.assignment, &proof).await?;
        if route.session_id != session_id
            || !manager
                .vod
                .commit_resolved_media(&session_id, &owner, None)
                .await
        {
            return Err(SourceWorkerError::Unavailable);
        }
        if guard
            .source
            .as_ref()
            .is_none_or(|source| !source.unchanged())
        {
            return Err(SourceWorkerError::Unavailable);
        }
        Ok(SourceOpenedResource { payload, guard })
    }
}
impl SourceProducerAuthority {
    async fn current_owned(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> Result<
        Box<plurx_core::sharing_source_sessions::SourceOwnedRouteAuthority>,
        SourceWorkerError,
    > {
        let members = self
            .membership
            .observe_source_admission_members()
            .await
            .map_err(|_| SourceWorkerError::Unavailable)?
            .ok_or(SourceWorkerError::Unavailable)?;
        match self
            .store
            .prepare_source_owned_route_authority(assignment, &self.master, &members)
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?
        {
            SourceOwnedRouteAuthorityRead::Ready(proof) => Ok(proof),
            _ => Err(SourceWorkerError::Unavailable),
        }
    }
    async fn renew_with(
        &self,
        assignment: &SourceDispatchAssignment,
        proof: &plurx_core::sharing_source_sessions::SourceOwnedRouteAuthority,
    ) -> Result<MediaSessionRoute, SourceWorkerError> {
        let incarnation = assignment.binding().incarnation_id().to_string();
        let route = self
            .store
            .media_session_route_by_incarnation(&incarnation)
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?
            .ok_or(SourceWorkerError::Unavailable)?;
        let now = crate::fragment_index_cluster::unix_ms();
        self.store
            .renew_source_media_session(
                proof,
                &MediaSessionRenewal {
                    incarnation_id: incarnation,
                    owner_epoch: route.owner_epoch,
                    produced_playable_through_ms: route.produced_playable_through_ms,
                    fetched_through_ms: route.fetched_through_ms,
                    media_sequence: route.media_sequence,
                },
                now,
                now.saturating_add(60_000),
            )
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?
            .ok_or(SourceWorkerError::Unavailable)
    }
}

impl TranscodeManager {
    pub(crate) fn lookup_source_worker(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> Option<SourceViewerActor> {
        self.source_workers
            .entries
            .lock()
            .expect("Source workers")
            .iter()
            .find(|owner| owner.assignment.same_identity(assignment))
            .map(|owner| SourceViewerActor(Arc::clone(owner)))
    }
    /// Registry insertion precedes every await. The detached task alone owns
    /// no-spawn/registered retirement; HTTP futures only retain waiters.
    pub(crate) async fn start_source_worker(
        self: &Arc<Self>,
        state: Arc<crate::state::AppState>,
        assignment: SourceDispatchAssignment,
        activation: SourceSessionWriteAuthority,
        prepared: crate::http::hls::PreparedSourcePlayback,
        deadline: Instant,
    ) -> Result<SourceViewerActor, SourceWorkerError> {
        if !Arc::ptr_eq(&self.store, &state.store)
            || !prepared.matches_assignment(&assignment)
            || !activation.assignment().same_identity(&assignment)
        {
            return Err(SourceWorkerError::Conflict);
        }
        let mut registry = self.source_workers.entries.lock().expect("Source workers");
        if let Some(owner) = registry
            .iter()
            .find(|owner| owner.assignment.same_identity(&assignment))
        {
            return Ok(SourceViewerActor(Arc::clone(owner)));
        }
        if registry.len() >= 8 {
            return Err(SourceWorkerError::Capacity);
        }
        let owner = Arc::new(SourceViewerInner {
            assignment,
            manager: Arc::downgrade(self),
            gate: Arc::new(SourceProducerAuthority {
                store: Arc::clone(&self.store),
                membership: state.membership.clone(),
                master: Arc::clone(&state.sharing.key),
            }),
            state: std::sync::Mutex::new(SourceViewerState {
                start: None,
                retirement_requested: false,
                settled: None,
                bodies: 0,
                planned_session: None,
            }),
            changed: tokio::sync::Notify::new(),
        });
        registry.push(Arc::clone(&owner));
        drop(registry);
        let actor = SourceViewerActor(Arc::clone(&owner));
        let manager = Arc::clone(self);
        tokio::spawn(Box::pin(async move {
            manager
                .run_source_owner(owner, state, prepared, deadline)
                .await;
        }));
        Ok(actor)
    }

    async fn run_source_owner(
        self: Arc<Self>,
        owner: Arc<SourceViewerInner>,
        state: Arc<crate::state::AppState>,
        prepared: crate::http::hls::PreparedSourcePlayback,
        deadline: Instant,
    ) {
        let actor = SourceViewerActor(Arc::clone(&owner));
        let mut reserved: Option<crate::vodserve::ReservedSourceCopyRendition> = None;
        let mut unowned_existing = true;
        let start = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            Box::pin(async {
                if Instant::now() >= deadline {
                    return Err(SourceWorkerError::Deadline);
                }
                if self
                    .store
                    .media_session_route_by_incarnation(
                        &owner.assignment.binding().incarnation_id().to_string(),
                    )
                    .await
                    .map_err(|_| SourceWorkerError::Unresolved)?
                    .is_some()
                {
                    unowned_existing = true;
                    return Err(SourceWorkerError::Unresolved);
                }
                unowned_existing = false;
                if prepared.native_subtitles().0
                    || prepared.native_subtitles().1.is_some()
                    || prepared.request().subtitle_burn.is_some()
                    || prepared.request().previous_session_id.is_some()
                    || prepared.request().reopen_reason.is_some()
                    || !matches!(prepared.request().kind, SessionKind::Copy { .. })
                {
                    return Err(SourceWorkerError::Unsupported);
                }
                let settings = self
                    .vod_settings(prepared.request())
                    .await
                    .map_err(|_| SourceWorkerError::Unavailable)?
                    .ok_or(SourceWorkerError::Unavailable)?;
                let admitted = Box::pin(self.vod.prepare_admitted_source_copy(
                    &prepared,
                    &owner.assignment,
                    &settings,
                    &self.admissions,
                    self.store.as_ref(),
                    deadline,
                ))
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?;
                reserved = Some(
                    self.vod
                        .reserve_source_copy(admitted)
                        .map_err(|_| SourceWorkerError::Unavailable)?,
                );
                if owner
                    .state
                    .lock()
                    .expect("Source worker state")
                    .retirement_requested
                {
                    return Err(SourceWorkerError::Unavailable);
                }
                let reservation = reserved.as_mut().expect("Source reservation");
                let session_id = uuid::Uuid::new_v4().to_string();
                let facts = reservation
                    .start_info(&session_id)
                    .map_err(|_| SourceWorkerError::Unavailable)?;
                let incarnation = owner.assignment.binding().incarnation_id().to_string();
                let response =
                    Box::pin(prepared.start_response(&state, facts.start_info(), &incarnation, 1))
                        .await
                        .map_err(|_| SourceWorkerError::Unavailable)?;
                // Capacity waiting invalidates the caller's earlier snapshot. Mint
                // actual current authority after admission and response construction.
                let members = owner
                    .gate
                    .membership
                    .observe_source_admission_members()
                    .await
                    .map_err(|_| SourceWorkerError::Unavailable)?
                    .ok_or(SourceWorkerError::Unavailable)?;
                let SourceWriteAuthorityRead::Ready(authority) = self
                    .store
                    .prepare_source_activation_authority(
                        &owner.assignment,
                        &owner.gate.master,
                        &members,
                    )
                    .await
                    .map_err(|_| SourceWorkerError::Unresolved)?
                else {
                    return Err(SourceWorkerError::Unavailable);
                };
                let binding = owner.assignment.binding();
                let now = crate::fragment_index_cluster::unix_ms();
                let activation = MediaSessionActivation {
                    incarnation_id: incarnation,
                    session_id: session_id.clone(),
                    principal: binding.principal().clone(),
                    playback_id: prepared.request().playback_id.clone(),
                    recovery_epoch: String::new(),
                    expected_predecessor_incarnation_id: None,
                    fence_predecessor: true,
                    request_id: Some(binding.request_id().into()),
                    request_fingerprint: prepared.fingerprint().into(),
                    owner_node_id: owner.assignment.owner_node_id().into(),
                    recipe_json: serde_json::to_string(prepared.request())
                        .map_err(|_| SourceWorkerError::Unavailable)?,
                    response_json: serde_json::to_string(&response)
                        .map_err(|_| SourceWorkerError::Unavailable)?,
                    publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
                    media_origin_ms: response.media_origin_ms.unwrap_or(0),
                    now_ms: now,
                    lease_expires_at_ms: now.saturating_add(60_000),
                    expected_desired_revision: None,
                };
                owner
                    .state
                    .lock()
                    .expect("Source worker state")
                    .planned_session = Some(session_id.clone());
                let outcome = self
                    .store
                    .activate_source_media_session(&authority, &activation)
                    .await
                    .map_err(|_| SourceWorkerError::Unresolved)?
                    .ok_or(SourceWorkerError::Unavailable)?;
                if outcome.route.session_id != session_id {
                    return Err(SourceWorkerError::Conflict);
                }
                authority
                    .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
                    .map_err(|_| SourceWorkerError::Unavailable)?;
                if owner
                    .state
                    .lock()
                    .expect("Source worker state")
                    .retirement_requested
                {
                    return Err(SourceWorkerError::Unavailable);
                }
                Box::pin(self.vod.commit_source_copy(
                    reservation,
                    &session_id,
                    &owner.gate,
                    &self.admissions,
                ))
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?;
                let physical = Box::pin(reservation.wait_ready(deadline))
                    .await
                    .map_err(|_| SourceWorkerError::Unavailable)?;
                if !physical.matches(&owner.assignment) {
                    return Err(SourceWorkerError::Conflict);
                }
                let members = owner
                    .gate
                    .membership
                    .observe_source_admission_members()
                    .await
                    .map_err(|_| SourceWorkerError::Unavailable)?
                    .ok_or(SourceWorkerError::Unavailable)?;
                let SourcePublicationAuthorityRead::Ready(proof) = self
                    .store
                    .prepare_source_publication_authority(
                        &owner.assignment,
                        &owner.gate.master,
                        &members,
                    )
                    .await
                    .map_err(|_| SourceWorkerError::Unresolved)?
                else {
                    return Err(SourceWorkerError::Unavailable);
                };
                let route = self
                    .store
                    .complete_source_media_session_publication(&proof)
                    .await
                    .map_err(|_| SourceWorkerError::Unresolved)?
                    .ok_or(SourceWorkerError::Unavailable)?;
                proof
                    .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
                    .map_err(|_| SourceWorkerError::Unavailable)?;
                if route.session_id != response.session_id || route.publication_ready_at_ms != 0 {
                    return Err(SourceWorkerError::Conflict);
                }
                Ok(response)
            }),
        )
        .await
        .unwrap_or(Err(SourceWorkerError::Deadline));
        owner.state.lock().expect("Source worker state").start = Some(start.clone());
        owner.changed.notify_waiters();
        if start.is_ok() {
            let mut renewal = tokio::time::interval(Duration::from_secs(10));
            renewal.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            renewal.tick().await;
            loop {
                let notification = owner.changed.notified();
                tokio::pin!(notification);
                notification.as_mut().enable();
                if owner
                    .state
                    .lock()
                    .expect("Source worker state")
                    .retirement_requested
                {
                    break;
                }
                tokio::select! {
                    _ = notification => {},
                    _ = renewal.tick() => {
                        let session_id = owner.state.lock().expect("Source worker state").start.as_ref().and_then(|start| start.as_ref().ok()).map(|response| response.session_id.clone());
                        if let Some(session_id) = session_id {
                            if self.vod.recovered_start(&session_id).await.is_none() { break; }
                            let Some(facts) = self.vod.hls_facts(&session_id).await else {break};
                            if self.vod.source_response_physical_fence(&facts.response_owner).await.is_err() {break;}
                        }
                        match owner.gate.current_owned(&owner.assignment).await {
                            Ok(proof) if owner.gate.renew_with(&owner.assignment, &proof).await.is_ok() => {},
                            _ => break,
                        }
                    }
                }
            }
        }
        actor.request_retirement();
        let mut physical = None;
        let settlement = loop {
            let result = if unowned_existing {
                Err(SourceWorkerError::Unresolved)
            } else {
                Box::pin(self.settle_source_owner(&owner, &mut reserved, &mut physical)).await
            };
            owner.state.lock().expect("Source worker state").settled = Some(result);
            owner.changed.notify_waiters();
            if result.is_ok() || unowned_existing {
                break result;
            }
            // A Store fault retains the actual owner and sealed physical
            // settlement evidence. Retry SQL without inventing a new producer.
            tokio::time::sleep(Duration::from_secs(5)).await;
        };
        // Genuine unknown outcomes remain bounded in the registry. Neither
        // caller cancellation nor absent in-memory producers releases a row.
        if settlement.is_ok() {
            self.source_workers
                .entries
                .lock()
                .expect("Source workers")
                .retain(|entry| !Arc::ptr_eq(entry, &owner));
        }
    }
    async fn settle_source_owner(
        &self,
        owner: &Arc<SourceViewerInner>,
        reserved: &mut Option<crate::vodserve::ReservedSourceCopyRendition>,
        physical: &mut Option<SourcePhysicalSettlement>,
    ) -> Result<(), SourceWorkerError> {
        if physical.is_none() {
            *physical = Some(if let Some(reserved) = reserved.as_mut() {
                let receipt = Box::pin(reserved.retire(&self.vod))
                    .await
                    .map_err(|_| SourceWorkerError::Unresolved)?;
                if !receipt.matches(&owner.assignment) {
                    return Err(SourceWorkerError::Unresolved);
                }
                SourcePhysicalSettlement::Registered(Box::new(receipt))
            } else {
                SourcePhysicalSettlement::NeverAdmitted
            });
        }
        if matches!(physical, Some(SourcePhysicalSettlement::Registered(receipt)) if !receipt.matches(&owner.assignment))
        {
            return Err(SourceWorkerError::Unresolved);
        }
        let incarnation = owner.assignment.binding().incarnation_id().to_string();
        let route = self
            .store
            .media_session_route_by_incarnation(&incarnation)
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?;
        let terminal = if let Some(route) = route {
            if Some(&route.session_id)
                != owner
                    .state
                    .lock()
                    .expect("Source worker state")
                    .planned_session
                    .as_ref()
                || route.owner_epoch != 1
                || route.owner_node_id != owner.assignment.owner_node_id()
                || route.principal != *owner.assignment.binding().principal()
            {
                return Err(SourceWorkerError::Unresolved);
            }
            if route.state == "ended" {
                Some(route)
            } else {
                Some(
                    self.store
                        .end_media_session_if_owner(&MediaSessionEnd {
                            incarnation_id: route.incarnation_id,
                            session_id: route.session_id,
                            expected_owner_node_id: route.owner_node_id,
                            expected_owner_epoch: route.owner_epoch,
                            expected_lease_expires_at_ms: route.lease_expires_at_ms,
                            terminal_reason: "deleted".into(),
                            now_ms: crate::fragment_index_cluster::unix_ms(),
                        })
                        .await
                        .map_err(|_| SourceWorkerError::Unresolved)?
                        .ok_or(SourceWorkerError::Unresolved)?,
                )
            }
        } else {
            None
        };
        loop {
            let notification = owner.changed.notified();
            tokio::pin!(notification);
            notification.as_mut().enable();
            if owner.state.lock().expect("Source worker state").bodies == 0 {
                break;
            }
            notification.await;
        }
        let released = if let Some(terminal) = terminal {
            self.store
                .settle_source_terminal_worker(&owner.assignment, &terminal)
                .await
        } else {
            self.store
                .settle_source_assigned_without_activation(&owner.assignment)
                .await
        }
        .map_err(|_| SourceWorkerError::Unresolved)?;
        if matches!(
            released,
            SourceReleaseOutcome::Released | SourceReleaseOutcome::ExactReplay
        ) {
            Ok(())
        } else {
            Err(SourceWorkerError::Unresolved)
        }
    }
}
