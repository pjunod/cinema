//! Actual Source-owned VOD observations. This is a private metrics projection,
//! never a Local session or a constructor for Source authority.
use super::*;

trait StatusHooks: std::any::Any + Send + Sync {
    fn observed(&self) -> crate::seam_hooks::HookFuture<'_>;
    fn reading(&self) -> crate::seam_hooks::HookFuture<'_>;
    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any;
}
struct NoStatusHooks;
impl StatusHooks for NoStatusHooks {
    fn observed(&self) -> crate::seam_hooks::HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }
    fn reading(&self) -> crate::seam_hooks::HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }
    #[cfg(test)]
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
static NO_CONTROL_HOOKS: NoStatusHooks = NoStatusHooks;
pub(super) struct SourceStatusHookOwner {
    slot: crate::seam_hooks::HookSlot<dyn StatusHooks>,
}
impl Default for SourceStatusHookOwner {
    fn default() -> Self {
        Self {
            slot: crate::seam_hooks::HookSlot::new(&NO_CONTROL_HOOKS),
        }
    }
}
#[cfg(test)]
#[derive(Default)]
struct PausingStatusHooks {
    observed: crate::seam_hooks::PauseSlot,
    reading: crate::seam_hooks::PauseSlot,
}
#[cfg(test)]
impl StatusHooks for PausingStatusHooks {
    fn observed(&self) -> crate::seam_hooks::HookFuture<'_> {
        self.observed.hold()
    }
    fn reading(&self) -> crate::seam_hooks::HookFuture<'_> {
        self.reading.hold()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
#[cfg(test)]
impl SourceStatusHookOwner {
    pub(super) fn pause(&self, after_read: bool) -> Arc<crate::seam_hooks::AsyncPause> {
        let hooks = self
            .slot
            .get_or_install(|| Box::new(PausingStatusHooks::default()))
            .as_any()
            .downcast_ref::<PausingStatusHooks>()
            .expect("Source status test hooks");
        if !after_read {
            hooks.observed.arm("source_status_observed")
        } else {
            hooks.reading.arm("source_status_reading")
        }
    }
}

#[derive(serde::Serialize)]
pub(crate) struct SourceVodStatus {
    pub(crate) active_encode_milli_realtime: Option<u32>,
    pub(crate) active_encode_age_ms: Option<u32>,
    pub(crate) active_encode_active_ms: Option<u32>,
    pub(crate) active_encode_segments: Option<u32>,
    pub(crate) active_encode_candidate_id: Option<plurx_core::playback::candidate::CandidateId>,
    pub(crate) target_height: i64,
    pub(crate) encoder: &'static str,
    pub(crate) tone_map_peak_nits: Option<u32>,
    pub(crate) tone_map_peak_source: Option<&'static str>,
    pub(crate) playlist_shape: &'static str,
    pub(crate) producer_state: &'static str,
    pub(crate) producer_hold: Option<&'static str>,
    pub(crate) producer_decision: Option<&'static str>,
    pub(crate) control_demand: Option<&'static str>,
    pub(crate) reported_position_ms: Option<i64>,
    pub(crate) client_runway_ms: Option<i64>,
    pub(crate) render_state: Option<&'static str>,
    pub(crate) server_ready_state: &'static str,
    pub(crate) server_ready_anchor_ms: Option<i64>,
    pub(crate) server_ready_end_ms: Option<i64>,
    pub(crate) server_ready_seconds: Option<f64>,
    pub(crate) server_next_ready_start_ms: Option<i64>,
    pub(crate) server_next_ready_end_ms: Option<i64>,
    pub(crate) published_end_ms: Option<i64>,
    pub(crate) ready_ahead_end_ms: Option<i64>,
    pub(crate) fetched_end_ms: i64,
    pub(crate) fetched_segment: Option<i64>,
    pub(crate) ahead_seconds: Option<i64>,
    pub(crate) materialized_segments: usize,
    pub(crate) planned_segments: usize,
    pub(crate) materialized_bytes: u64,
    pub(crate) planned_bytes: u64,
    pub(crate) working_set_bytes: u64,
    pub(crate) working_set_budget_bytes: u64,
    pub(crate) completed_cache_bytes: u64,
    pub(crate) admitted: bool,
    pub(crate) delivered_bytes: i64,
    pub(crate) delivered_bps: Option<i64>,
    pub(crate) delivered_idle_ms: i64,
    pub(crate) http_wait_count: usize,
    pub(crate) http_wait_oldest_ms: Option<i64>,
    pub(crate) http_wait_segment: Option<i64>,
    pub(crate) status_generated_unix_ms: i64,
    pub(crate) suspended: bool,
    #[serde(rename = "final")]
    pub(crate) final_: bool,
}

