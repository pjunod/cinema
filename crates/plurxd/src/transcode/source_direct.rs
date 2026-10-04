//! Owned Source direct play. The same admission, assignment, lease, body
//! ledger, retirement and settlement as a Source VOD owner, with no producer:
//! the held file is the fence. HTTP callers only wait.
use super::*;
use crate::http::sharing_direct_wire::SourceDirectStart;

/// A published direct session with no open body and no new byte request for
/// this long retires, the same idle bound the VOD presentation uses. Status
/// renews the lease but is never viewer activity.
pub(crate) const SOURCE_DIRECT_IDLE: Duration = Duration::from_secs(300);

/// The file a direct owner may serve, frozen by current Source authority at
/// preparation. Only the HTTP preparation constructs it; nothing on the wire
/// can. Every open re-proves the same object without following links.
pub(crate) struct SourceDirectFile {
    file: plurx_core::domain::MediaFile,
    object_version: String,
    length: u64,
    mime: &'static str,
    recipe_json: String,
}
impl SourceDirectFile {
    pub(crate) fn new(
        file: plurx_core::domain::MediaFile,
        object_version: String,
        length: u64,
        mime: &'static str,
        recipe_json: String,
    ) -> Self {
        Self {
            file,
            object_version,
            length,
            mime,
            recipe_json,
        }
    }
    async fn fence(&self) -> Result<crate::fragment_index_cluster::SourceFence, SourceWorkerError> {
        let fence = crate::fragment_index_cluster::open_source_playback_fence(
            &self.file,
            Some(&self.object_version),
        )
        .await
        .map_err(|_| SourceWorkerError::Unavailable)?;
        if fence
            .handle
            .metadata()
            .map_err(|_| SourceWorkerError::Unavailable)?
            .len()
            != self.length
        {
            return Err(SourceWorkerError::Unavailable);
        }
        Ok(fence)
    }
}

/// One admitted direct byte open: a private read descriptor of the fenced
/// object and the response guard that holds the fence and the body credit.
pub(crate) struct SourceDirectOpened {
    pub(crate) file: std::fs::File,
    pub(crate) length: u64,
    pub(crate) mime: &'static str,
    guard: SourceResponseGuard,
}
impl SourceDirectOpened {
    /// The transport must move the guard into its actual Body, including
    /// empty bodies (HEAD and 416).
    pub(crate) fn into_parts(self) -> (std::fs::File, u64, &'static str, SourceResponseGuard) {
        (self.file, self.length, self.mime, self.guard)
    }
}

