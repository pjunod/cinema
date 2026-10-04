//! Current-rendition Source controls. Only the actual viewer actor constructs
//! this authority; wire fields cannot mint its assignment or physical fence.
use super::*;
use crate::playback_control::{
    ControlRequestV1, ControlStateError, LocalControlRequest, LocalControlResult, PlaybackDemand,
    PlaybackDemandSnapshot, PreparedSuccessorObservation,
};

/// One owned current-rendition exchange. `route` and `start` are the exact
/// Source facts the acceptance was fenced against; the HTTP adapter projects
/// the wire response from them and never from a stored or caller value.
pub(crate) struct SourceOpenedControl {
    result: Result<LocalControlResult, ControlStateError>,
    guard: SourceResponseGuard,
    route: MediaSessionRoute,
    start: crate::http::hls::StartResponse,
}
pub(crate) struct SourceControlParts {
    pub(crate) result: Result<LocalControlResult, ControlStateError>,
    pub(crate) guard: SourceResponseGuard,
    pub(crate) route: MediaSessionRoute,
    pub(crate) start: crate::http::hls::StartResponse,
}
impl SourceOpenedControl {
    pub(crate) fn into_parts(
        self,
    ) -> (
        Result<LocalControlResult, ControlStateError>,
        SourceResponseGuard,
    ) {
        (self.result, self.guard)
    }
    /// The HTTP adapter projects its wire answer from these exact facts.
    pub(crate) fn into_wire_parts(self) -> SourceControlParts {
        SourceControlParts {
            result: self.result,
            guard: self.guard,
            route: self.route,
            start: self.start,
        }
    }
}

trait ControlHooks: std::any::Any + Send + Sync {
    fn observed(&self) -> crate::seam_hooks::HookFuture<'_>;
    fn accepting(&self) -> crate::seam_hooks::HookFuture<'_>;
    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any;
}
struct NoControlHooks;
impl ControlHooks for NoControlHooks {
    fn observed(&self) -> crate::seam_hooks::HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }
    fn accepting(&self) -> crate::seam_hooks::HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }
    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
static NO_CONTROL_HOOKS: NoControlHooks = NoControlHooks;
pub(super) struct SourceControlHookOwner {
    slot: crate::seam_hooks::HookSlot<dyn ControlHooks>,
}
impl Default for SourceControlHookOwner {
    fn default() -> Self {
        Self {
            slot: crate::seam_hooks::HookSlot::new(&NO_CONTROL_HOOKS),
        }
    }
}
#[cfg(test)]
#[derive(Default)]
struct PausingControlHooks {
    observed: crate::seam_hooks::PauseSlot,
    accepting: crate::seam_hooks::PauseSlot,
}
#[cfg(test)]
impl ControlHooks for PausingControlHooks {
    fn observed(&self) -> crate::seam_hooks::HookFuture<'_> {
        self.observed.hold()
    }
    fn accepting(&self) -> crate::seam_hooks::HookFuture<'_> {
        self.accepting.hold()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
#[cfg(test)]
impl SourceControlHookOwner {
    pub(super) fn pause(&self, before_write: bool) -> Arc<crate::seam_hooks::AsyncPause> {
        let hooks = self
            .slot
            .get_or_install(|| Box::new(PausingControlHooks::default()))
            .as_any()
            .downcast_ref::<PausingControlHooks>()
            .expect("Source control test hooks");
        if before_write {
            hooks.observed.arm("source_control_observed")
        } else {
            hooks.accepting.arm("source_control_accepting")
        }
    }
}

