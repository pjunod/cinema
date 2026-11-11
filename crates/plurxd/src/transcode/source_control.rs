//! Current-rendition Source controls. Only the actual viewer actor constructs
//! this authority; wire fields cannot mint its assignment or physical fence.
use super::*;
use crate::playback_control::{
    ControlRequestV1, ControlStateError, LocalControlRequest, LocalControlResult, PlaybackDemand,
    PlaybackDemandSnapshot, PreparedSuccessorObservation,
};

#[allow(dead_code)] // Source HTTP control transport follows actual actor qualification.
pub(crate) struct SourceOpenedControl {
    result: Result<LocalControlResult, ControlStateError>,
    guard: SourceResponseGuard,
    subtitle_revision: Option<String>,
    projection: Box<(
        MediaSessionRoute,
        crate::http::hls::StartResponse,
        crate::transcode::SessionRequest,
    )>,
}
impl SourceOpenedControl {
    pub(crate) fn into_response(
        self,
        request: &ControlRequestV1,
    ) -> (
        Result<crate::playback_control::ControlResponseV1, ControlStateError>,
        SourceResponseGuard,
    ) {
        let (route, start, recipe) = *self.projection;
        let response = self.result.map(|result| {
            let mut response = crate::http::hls::source_control_response(
                &route,
                &start,
                &recipe,
                request,
                &result,
                crate::media_sessions::unix_ms(),
            );
            if let Some(revision) = self.subtitle_revision {
                response.delivery.subtitle_readiness = Some("ready".into());
                response.delivery.subtitle_revision = Some(revision);
            }
            response
        });
        (response, self.guard)
    }
    #[allow(dead_code)] // Source HTTP control transport follows actual actor qualification.
    pub(crate) fn into_parts(
        self,
    ) -> (
        Result<LocalControlResult, ControlStateError>,
        SourceResponseGuard,
    ) {
        (self.result, self.guard)
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
    #[allow(dead_code)] // HTTP control transport follows the qualified actor slice.
    pub(crate) async fn control(
        &self,
        request: ControlRequestV1,
        deadline: Instant,
    ) -> Result<SourceOpenedControl, SourceWorkerError> {
        if Instant::now() >= deadline {
            return Err(SourceWorkerError::Deadline);
        }
        // A changed selection is accepted and recorded like any other ask.
        // The Source never stages a successor, so the answer says
        // `preparation: none` and a client waiting for a directed change
        // reopens at once with a fresh Start. An acknowledgement can only
        // name a slot this Source never offered, so it is refused exactly;
        // a direct owner has no rendition to control.
        if request.demand == PlaybackDemand::End
            || request.acknowledgement.is_some()
            || request.intent.is_some()
            || self.0.direct.is_some()
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
        let recipe: crate::transcode::SessionRequest =
            serde_json::from_str(&route.recipe_json).map_err(|_| SourceWorkerError::Unavailable)?;
        if !crate::media_sessions::source_session_request_is_valid(
            &recipe,
            self.0.assignment.binding().principal(),
        ) || recipe.playback_id != self.0.assignment.binding().playback_id()
            || recipe.file_id.to_string() != self.0.assignment.binding().file_id().as_str()
        {
            return Err(SourceWorkerError::Unavailable);
        }
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
        #[cfg(unix)]
        let result = if let Some(backing) = self.growing_backing() {
            backing
                .control_current(&mut authority, control, deadline_unix_ms)
                .await
        } else {
            manager
                .vod
                .control_source(&mut authority, control, deadline_unix_ms)
                .await
                .ok_or(SourceWorkerError::Unavailable)?
        };
        #[cfg(not(unix))]
        let result = manager
            .vod
            .control_source(&mut authority, control, deadline_unix_ms)
            .await
            .ok_or(SourceWorkerError::Unavailable)?;
        drop(authority);
        let subtitle_revision = {
            let state = self.0.state.lock().expect("Source native control");
            match (
                request.selection.subtitle.mode,
                request.selection.subtitle.track,
                state.native.as_ref(),
            ) {
                (crate::playback_control::SubtitleMode::Native, Some(index), Some(tracks)) => {
                    u16::try_from(index)
                        .ok()
                        .and_then(|index| tracks.revision(index))
                }
                _ => None,
            }
        };
        Ok(SourceOpenedControl {
            subtitle_revision,
            result,
            guard,
            projection: Box::new((route, current_response, recipe)),
        })
    }
}

/// One immutable physical producer; a replacement creates a fresh backing/state.
#[cfg(unix)]
#[derive(Default)]
pub(crate) struct GrowingControlState {
    pub(super) snapshot: Option<PlaybackDemandSnapshot>,
    fence: crate::playback_control::ControlState,
    presented: crate::playback_control::PresentedFrameMilestone,
    lease_expires_at_unix_ms: i64,
}

#[cfg(unix)]
impl SourceGrowingBacking {
    /// Called only by authenticated media acceptance, never readiness polling.
    pub(crate) async fn note_media_served(&self) {
        if self.is_live() {
            self.control
                .lock()
                .await
                .presented
                .activate_at(Instant::now());
        }
    }
    pub(crate) async fn control_current(
        &self,
        authority: &mut SourceControlAuthority<'_>,
        control: LocalControlRequest<'_>,
        deadline_unix_ms: i64,
    ) -> Result<LocalControlResult, ControlStateError> {
        let _lifecycle = self.lifecycle.lock().await;
        if control.session_id != self.session_id || !self.is_live() {
            return Err(ControlStateError::Unavailable);
        }
        authority.refresh(&control).await?;
        let target = control.snapshot.buffer_anchor_ms();
        let files = self.production.files.lock().await;
        let origin = files
            .media_origin_ms()
            .ok_or(ControlStateError::Unavailable)?;
        let seconds = files
            .segments()
            .iter()
            .map(|segment| segment.seconds)
            .sum::<f64>();
        let (start, end) = super::status::growing_range_bounds(origin, seconds)
            .map_err(|_| ControlStateError::Unavailable)?;
        if target < start
            || target > end
            || (target == end && control.snapshot.seek_target_ms.is_some())
            || files.segments().is_empty()
        {
            return Err(ControlStateError::Unavailable);
        }
        drop(files);
        let mut state = self.control.lock().await;
        authority.before_acceptance().await;
        authority.validate()?;
        if !self.is_live()
            || crate::media_sessions::unix_ms() >= deadline_unix_ms
            || !self.owners.contains_live_assignment(authority.assignment())
        {
            return Err(ControlStateError::Unavailable);
        }
        let (disposition, accepted_sequence, action, platform, action_suppressed) =
            state.fence.accept(
                control.generation,
                control.owner_epoch,
                control.client_instance_id,
                control.sequence,
                crate::playback_control::ControlAcceptance::observed(
                    control.snapshot.platform(),
                    &control.prepared_successor,
                    control.snapshot.acknowledgement.as_ref(),
                    control.snapshot.request_fingerprint.as_deref(),
                    &control.snapshot.selection,
                    control.snapshot.capabilities.as_ref(),
                ),
            )?;
        let accepted = disposition == crate::playback_control::ControlDisposition::Accepted;
        let selection = if accepted {
            state.fence.observe(
                &control.snapshot.selection,
                control.snapshot.capabilities.as_ref(),
            )
        } else {
            crate::playback_control::SelectionObservation::default()
        };
        let accepted_demand = accepted.then(|| control.snapshot.clone());
        if accepted {
            let now = Instant::now();
            // This backing never restarts its producer in place. Generation,
            // epoch and the fresh state bind this stable producer-attempt tag.
            state.presented.observe_control(
                now,
                control.generation,
                control.owner_epoch,
                1,
                accepted_sequence,
                &control.snapshot,
            );
            state.snapshot = Some(control.snapshot);
            state.lease_expires_at_unix_ms = crate::media_sessions::unix_ms()
                .saturating_add(i64::from(crate::playback_control::VOD_LEASE_TIMEOUT_MS));
        }
        let mark_rendered = state.presented.presented();
        let lease_expires_at_unix_ms = state.lease_expires_at_unix_ms;
        drop(state);
        if let Some(demand) = accepted_demand {
            self.observe_demand(demand)
                .await
                .map_err(|_| ControlStateError::Unavailable)?;
        }
        if mark_rendered {
            self.production
                .mark_rendered()
                .await
                .map_err(|_| ControlStateError::Unavailable)?;
        }
        let status = self
            .status_info()
            .await
            .map_err(|_| ControlStateError::Unavailable)?;
        Ok(LocalControlResult {
            disposition,
            accepted_sequence,
            action,
            action_suppressed,
            preparation_directive: None,
            lease_expires_at_unix_ms,
            lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
            lease_state: "active",
            status: crate::transcode::HlsSessionInfo::Vod(Box::new(status)),
            platform,
            terminal_handoff: None,
            terminal_commit: None,
            selection,
        })
    }
}

#[cfg(all(test, unix))]
mod growing_control_tests {
    use super::*;
    #[test]
    fn growing_presentation_needs_media_and_same_timeline_progress() {
        let mut state = GrowingControlState::default();
        let mut demand =
            PlaybackDemandSnapshot::test_default(crate::playback_control::ClientPlatform::Apple);
        let now = Instant::now();
        state
            .presented
            .observe_control(now, "range-a", 3, 1, 1, &demand);
        demand.position_ms += 300;
        state
            .presented
            .observe_control(now, "range-a", 3, 1, 2, &demand);
        assert!(
            !state.presented.presented(),
            "unserved observations cannot claim D1"
        );
        state.presented.activate_at(now);
        state
            .presented
            .observe_control(now, "range-a", 3, 1, 3, &demand);
        demand.position_ms += 300;
        state
            .presented
            .observe_control(now, "range-b", 3, 1, 4, &demand);
        assert!(
            !state.presented.presented(),
            "a different generation resets progress"
        );
        demand.seek_target_ms = Some(demand.position_ms);
        state
            .presented
            .observe_control(now, "range-b", 3, 1, 5, &demand);
        demand.seek_target_ms = None;
        state
            .presented
            .observe_control(now, "range-b", 3, 1, 6, &demand);
        demand.position_ms += 249;
        state
            .presented
            .observe_control(now, "range-b", 3, 1, 7, &demand);
        assert!(!state.presented.presented());
        demand.position_ms += 250;
        state
            .presented
            .observe_control(now, "range-b", 3, 1, 8, &demand);
        assert!(
            state.presented.presented(),
            "new accepted same-timeline progress crosses D1"
        );
    }
}