impl SourceViewerActor {
    pub(crate) fn is_direct(&self) -> bool {
        self.0.direct.is_some()
    }
    async fn wait_direct_ready(
        &self,
        deadline: Instant,
    ) -> Result<SourceDirectStart, SourceWorkerError> {
        if self.0.direct.is_none() {
            return Err(SourceWorkerError::Unsupported);
        }
        loop {
            let notification = self.0.changed.notified();
            tokio::pin!(notification);
            notification.as_mut().enable();
            {
                let state = self.0.state.lock().expect("Source worker state");
                if state.retirement_requested {
                    return Err(SourceWorkerError::Unavailable);
                }
                if let Some(start) = &state.direct_start {
                    return start.clone();
                }
            }
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), notification)
                .await
                .map_err(|_| SourceWorkerError::Deadline)?;
        }
    }
    /// Reserve one body credit against this owner. Refused once retirement
    /// is requested, so a retiring owner's settlement waits on a finite set.
    fn reserve_direct_body(
        &self,
        start: &SourceDirectStart,
    ) -> Result<SourceResponseGuard, SourceWorkerError> {
        let mut state = self.0.state.lock().expect("Source worker state");
        if state.retirement_requested
            || state.bodies >= 64
            || state
                .direct_start
                .as_ref()
                .and_then(|current| current.as_ref().ok())
                != Some(start)
        {
            return Err(SourceWorkerError::Unavailable);
        }
        state.bodies += 1;
        drop(state);
        Ok(SourceResponseGuard {
            owner: Arc::clone(&self.0),
            source: None,
        })
    }
    /// The published direct session for Start replay and status. It renews
    /// the Source lease under fresh owned-route authority and re-proves the
    /// file, but is never viewer activity.
    pub(crate) async fn open_direct_start(
        &self,
        deadline: Instant,
    ) -> Result<(SourceDirectStart, SourceResponseGuard), SourceWorkerError> {
        let start = self.wait_direct_ready(deadline).await?;
        let direct = self
            .0
            .direct
            .clone()
            .ok_or(SourceWorkerError::Unsupported)?;
        let mut guard = self.reserve_direct_body(&start)?;
        guard.source = Some(direct.fence().await?);
        let proof = self.0.gate.current_owned(&self.0.assignment).await?;
        let route = self.0.gate.renew_with(&self.0.assignment, &proof).await?;
        if route.session_id != start.session_id
            || route.publication_ready_at_ms != 0
            || route.response_json
                != serde_json::to_string(&start).map_err(|_| SourceWorkerError::Unavailable)?
            || self.retirement_requested()
            || guard
                .source
                .as_ref()
                .is_none_or(|source| !source.unchanged())
        {
            return Err(SourceWorkerError::Unavailable);
        }
        Ok((start, guard))
    }
    fn retirement_requested(&self) -> bool {
        self.0
            .state
            .lock()
            .expect("Source worker state")
            .retirement_requested
    }
    /// Open the fenced object for one admitted byte request. The open runs in
    /// an owned task, so a dropped HTTP waiter cannot abandon its body credit
    /// or nested Store observations.
    pub(crate) async fn open_direct(
        &self,
        deadline: Instant,
    ) -> Result<SourceDirectOpened, SourceWorkerError> {
        let actor = self.clone();
        let job = tokio::spawn(Box::pin(
            async move { actor.open_direct_owned(deadline).await },
        ));
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), job)
            .await
            .map_err(|_| SourceWorkerError::Deadline)?
            .map_err(|_| SourceWorkerError::Unresolved)?
    }
    async fn open_direct_owned(
        &self,
        deadline: Instant,
    ) -> Result<SourceDirectOpened, SourceWorkerError> {
        if Instant::now() >= deadline {
            return Err(SourceWorkerError::Deadline);
        }
        let direct = self
            .0
            .direct
            .clone()
            .ok_or(SourceWorkerError::Unsupported)?;
        let start = self
            .0
            .state
            .lock()
            .expect("Source worker state")
            .direct_start
            .clone()
            .ok_or(SourceWorkerError::Unavailable)??;
        let mut guard = self.reserve_direct_body(&start)?;
        let initial = self.0.gate.current_owned(&self.0.assignment).await?;
        initial
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| SourceWorkerError::Unavailable)?;
        let fence = direct.fence().await?;
        let file = fence
            .handle
            .try_clone()
            .map_err(|_| SourceWorkerError::Unavailable)?;
        guard.source = Some(fence);
        // The open can park on a cold disk. Re-observe real membership and the
        // current binding after it, then couple delivery to a lease renewal.
        let proof = self.0.gate.current_owned(&self.0.assignment).await?;
        if Instant::now() >= deadline
            || proof
                .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
                .is_err()
            || self.retirement_requested()
            || guard
                .source
                .as_ref()
                .is_none_or(|source| !source.unchanged())
        {
            return Err(SourceWorkerError::Unavailable);
        }
        let route = self.0.gate.renew_with(&self.0.assignment, &proof).await?;
        if route.session_id != start.session_id
            || guard
                .source
                .as_ref()
                .is_none_or(|source| !source.unchanged())
        {
            return Err(SourceWorkerError::Unavailable);
        }
        self.0
            .state
            .lock()
            .expect("Source worker state")
            .direct_activity = Some(Instant::now());
        Ok(SourceDirectOpened {
            file,
            length: direct.length,
            mime: direct.mime,
            guard,
        })
    }
}

