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

#[path = "source_control.rs"]
pub(crate) mod control;

#[path = "source_status.rs"]
pub(crate) mod status;

#[path = "source_resource.rs"]
pub(crate) mod resource;

#[path = "source_direct.rs"]
pub(crate) mod direct;

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

/// How long a Source owner waits before retrying a settlement that a Store or
/// physical fault left unresolved, and how many attempts it makes in all. The
/// same bounded detached-owner shape as the rolling scratch conversion. Past
/// the bound the owner has already stopped renewing, so the media-session
/// lease sweep reclaims the row; holding a registry slot for it would only
/// refuse every later shared start on this node.
const SOURCE_SETTLEMENT_RETRY: Duration = Duration::from_secs(5);
const SOURCE_SETTLEMENT_ATTEMPTS: u32 = 24;

/// Why one settlement attempt did not release this owner's Source row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceSettlementFault {
    /// A Store or physical-retirement fault. The retained witness is still
    /// this owner's, and a later attempt can succeed with it.
    Transient,
    /// The retained witness can never settle: the durable route names another
    /// planned session, owner epoch, node or principal; a physical or
    /// preparation receipt belongs to another assignment; or the Store refused
    /// this exact immutable release. Retrying the same witness cannot change
    /// any of these.
    Mismatch,
}

#[derive(Default)]
pub(super) struct SourceWorkerRegistry {
    entries: std::sync::Mutex<Vec<Arc<SourceViewerInner>>>,
    index_hooks: Arc<crate::fragindex::SourceIndexHookOwner>,
    probe_hooks: Arc<super::source_preparation::SourceProbeHookOwner>,
    native_hooks: Arc<super::source_preparation::SourceProbeHookOwner>,
}
struct SourceViewerInner {
    control_target_duration_ms: i64,
    control_hooks: control::SourceControlHookOwner,
    status_hooks: status::SourceStatusHookOwner,
    resource_hooks: Arc<resource::SourceResourceHookOwner>,
    original_selection: Option<plurx_core::playback::DesiredSelection>,
    assignment: SourceDispatchAssignment,
    manager: std::sync::Weak<TranscodeManager>,
    gate: Arc<SourceProducerAuthority>,
    /// Present only for a direct-play owner: the fenced file it serves. Such
    /// an owner has no producer and never publishes an HLS Start.
    direct: Option<Arc<direct::SourceDirectFile>>,
    state: std::sync::Mutex<SourceViewerState>,
    changed: tokio::sync::Notify,
}
#[derive(Default)]
struct SourcePreparationSettlements {
    index: Option<crate::fragindex::SourceIndexSettlement>,
    probe: Option<super::source_preparation::SourceProbeSettlement>,
    native: Option<super::source_subtitles::SourceNativeSettlement>,
}
impl SourcePreparationSettlements {
    fn matches(&self, assignment: &SourceDispatchAssignment) -> bool {
        self.index
            .as_ref()
            .is_none_or(|receipt| receipt.matches(assignment))
            && self
                .probe
                .as_ref()
                .is_none_or(|receipt| receipt.matches(assignment))
            && self
                .native
                .as_ref()
                .is_none_or(|receipt| receipt.matches(assignment))
    }
}
enum SourcePhysicalSettlement {
    NoProducer(SourcePreparationSettlements),
    Registered(
        Box<crate::vodserve::SourceProducerAssociationsSettled>,
        SourcePreparationSettlements,
    ),
}
struct SourceViewerState {
    native: Option<Arc<super::source_subtitles::SourceNativeTracks>>,
    start: Option<Result<crate::http::hls::StartResponse, SourceWorkerError>>,
    retirement_requested: bool,
    settled: Option<Result<(), SourceWorkerError>>,
    /// The owner has stopped for good and left the worker registry. Until
    /// then an unresolved `settled` may still be retried.
    finished: bool,
    bodies: usize,
    planned_session: Option<String>,
    /// A direct owner's published session, in place of `start`.
    direct_start:
        Option<Result<crate::http::sharing_direct_wire::SourceDirectStart, SourceWorkerError>>,
    /// The last admitted direct byte open; status never moves it.
    direct_activity: Option<Instant>,
}