pub(crate) struct SourceControlAuthority<'a> {
    actor: SourceViewerActor,
    source: &'a crate::fragment_index_cluster::SourceFence,
    proof: Option<Box<plurx_core::sharing_source_sessions::SourceOwnedRouteAuthority>>,
    deadline: Instant,
}
impl SourceControlAuthority<'_> {
    pub(crate) fn assignment(&self) -> &SourceDispatchAssignment {
        &self.actor.0.assignment
    }
    /// Called while the actual VOD session lifecycle gate is held. A fresh
    /// observation and guarded write precede any sequence/activity mutation.
    pub(crate) async fn refresh(
        &mut self,
        control: &LocalControlRequest<'_>,
    ) -> Result<(), ControlStateError> {
        self.validate_physical()?;
        let proof = self
            .actor
            .0
            .gate
            .current_owned(self.assignment())
            .await
            .map_err(|_| ControlStateError::Unavailable)?;
        self.actor.0.control_hooks.slot.get().observed().await;
        self.validate_physical()?;
        let route = self
            .actor
            .0
            .gate
            .renew_with(self.assignment(), &proof)
            .await
            .map_err(|_| ControlStateError::Unavailable)?;
        if route.session_id != control.session_id
            || route.incarnation_id != control.generation
            || route.owner_node_id != control.owner_node_id
            || u64::try_from(route.owner_epoch).ok() != Some(control.owner_epoch)
            || route.publication_ready_at_ms != 0
        {
            return Err(ControlStateError::Unavailable);
        }
        self.proof = Some(proof);
        self.validate()
    }
    fn validate_physical(&self) -> Result<(), ControlStateError> {
        if Instant::now() >= self.deadline
            || !self.source.unchanged()
            || self
                .actor
                .0
                .state
                .lock()
                .expect("Source worker state")
                .retirement_requested
        {
            return Err(ControlStateError::Unavailable);
        }
        Ok(())
    }
    pub(crate) async fn before_acceptance(&self) {
        self.actor.0.control_hooks.slot.get().accepting().await;
    }
    /// Rechecked after reader-lock acquisition, immediately before acceptance.
    pub(crate) fn validate(&self) -> Result<(), ControlStateError> {
        self.validate_physical()?;
        if self.proof.as_ref().is_none_or(|proof| {
            proof
                .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
                .is_err()
        }) {
            return Err(ControlStateError::Unavailable);
        }
        Ok(())
    }
}

impl SourceViewerActor {
    pub(crate) async fn control(
        &self,
        request: ControlRequestV1,
        deadline: Instant,
    ) -> Result<SourceOpenedControl, SourceWorkerError> {
        if Instant::now() >= deadline {
            return Err(SourceWorkerError::Deadline);
        }
        if request.demand == PlaybackDemand::End
            || request.acknowledgement.is_some()
            || request.intent.is_some()
            || self
                .0
                .original_selection
                .as_ref()
                .is_none_or(|selection| *selection != request.selection.desired())
        {
            return Err(SourceWorkerError::Unsupported);
        }
        // Validate immutable wire identity before any authority extension.
        let response = self.wait_ready(deadline).await?;
        request
            .validate(response.duration_ms, self.0.control_target_duration_ms)
            .map_err(|_| SourceWorkerError::Conflict)?;
        let bootstrap = response
            .control
            .as_ref()
            .ok_or(SourceWorkerError::Unavailable)?;
        if request.generation != bootstrap.generation
            || request.control_epoch != bootstrap.control_epoch
        {
            return Err(SourceWorkerError::Conflict);
        }
        if Instant::now() >= deadline {
            return Err(SourceWorkerError::Deadline);
        }
        let (current_response, guard) = self.open_start_response(deadline).await?;
        if current_response.session_id != response.session_id {
            return Err(SourceWorkerError::Unavailable);
        }
        let manager = self
            .0
            .manager
            .upgrade()
            .ok_or(SourceWorkerError::Unavailable)?;
        let proof = self.0.gate.current_owned(&self.0.assignment).await?;
        let route = self.0.gate.renew_with(&self.0.assignment, &proof).await?;
        let mut authority = SourceControlAuthority {
            actor: self.clone(),
            source: guard
                .source
                .as_ref()
                .ok_or(SourceWorkerError::Unavailable)?,
            proof: None,
            deadline,
        };
        let control = LocalControlRequest {
            session_id: &response.session_id,
            generation: &request.generation,
            owner_node_id: &route.owner_node_id,
            owner_epoch: request.control_epoch,
            client_instance_id: &request.client_instance_id,
            sequence: request.sequence,
            snapshot: PlaybackDemandSnapshot::from(&request),
            prepared_successor: PreparedSuccessorObservation::Inactive,
        };
        let deadline_unix_ms = crate::media_sessions::unix_ms().saturating_add(
            i64::try_from(
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis(),
            )
            .unwrap_or(i64::MAX),
        );
        let result = manager
            .vod
            .control_source(&mut authority, control, deadline_unix_ms)
            .await
            .ok_or(SourceWorkerError::Unavailable)?;
        drop(authority);
        Ok(SourceOpenedControl {
            result,
            guard,
            route,
            start: response,
        })
    }
}