impl TranscodeManager {
    /// Registry insertion precedes every await, into the same bounded registry
    /// as VOD owners, so direct sessions consume the same Source slots.
    pub(crate) fn start_source_direct_worker(
        self: &Arc<Self>,
        state: Arc<crate::state::AppState>,
        assignment: SourceDispatchAssignment,
        activation: SourceSessionWriteAuthority,
        prepared: crate::http::shared_source_playback::direct::PreparedSourceDirect,
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
            control_target_duration_ms: 0,
            control_hooks: Default::default(),
            status_hooks: Default::default(),
            resource_hooks: Arc::new(Default::default()),
            original_selection: None,
            assignment,
            manager: Arc::downgrade(self),
            gate: Arc::new(SourceProducerAuthority {
                store: Arc::clone(&self.store),
                membership: state.membership.clone(),
                master: Arc::clone(&state.sharing.key),
            }),
            direct: Some(Arc::new(prepared.into_direct_file())),
            state: std::sync::Mutex::new(SourceViewerState {
                start: None,
                retirement_requested: false,
                settled: None,
                finished: false,
                bodies: 0,
                planned_session: None,
                native: None,
                direct_start: None,
                direct_activity: None,
            }),
            changed: tokio::sync::Notify::new(),
        });
        registry.push(Arc::clone(&owner));
        drop(registry);
        let actor = SourceViewerActor(Arc::clone(&owner));
        let manager = Arc::clone(self);
        // Owner: this detached task; it ends on retirement, idle, lease loss
        // or drain and always settles the row and leaves the registry.
        tokio::spawn(Box::pin(async move {
            manager
                .run_source_direct_owner(owner, state, deadline)
                .await;
        }));
        Ok(actor)
    }

    async fn publish_source_direct(
        &self,
        owner: &Arc<SourceViewerInner>,
        direct: &SourceDirectFile,
        unowned_existing: &mut bool,
    ) -> Result<SourceDirectStart, SourceWorkerError> {
        let incarnation = owner.assignment.binding().incarnation_id().to_string();
        if self
            .store
            .media_session_route_by_incarnation(&incarnation)
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?
            .is_some()
        {
            return Err(SourceWorkerError::Unresolved);
        }
        *unowned_existing = false;
        drop(direct.fence().await?);
        let session_id = uuid::Uuid::new_v4().to_string();
        let start = SourceDirectStart {
            session_id: session_id.clone(),
            control_epoch: 1,
            length: direct.length,
            mime: direct.mime.to_owned(),
        };
        let members = owner
            .gate
            .membership
            .observe_source_admission_members()
            .await
            .map_err(|_| SourceWorkerError::Unavailable)?
            .ok_or(SourceWorkerError::Unavailable)?;
        let SourceWriteAuthorityRead::Ready(authority) = self
            .store
            .prepare_source_activation_authority(&owner.assignment, &owner.gate.master, &members)
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
            playback_id: binding.playback_id().to_owned(),
            recovery_epoch: String::new(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: true,
            request_id: Some(binding.request_id().into()),
            request_fingerprint: binding.request_fingerprint().into(),
            owner_node_id: owner.assignment.owner_node_id().into(),
            recipe_json: direct.recipe_json.clone(),
            response_json: serde_json::to_string(&start)
                .map_err(|_| SourceWorkerError::Unavailable)?,
            publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
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
        let members = owner
            .gate
            .membership
            .observe_source_admission_members()
            .await
            .map_err(|_| SourceWorkerError::Unavailable)?
            .ok_or(SourceWorkerError::Unavailable)?;
        let SourcePublicationAuthorityRead::Ready(proof) = self
            .store
            .prepare_source_publication_authority(&owner.assignment, &owner.gate.master, &members)
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
        if route.session_id != session_id || route.publication_ready_at_ms != 0 {
            return Err(SourceWorkerError::Conflict);
        }
        Ok(start)
    }

    async fn run_source_direct_owner(
        self: Arc<Self>,
        owner: Arc<SourceViewerInner>,
        state: Arc<crate::state::AppState>,
        deadline: Instant,
    ) {
        let actor = SourceViewerActor(Arc::clone(&owner));
        let direct = Arc::clone(owner.direct.as_ref().expect("direct Source owner"));
        let mut unowned_existing = true;
        let start = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            Box::pin(self.publish_source_direct(&owner, &direct, &mut unowned_existing)),
        )
        .await
        .unwrap_or(Err(SourceWorkerError::Deadline));
        {
            let mut current = owner.state.lock().expect("Source worker state");
            current.direct_start = Some(start.clone());
            current.direct_activity = Some(Instant::now());
        }
        owner.changed.notify_waiters();
        let shutdown = state.shutdown.clone();
        if start.is_ok() {
            // The tick is the lease heartbeat and the idle check. Every open,
            // body drop and retirement request notifies `changed`.
            let mut renewal = tokio::time::interval(Duration::from_secs(10));
            renewal.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            renewal.tick().await;
            loop {
                let notification = owner.changed.notified();
                tokio::pin!(notification);
                notification.as_mut().enable();
                {
                    let current = owner.state.lock().expect("Source worker state");
                    if current.retirement_requested {
                        break;
                    }
                }
                tokio::select! {
                    _ = notification => {},
                    () = shutdown.cancelled() => break,
                    _ = renewal.tick() => {
                        let idle = {
                            let current = owner.state.lock().expect("Source worker state");
                            current.bodies == 0
                                && current
                                    .direct_activity
                                    .is_none_or(|at| at.elapsed() >= SOURCE_DIRECT_IDLE)
                        };
                        if idle {
                            break;
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
        self.finish_source_owner(
            &owner,
            shutdown,
            unowned_existing,
            None,
            SourcePreparationSettlements::default(),
        )
        .await;
    }
}