impl SourceVodStatus {
    fn from_actual(info: crate::vodserve::VodSessionInfo) -> Result<Self, SourceWorkerError> {
        if info.target_height <= 0
            || info.target_height > 16384
            || info
                .server_ready_seconds
                .is_some_and(|value| !value.is_finite() || value < 0.0)
            || info.delivered_bytes < 0
            || info.delivered_bps.is_some_and(|value| value < 0)
            || info.delivered_idle_ms < 0
            || info.fetched_end_ms < 0
            || info.status_generated_unix_ms < 0
            || [
                info.reported_position_ms,
                info.client_runway_ms,
                info.server_ready_anchor_ms,
                info.server_ready_end_ms,
                info.server_next_ready_start_ms,
                info.server_next_ready_end_ms,
                info.published_end_ms,
                info.ready_ahead_end_ms,
                info.fetched_segment,
                info.ahead_seconds,
                info.http_wait_oldest_ms,
                info.http_wait_segment,
            ]
            .into_iter()
            .flatten()
            .any(|value| value < 0)
            || [
                Some(info.encoder),
                Some(info.playlist_shape),
                Some(info.producer_state),
                info.producer_hold,
                info.producer_decision,
                info.control_demand,
                info.render_state,
                Some(info.server_ready_state),
                info.tone_map_peak_source,
            ]
            .into_iter()
            .flatten()
            .any(|text| text.len() > 32)
        {
            return Err(SourceWorkerError::Unavailable);
        }
        Ok(Self {
            active_encode_milli_realtime: info.active_encode_milli_realtime,
            active_encode_age_ms: info.active_encode_age_ms,
            active_encode_active_ms: info.active_encode_active_ms,
            active_encode_segments: info.active_encode_segments,
            active_encode_candidate_id: info.active_encode_candidate_id,
            target_height: info.target_height,
            encoder: info.encoder,
            tone_map_peak_nits: info.tone_map_peak_nits,
            tone_map_peak_source: info.tone_map_peak_source,
            playlist_shape: info.playlist_shape,
            producer_state: info.producer_state,
            producer_hold: info.producer_hold,
            producer_decision: info.producer_decision,
            control_demand: info.control_demand,
            reported_position_ms: info.reported_position_ms,
            client_runway_ms: info.client_runway_ms,
            render_state: info.render_state,
            server_ready_state: info.server_ready_state,
            server_ready_anchor_ms: info.server_ready_anchor_ms,
            server_ready_end_ms: info.server_ready_end_ms,
            server_ready_seconds: info.server_ready_seconds,
            server_next_ready_start_ms: info.server_next_ready_start_ms,
            server_next_ready_end_ms: info.server_next_ready_end_ms,
            published_end_ms: info.published_end_ms,
            ready_ahead_end_ms: info.ready_ahead_end_ms,
            fetched_end_ms: info.fetched_end_ms,
            fetched_segment: info.fetched_segment,
            ahead_seconds: info.ahead_seconds,
            materialized_segments: info.materialized_segments,
            planned_segments: info.planned_segments,
            materialized_bytes: info.materialized_bytes,
            planned_bytes: info.planned_bytes,
            working_set_bytes: info.working_set_bytes,
            working_set_budget_bytes: info.working_set_budget_bytes,
            completed_cache_bytes: info.completed_cache_bytes,
            admitted: info.admitted,
            delivered_bytes: info.delivered_bytes,
            delivered_bps: info.delivered_bps,
            delivered_idle_ms: info.delivered_idle_ms,
            http_wait_count: info.http_wait_count,
            http_wait_oldest_ms: info.http_wait_oldest_ms,
            http_wait_segment: info.http_wait_segment,
            status_generated_unix_ms: info.status_generated_unix_ms,
            suspended: info.suspended,
            final_: info.final_,
        })
    }
}