/// A waiter for one actual registered owner. Dropping this value cannot cancel
/// its detached producer or imply release of its durable dispatch obligation.
#[derive(Clone)]
pub(crate) struct SourceViewerActor(Arc<SourceViewerInner>);

pub(crate) enum SourceResourcePayload {
    Playlist(Vec<u8>),
    SubtitleText(Vec<u8>),
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
            if self
                .source
                .as_ref()
                .is_some_and(|source| !source.unchanged())
            {
                return;
            }
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
    /// Whether the owner has stopped for good: settled, refused a witness that
    /// can never settle, or exhausted its bounded retries. Nothing in this
    /// process still works for it once this is true.
    pub(crate) fn settlement_final(&self) -> bool {
        self.0.state.lock().expect("Source worker state").finished
    }
    pub(crate) async fn wait_ready(
        &self,
        deadline: Instant,
    ) -> Result<crate::http::hls::StartResponse, SourceWorkerError> {
        // A direct owner has no HLS Start, status, control or resources.
        if self.0.direct.is_some() {
            return Err(SourceWorkerError::Unsupported);
        }
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
        let actor = self.clone();
        let resource = resource.clone();
        let job = tokio::spawn(Box::pin(async move {
            actor.open_resource_owned(&resource, deadline).await
        }));
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), job)
            .await
            .map_err(|_| SourceWorkerError::Deadline)?
            .map_err(|_| SourceWorkerError::Unresolved)?
    }
    async fn open_resource_owned(
        &self,
        resource: &SharingHlsResource,
        deadline: Instant,
    ) -> Result<SourceOpenedResource, SourceWorkerError> {
        if Instant::now() >= deadline {
            return Err(SourceWorkerError::Deadline);
        }
        if self.0.direct.is_some() {
            return Err(SourceWorkerError::Unsupported);
        }
        let native = self
            .0
            .state
            .lock()
            .expect("Source native resource")
            .native
            .clone();
        let query = resource.as_str().split_once('?').map(|(_, query)| query);
        if query.is_some_and(|query| {
            query.split('&').any(|part| match part.split_once('=') {
                Some(("native", "1")) => false,
                Some(("subtitle", value)) => native.as_ref().is_none_or(|tracks| {
                    value.parse::<i64>().ok() != Some(tracks.selected().unwrap_or(-1))
                }),
                _ => true,
            })
        }) || (query.is_some() && native.is_none())
            || (matches!(
                resource.kind(),
                SharingHlsResourceKind::Master
                    | SharingHlsResourceKind::Video
                    | SharingHlsResourceKind::SubtitlePlaylist { .. }
                    | SharingHlsResourceKind::SubtitleSegment { .. }
            ) && native.is_none())
        {
            return Err(SourceWorkerError::Unsupported);
        }
        let kind = if resource.kind() == SharingHlsResourceKind::Index
            && query.is_some_and(|query| query.split('&').any(|part| part == "native=1"))
        {
            SharingHlsResourceKind::Master
        } else {
            resource.kind()
        };
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
        let facts = manager
            .vod
            .hls_facts(&session_id)
            .await
            .ok_or(SourceWorkerError::Unavailable)?;
        if !manager
            .vod
            .source_status_owner_is_current(&session_id, &facts.response_owner, &self.0.assignment)
            .await
        {
            return Err(SourceWorkerError::Unavailable);
        }
        guard.source = Some(
            manager
                .vod
                .source_response_physical_fence(&facts.response_owner)
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?,
        );
        let guard = Arc::new(guard);
        let custody = resource::SourceResourceReadCustody::new(
            Arc::clone(&guard),
            Arc::clone(&self.0.resource_hooks),
        );
        let (payload, owner) = match kind {
            SharingHlsResourceKind::Index | SharingHlsResourceKind::Video => {
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
                    .source_segment_before(
                        &session_id,
                        resource.as_str(),
                        Some(deadline),
                        custody.clone(),
                    )
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
            SharingHlsResourceKind::Master
            | SharingHlsResourceKind::SubtitlePlaylist { .. }
            | SharingHlsResourceKind::SubtitleSegment { .. } => {
                let tracks = native.as_ref().ok_or(SourceWorkerError::Unsupported)?;
                let publication = manager
                    .vod
                    .playlist(&session_id)
                    .await
                    .ok_or(SourceWorkerError::Unavailable)?;
                let video = publication
                    .result
                    .map_err(|_| SourceWorkerError::Unavailable)?;
                let facts = manager
                    .vod
                    .hls_facts(&session_id)
                    .await
                    .ok_or(SourceWorkerError::Unavailable)?;
                if facts.file.id.to_string() != self.0.assignment.binding().file_id().as_str()
                    || !manager
                        .vod
                        .response_owner_is_live(&session_id, &facts.response_owner)
                        .await
                {
                    return Err(SourceWorkerError::Unavailable);
                }
                let (file, context) = source_native_presentation(facts, tracks.probe());
                let payload = match kind {
                    SharingHlsResourceKind::Master => {
                        SourceResourcePayload::Playlist(crate::http::hls::source_native_master(
                            &file,
                            tracks.selected(),
                            &context,
                            &tracks.indexes(),
                        ))
                    }
                    SharingHlsResourceKind::SubtitlePlaylist { index } => {
                        tracks.track(index).ok_or(SourceWorkerError::Unsupported)?;
                        SourceResourcePayload::Playlist(crate::http::hls::source_native_playlist(
                            &video,
                        ))
                    }
                    SharingHlsResourceKind::SubtitleSegment { index } => {
                        let track = tracks.track(index).ok_or(SourceWorkerError::Unsupported)?;
                        let name = resource
                            .as_str()
                            .split('?')
                            .next()
                            .unwrap_or("")
                            .rsplit('/')
                            .next()
                            .unwrap_or("");
                        let sequence = name
                            .strip_prefix("seg")
                            .and_then(|name| name.strip_suffix(".vtt"))
                            .and_then(|name| name.parse::<u64>().ok())
                            .ok_or(SourceWorkerError::Unsupported)?;
                        let bytes = crate::http::hls::source_native_segment(
                            &video,
                            track,
                            sequence,
                            context.media_origin_seconds,
                        )
                        .ok_or(SourceWorkerError::Unavailable)?;
                        if bytes.len() > 2 * 1024 * 1024 {
                            return Err(SourceWorkerError::Capacity);
                        }
                        SourceResourcePayload::SubtitleText(bytes)
                    }
                    _ => return Err(SourceWorkerError::Unsupported),
                };
                (payload, publication.owner)
            }
        };
        if let SourceResourcePayload::Playlist(bytes) = &payload {
            plurx_core::sharing_resources::validate_sharing_playlist(resource, bytes)
                .map_err(|_| SourceWorkerError::Unsupported)?;
        }

        if let Some(native) = native.as_ref() {
            if !native.matches(
                &self.0.assignment,
                guard
                    .source
                    .as_ref()
                    .ok_or(SourceWorkerError::Unavailable)?
                    .object_version(),
            ) {
                return Err(SourceWorkerError::Unavailable);
            }
        }
        // Detached observation survives waiter timeout only to settle actual
        // jobs; an expired request cannot extend authority or viewer activity.
        if Instant::now() >= deadline {
            return Err(SourceWorkerError::Deadline);
        }
        // A parked segment can outlive the first observation's five-second
        // window. Re-observe real membership/current binding after the wait.
        let proof = self.0.gate.current_owned(&self.0.assignment).await?;
        proof
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| SourceWorkerError::Unavailable)?;
        if !manager
            .vod
            .source_status_owner_is_current(&session_id, &owner, &self.0.assignment)
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
        if Instant::now() >= deadline
            || proof
                .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
                .is_err()
            || guard
                .source
                .as_ref()
                .is_none_or(|source| !source.unchanged())
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
        drop(custody);
        let guard = Arc::try_unwrap(guard).map_err(|_| SourceWorkerError::Unresolved)?;
        Ok(SourceOpenedResource { payload, guard })
    }
}
impl SourceProducerAuthority {
    pub(super) async fn current_preparation(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> Result<
        Box<plurx_core::sharing_source_sessions::SourceSessionWriteAuthority>,
        SourceWorkerError,
    > {
        let members = self
            .membership
            .observe_source_admission_members()
            .await
            .map_err(|_| SourceWorkerError::Unavailable)?
            .ok_or(SourceWorkerError::Unavailable)?;
        let SourceWriteAuthorityRead::Ready(proof) = self
            .store
            .prepare_source_activation_authority(assignment, &self.master, &members)
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?
        else {
            return Err(SourceWorkerError::Unavailable);
        };
        if !self
            .store
            .authorize_source_media_preparation(&proof)
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?
        {
            return Err(SourceWorkerError::Unavailable);
        }
        Ok(proof)
    }

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
    /// Renew this owner's Source session lease.
    ///
    /// The Store guard is optimistic: it pins the exact lease revision the
    /// proof observed. Status, resources, control and the actor heartbeat all
    /// renew the same session, so one renewal can lose the race to another by
    /// this same owner. That is not a loss of authority. Re-observe the owned
    /// route and retry against the newer lease; a refusal against an
    /// unchanged lease, or any change of route identity, is a real refusal.
    async fn renew_with(
        &self,
        assignment: &SourceDispatchAssignment,
        proof: &plurx_core::sharing_source_sessions::SourceOwnedRouteAuthority,
    ) -> Result<MediaSessionRoute, SourceWorkerError> {
        const ATTEMPTS: usize = 4;
        let mut fresh: Option<Box<plurx_core::sharing_source_sessions::SourceOwnedRouteAuthority>> =
            None;
        for _ in 0..ATTEMPTS {
            let observed = fresh.as_deref().unwrap_or(proof);
            if let Some(route) = self.renew_observed(assignment, observed).await? {
                return Ok(route);
            }
            let current = self.current_owned(assignment).await?;
            if !observed.renewed_by_same_owner(&current) {
                return Err(SourceWorkerError::Unavailable);
            }
            fresh = Some(current);
        }
        Err(SourceWorkerError::Unavailable)
    }
    async fn renew_observed(
        &self,
        assignment: &SourceDispatchAssignment,
        proof: &plurx_core::sharing_source_sessions::SourceOwnedRouteAuthority,
    ) -> Result<Option<MediaSessionRoute>, SourceWorkerError> {
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
            .map_err(|_| SourceWorkerError::Unresolved)
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
            control_target_duration_ms: i64::from(match prepared.request().kind {
                SessionKind::Copy { .. } => plurx_core::transcode::COPY_SEGMENT_MAX_SECS,
                SessionKind::Transcode { .. } => plurx_core::transcode::SEGMENT_SECONDS,
            }) * 1_000,
            control_hooks: Default::default(),
            status_hooks: Default::default(),
            resource_hooks: Arc::new(Default::default()),
            original_selection: prepared.original_selection().copied(),
            assignment,
            manager: Arc::downgrade(self),
            gate: Arc::new(SourceProducerAuthority {
                store: Arc::clone(&self.store),
                membership: state.membership.clone(),
                master: Arc::clone(&state.sharing.key),
            }),
            direct: None,
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
        tokio::spawn(Box::pin(async move {
            manager
                .run_source_owner(owner, state, prepared, deadline)
                .await;
        }));
        Ok(actor)
    }

    /// The Source preparation permit: one live wait, bounded by the start
    /// deadline and the foreground queue wait together, holding its waiter
    /// for the whole wait so background work yields and cannot refill the
    /// pool between attempts.
    async fn admit_source_copy(
        &self,
        deadline: Instant,
    ) -> Result<crate::vodencode::EncodePermit, SourceWorkerError> {
        let admit_deadline = deadline.min(Instant::now() + crate::admission::QUEUE_WAIT);
        match crate::vodencode::EncodePermit::admit_source_copy(
            &self.admissions,
            self.store.as_ref(),
            admit_deadline,
        )
        .await
        {
            crate::vodencode::SourceCopyPermitRead::Admitted(permit) => Ok(permit),
            crate::vodencode::SourceCopyPermitRead::Unavailable => {
                Err(SourceWorkerError::Unavailable)
            }
            crate::vodencode::SourceCopyPermitRead::Capacity => Err(SourceWorkerError::Capacity),
        }
    }

    /// Index cache bytes are inert evidence. Actual Source permission and the
    /// held file fence surround the scan; first activation remains the atomic
    /// authority gate. No ordinary index queue or anonymous demand is used.
    async fn prepare_source_cold_index(
        &self,
        owner: &Arc<SourceViewerInner>,
        prepared: &crate::http::hls::PreparedSourcePlayback,
        deadline: Instant,
        work: &mut Option<crate::fragindex::SourceIndexOperation>,
    ) -> Result<(), SourceWorkerError> {
        let file = prepared.file();
        if file.video_codec.as_deref() != Some("h264") {
            // Existing warm indexes retain their established support policy.
            return Ok(());
        }
        let SessionKind::Copy {
            preserve_dolby_vision,
            convert_dolby_vision,
            ..
        } = prepared.request().kind
        else {
            return Err(SourceWorkerError::Unsupported);
        };
        if preserve_dolby_vision || convert_dolby_vision {
            return Err(SourceWorkerError::Unsupported);
        }
        let video = plurx_core::transcode::CopyVideoOptions::new(false, false);
        let identity = crate::fragindex::identity_for(file, video);
        if self
            .store
            .fragment_index(file.id, &identity)
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?
            .is_some()
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(SourceWorkerError::Deadline);
        }
        let permit = self.admit_source_copy(deadline).await?;
        let source = crate::fragment_index_cluster::open_source_playback_fence(file, None)
            .await
            .map_err(|_| SourceWorkerError::Unavailable)?;
        let object_version = source.object_version().to_owned();
        let authority = owner.gate.current_preparation(&owner.assignment).await?;
        let stored_probe = self
            .store
            .source_index_probe_evidence(&authority)
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?
            .ok_or(SourceWorkerError::Unsupported)?;
        authority
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| SourceWorkerError::Unavailable)?;
        *work = Some(
            crate::fragindex::start_source_index(
                file.clone(),
                source,
                stored_probe,
                video,
                self.runtime_cache.clone(),
                deadline,
                permit,
                authority,
                Arc::clone(&self.store),
                Arc::clone(&self.source_workers.index_hooks),
            )
            .map_err(|_| SourceWorkerError::Unsupported)?,
        );
        let built = work
            .as_ref()
            .expect("Source index owner")
            .outcome()
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?;
        if !built.source_unchanged {
            return Err(SourceWorkerError::Unavailable);
        }
        let crate::fragindex::IndexOutcome::Built(index) = built.outcome else {
            return Err(SourceWorkerError::Unavailable);
        };
        let fence =
            crate::fragment_index_cluster::open_source_playback_fence(file, Some(&object_version))
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?;
        owner.gate.current_preparation(&owner.assignment).await?;
        self.store
            .put_fragment_index(file.id, &index)
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?;
        if !fence.unchanged() {
            return Err(SourceWorkerError::Unavailable);
        }
        owner.gate.current_preparation(&owner.assignment).await?;
        Ok(())
    }

    async fn prepare_source_native_tracks(
        &self,
        owner: &Arc<SourceViewerInner>,
        prepared: &crate::http::hls::PreparedSourcePlayback,
        deadline: Instant,
        work: &mut Option<super::source_subtitles::SourceNativeOperation>,
    ) -> Result<Arc<super::source_subtitles::SourceNativeTracks>, SourceWorkerError> {
        super::source_subtitles::supported_tracks(prepared.file(), prepared.native_subtitles().1)
            .map_err(|_| SourceWorkerError::Unsupported)?;
        let permit = self.admit_source_copy(deadline).await?;
        let source =
            crate::fragment_index_cluster::open_source_playback_fence(prepared.file(), None)
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?;
        *work = Some(super::source_subtitles::start_source_native(
            prepared.file().clone(),
            source,
            owner.assignment.clone(),
            Arc::clone(&owner.gate),
            prepared.native_subtitles().1,
            permit,
            Arc::clone(&self.store),
            deadline,
            Arc::clone(&self.source_workers.native_hooks),
        ));
        let tracks = work
            .as_ref()
            .expect("Source native owner")
            .outcome()
            .await
            .map_err(|_| SourceWorkerError::Unavailable)?;
        Ok(Arc::new(tracks))
    }

    async fn prepare_source_encoded_recipe(
        &self,
        owner: &Arc<SourceViewerInner>,
        prepared: &crate::http::hls::PreparedSourcePlayback,
        deadline: Instant,
        work: &mut Option<super::source_preparation::SourceProbeOperation>,
    ) -> Result<Arc<crate::vodencode::Encoding>, SourceWorkerError> {
        let permit = self.admit_source_copy(deadline).await?;
        let source =
            crate::fragment_index_cluster::open_source_playback_fence(prepared.file(), None)
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?;
        let proof = owner.gate.current_preparation(&owner.assignment).await?;
        *work = Some(super::source_preparation::start_source_probe(
            prepared.file().clone(),
            source,
            proof,
            permit,
            Arc::clone(&self.store),
            deadline,
            Arc::clone(&self.source_workers.probe_hooks),
        ));
        let evidence = work
            .as_ref()
            .expect("Source probe owner")
            .outcome()
            .await
            .map_err(|_| SourceWorkerError::Unavailable)?;
        self.source_workers.probe_hooks.after_evidence().await;
        // The probe permit is already dropped only after actual reap/pipes.
        // Encoder admission is separate and cannot self-deadlock against it.
        let current = owner.gate.current_preparation(&owner.assignment).await?;
        Box::pin(self.prepare_source_vod_encoding(prepared, &evidence, &current))
            .await
            .map_err(|_| SourceWorkerError::Unavailable)
    }

    async fn run_source_owner(
        self: Arc<Self>,
        owner: Arc<SourceViewerInner>,
        state: Arc<crate::state::AppState>,
        prepared: crate::http::hls::PreparedSourcePlayback,
        deadline: Instant,
    ) {
        let actor = SourceViewerActor(Arc::clone(&owner));
        let mut reserved: Option<crate::vodserve::ReservedSourceVodRendition> = None;
        let mut index_work = None;
        let mut probe_work = None;
        let mut native_work = None;
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
                if (!prepared.native_subtitles().0 && prepared.native_subtitles().1.is_some())
                    || prepared.request().subtitle_burn.is_some()
                    || prepared.request().previous_session_id.is_some()
                    || prepared.request().reopen_reason.is_some()
                    || (matches!(prepared.request().kind, SessionKind::Transcode { .. })
                        && (plurx_core::playback::hdr_route(prepared.file()).is_some()
                            || plurx_core::playback::is_dolby_vision(prepared.file())))
                    || !matches!(
                        prepared.request().kind,
                        SessionKind::Copy { .. } | SessionKind::Transcode { .. }
                    )
                {
                    return Err(SourceWorkerError::Unsupported);
                }
                let settings = self
                    .vod_settings(prepared.request())
                    .await
                    .map_err(|_| SourceWorkerError::Unavailable)?
                    .ok_or(SourceWorkerError::Unavailable)?;
                if prepared.native_subtitles().0 {
                    if plurx_core::playback::hdr_route(prepared.file()).is_some()
                        || plurx_core::playback::is_dolby_vision(prepared.file())
                    {
                        return Err(SourceWorkerError::Unsupported);
                    }
                    let tracks = Box::pin(self.prepare_source_native_tracks(
                        &owner,
                        &prepared,
                        deadline,
                        &mut native_work,
                    ))
                    .await?;
                    owner.state.lock().expect("Source native actor").native = Some(tracks);
                }
                let encoding = if matches!(prepared.request().kind, SessionKind::Transcode { .. }) {
                    Some(
                        Box::pin(self.prepare_source_encoded_recipe(
                            &owner,
                            &prepared,
                            deadline,
                            &mut probe_work,
                        ))
                        .await?,
                    )
                } else {
                    Box::pin(self.prepare_source_cold_index(
                        &owner,
                        &prepared,
                        deadline,
                        &mut index_work,
                    ))
                    .await?;
                    None
                };
                let admitted = Box::pin(self.vod.prepare_admitted_source_vod(
                    &prepared,
                    &owner.assignment,
                    &settings,
                    &self.admissions,
                    self.store.as_ref(),
                    deadline,
                    encoding,
                ))
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?;
                reserved = Some(
                    self.vod
                        .reserve_source_vod(admitted)
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
                Box::pin(self.vod.commit_source_vod(
                    reservation,
                    &session_id,
                    &owner.gate,
                    &self.admissions,
                ))
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?;
                let physical = Box::pin(reservation.wait_ready(&self.vod, deadline))
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
        // A graceful drain ends this owner the way retirement does: stop
        // renewing, settle the physical producer, release the row.
        let shutdown = state.shutdown.clone();
        if let (Ok(_), Some(reservation)) = (&start, reserved.as_ref()) {
            // The tick is the lease heartbeat. A rendition failure is
            // published on the rendition itself and retires this owner at
            // once rather than at the next renewal.
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
                    () = shutdown.cancelled() => break,
                    _ = reservation.failed() => break,
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
        // Deadline/cancellation never releases an admitted scan. The detached
        // scan owner settles its actual child/readers before Source SQL release.
        let mut preparations = SourcePreparationSettlements::default();
        if let Some(work) = index_work.as_ref() {
            work.cancel();
            preparations.index = Some(work.settle().await);
        }
        if let Some(work) = probe_work.as_ref() {
            work.cancel();
            preparations.probe = Some(work.settle().await);
        }
        if let Some(work) = native_work.as_ref() {
            work.cancel();
            preparations.native = Some(work.settle().await);
        }
        self.finish_source_owner(&owner, shutdown, unowned_existing, reserved, preparations)
            .await;
    }
    /// Settle a stopped Source owner, VOD or direct: release its physical
    /// producer (if any) and its row through the bounded detached retry, then
    /// leave the registry. The owner is finished on every exit.
    async fn finish_source_owner(
        &self,
        owner: &Arc<SourceViewerInner>,
        shutdown: tokio_util::sync::CancellationToken,
        unowned_existing: bool,
        mut reserved: Option<crate::vodserve::ReservedSourceVodRendition>,
        mut preparations: SourcePreparationSettlements,
    ) {
        let mut physical = None;
        let mut attempts = 0;
        let settlement = loop {
            attempts += 1;
            // A route this owner did not activate is never its to release.
            let result = if unowned_existing {
                Err(SourceSettlementFault::Mismatch)
            } else {
                Box::pin(self.settle_source_owner(
                    owner,
                    &mut reserved,
                    &mut physical,
                    &mut preparations,
                ))
                .await
            };
            if result != Err(SourceSettlementFault::Transient)
                || attempts >= SOURCE_SETTLEMENT_ATTEMPTS
            {
                break result;
            }
            owner.state.lock().expect("Source worker state").settled =
                Some(Err(SourceWorkerError::Unresolved));
            owner.changed.notify_waiters();
            // A Store fault retains the actual owner and sealed physical
            // settlement evidence. Retry SQL without inventing a new producer.
            tokio::select! {
                () = shutdown.cancelled() => break result,
                () = tokio::time::sleep(SOURCE_SETTLEMENT_RETRY) => {}
            }
        };
        if let Err(fault) = settlement {
            // Renewal ended with retirement, so the media-session lease sweep
            // reclaims the row; this owner only stops claiming it.
            tracing::warn!(
                target: "plurxd::transcode",
                incarnation = %owner.assignment.binding().incarnation_id(),
                attempts,
                reason = ?fault,
                "Source owner left its settlement unresolved; the session lease sweep reclaims its row"
            );
        }
        // The owner is finished either way. Its registry slot is not capacity
        // the stopped owner can still use.
        self.source_workers
            .entries
            .lock()
            .expect("Source workers")
            .retain(|entry| !Arc::ptr_eq(entry, owner));
        {
            let mut current = owner.state.lock().expect("Source worker state");
            current.settled = Some(settlement.map_err(|_| SourceWorkerError::Unresolved));
            current.finished = true;
        }
        owner.changed.notify_waiters();
    }
    async fn settle_source_owner(
        &self,
        owner: &Arc<SourceViewerInner>,
        reserved: &mut Option<crate::vodserve::ReservedSourceVodRendition>,
        physical: &mut Option<SourcePhysicalSettlement>,
        preparations: &mut SourcePreparationSettlements,
    ) -> Result<(), SourceSettlementFault> {
        if physical.is_none() {
            *physical = Some(if let Some(reserved) = reserved.as_mut() {
                let receipt = Box::pin(reserved.retire(&self.vod))
                    .await
                    .map_err(|_| SourceSettlementFault::Transient)?;
                if !receipt.matches(&owner.assignment) {
                    return Err(SourceSettlementFault::Mismatch);
                }
                SourcePhysicalSettlement::Registered(
                    Box::new(receipt),
                    std::mem::take(preparations),
                )
            } else {
                SourcePhysicalSettlement::NoProducer(std::mem::take(preparations))
            });
        }
        if matches!(physical, Some(SourcePhysicalSettlement::Registered(receipt, _)) if !receipt.matches(&owner.assignment))
        {
            return Err(SourceSettlementFault::Mismatch);
        }
        let preparations = match physical.as_ref().expect("Source physical settlement") {
            SourcePhysicalSettlement::NoProducer(preparations)
            | SourcePhysicalSettlement::Registered(_, preparations) => preparations,
        };
        if !preparations.matches(&owner.assignment) {
            return Err(SourceSettlementFault::Mismatch);
        }
        let incarnation = owner.assignment.binding().incarnation_id().to_string();
        let route = self
            .store
            .media_session_route_by_incarnation(&incarnation)
            .await
            .map_err(|_| SourceSettlementFault::Transient)?;
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
                return Err(SourceSettlementFault::Mismatch);
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
                        .map_err(|_| SourceSettlementFault::Transient)?
                        // The guarded End lost to a lease change; the next
                        // attempt re-reads the route it raced.
                        .ok_or(SourceSettlementFault::Transient)?,
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
        .map_err(|_| SourceSettlementFault::Transient)?;
        match released {
            SourceReleaseOutcome::Released | SourceReleaseOutcome::ExactReplay => Ok(()),
            SourceReleaseOutcome::Refused => Err(SourceSettlementFault::Mismatch),
        }
    }
}

/// Actual frozen VOD recipe facts plus SQL-bounded captured Source probe. This
/// avoids the generic Copy metadata path's mutable unbounded probe read.
fn source_native_presentation(
    facts: crate::vodserve::VodHlsFacts,
    probe: &str,
) -> (plurx_core::domain::MediaFile, HlsContext) {
    if let Some(encoding) = facts.encoding.as_ref() {
        let file = encoded_vod_presentation_file(
            facts.file,
            encoding.options.target_height,
            encoding.options.pipeline.output_grade(),
            Some(encoding.plan.output_contract()),
        );
        let mut codecs = encoding
            .plan
            .output_contract()
            .hls_codecs()
            .unwrap_or_else(|| {
                transcoded_hls_codecs(
                    encoding.options.pipeline.output_grade(),
                    encoding.options.target_height,
                )
            });
        if file.audio_streams.is_empty() {
            codecs.truncate(codecs.find(',').unwrap_or(codecs.len()));
        }
        let context = HlsContext {
            bandwidth: encoding.plan.output_contract().output_bandwidth(),
            file_id: file.id,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs,
            supplemental_codecs: None,
            frame_rate: Some(
                f64::from(encoding.grid.numerator) / f64::from(encoding.grid.denominator),
            ),
        };
        (file, context)
    } else {
        let (codecs, supplemental_codecs) = copied_hls_codecs(
            &facts.file,
            facts.audio_index,
            CopySessionOptions {
                transcode_audio: facts.aac,
                preserve_dolby_vision: facts.preserve_dolby_vision,
                convert_dolby_vision: facts.convert_dolby_vision,
            },
            Some(probe),
        );
        let context = HlsContext {
            bandwidth: None,
            file_id: facts.file.id,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs,
            supplemental_codecs,
            frame_rate: frozen_video_frame_rate(Some(probe)),
        };
        (facts.file, context)
    }
}