#[allow(dead_code)] // Private /vod-status transport follows actor qualification.
pub(crate) struct SourceOpenedStatus {
    status: SourceVodStatus,
    guard: SourceResponseGuard,
}
impl SourceOpenedStatus {
    /// Keep the real guard through the entire response Body and accepted writer.
    #[allow(dead_code)] // Private /vod-status transport follows actor qualification.
    pub(crate) fn into_parts(self) -> (SourceVodStatus, SourceResponseGuard) {
        (self.status, self.guard)
    }
}

impl SourceViewerActor {
    /// The owned observation retains its Body obligation through VOD's nested
    /// metadata jobs even if its HTTP waiter disappears. It never touches the
    /// viewer demand, accepted control sequence, or VOD inactivity clock.
    #[allow(dead_code)] // Private /vod-status transport follows actor qualification.
    pub(crate) async fn open_status(
        &self,
        deadline: Instant,
    ) -> Result<SourceOpenedStatus, SourceWorkerError> {
        let actor = self.clone();
        let observed = tokio::spawn(Box::pin(async move {
            let (response, guard) = actor.open_start_response(deadline).await?;
            let manager = actor
                .0
                .manager
                .upgrade()
                .ok_or(SourceWorkerError::Unavailable)?;
            let publication = manager
                .vod
                .status_publication(&response.session_id)
                .await
                .ok_or(SourceWorkerError::Unavailable)?;
            let status = publication
                .result
                .map_err(|_| SourceWorkerError::Unavailable)?;
            if status.id != response.session_id {
                return Err(SourceWorkerError::Unavailable);
            }
            actor.0.status_hooks.slot.get().reading().await;
            let proof = actor.0.gate.current_owned(&actor.0.assignment).await?;
            actor.0.status_hooks.slot.get().observed().await;
            if Instant::now() >= deadline
                || proof
                    .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
                    .is_err()
                || guard
                    .source
                    .as_ref()
                    .is_none_or(|source| !source.unchanged())
                || actor
                    .0
                    .state
                    .lock()
                    .expect("Source worker state")
                    .retirement_requested
            {
                return Err(SourceWorkerError::Unavailable);
            }
            let route = actor.0.gate.renew_with(&actor.0.assignment, &proof).await?;
            if Instant::now() >= deadline
                || proof
                    .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
                    .is_err()
                || route.session_id != response.session_id
                || route.incarnation_id != actor.0.assignment.binding().incarnation_id().to_string()
                || route.publication_ready_at_ms != 0
                || route.response_json
                    != serde_json::to_string(&response)
                        .map_err(|_| SourceWorkerError::Unavailable)?
                || actor
                    .0
                    .state
                    .lock()
                    .expect("Source worker state")
                    .retirement_requested
                || guard
                    .source
                    .as_ref()
                    .is_none_or(|source| !source.unchanged())
                || !manager
                    .vod
                    .source_status_owner_is_current(
                        &response.session_id,
                        &publication.owner,
                        &actor.0.assignment,
                    )
                    .await
            {
                return Err(SourceWorkerError::Unavailable);
            }
            // Owner validation may park on the real lifecycle gate. Recheck
            // the original observation and physical fence after the last await.
            if Instant::now() >= deadline
                || proof
                    .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
                    .is_err()
                || guard
                    .source
                    .as_ref()
                    .is_none_or(|source| !source.unchanged())
                || actor
                    .0
                    .state
                    .lock()
                    .expect("Source worker state")
                    .retirement_requested
            {
                return Err(SourceWorkerError::Unavailable);
            }
            let status = SourceVodStatus::from_actual(status)?;
            Ok(SourceOpenedStatus { status, guard })
        }));
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), observed)
            .await
            .map_err(|_| SourceWorkerError::Deadline)?
            .map_err(|_| SourceWorkerError::Unresolved)?
    }
}
