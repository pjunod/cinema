//! Owned Source producer admission and lifetime. HTTP callers only wait.
use super::*;
#[cfg(unix)]
#[path = "source_actor/growing.rs"]
mod growing;
#[cfg(unix)]
pub(crate) use growing::{
    SourceGrowingBacking, SourceGrowingPreparation, GROWING_LEASE_TIMEOUT_MS,
};

#[derive(Clone)]
enum SourceBacking {
    ImmutableVod,
    #[cfg(unix)]
    Growing(Arc<SourceGrowingBacking>),
    #[cfg(unix)]
    PreparingRange(Arc<SourceGrowingPreparation>),
}

/// Actual daemon components retained by immutable Source renditions. Only
/// the owned worker factory constructs this gate; there is no wire constructor.
pub(crate) struct SourceProducerAuthority {
    store: Arc<dyn Store>,
    source_members: crate::cluster::SourceMembers,
    master: Arc<plurx_core::secrets::CredentialKey>,
    registry_boot_id: uuid::Uuid,
    ingress: plurx_core::sharing_source_sessions::SourceIngressAdmissionPermission,
    /// The latest Source session lease the Store confirmed for this owner.
    lease: LeaseBound,
    /// Renewal ticks begun, for a test that waits on the tick itself.
    #[cfg(test)]
    ticks: tokio::sync::watch::Sender<u64>,
}

/// What a renewal tick does after re-proving its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TickFence {
    Renew,
    Skip,
    End,
}

/// How far one renewal extends this owner's Source session lease.
pub(crate) const SOURCE_SESSION_LEASE_MS: i64 = 60_000;

/// The Store's own lease, enforced where the Store cannot say so. A refusal
/// that passes on its own is transient only while the lease the Store last
/// confirmed still holds: past it, the session is over for this owner
/// whether or not the Store can be reached to say it, so the refusal is
/// terminal. Not a timer: it only reads the lease the Store wrote.
pub(crate) struct LeaseBound {
    confirmed_until_ms: std::sync::atomic::AtomicI64,
}
impl LeaseBound {
    /// A lease presumed to the renewal window from `now_ms`, as the
    /// activation that created this owner wrote it.
    pub(crate) fn from(now_ms: i64) -> Self {
        Self {
            confirmed_until_ms: std::sync::atomic::AtomicI64::new(
                now_ms.saturating_add(SOURCE_SESSION_LEASE_MS),
            ),
        }
    }
    /// The lease the activation wrote: the owner's first confirmed lease,
    /// replacing the presumed one it was constructed with.
    pub(crate) fn activated(&self, until_ms: i64) {
        self.confirmed_until_ms
            .store(until_ms, std::sync::atomic::Ordering::SeqCst);
    }
    /// Record a lease a guarded renewal wrote; never moves it backward.
    pub(crate) fn confirmed(&self, until_ms: i64) {
        self.confirmed_until_ms
            .fetch_max(until_ms, std::sync::atomic::Ordering::SeqCst);
    }
    /// [`SourceProducerAuthority::tick_fence`] at `now_ms`.
    pub(crate) fn tick_fence<T>(&self, fenced: Result<T, LiveRefusal>, now_ms: i64) -> TickFence {
        match fenced.map_err(|refusal| self.bound(refusal, now_ms)) {
            Ok(_) => TickFence::Renew,
            Err(refusal) if refusal.is_transient() => TickFence::Skip,
            Err(_) => TickFence::End,
        }
    }
    pub(crate) fn bound(&self, refusal: LiveRefusal, now_ms: i64) -> LiveRefusal {
        let until = self
            .confirmed_until_ms
            .load(std::sync::atomic::Ordering::SeqCst);
        if refusal.is_transient() && now_ms >= until {
            tracing::warn!(target: "plurxd::transcode", stage = "source_owner.lease", lease_until_ms = until, "Source admission refused past its confirmed lease");
            LiveRefusal::terminal(refusal.error)
        } else {
            refusal
        }
    }
}

pub(crate) struct SourceGenerationAuthority {
    proofs: Vec<plurx_core::sharing_source_sessions::SourcePublicationAuthority>,
    preparing: Vec<plurx_core::sharing_source_sessions::SourceSessionWriteAuthority>,
    ingress: Vec<plurx_core::sharing_source_sessions::SourceIngressAdmissionPermission>,
}

impl SourceGenerationAuthority {
    pub(crate) fn validate_before_spawn(&self) -> Result<(), String> {
        let now = crate::fragment_index_cluster::unix_ms();
        for permission in &self.ingress {
            permission
                .validate_observation_freshness(now)
                .map_err(|_| "Source ingress observation expired before spawn".to_owned())?;
        }
        for proof in &self.preparing {
            proof
                .validate_observation_freshness(now)
                .map_err(|_| "Source preparing observation expired before spawn".to_owned())?;
        }
        for proof in &self.proofs {
            proof
                .validate_observation_freshness(now)
                .map_err(|_| "Source producer observation expired before spawn".to_owned())?;
        }
        Ok(())
    }
}

impl SourceProducerAuthority {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        source_members: crate::cluster::SourceMembers,
        master: Arc<plurx_core::secrets::CredentialKey>,
        registry_boot_id: uuid::Uuid,
        ingress: plurx_core::sharing_source_sessions::SourceIngressAdmissionPermission,
    ) -> Self {
        Self {
            store,
            source_members,
            master,
            registry_boot_id,
            ingress,
            lease: LeaseBound::from(crate::fragment_index_cluster::unix_ms()),
            #[cfg(test)]
            ticks: tokio::sync::watch::Sender::new(0),
        }
    }
    #[cfg(test)]
    pub(super) fn ticked(&self) {
        self.ticks.send_modify(|ticks| *ticks += 1);
    }
    pub(crate) fn retained_ingress(
        &self,
        assignment: &plurx_core::sharing_source_sessions::SourceDispatchAssignment,
    ) -> Option<plurx_core::sharing_source_sessions::SourceIngressAdmissionPermission> {
        (self.ingress.assignment().same_identity(assignment)
            && self.ingress.registry_boot_id() == self.registry_boot_id)
            .then(|| self.ingress.clone())
    }
    /// Private first-range bootstrap only: the exact retained preparing
    /// assignment and its existing index-preparation guard authorize physical
    /// input before a measured timeline origin exists. No media is servable.
    pub(crate) async fn authorize_preparing_generation(
        &self,
        permissions: &[plurx_core::sharing_source_sessions::SourceIngressAdmissionPermission],
    ) -> Result<SourceGenerationAuthority, String> {
        if permissions.is_empty() || permissions.len() > 8 {
            return Err("Source preparing assignment bound exceeded".into());
        }
        let mut preparing = Vec::new();
        let mut ingress = Vec::new();
        for permission in permissions {
            if permission.registry_boot_id() != self.registry_boot_id
                || !permission
                    .assignment()
                    .same_identity(self.ingress.assignment())
            {
                continue;
            }
            let proof = self
                .current_preparation(permission.assignment())
                .await
                .map_err(|_| "Source preparing authority unavailable".to_owned())?;
            if !self
                .store
                .authorize_source_index_preparation(&proof)
                .await
                .map_err(|_| "Source preparing input guard failed".to_owned())?
            {
                continue;
            }
            preparing.push(*proof);
            ingress.push(permission.clone());
        }
        if preparing.is_empty() {
            return Err("Source bootstrap has no current preparing owner".into());
        }
        let authority = SourceGenerationAuthority {
            proofs: Vec::new(),
            preparing,
            ingress,
        };
        authority.validate_before_spawn()?;
        Ok(authority)
    }

    pub(crate) async fn authorize_generation(
        &self,
        permissions: &[plurx_core::sharing_source_sessions::SourceIngressAdmissionPermission],
    ) -> Result<SourceGenerationAuthority, String> {
        if permissions.is_empty() || permissions.len() > 8 {
            return Err("Source producer assignment bound exceeded".into());
        }
        use plurx_core::sharing_source_sessions::SourcePublicationAuthorityRead;
        let members = self
            .source_members
            .observe()
            .await
            .map_err(|_| "Source member observation failed".to_owned())?
            .ok_or_else(|| "Source member floor is unavailable".to_owned())?;
        let mut proofs = Vec::with_capacity(permissions.len());
        let mut ingress = Vec::with_capacity(permissions.len());
        for retained in permissions {
            if retained.registry_boot_id() != self.registry_boot_id {
                continue;
            }
            let assignment = retained.assignment();
            let Some(permission) = self
                .store
                .refresh_source_ingress_admission(retained, &members)
                .await
                .map_err(|_| "Source ingress authority read failed".to_owned())?
            else {
                continue;
            };
            permission
                .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
                .map_err(|_| "Source ingress authority expired".to_owned())?;
            match self
                .store
                .prepare_source_publication_authority(assignment, &self.master, &members)
                .await
                .map_err(|_| "Source producer authority read failed".to_owned())?
            {
                SourcePublicationAuthorityRead::Ready(proof) => {
                    proofs.push(*proof);
                    ingress.push(*permission);
                }
                SourcePublicationAuthorityRead::Unavailable(_)
                | SourcePublicationAuthorityRead::Capacity => {}
            }
        }
        if proofs.is_empty() {
            return Err("Source producer has no current authorized viewer".to_owned());
        }
        let authority = SourceGenerationAuthority {
            proofs,
            ingress,
            preparing: Vec::new(),
        };
        authority.validate_before_spawn()?;
        Ok(authority)
    }
}

impl TranscodeManager {
    #[cfg(test)]
    pub(crate) async fn source_fixture_body_budget() -> tokio::sync::OwnedSemaphorePermit {
        static BUDGET: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
            std::sync::OnceLock::new();
        std::sync::Arc::clone(
            BUDGET.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4))),
        )
        .acquire_owned()
        .await
        .expect("Source fixture body budget stays open")
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
        SourceDispatchAssignment, SourceIngressAdmissionPermission, SourceOwnedRouteAuthorityRead,
        SourcePublicationAuthorityRead, SourceReleaseOutcome, SourceSessionWriteAuthority,
        SourceWriteAuthorityRead,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceWorkerError {
    Unavailable,
    NotYet,
    RescanRequired,
    CopyTargetExceeded,
    CopyInitContractChanged,
    CopyClockUnknown,
    CopySyncUnverified,
    /// The copy's output outgrew the in-memory hold that pairs its whole
    /// canonical and AV fragments: a source whose keyframe spans are too
    /// large to copy here.
    CopyInputLimit,
    Capacity,
    Conflict,
    Deadline,
    Unresolved,
    Unsupported,
}

/// Whether a refusal to serve a live Source session can pass on its own.
///
/// Status, control and resources all ask the actor to open its live response,
/// and every answer that is not that response reads to the receiver as the
/// session's end unless it says it will pass. So each refusal states, where
/// its check lives, which one it is: a store or route renewal that failed or
/// lost a race, a body limit, a start not yet published is `Transient` and is
/// answered with a retry; a producer that died, a requested retirement, or a
/// changed session, route, authority or source is `Terminal`, retires the
/// actor and is answered as the end it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefusalPersistence {
    Transient,
    Terminal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LiveRefusal {
    pub(crate) error: SourceWorkerError,
    pub(crate) persistence: RefusalPersistence,
}
impl LiveRefusal {
    pub(crate) const fn transient(error: SourceWorkerError) -> Self {
        Self {
            error,
            persistence: RefusalPersistence::Transient,
        }
    }
    /// A fence failure: only an object that could not be read just now
    /// can pass.
    pub(crate) fn from_fence(error: &crate::fragment_index_cluster::SourceFenceError) -> Self {
        if error.is_transient() {
            Self::transient(SourceWorkerError::Unavailable)
        } else {
            Self::terminal(SourceWorkerError::Unavailable)
        }
    }
    pub(crate) const fn terminal(error: SourceWorkerError) -> Self {
        Self {
            error,
            persistence: RefusalPersistence::Terminal,
        }
    }
    pub(crate) fn is_transient(self) -> bool {
        self.persistence == RefusalPersistence::Transient
    }
}
/// Callers that do not answer a live viewer keep the plain error.
impl From<LiveRefusal> for SourceWorkerError {
    fn from(refusal: LiveRefusal) -> Self {
        refusal.error
    }
}

/// [`SourceViewerActor::wait_ready`]'s refusal for a live viewer: its own
/// timeout and a start not yet prepared (`NotYet`) pass; any other stored
/// start failure, or a retirement, is that start's final answer.
pub(super) fn wait_ready_refusal(error: SourceWorkerError) -> LiveRefusal {
    if matches!(
        error,
        SourceWorkerError::Deadline | SourceWorkerError::NotYet
    ) {
        LiveRefusal::transient(error)
    } else {
        LiveRefusal::terminal(error)
    }
}

#[cfg(test)]
mod live_refusal_tests {
    use super::*;
    use SourceWorkerError as E;

    /// A copy refused because its output outgrew the mux's pairing hold is
    /// its own terminal failure: a retry meets the same fragments again.
    #[test]
    fn a_copy_input_limit_is_its_own_terminal_failure() {
        let error = E::copy_failure("copy_range_input_limit");
        assert_eq!(error, E::CopyInputLimit);
        assert!(!wait_ready_refusal(error).is_transient());
    }

    /// Every renewal tick (VOD, growing and direct) decides by this after
    /// re-proving its source: an object proven renews; one that could not be
    /// read just now skips the renewal while the confirmed lease holds and
    /// ends the owner once it has passed; one proven changed ends it.
    #[test]
    fn a_tick_renews_only_for_a_proven_object() {
        use crate::fragment_index_cluster::SourceFenceError as F;
        let lease = LeaseBound::from(1_000);
        let until = 1_000 + SOURCE_SESSION_LEASE_MS;
        let unreadable = || Err::<(), _>(LiveRefusal::from_fence(&F::Unreachable("io".into())));
        let changed = || Err::<(), _>(LiveRefusal::from_fence(&F::Changed("size".into())));
        assert_eq!(lease.tick_fence(Ok(()), until + 1), TickFence::Renew);
        assert_eq!(lease.tick_fence(unreadable(), until - 1), TickFence::Skip);
        assert_eq!(lease.tick_fence(unreadable(), until), TickFence::End);
        assert_eq!(lease.tick_fence(changed(), 0), TickFence::End);
        // A skipped renewal confirms nothing, so the bound does not move;
        // only a guarded renewal does.
        lease.confirmed(until + 10_000);
        assert_eq!(lease.tick_fence(unreadable(), until), TickFence::Skip);
        lease.activated(until - 10_000);
        assert_eq!(
            lease.tick_fence(unreadable(), until - 10_000),
            TickFence::End
        );
    }

    /// A refusal that passes stays transient while the lease the Store last
    /// confirmed holds, and is terminal once it has passed; a terminal one
    /// stays terminal, and a confirmation never moves the lease back.
    #[test]
    fn the_confirmed_lease_bounds_what_passes() {
        let lease = LeaseBound::from(1_000);
        let until = 1_000 + SOURCE_SESSION_LEASE_MS;
        assert!(lease
            .bound(LiveRefusal::transient(E::Unresolved), until - 1)
            .is_transient());
        let past = lease.bound(LiveRefusal::transient(E::Unresolved), until);
        assert!(!past.is_transient());
        assert_eq!(past.error, E::Unresolved);
        assert!(!lease
            .bound(LiveRefusal::terminal(E::Unavailable), 0)
            .is_transient());
        lease.confirmed(until + 30_000);
        assert!(lease
            .bound(LiveRefusal::transient(E::Unresolved), until)
            .is_transient());
        lease.confirmed(0);
        assert!(lease
            .bound(LiveRefusal::transient(E::Unresolved), until + 29_999)
            .is_transient());
    }
}

impl SourceWorkerError {
    pub(super) fn copy_failure(code: &str) -> Self {
        match code {
            "copy_target_exceeded" => Self::CopyTargetExceeded,
            "copy_sync_unverified" => Self::CopySyncUnverified,
            "copy_range_input_limit" => Self::CopyInputLimit,
            "copy_init_contract_changed" | "copy_head_init_changed" => {
                Self::CopyInitContractChanged
            }
            "copy_clock_unknown"
            | "copy_format_start_unknown"
            | "copy_clock_invalid"
            | "copy_clock_overflow"
            | "copy_head_clock_invalid"
            | "copy_head_clock_missing"
            | "copy_head_clock_sample_mismatch"
            | "copy_head_clock_truncated"
            | "copy_head_not_sync" => Self::CopyClockUnknown,
            _ => Self::Unavailable,
        }
    }
}

/// One actual factory invocation returned before registry insertion or spawn.
/// No wire decoder or HTTP task-stage observer can mint this receipt. The
/// coordinator must separately retain its fresh acquired claim: this receipt
/// describes this invocation, never another process's historical admission.
#[derive(Clone)]
pub(crate) struct SourceWorkerNoAdmission {
    assignment: Box<SourceDispatchAssignment>,
    reason: SourceWorkerError,
}
impl std::fmt::Debug for SourceWorkerNoAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceWorkerNoAdmission")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}
impl SourceWorkerNoAdmission {
    fn refused(assignment: &SourceDispatchAssignment, reason: SourceWorkerError) -> Self {
        Self {
            assignment: Box::new(assignment.clone()),
            reason,
        }
    }
    pub(crate) fn assignment(&self) -> &SourceDispatchAssignment {
        &self.assignment
    }
    pub(crate) fn reason(&self) -> SourceWorkerError {
        self.reason
    }
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
    pub(super) native_hooks: Arc<super::source_preparation::SourceProbeHookOwner>,
    #[cfg(test)]
    processed_activity_hook: std::sync::Mutex<Option<SourceProcessedActivityHook>>,
}
#[cfg(test)]
pub(crate) type SourceProcessedActivityHook = Box<
    dyn FnOnce(
            Arc<crate::prodrun::ProducerSlot>,
            tokio_util::sync::CancellationToken,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send,
>;
struct SourceViewerInner {
    control_target_duration_ms: i64,
    control_hooks: control::SourceControlHookOwner,
    status_hooks: status::SourceStatusHookOwner,
    resource_hooks: Arc<resource::SourceResourceHookOwner>,
    assignment: SourceDispatchAssignment,
    manager: std::sync::Weak<TranscodeManager>,
    gate: Arc<SourceProducerAuthority>,
    /// Present only for a direct-play owner: the fenced file it serves. Such
    /// an owner has no producer and never publishes an HLS Start.
    direct: Option<Arc<direct::SourceDirectFile>>,
    retirement_deadline: std::sync::OnceLock<Instant>,
    state: std::sync::Mutex<SourceViewerState>,
    changed: tokio::sync::Notify,
}
#[derive(Default)]
struct SourcePreparationSettlements {
    index: Option<crate::fragindex::SourceIndexSettlement>,
    probe: Option<super::source_preparation::SourceProbeSettlement>,
    processed: Option<super::source_preparation::SourceProcessedSettlement>,
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
                .processed
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
    backing: SourceBacking,
    native: Option<Arc<super::source_subtitles::SourceNativeTracks>>,
    /// The bounded probe evidence a copy was planned from. The master
    /// presents it whether or not native subtitle tracks were prepared: an
    /// HDR or Dolby Vision session opens on the master for its range facts.
    presentation_probe: Option<String>,
    start: Option<Result<crate::http::hls::StartResponse, SourceWorkerError>>,
    retirement_requested: bool,
    retirement_mode: crate::sharing_connection_custody::DriverCloseMode,
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
    processed_cancel: Option<tokio_util::sync::CancellationToken>,
    processed_activity: Option<SourceProcessedActivity>,
}

#[derive(Clone)]
pub(crate) struct SourceProcessedActivity {
    pub(crate) stop_id: String,
    pub(crate) session_id: Option<String>,
    pub(crate) title: String,
    pub(crate) viewer: String,
    pub(crate) grant: String,
    pub(crate) file_id: i64,
    pub(crate) item_id: i64,
    pub(crate) started_unix: i64,
    pub(crate) reason: String,
}

/// A waiter for one actual registered owner. Dropping this value cannot cancel
/// its detached producer or imply release of its durable dispatch obligation.
#[derive(Clone)]
pub(crate) struct SourceViewerActor(Arc<SourceViewerInner>);

pub(crate) enum SourceResourcePayload {
    Playlist(Vec<u8>),
    SubtitleText(Vec<u8>),
    CachedSubtitle {
        bytes: Vec<u8>,
        complete: bool,
        absent: bool,
    },
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
    pub(crate) source_request: Option<crate::source_custody::SourceRequest>,
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
    pub(crate) fn assignment(&self) -> &SourceDispatchAssignment {
        &self.owner.assignment
    }
    pub(crate) fn retirement_deadline(&self) -> Instant {
        *self
            .owner
            .retirement_deadline
            .get_or_init(|| Instant::now() + Duration::from_secs(305))
    }
    pub(crate) fn retirement_mode(&self) -> crate::sharing_connection_custody::DriverCloseMode {
        if self
            .source
            .as_ref()
            .is_some_and(|source| !source.unchanged())
        {
            crate::sharing_connection_custody::DriverCloseMode::Revoke
        } else {
            self.owner
                .state
                .lock()
                .expect("Source retirement cause")
                .retirement_mode
        }
    }
    pub(crate) fn request_revocation(&self) {
        let mut state = self.owner.state.lock().expect("Source retirement cause");
        state.retirement_mode = crate::sharing_connection_custody::DriverCloseMode::Revoke;
        state.retirement_requested = true;
        drop(state);
        self.owner.changed.notify_waiters();
    }
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
#[cfg(test)]
impl TranscodeManager {
    /// Hold the Source's cold-index scan after its child spawns, so a test can
    /// act while a start is mid-preparation.
    pub(crate) fn pause_source_index_after_spawn(
        &self,
        bound: Duration,
    ) -> Arc<crate::seam_hooks::AsyncPause> {
        self.source_workers.index_hooks.pause_after_spawn_for(bound)
    }
    /// Source owners still holding a worker registry slot.
    pub(crate) fn source_worker_count(&self) -> usize {
        self.source_workers
            .entries
            .lock()
            .expect("Source workers")
            .len()
    }
}
impl SourceViewerActor {
    /// Transport bookkeeping only. This observes the real physical/body/SQL
    /// result; a missing actor, lease expiry or acknowledgement cannot mint it.
    pub(crate) fn settlement_status(&self) -> Option<Result<(), SourceWorkerError>> {
        self.0.state.lock().expect("Source worker state").settled
    }
    /// The only answer here that can pass is its own `Deadline`: a stored
    /// start failure is that start's final answer and a retirement is final.
    /// [`wait_ready_refusal`] says so for callers that answer a live viewer.
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
    pub(crate) async fn retire_with_deadline(
        &self,
        deadline: Instant,
    ) -> Result<(), SourceWorkerError> {
        self.0.retirement_deadline.get_or_init(|| deadline);
        self.retire().await
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
        let backing = {
            let mut current = self.0.state.lock().expect("Source worker state");
            current.retirement_requested = true;
            if let Some(cancel) = current.processed_cancel.as_ref() {
                cancel.cancel();
            }
            current.backing.clone()
        };
        #[cfg(unix)]
        match backing {
            SourceBacking::Growing(backing) => backing.retire(),
            SourceBacking::PreparingRange(preparing) => preparing.retire(),
            SourceBacking::ImmutableVod => {}
        }
        self.0.changed.notify_waiters();
    }
    /// Metadata retries have a real Body obligation but do not represent
    /// viewer media activity and therefore never touch the VOD idle clock.
    #[cfg(unix)]
    pub(crate) fn growing_backing(&self) -> Option<Arc<SourceGrowingBacking>> {
        match &self.0.state.lock().expect("Source worker state").backing {
            SourceBacking::ImmutableVod | SourceBacking::PreparingRange(_) => None,
            SourceBacking::Growing(backing) => Some(Arc::clone(backing)),
        }
    }
    /// Open the live Start response. Every refusal is a [`LiveRefusal`]
    /// decided at its check; a terminal one retires this actor, so the
    /// session's end is also the end Status reports.
    pub(crate) async fn open_start_response(
        &self,
        deadline: Instant,
    ) -> Result<(crate::http::hls::StartResponse, SourceResponseGuard), LiveRefusal> {
        let opened = self.open_start_response_classified(deadline).await;
        self.retire_on_terminal(opened.as_ref().err());
        opened
    }
    /// A Start or Status refusal that cannot pass ends this owner, in the
    /// same mode its renewal tick ends with when it loses authority: the
    /// session cannot continue, so its connections close rather than drain.
    pub(super) fn retire_on_terminal(&self, refusal: Option<&LiveRefusal>) {
        if refusal.is_some_and(|refusal| !refusal.is_transient()) {
            self.0
                .state
                .lock()
                .expect("Source retirement cause")
                .retirement_mode = crate::sharing_connection_custody::DriverCloseMode::Revoke;
            self.request_retirement();
        }
    }
    /// The source file this owner serves, for a test that makes it
    /// unreadable without changing it.
    #[cfg(test)]
    pub(crate) fn source_path_for_test(&self) -> std::path::PathBuf {
        match (&self.0.direct, self.growing_backing()) {
            (Some(direct), _) => direct.path_for_test().to_path_buf(),
            (None, Some(backing)) => backing.file.path.clone(),
            (None, None) => panic!("a test source owner serves a file"),
        }
    }
    /// Stand in for a minute of wall time: the lease the Store last
    /// confirmed for this owner has passed.
    #[cfg(test)]
    pub(crate) fn lapse_lease_for_test(&self) {
        self.0
            .gate
            .lease
            .confirmed_until_ms
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }
    /// Whether this owner is ending: nothing it answers can pass any more.
    pub(crate) fn is_retiring(&self) -> bool {
        self.0
            .state
            .lock()
            .expect("Source worker state")
            .retirement_requested
    }
    #[cfg(test)]
    pub(crate) fn retirement_requested_for_test(&self) -> bool {
        self.is_retiring()
    }
    /// Wait until retirement is requested, without polling.
    #[cfg(test)]
    pub(crate) async fn retirement_requested_wait_for_test(&self) {
        loop {
            let notification = self.0.changed.notified();
            tokio::pin!(notification);
            notification.as_mut().enable();
            if self.is_retiring() {
                return;
            }
            notification.await;
        }
    }
    /// Wait until `count` more renewal ticks have begun. The second one
    /// beginning means the first ran to its end without ending the owner.
    #[cfg(test)]
    pub(crate) async fn ticks_for_test(&self, count: u64) {
        let mut ticks = self.0.gate.ticks.subscribe();
        let target = *ticks.borrow_and_update() + count;
        ticks
            .wait_for(|ticks| *ticks >= target)
            .await
            .expect("the gate outlives its owner");
    }
    /// The growing deadline judgement, on this real owner.
    #[cfg(all(test, unix))]
    pub(crate) fn deadline_refusal_for_test(&self, stalled: bool) -> LiveRefusal {
        self.deadline_refusal(stalled)
    }
    /// Wait for settlement without requesting it.
    #[cfg(test)]
    pub(crate) async fn settled_for_test(&self) -> Result<(), SourceWorkerError> {
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
    async fn open_start_response_classified(
        &self,
        deadline: Instant,
    ) -> Result<(crate::http::hls::StartResponse, SourceResponseGuard), LiveRefusal> {
        // Each refusal names its check and how long it lasts. Hashed as every
        // log line carries it: the id is a capability.
        let incarnation =
            super::session_log_id(&self.0.assignment.binding().incarnation_id().to_string());
        let refused = |check: &'static str, refusal: LiveRefusal| {
            tracing::warn!(target: "plurxd::transcode", incarnation = %incarnation, check, transient = refusal.is_transient(), "Source start response refused");
            refusal
        };
        // A stored start failure was already warned once, where it happened
        // ("Source producer startup failed"); a retirement, a deadline or a
        // start not yet published are expected or transient. Every Status or
        // control call replays these, so they stay at debug.
        let response = self.wait_ready(deadline).await.map_err(|error| {
            let refusal = wait_ready_refusal(error);
            tracing::debug!(target: "plurxd::transcode", incarnation = %incarnation, check = "wait_ready", error_class = ?error, transient = refusal.is_transient(), "Source start response refused");
            refusal
        })?;
        let manager = self.0.manager.upgrade().ok_or_else(|| {
            refused(
                "manager",
                LiveRefusal::terminal(SourceWorkerError::Unavailable),
            )
        })?;
        {
            let mut state = self.0.state.lock().expect("Source worker state");
            let check = if state.retirement_requested {
                Some((
                    "retirement_requested",
                    LiveRefusal::terminal(SourceWorkerError::Unavailable),
                ))
            } else if state.bodies >= 64 {
                // Bodies close as their transfers end.
                Some((
                    "body_limit",
                    LiveRefusal::transient(SourceWorkerError::Unavailable),
                ))
            } else if state
                .start
                .as_ref()
                .and_then(|start| start.as_ref().ok())
                .is_none_or(|start| start.session_id != response.session_id)
            {
                Some((
                    "session_changed",
                    LiveRefusal::terminal(SourceWorkerError::Unavailable),
                ))
            } else {
                None
            };
            if let Some((check, refusal)) = check {
                drop(state);
                return Err(refused(check, refusal));
            }
            state.bodies += 1;
        }
        let mut guard = SourceResponseGuard {
            owner: Arc::clone(&self.0),
            source: None,
            source_request: None,
        };
        let traced = |check: &'static str| {
            let incarnation = &incarnation;
            move |refusal: &LiveRefusal| {
                tracing::warn!(target: "plurxd::transcode", incarnation = %incarnation, check, error_class = ?refusal.error, transient = refusal.is_transient(), "Source start response refused");
            }
        };
        #[cfg(unix)]
        if let Some(backing) = self.growing_backing() {
            guard.source = Some(
                backing
                    .physical_fence()
                    .await
                    .map_err(|refusal| self.0.gate.bounded(refusal))
                    .inspect_err(traced("physical_fence"))?,
            );
            let proof = self
                .0
                .gate
                .current_owned(&self.0.assignment)
                .await
                .inspect_err(traced("current_owned"))?;
            let route = self
                .0
                .gate
                .renew_with(&self.0.assignment, &proof)
                .await
                .inspect_err(traced("route_renewal"))?;
            let terminal = LiveRefusal::terminal(SourceWorkerError::Unavailable);
            let check = if route.session_id != response.session_id
                || route.publication_ready_at_ms != 0
                || route.response_json
                    != serde_json::to_string(&response)
                        .map_err(|_| refused("response_json", terminal))?
            {
                Some("route_changed")
            } else if self
                .0
                .state
                .lock()
                .expect("Source worker state")
                .retirement_requested
            {
                Some("retirement_requested")
            } else if !backing.is_live() {
                Some("growing_copy_not_live")
            } else if guard
                .source
                .as_ref()
                .is_none_or(|source| !source.unchanged())
            {
                Some("source_changed")
            } else {
                None
            };
            if let Some(check) = check {
                return Err(refused(check, terminal));
            }
            return Ok((response, guard));
        }
        // A frozen VOD's facts or owner gone, or an object that cannot be
        // re-proven, is its end.
        let terminal = LiveRefusal::terminal(SourceWorkerError::Unavailable);
        let facts = manager
            .vod
            .hls_facts(&response.session_id)
            .await
            .ok_or_else(|| refused("vod_facts", terminal))?;
        guard.source = Some(
            manager
                .vod
                .source_response_physical_fence(&facts.response_owner)
                .await
                .map_err(|error| refused("physical_fence", self.0.gate.fence_refusal(&error)))?,
        );
        let proof = self
            .0
            .gate
            .current_owned(&self.0.assignment)
            .await
            .inspect_err(traced("current_owned"))?;
        let route = self
            .0
            .gate
            .renew_with(&self.0.assignment, &proof)
            .await
            .inspect_err(traced("route_renewal"))?;
        let check = if route.session_id != response.session_id
            || route.publication_ready_at_ms != 0
            || route.response_json
                != serde_json::to_string(&response)
                    .map_err(|_| refused("response_json", terminal))?
        {
            Some("route_changed")
        } else if self
            .0
            .state
            .lock()
            .expect("Source worker state")
            .retirement_requested
        {
            Some("retirement_requested")
        } else if !manager
            .vod
            .response_owner_is_live(&response.session_id, &facts.response_owner)
            .await
        {
            Some("vod_owner_not_live")
        } else if guard
            .source
            .as_ref()
            .is_none_or(|source| !source.unchanged())
        {
            Some("source_changed")
        } else {
            None
        };
        if let Some(check) = check {
            return Err(refused(check, terminal));
        }
        Ok((response, guard))
    }
    /// Actual selected-caption work shares the control's counted physical
    /// guard. Its HTTP observer may leave; this factory retains custody until
    /// the child, pipes, sparse readers and cache jobs have really settled.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_native_window_work(
        &self,
        tracks: Arc<super::source_subtitles::SourceNativeTracks>,
        manager: Arc<TranscodeManager>,
        index: u16,
        demand: i64,
        span: i64,
        custody: resource::SourceResourceReadCustody,
        deadline: Instant,
    ) -> tokio::task::JoinHandle<Result<Vec<u8>, SourceWorkerError>> {
        tokio::spawn(async move {
            let result = tracks
                .window(&manager, index, demand, span, &custody, deadline)
                .await;
            if matches!(
                result,
                Err(SourceWorkerError::Unavailable
                    | SourceWorkerError::Conflict
                    | SourceWorkerError::Unresolved)
            ) {
                custody.request_revocation();
            }
            result
        })
    }
    /// Every refusal states whether it passes ([`LiveRefusal`]); the
    /// receiver retires a live session only over one that does not.
    pub(crate) async fn open_resource(
        &self,
        resource: &SharingHlsResource,
        deadline: Instant,
    ) -> Result<SourceOpenedResource, LiveRefusal> {
        // For a segment or init, this deadline bounds the same wait the
        // growing segment wait does, so it is judged the same way: by whether
        // production advanced. Any other resource waits on no production,
        // and its deadline says nothing about the producer.
        let segment = matches!(
            resource.kind(),
            SharingHlsResourceKind::Init | SharingHlsResourceKind::MediaSegment
        );
        #[cfg(unix)]
        let growing = match self.growing_backing().filter(|_| segment) {
            Some(backing) => {
                let waited = backing.production.mark().await;
                Some((backing, waited))
            }
            None => None,
        };
        // A frozen VOD's segment wait has no progress signal here; its
        // deadline ends the request as it always has.
        let vod_segment = segment && self.0.direct.is_none() && self.growing_backing().is_none();
        let actor = self.clone();
        let resource = resource.clone();
        let job = tokio::spawn(Box::pin(async move {
            actor.open_resource_owned(&resource, deadline).await
        }));
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), job).await {
            Ok(joined) => {
                joined.map_err(|_| LiveRefusal::terminal(SourceWorkerError::Unresolved))?
            }
            #[cfg(unix)]
            Err(_) if growing.is_some() => {
                let (backing, waited) = growing.expect("growing wait");
                Err(self.deadline_refusal(backing.production.stalled_since(waited).await))
            }
            Err(_) if vod_segment => Err(LiveRefusal::terminal(SourceWorkerError::Deadline)),
            Err(_) => Err(LiveRefusal::transient(SourceWorkerError::Deadline)),
        }
    }
    async fn open_resource_owned(
        &self,
        resource: &SharingHlsResource,
        deadline: Instant,
    ) -> Result<SourceOpenedResource, LiveRefusal> {
        let unsupported = LiveRefusal::terminal(SourceWorkerError::Unsupported);
        let ended = LiveRefusal::terminal(SourceWorkerError::Unavailable);
        let pending = LiveRefusal::transient(SourceWorkerError::Unavailable);
        if Instant::now() >= deadline {
            return Err(LiveRefusal::transient(SourceWorkerError::Deadline));
        }
        if self.0.direct.is_some() {
            return Err(unsupported);
        }
        #[cfg(unix)]
        if let Some(backing) = self.growing_backing() {
            return self
                .open_growing_resource_owned(backing, resource, deadline)
                .await;
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
                SharingHlsResourceKind::Video
                    | SharingHlsResourceKind::SubtitlePlaylist { .. }
                    | SharingHlsResourceKind::SubtitleSegment { .. }
            ) && native.is_none())
        {
            return Err(unsupported);
        }
        let kind = if resource.kind() == SharingHlsResourceKind::Index
            && query.is_some_and(|query| query.split('&').any(|part| part == "native=1"))
        {
            SharingHlsResourceKind::Master
        } else {
            resource.kind()
        };
        let manager = self.0.manager.upgrade().ok_or(ended)?;
        let session_id = {
            let mut state = self.0.state.lock().expect("Source worker state");
            if state.retirement_requested {
                return Err(ended);
            }
            // Bodies close as their transfers end.
            if state.bodies >= 64 {
                return Err(pending);
            }
            let session_id = state
                .start
                .as_ref()
                .and_then(|start| start.as_ref().ok())
                .ok_or(ended)?
                .session_id
                .clone();
            state.bodies += 1;
            session_id
        };
        let mut guard = SourceResponseGuard {
            owner: Arc::clone(&self.0),
            source: None,
            source_request: None,
        };
        let initial = self.0.gate.current_owned(&self.0.assignment).await?;
        // An observation older than its bound is refreshed by the next read.
        initial
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| pending)?;
        let facts = manager.vod.hls_facts(&session_id).await.ok_or(ended)?;
        if !manager
            .vod
            .source_status_owner_is_current(&session_id, &facts.response_owner, &self.0.assignment)
            .await
        {
            return Err(ended);
        }
        // An object proven changed or gone is the session's end; one that
        // could not be read just now passes, within the confirmed lease.
        guard.source = Some(
            manager
                .vod
                .source_response_physical_fence(&facts.response_owner)
                .await
                .map_err(|error| self.0.gate.fence_refusal(&error))?,
        );
        let guard = Arc::new(guard);
        let custody = resource::SourceResourceReadCustody::new(
            Arc::clone(&guard),
            Arc::clone(&self.0.resource_hooks),
        );
        let (payload, owner) = match kind {
            SharingHlsResourceKind::Index | SharingHlsResourceKind::Video => {
                // A playlist not yet published, or not readable this once.
                let publication = manager.vod.playlist(&session_id).await.ok_or(pending)?;
                (
                    SourceResourcePayload::Playlist(publication.result.map_err(|_| pending)?),
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
                    .ok_or(ended)?;
                // A VOD segment not produced before its deadline ends the
                // request, as it always has: the VOD generation that produces
                // it reports no progress here to tell a slow job from a stuck
                // one.
                (
                    SourceResourcePayload::File(
                        publication.result.map_err(|_| ended)?.ok_or(ended)?,
                    ),
                    publication.owner,
                )
            }
            SharingHlsResourceKind::Master
            | SharingHlsResourceKind::SubtitlePlaylist { .. }
            | SharingHlsResourceKind::SubtitleSegment { .. } => {
                // The master needs no prepared subtitle tracks: without them
                // it admits none and still declares the session's range.
                let tracks = native.as_ref();
                if kind != SharingHlsResourceKind::Master && tracks.is_none() {
                    return Err(unsupported);
                }
                let probe = self
                    .0
                    .state
                    .lock()
                    .expect("Source copy presentation")
                    .presentation_probe
                    .clone();
                let publication = manager.vod.playlist(&session_id).await.ok_or(pending)?;
                let video = publication.result.map_err(|_| pending)?;
                let facts = manager.vod.hls_facts(&session_id).await.ok_or(ended)?;
                if facts.file.id.to_string() != self.0.assignment.binding().file_id().as_str()
                    || !manager
                        .vod
                        .response_owner_is_live(&session_id, &facts.response_owner)
                        .await
                {
                    return Err(ended);
                }
                if tracks.is_some_and(|tracks| !tracks.matches(&self.0.assignment, &facts.file)) {
                    return Err(ended);
                }
                let (file, context) = source_native_presentation(facts, probe.as_deref());
                let payload = match (kind, tracks) {
                    (SharingHlsResourceKind::Master, tracks) => {
                        SourceResourcePayload::Playlist(crate::http::hls::source_native_master(
                            &file,
                            tracks.and_then(|tracks| tracks.selected()),
                            &context,
                            &tracks.map(|tracks| tracks.indexes()).unwrap_or_default(),
                        ))
                    }
                    (SharingHlsResourceKind::SubtitlePlaylist { index }, Some(tracks)) => {
                        if !tracks.has_track(index) {
                            return Err(unsupported);
                        }
                        SourceResourcePayload::Playlist(crate::http::hls::source_native_playlist(
                            &video,
                        ))
                    }
                    (SharingHlsResourceKind::SubtitleSegment { index }, Some(tracks)) => {
                        if !tracks.has_track(index) {
                            return Err(unsupported);
                        }
                        let name = resource
                            .as_str()
                            .split('?')
                            .next()
                            .unwrap_or("")
                            .rsplit('/')
                            .next()
                            .unwrap_or("");
                        if let Some(revision) = resource.cached_subtitle_revision() {
                            let cached = tracks
                                .cached(index, revision, &custody)
                                .await
                                .map_err(growing::subtitle_refusal)?;
                            let absent = cached.is_none();
                            let (bytes, complete) = cached.unwrap_or_default();
                            SourceResourcePayload::CachedSubtitle {
                                bytes,
                                complete,
                                absent,
                            }
                        } else {
                            let sequence = name
                                .strip_prefix("seg")
                                .and_then(|name| name.strip_suffix(".vtt"))
                                .and_then(|name| name.parse::<u64>().ok())
                                .ok_or(unsupported)?;
                            let bytes = tracks
                                .segment(
                                    &manager,
                                    index,
                                    &video,
                                    sequence,
                                    context.media_origin_seconds,
                                    &custody,
                                    deadline,
                                )
                                .await
                                .map_err(growing::subtitle_refusal)?
                                .ok_or(pending)?;
                            if bytes.len() > 2 * 1024 * 1024 {
                                return Err(unsupported);
                            }
                            SourceResourcePayload::SubtitleText(bytes)
                        }
                    }
                    _ => return Err(unsupported),
                };
                (payload, publication.owner)
            }
        };
        if let SourceResourcePayload::Playlist(bytes) = &payload {
            plurx_core::sharing_resources::validate_sharing_playlist(resource, bytes)
                .map_err(|_| unsupported)?;
        }

        // Detached observation survives waiter timeout only to settle actual
        // jobs; an expired request cannot extend authority or viewer activity.
        // A VOD segment past its deadline ends the request, as it always has.
        if Instant::now() >= deadline {
            return Err(
                if matches!(
                    kind,
                    SharingHlsResourceKind::Init | SharingHlsResourceKind::MediaSegment
                ) {
                    LiveRefusal::terminal(SourceWorkerError::Deadline)
                } else {
                    LiveRefusal::transient(SourceWorkerError::Deadline)
                },
            );
        }
        // A parked segment can outlive the first observation's five-second
        // window. Re-observe real membership/current binding after the wait.
        let proof = self.0.gate.current_owned(&self.0.assignment).await?;
        proof
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| pending)?;
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
            return Err(ended);
        }
        if guard
            .source
            .as_ref()
            .is_none_or(|source| !source.unchanged())
        {
            return Err(ended);
        }
        if Instant::now() >= deadline
            || proof
                .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
                .is_err()
        {
            return Err(pending);
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
            return Err(ended);
        }
        if guard
            .source
            .as_ref()
            .is_none_or(|source| !source.unchanged())
        {
            return Err(ended);
        }
        drop(custody);
        // A response body still shares this guard; it is released with it.
        let guard = Arc::try_unwrap(guard).map_err(|_| pending)?;
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
            .source_members.observe()
            .await
            .map_err(|_| { tracing::warn!(target: "plurxd::transcode", stage = "source_preparation.members_store", "Source preparation refused"); SourceWorkerError::Unavailable })?
            .ok_or_else(|| { tracing::warn!(target: "plurxd::transcode", stage = "source_preparation.members_absent", "Source preparation refused"); SourceWorkerError::Unavailable })?;
        if !self.ingress.assignment().same_identity(assignment) {
            tracing::warn!(target: "plurxd::transcode", stage = "source_preparation.assignment_identity", "Source preparation refused");
            return Err(SourceWorkerError::Conflict);
        }
        let Some(permission) = self
            .store
            .refresh_source_ingress_admission(&self.ingress, &members)
            .await
            .map_err(|_| { tracing::warn!(target: "plurxd::transcode", stage = "source_preparation.permission_store", "Source preparation refused"); SourceWorkerError::Unresolved })?
        else {
            tracing::warn!(target: "plurxd::transcode", stage = "source_preparation.permission_refused", "Source preparation refused"); return Err(SourceWorkerError::Unavailable);
        };
        permission
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| { tracing::warn!(target: "plurxd::transcode", stage = "source_preparation.permission_freshness", "Source preparation refused"); SourceWorkerError::Unavailable })?;
        let SourceWriteAuthorityRead::Ready(proof) = self
            .store
            .prepare_source_activation_authority(assignment, &self.master, &members)
            .await
            .map_err(|_| { tracing::warn!(target: "plurxd::transcode", stage = "source_preparation.activation_store", "Source preparation refused"); SourceWorkerError::Unresolved })?
        else {
            tracing::warn!(target: "plurxd::transcode", stage = "source_preparation.activation_refused", "Source preparation refused"); return Err(SourceWorkerError::Unavailable);
        };
        if !self
            .store
            .authorize_source_media_preparation(&proof)
            .await
            .map_err(|_| { tracing::warn!(target: "plurxd::transcode", stage = "source_preparation.preparation_store", "Source preparation refused"); SourceWorkerError::Unresolved })?
        {
            tracing::warn!(target: "plurxd::transcode", stage = "source_preparation.preparation_refused", "Source preparation refused"); return Err(SourceWorkerError::Unavailable);
        }
        Ok(proof)
    }

    /// A fence failure as a live refusal: an object proven changed or gone is
    /// terminal; one that could not be read just now passes, but only within
    /// the lease the Store last confirmed.
    pub(super) fn fence_refusal(
        &self,
        error: &crate::fragment_index_cluster::SourceFenceError,
    ) -> LiveRefusal {
        self.bounded(LiveRefusal::from_fence(error))
    }
    /// Seed the confirmed lease from the activation's own route.
    pub(super) fn activated(&self, route: &MediaSessionRoute) {
        self.lease.activated(route.lease_expires_at_ms);
    }
    /// What a renewal tick does after re-proving its source: renew the lease
    /// only for an object proven; skip this renewal for one that could not be
    /// read just now, so the confirmed lease bounds how long that can pass;
    /// end the owner for one proven changed, or once that lease has passed.
    pub(super) fn tick_fence<T>(&self, fenced: Result<T, LiveRefusal>) -> TickFence {
        self.lease
            .tick_fence(fenced, crate::fragment_index_cluster::unix_ms())
    }
    /// A refusal bounded by the lease the Store last confirmed.
    pub(super) fn bounded(&self, refusal: LiveRefusal) -> LiveRefusal {
        self.lease
            .bound(refusal, crate::fragment_index_cluster::unix_ms())
    }
    /// The owned route, its refusals bounded by the confirmed lease.
    async fn current_owned(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> Result<Box<plurx_core::sharing_source_sessions::SourceOwnedRouteAuthority>, LiveRefusal>
    {
        self.current_owned_read(assignment)
            .await
            .map_err(|refusal| {
                self.lease
                    .bound(refusal, crate::fragment_index_cluster::unix_ms())
            })
    }
    /// A store that could not answer, or a membership not yet observed, can
    /// pass; an owned-route authority the store refused cannot.
    async fn current_owned_read(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> Result<Box<plurx_core::sharing_source_sessions::SourceOwnedRouteAuthority>, LiveRefusal>
    {
        let members = self
            .source_members.observe()
            .await
            .map_err(|_| { tracing::warn!(target: "plurxd::transcode", stage = "source_owner.members", error_class = "store", "Source admission refused"); LiveRefusal::transient(SourceWorkerError::Unavailable) })?
            .ok_or_else(|| { tracing::warn!(target: "plurxd::transcode", stage = "source_owner.members", error_class = "absent", "Source admission refused"); LiveRefusal::transient(SourceWorkerError::Unavailable) })?;
        match self
            .store
            .prepare_source_owned_route_authority(assignment, &self.master, &members)
            .await
            .map_err(|_| { tracing::warn!(target: "plurxd::transcode", stage = "source_owner.authority", error_class = "store", "Source admission refused"); LiveRefusal::transient(SourceWorkerError::Unresolved) })?
        {
            SourceOwnedRouteAuthorityRead::Ready(proof) => Ok(proof),
            // The Store decides whether its refusal passes: a stale
            // membership guard, a lost lease race or a route not yet
            // published does; a refused authority does not.
            SourceOwnedRouteAuthorityRead::Unavailable(reason) => {
                tracing::warn!(target: "plurxd::transcode", stage = "source_owner.authority", error_class = ?reason, "Source admission refused");
                Err(if reason.passes() {
                    LiveRefusal::transient(SourceWorkerError::Unavailable)
                } else {
                    LiveRefusal::terminal(SourceWorkerError::Unavailable)
                })
            }
            // Slots free as other viewers' sessions end.
            SourceOwnedRouteAuthorityRead::Capacity => {
                tracing::warn!(target: "plurxd::transcode", stage = "source_owner.authority", error_class = "capacity", "Source admission refused");
                Err(LiveRefusal::transient(SourceWorkerError::Capacity))
            }
        }
    }
    /// One owner heartbeat: observe the owned route and renew its lease.
    pub(super) async fn renew_owned(
        &self,
        assignment: &SourceDispatchAssignment,
    ) -> Result<MediaSessionRoute, LiveRefusal> {
        let proof = self.current_owned(assignment).await?;
        self.renew_with(assignment, &proof).await
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
    ) -> Result<MediaSessionRoute, LiveRefusal> {
        self.renew_with_read(assignment, proof)
            .await
            .map_err(|refusal| {
                self.lease
                    .bound(refusal, crate::fragment_index_cluster::unix_ms())
            })
    }
    async fn renew_with_read(
        &self,
        assignment: &SourceDispatchAssignment,
        proof: &plurx_core::sharing_source_sessions::SourceOwnedRouteAuthority,
    ) -> Result<MediaSessionRoute, LiveRefusal> {
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
                tracing::warn!(target: "plurxd::transcode", stage = "source_owner.renew", error_class = "authority_changed_or_unchanged_refusal", "Source admission refused");
                return Err(LiveRefusal::terminal(SourceWorkerError::Unavailable));
            }
            fresh = Some(current);
        }
        // Every attempt lost the optimistic race to this same owner's other
        // renewals: contention, not a loss of authority.
        tracing::warn!(target: "plurxd::transcode", stage = "source_owner.renew", error_class = "revision_attempts_exhausted", "Source admission refused");
        Err(LiveRefusal::transient(SourceWorkerError::Unavailable))
    }
    /// A store that failed to read or write can pass; a route row that is
    /// gone is the session's end.
    async fn renew_observed(
        &self,
        assignment: &SourceDispatchAssignment,
        proof: &plurx_core::sharing_source_sessions::SourceOwnedRouteAuthority,
    ) -> Result<Option<MediaSessionRoute>, LiveRefusal> {
        let incarnation = assignment.binding().incarnation_id().to_string();
        let route = self
            .store
            .media_session_route_by_incarnation(&incarnation)
            .await
            .map_err(|_| { tracing::warn!(target: "plurxd::transcode", stage = "source_owner.route", error_class = "store", "Source admission refused"); LiveRefusal::transient(SourceWorkerError::Unresolved) })?
            .ok_or_else(|| { tracing::warn!(target: "plurxd::transcode", stage = "source_owner.route", error_class = "absent", "Source admission refused"); LiveRefusal::terminal(SourceWorkerError::Unavailable) })?;
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
                now.saturating_add(SOURCE_SESSION_LEASE_MS),
            )
            .await
            .map_err(|_| { tracing::warn!(target: "plurxd::transcode", stage = "source_owner.renew_write", error_class = "store", "Source admission refused"); LiveRefusal::transient(SourceWorkerError::Unresolved) })
            .inspect(|renewed| {
                if let Some(route) = renewed {
                    self.lease.confirmed(route.lease_expires_at_ms);
                }
            })
    }
}

impl TranscodeManager {
    /// The copy pipeline a Source cuts for a prepared copy recipe.
    ///
    /// Exactly the options local VOD builds (`copy_video_pipeline`) from the
    /// same inputs, with the node's boot-measured `dovi_rpu`
    /// ([`Self::dv_strippable`]): a Dolby Vision copy then has the
    /// fragment-index identity local analysis built, and a base strip uses
    /// `dovi_rpu=strip=1` rather than leaving a stale `dvcC` behind
    /// `filter_units`.
    pub(crate) fn source_copy_video_options(
        &self,
        file: &plurx_core::domain::MediaFile,
        probe: Option<&str>,
        preserve_dolby_vision: bool,
        convert_dolby_vision: bool,
    ) -> plurx_core::transcode::CopyVideoOptions {
        plurx_core::transcode::CopyVideoOptions::from_probe(
            file,
            probe,
            self.dv_strippable(),
            preserve_dolby_vision,
        )
        .with_dolby_vision_conversion(convert_dolby_vision)
    }

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
        ingress: SourceIngressAdmissionPermission,
        prepared: crate::http::hls::PreparedSourcePlayback,
        deadline: Instant,
    ) -> Result<SourceViewerActor, SourceWorkerNoAdmission> {
        if !Arc::ptr_eq(&self.store, &state.store)
            || !prepared.matches_assignment(&assignment)
            || !activation.assignment().same_identity(&assignment)
            || !ingress.assignment().same_identity(&assignment)
            || ingress.registry_boot_id() != state.sharing.accepted_drivers.boot_id()
        {
            tracing::warn!(target: "plurxd::transcode", stage = "source_factory.identity", same_store = Arc::ptr_eq(&self.store, &state.store), prepared_matches = prepared.matches_assignment(&assignment), activation_matches = activation.assignment().same_identity(&assignment), ingress_matches = ingress.assignment().same_identity(&assignment), registry_boot_matches = ingress.registry_boot_id() == state.sharing.accepted_drivers.boot_id(), "Source factory admission refused");
            return Err(SourceWorkerNoAdmission::refused(
                &assignment,
                SourceWorkerError::Conflict,
            ));
        }
        if ingress
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .is_err()
        {
            tracing::warn!(target: "plurxd::transcode", stage = "source_factory.permission_freshness", "Source factory admission refused");
            return Err(SourceWorkerNoAdmission::refused(
                &assignment,
                SourceWorkerError::Unavailable,
            ));
        }
        let registry_boot_id = state.sharing.accepted_drivers.boot_id();
        let mut registry = self.source_workers.entries.lock().expect("Source workers");
        if let Some(owner) = registry
            .iter()
            .find(|owner| owner.assignment.same_identity(&assignment))
        {
            return Ok(SourceViewerActor(Arc::clone(owner)));
        }
        if registry.len() >= 8 {
            tracing::warn!(target: "plurxd::transcode", stage = "source_factory.registry_capacity", "Source factory admission refused");
            return Err(SourceWorkerNoAdmission::refused(
                &assignment,
                SourceWorkerError::Capacity,
            ));
        }
        let owner = Arc::new(SourceViewerInner {
            control_target_duration_ms: i64::from(match prepared.request().kind {
                SessionKind::Copy { .. } => plurx_core::transcode::COPY_SEGMENT_MAX_SECS,
                SessionKind::Transcode { .. } => plurx_core::transcode::SEGMENT_SECONDS,
            }) * 1_000,
            control_hooks: Default::default(),
            status_hooks: Default::default(),
            resource_hooks: Arc::new(Default::default()),
            assignment,
            manager: Arc::downgrade(self),
            gate: Arc::new(SourceProducerAuthority::new(
                Arc::clone(&self.store),
                state.cluster.source_members(),
                Arc::clone(&state.sharing.key),
                registry_boot_id,
                ingress,
            )),
            retirement_deadline: Default::default(),
            direct: None,
            state: std::sync::Mutex::new(SourceViewerState {
                backing: SourceBacking::ImmutableVod,
                start: None,
                retirement_requested: false,
                retirement_mode: crate::sharing_connection_custody::DriverCloseMode::Drain,
                settled: None,
                finished: false,
                bodies: 0,
                planned_session: None,
                native: None,
                presentation_probe: None,
                direct_start: None,
                direct_activity: None,
                processed_cancel: None,
                processed_activity: None,
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
    pub(super) async fn admit_source_copy(
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
        source_node_id: &str,
        deadline: Instant,
        work: &mut Option<crate::fragindex::SourceIndexOperation>,
    ) -> Result<(), SourceWorkerError> {
        let file = prepared.file();
        if !matches!(file.video_codec.as_deref(), Some("h264" | "hevc" | "h265")) {
            // Existing warm indexes retain their established support policy.
            return Ok(());
        }
        // A Dolby Vision copy (strip, preserve or the Profile 7 conversion)
        // is indexed exactly as local indexes it: `index_pass` takes the
        // Dolby Vision passes from the same options the copy is cut with.
        let SessionKind::Copy {
            preserve_dolby_vision,
            convert_dolby_vision,
            ..
        } = prepared.request().kind
        else {
            return Err(SourceWorkerError::Unsupported);
        };
        let authority = owner.gate.current_preparation(&owner.assignment).await?;
        let stored_probe = self
            .store
            .source_index_probe_evidence(&authority)
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?;
        if stored_probe
            .as_ref()
            .is_some_and(|probe| probe.len() > 1024 * 1024)
        {
            return Err(SourceWorkerError::Unsupported);
        }
        owner
            .state
            .lock()
            .expect("Source copy presentation")
            .presentation_probe
            .clone_from(&stored_probe);
        // Match Source VOD's exact recipe, including parameter-set promotion
        // and retention. A plain-copy identity cannot stand in for either.
        let video = self.source_copy_video_options(
            file,
            stored_probe.as_deref(),
            preserve_dolby_vision,
            convert_dolby_vision,
        );
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
        let stored_probe = stored_probe.ok_or(SourceWorkerError::Unsupported)?;
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
        let crate::fragindex::IndexOutcome::Built(mut index) = built.outcome else {
            return Err(SourceWorkerError::Unavailable);
        };
        let fence =
            crate::fragment_index_cluster::open_source_playback_fence(file, Some(&object_version))
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?;
        owner.gate.current_preparation(&owner.assignment).await?;
        if matches!(file.video_codec.as_deref(), Some("hevc" | "h265")) {
            // This scan read the held local source, not a peer's index. Bind
            // its original-header proof to the actual assigned physical node
            // before VOD copy admission can rely on it.
            if source_node_id.is_empty() || source_node_id != owner.assignment.owner_node_id() {
                return Err(SourceWorkerError::Unavailable);
            }
            let proof = index
                .promotion
                .hevc_configuration
                .as_mut()
                .ok_or(SourceWorkerError::Unsupported)?;
            proof.source_node_id = source_node_id.to_owned();
            let verified = if video.retains_hevc_parameter_sets() {
                // Retaining copies deliver changing parameter sets rather
                // than deleting them; VOD does not require a constant proof.
                proof.current_on_node(&object_version, source_node_id)
            } else {
                proof.permits_on_node(&object_version, source_node_id)
            };
            if !verified {
                return Err(SourceWorkerError::Unsupported);
            }
        }
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

    pub(crate) fn source_processed_activities(&self) -> Vec<SourceProcessedActivity> {
        let entries = self.source_workers.entries.lock().expect("Source workers");
        entries
            .iter()
            .filter_map(|owner| {
                let current = owner.state.lock().expect("Source worker state");
                (!current.finished)
                    .then(|| current.processed_activity.clone())
                    .flatten()
            })
            .collect()
    }
    #[cfg(test)]
    pub(crate) fn arm_source_processed_activity_hook(&self, hook: SourceProcessedActivityHook) {
        *self
            .source_workers
            .processed_activity_hook
            .lock()
            .expect("Source processed activity hook") = Some(hook);
    }
    pub(crate) async fn stop_source_processed_session(
        &self,
        id: &str,
    ) -> Result<bool, SourceWorkerError> {
        let owner = {
            let entries = self.source_workers.entries.lock().expect("Source workers");
            entries
                .iter()
                .find(|owner| {
                    let current = owner.state.lock().expect("Source worker state");
                    !current.finished
                        && current
                            .processed_activity
                            .as_ref()
                            .is_some_and(|activity| activity.stop_id == id)
                })
                .cloned()
        };
        let Some(owner) = owner else {
            return Ok(false);
        };
        let actor = SourceViewerActor(owner);
        actor.request_retirement();
        actor.retire().await?;
        Ok(true)
    }

    async fn prepare_source_encoded_recipe(
        self: &Arc<Self>,
        owner: &Arc<SourceViewerInner>,
        prepared: &crate::http::hls::PreparedSourcePlayback,
        deadline: Instant,
        work: &mut Option<super::source_preparation::SourceProbeOperation>,
        processed_work: &mut Option<super::source_preparation::SourceProcessedOperation>,
        processed_root: &mut Option<crate::vodserve::PreparedRootAdmission>,
    ) -> Result<Arc<crate::vodencode::Encoding>, SourceWorkerError> {
        let permit = self.admit_source_copy(deadline).await.inspect_err(|error| { tracing::warn!(target: "plurxd::transcode", stage = "source_encoded.probe_admission", error_class = ?error, "Source encoded preparation refused"); })?;
        let source =
            crate::fragment_index_cluster::open_source_playback_fence(prepared.file(), None)
                .await
                .map_err(|_| { tracing::warn!(target: "plurxd::transcode", stage = "source_encoded.file_fence", "Source encoded preparation refused"); SourceWorkerError::Unavailable })?;
        let proof = owner.gate.current_preparation(&owner.assignment).await.inspect_err(|error| { tracing::warn!(target: "plurxd::transcode", stage = "source_encoded.probe_authority", error_class = ?error, "Source encoded preparation refused"); })?;
        // A burn's artifacts are made inside this same owned operation, from
        // the Source's own prepared request and scanned track facts.
        let burn = match prepared.request().subtitle_burn {
            Some(index) => {
                let stream = usize::try_from(index)
                    .ok()
                    .and_then(|ordinal| prepared.file().subtitle_streams.get(ordinal))
                    .ok_or(SourceWorkerError::Unsupported)?;
                Some(super::source_preparation::SourceBurnAsk {
                    subtitle_index: index,
                    bitmap: plurx_core::tracks::is_bitmap_subtitle(&stream.codec),
                    runtime_dir: self.runtime_cache.clone(),
                })
            }
            None => None,
        };
        // A Profile 5 encode needs the per-source pixel proof the RPU renderer
        // demands. The Source runs it as this operation's own children and
        // records the verdict, so the planner below finds it and spawns
        // nothing. Without the boot-proved renderer the planner refuses as it
        // does locally, so no proof is worth running.
        let key = Self::dovi_proof_key(prepared.file());
        let dovi_proof = self.dovi_reshape
            && plurx_core::playback::dolby_vision_needs_rpu_render(prepared.file())
            && self.dovi_proofs.verdict(&key).is_none();
        let scripted = if dovi_proof {
            self.hooks.get().scripted_dovi_pixel_proof(prepared.file())
        } else {
            None
        };
        *work = Some(super::source_preparation::start_source_probe(
            prepared.file().clone(),
            source,
            proof,
            permit,
            Arc::clone(&self.store),
            deadline,
            Arc::clone(&self.source_workers.probe_hooks),
            burn,
            dovi_proof && scripted.is_none(),
        ));
        let evidence = work
            .as_ref()
            .expect("Source probe owner")
            .outcome()
            .await
            .map_err(|_| { tracing::warn!(target: "plurxd::transcode", stage = "source_encoded.probe_outcome", "Source encoded preparation refused"); SourceWorkerError::Unavailable })?;
        self.source_workers.probe_hooks.after_evidence().await;
        if let Some(proof) = scripted.as_ref().or(evidence.dovi_proof()) {
            match proof.verdict() {
                Some(verdict) => self.dovi_proofs.record(key, verdict),
                // Not a verdict: nothing is remembered and the start answers
                // not-yet, exactly as an unfinished local proof does. The
                // planner is not reached, so it cannot run an unowned proof.
                None => {
                    tracing::warn!(target: "plurxd::transcode", stage = "source_encoded.dovi_proof", "Source encoded preparation refused");
                    return Err(SourceWorkerError::Unavailable);
                }
            }
        }
        if prepared.processed_upgrade_requested() {
            let manager = Arc::clone(self);
            let trial_owner = Arc::clone(owner);
            let request = prepared.request().clone();
            let ask = prepared.processed_upgrade_ask().clone();
            let deadline = deadline.min(Instant::now() + super::PROCESSED_UPGRADE_TRIAL_BUDGET);
            let activity = SourceProcessedActivity {
                stop_id: format!(
                    "source-{}",
                    super::session_log_id(&owner.assignment.binding().incarnation_id().to_string())
                ),
                session_id: None,
                title: prepared.file().path.file_name().map_or_else(
                    || "Shared title".to_owned(),
                    |name| name.to_string_lossy().into_owned(),
                ),
                viewer: match owner.assignment.binding().principal() {
                    plurx_core::playback_principal::PlaybackPrincipal::Sharing {
                        viewer_key,
                        ..
                    } => viewer_key.as_str().to_owned(),
                    _ => String::new(),
                },
                grant: match owner.assignment.binding().principal() {
                    plurx_core::playback_principal::PlaybackPrincipal::Sharing {
                        grant_id, ..
                    } => grant_id.to_string(),
                    _ => String::new(),
                },
                file_id: prepared.file().id,
                item_id: prepared.file().item_id,
                started_unix: crate::fragment_index_cluster::unix_ms() / 1000,
                reason: match prepared.request().dv_processing_destination {
                    Some(plurx_core::transcode::dv_processing::DvDestination::Profile81) => {
                        "Processed Dolby Vision upgrade; Source FEL re-encode setting".to_owned()
                    }
                    Some(plurx_core::transcode::dv_processing::DvDestination::Sdr) => {
                        "Processed SDR upgrade; Source Dolby Vision processing setting".to_owned()
                    }
                    _ => {
                        "Processed HDR10 upgrade; Source Dolby Vision processing setting".to_owned()
                    }
                },
            };
            *processed_work = Some(super::source_preparation::start_source_processed(
                owner.assignment.clone(),
                deadline,
                move |producer, cancel| async move {
                    {
                        let mut current =
                            trial_owner.state.lock().expect("Source processed activity");
                        current.processed_cancel = Some(cancel.clone());
                        current.processed_activity = Some(activity);
                        if current.retirement_requested {
                            cancel.cancel();
                        }
                    }
                    #[cfg(test)]
                    {
                        let hook = manager
                            .source_workers
                            .processed_activity_hook
                            .lock()
                            .expect("Source processed activity hook")
                            .take();
                        if let Some(hook) = hook {
                            hook(Arc::clone(&producer), cancel.clone()).await;
                        }
                    }
                    let current = trial_owner
                        .gate
                        .current_preparation(&trial_owner.assignment)
                        .await
                        .map_err(|_| {
                            "Source processed preparation authority unavailable".to_owned()
                        })?;
                    let trial = manager
                        .prepare_source_processed_upgrade(
                            &request,
                            &ask,
                            &evidence,
                            &current,
                            Arc::clone(&trial_owner.gate),
                            producer,
                            cancel,
                            tokio::time::Instant::from_std(deadline),
                        )
                        .await?;
                    trial_owner
                        .gate
                        .current_preparation(&trial_owner.assignment)
                        .await
                        .map_err(|_| {
                            "Source authority changed during processed preparation".to_owned()
                        })?;
                    Ok(trial)
                },
            ));
            let operation = processed_work.as_ref().expect("Source processed owner");
            let trial = operation.outcome().await.map_err(|reason| {
                if super::pending_fel_qualification(&reason).is_some() {
                    SourceWorkerError::NotYet
                } else {
                    SourceWorkerError::Unavailable
                }
            })?;
            owner.gate.current_preparation(&owner.assignment).await?;
            let encoding = Arc::clone(&trial.root.encoding);
            *processed_root = Some(trial.root);
            return Ok(encoding);
        }
        // The probe permit is already dropped only after actual reap/pipes.
        // Encoder admission is separate and cannot self-deadlock against it.
        let current = owner.gate.current_preparation(&owner.assignment).await?;
        Box::pin(self.prepare_source_vod_encoding(prepared, &evidence, &current))
            .await
            .map_err(|error| {
                let error_class = match super::vod_refusal(&error).map(|(code, _)| code) {
                    Some("vod_source_rescan_required") => "source_rescan_required",
                    Some("vod_video_geometry_unknown") => "video_geometry_unknown",
                    Some("vod_invalid_height") => "invalid_height",
                    Some("vod_audio_track_missing") => "audio_track_missing",
                    Some("vod_subtitle_track_missing") => "subtitle_track_missing",
                    Some("vod_subtitle_burn_unavailable") => "subtitle_burn_unavailable",
                    Some("vod_frame_cadence_unknown") => "frame_cadence_unknown",
                    Some("vod_decoder_plan_refused") => "decoder_plan_refused",
                    Some(_) => "other_vod_refusal",
                    None if super::is_start_infrastructure_error(&error) => "infrastructure",
                    None if super::unsupported_build_reason(&error).is_some() => "unsupported_build",
                    None if super::is_retryable_capacity_error(&error) => "capacity",
                    None => "unclassified",
                };
                tracing::warn!(target: "plurxd::transcode", stage = "source_encoded.encoder_plan", error_class, "Source encoded preparation refused");
                // The planner's own definite refusal (a refuted Profile 5
                // proof, a renderer this build did not prove) is the same
                // answer local gives, not an outage to retry.
                if error_class == "unsupported_build" {
                    SourceWorkerError::Unsupported
                } else {
                    SourceWorkerError::Unavailable
                }
            })
    }

    async fn run_source_owner(
        self: Arc<Self>,
        owner: Arc<SourceViewerInner>,
        state: Arc<crate::state::AppState>,
        prepared: crate::http::hls::PreparedSourcePlayback,
        deadline: Instant,
    ) {
        #[cfg(unix)]
        if prepared.source_presentation()
            == Some(crate::http::hls::SourceCopyPresentation::BoundedRange)
            && !source_recipe_is_encoded(prepared.request())
        {
            self.run_source_growing_owner(owner, state, prepared, deadline)
                .await;
            return;
        }
        let actor = SourceViewerActor(Arc::clone(&owner));
        let source_request = self.foreground_source_request(
            prepared.file(),
            &owner.assignment.binding().incarnation_id().to_string(),
            owner.assignment.binding().incarnation_id(),
            plurx_core::store::background_jobs_resources::ForegroundSourcePolicy::Rolling,
        );
        let mut preparation_custody = match source_request {
            Some(request) => match request.register("source-preparation").await {
                Ok(custody) => Some(custody),
                Err(_) => {
                    tracing::warn!(target: "plurxd::transcode", stage = "preparation_custody", error_class = ?SourceWorkerError::Unavailable, "Source producer startup failed");
                    let mut current = owner.state.lock().expect("Source worker state");
                    current.start = Some(Err(SourceWorkerError::Unavailable));
                    current.settled = Some(Err(SourceWorkerError::Unavailable));
                    current.finished = true;
                    drop(current);
                    owner.changed.notify_waiters();
                    return;
                }
            },
            None => None,
        };
        let mut reserved: Option<crate::vodserve::ReservedSourceVodRendition> = None;
        let mut index_work = None;
        let mut probe_work = None;
        let mut processed_work = None;
        let mut processed_root = None;
        let mut unowned_existing = true;
        let mut startup_stage = "deadline";
        // Retirement (a Source End, a drain, a revocation) ends a start that
        // has not published at its next stage boundary: after the route
        // proof, after native tracks, after recipe preparation, and after
        // reservation. Each stage runs to its own end under the start's
        // bounded budget, so a cold index the viewer left still completes
        // and persists for the next play, and nothing is dropped mid-await:
        // no custody registration, index write or file fence is abandoned
        // half-done. The registry slot and copy permit are freed when the
        // boundary is reached, by the ordinary settlement below.
        let retirement_requested = || {
            owner
                .state
                .lock()
                .expect("Source worker state")
                .retirement_requested
        };
        let start = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            Box::pin(async {
                if Instant::now() >= deadline {
                    return Err(SourceWorkerError::Deadline);
                }
                startup_stage = "route_absence";
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
                if retirement_requested() {
                    return Err(SourceWorkerError::Unavailable);
                }
                // This actual worker owns the configured queue/materialization
                // allowance. Its inherited deadline can only become shorter.
                startup_stage = "startup_budget";
                let startup_budget = self
                    .source_start_budget_for_request(prepared.request())
                    .await
                    .map_err(|_| SourceWorkerError::Unavailable)?;
                let deadline = deadline.min(Instant::now() + startup_budget);
                // The HTTP preparation refused these with typed reasons before
                // factory admission; this owner rechecks the same prepared facts
                // before admitting physical producer work.
                startup_stage = "delivery_policy";
                if (!prepared.native_subtitles().0 && prepared.native_subtitles().1.is_some())
                    || prepared.request().previous_session_id.is_some()
                    || prepared.request().reopen_reason.is_some()
                    || source_delivery_refusal(
                        prepared.request(),
                        prepared.file(),
                        prepared.native_subtitles(),
                    )
                    .is_some()
                {
                    return Err(SourceWorkerError::Unsupported);
                }
                startup_stage = "vod_settings";
                let settings = self
                    .vod_settings(prepared.request())
                    .await
                    .map_err(|_| SourceWorkerError::Unavailable)?
                    .ok_or(SourceWorkerError::Unavailable)?;
                if prepared.native_subtitles().0 {
                    let tracks = super::source_subtitles::SourceNativeTracks::prepare(
                        prepared.file(),
                        &owner.assignment,
                        prepared.native_subtitles().1,
                        state.subs_dir.clone(),
                    )
                    .map_err(|_| SourceWorkerError::Unsupported)?;
                    owner.state.lock().expect("Source native actor").native =
                        Some(Arc::new(tracks));
                }
                if retirement_requested() {
                    return Err(SourceWorkerError::Unavailable);
                }
                startup_stage = "recipe_preparation";
                let encoding = if source_recipe_is_encoded(prepared.request()) {
                    Some(
                        Box::pin(self.prepare_source_encoded_recipe(
                            &owner,
                            &prepared,
                            deadline,
                            &mut probe_work,
                            &mut processed_work,
                            &mut processed_root,
                        ))
                        .await?,
                    )
                } else {
                    Box::pin(self.prepare_source_cold_index(
                        &owner,
                        &prepared,
                        &state.node_id,
                        deadline,
                        &mut index_work,
                    ))
                    .await?;
                    None
                };
                // The prepared recipe (and a cold index, persisted) is done;
                // a viewer who left during it is acted on before admission.
                if retirement_requested() {
                    return Err(SourceWorkerError::Unavailable);
                }
                startup_stage = "vod_admission";
                let admitted = Box::pin(self.vod.prepare_admitted_source_vod_with_processed_root(
                    &prepared,
                    &owner.assignment,
                    &settings,
                    &self.admissions,
                    self.store.as_ref(),
                    deadline,
                    encoding,
                    self.dv_strippable(),
                    processed_root.take(),
                ))
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?;
                startup_stage = "vod_reservation";
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
                startup_stage = "start_facts";
                let reservation = reserved.as_mut().expect("Source reservation");
                let session_id = uuid::Uuid::new_v4().to_string();
                if let Some(activity) = owner
                    .state
                    .lock()
                    .expect("Source processed activity")
                    .processed_activity
                    .as_mut()
                {
                    activity.session_id = Some(super::session_log_id(&session_id));
                }
                let facts = reservation
                    .start_info(&session_id)
                    .map_err(|_| SourceWorkerError::Unavailable)?;
                let incarnation = owner.assignment.binding().incarnation_id().to_string();
                startup_stage = "response_projection";
                let response =
                    Box::pin(prepared.start_response(&state, facts.start_info(), &incarnation, 1))
                        .await
                        .map_err(|_| SourceWorkerError::Unavailable)?;
                // Capacity waiting invalidates the caller's earlier snapshot. Mint
                // actual current authority after admission and response construction.
                startup_stage = "activation_members";
                let members = owner
                    .gate
                    .source_members
                    .observe()
                    .await
                    .map_err(|_| SourceWorkerError::Unavailable)?
                    .ok_or(SourceWorkerError::Unavailable)?;
                startup_stage = "activation_authority";
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
                    lease_expires_at_ms: now.saturating_add(SOURCE_SESSION_LEASE_MS),
                    expected_desired_revision: None,
                };
                owner
                    .state
                    .lock()
                    .expect("Source worker state")
                    .planned_session = Some(session_id.clone());
                startup_stage = "activation_write";
                let outcome = self
                    .store
                    .activate_source_media_session(&authority, &activation)
                    .await
                    .map_err(|_| SourceWorkerError::Unresolved)?
                    .ok_or(SourceWorkerError::Unavailable)?;
                if outcome.route.session_id != session_id {
                    return Err(SourceWorkerError::Conflict);
                }
                owner.gate.activated(&outcome.route);
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
                startup_stage = "vod_commit";
                Box::pin(self.vod.commit_source_vod(
                    reservation,
                    &session_id,
                    &owner.gate,
                    &self.admissions,
                ))
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?;
                startup_stage = "physical_readiness";
                let physical = Box::pin(reservation.wait_ready(&self.vod, deadline))
                    .await
                    .map_err(|_| SourceWorkerError::Unavailable)?;
                if !physical.matches(&owner.assignment) {
                    return Err(SourceWorkerError::Conflict);
                }
                let members = owner
                    .gate
                    .source_members
                    .observe()
                    .await
                    .map_err(|_| SourceWorkerError::Unavailable)?
                    .ok_or(SourceWorkerError::Unavailable)?;
                startup_stage = "publication_authority";
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
                startup_stage = "publication_write";
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
        // Every start that does not publish ends here with its stage, once.
        if let Err(error) = &start {
            tracing::warn!(target: "plurxd::transcode", stage = startup_stage, error_class = ?error, retirement_requested = owner.state.lock().expect("Source worker state").retirement_requested, "Source producer startup failed");
        }
        if start.is_ok() {
            // Readiness helpers have returned; join their independent owners
            // before dropping this preparation-only source charge. Materializer
            // generations have separate exact reaper-owned ledger tokens.
            if let Some(work) = index_work.as_ref() {
                let _ = work.settle().await;
            }
            if let Some(work) = probe_work.as_ref() {
                let _ = work.settle().await;
            }
            drop(preparation_custody.take());
        }
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
                            // The lease is renewed only for an object proven
                            // unchanged. One that could not be read just now is
                            // retried at the next tick without renewing, so the
                            // confirmed lease bounds how long it can pass.
                            let fenced = self.vod.source_response_physical_fence(&facts.response_owner).await.map_err(|error| LiveRefusal::from_fence(&error));
                            match owner.gate.tick_fence(fenced) { TickFence::Renew => {}, TickFence::Skip => continue, TickFence::End => break }
                        }
                        // Only a refusal that does not pass ends the owner. A
                        // transient one is retried at the next tick, bounded by
                        // the lease the Store last confirmed for this owner.
                        if owner.gate.renew_owned(&owner.assignment).await.is_err_and(|refusal| !refusal.is_transient()) {
                            owner.state.lock().expect("Source retirement cause").retirement_mode=crate::sharing_connection_custody::DriverCloseMode::Revoke;
                            break;
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
        if let Some(work) = processed_work.as_ref() {
            work.cancel();
            preparations.processed = Some(work.settle().await);
        }
        // A trial completed just before retirement can still have an unattached
        // root. Its held source and admission close before the SQL owner does.
        drop(processed_root.take());
        // These operations' settle methods join the detached reader/child,
        // including work which outlived the startup deadline.
        drop(preparation_custody);
        self.finish_source_owner(
            &owner,
            &state,
            shutdown,
            unowned_existing,
            reserved,
            preparations,
        )
        .await;
    }
    /// Settle a stopped Source owner, VOD or direct: release its physical
    /// producer (if any) and its row through the bounded detached retry, then
    /// leave the registry. The owner is finished on every exit.
    async fn finish_source_owner(
        &self,
        owner: &Arc<SourceViewerInner>,
        app_state: &crate::state::AppState,
        shutdown: tokio_util::sync::CancellationToken,
        unowned_existing: bool,
        mut reserved: Option<crate::vodserve::ReservedSourceVodRendition>,
        mut preparations: SourcePreparationSettlements,
    ) {
        let deadline = *owner
            .retirement_deadline
            .get_or_init(|| Instant::now() + Duration::from_secs(305));
        let mut physical = None;
        let mut attempts = 0;
        let settlement = loop {
            attempts += 1;
            // A route this owner did not activate is never its to release.
            let result = if unowned_existing {
                Err(SourceSettlementFault::Mismatch)
            } else {
                let mode = owner
                    .state
                    .lock()
                    .expect("Source retirement cause")
                    .retirement_mode;
                if crate::http::shared_source_playback::settle_ingress_custody(
                    app_state,
                    &owner.assignment,
                    deadline,
                    mode,
                )
                .await
                .is_err()
                {
                    Err(SourceSettlementFault::Transient)
                } else {
                    Box::pin(self.settle_source_owner(
                        owner,
                        &mut reserved,
                        &mut physical,
                        &mut preparations,
                    ))
                    .await
                }
            };
            if Instant::now() >= deadline
                || result != Err(SourceSettlementFault::Transient)
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
            #[cfg(unix)]
            {
                let backing = owner
                    .state
                    .lock()
                    .expect("Source worker state")
                    .backing
                    .clone();
                let receipt = match backing {
                    SourceBacking::Growing(backing) => Some(
                        backing
                            .wait_stopped()
                            .await
                            .map_err(|_| SourceSettlementFault::Transient)?,
                    ),
                    SourceBacking::PreparingRange(preparing) => {
                        Some(preparing.wait_stopped().await)
                    }
                    SourceBacking::ImmutableVod => None,
                };
                if let Some(receipt) = receipt {
                    *physical = Some(SourcePhysicalSettlement::Registered(
                        Box::new(receipt),
                        std::mem::take(preparations),
                    ));
                }
            }
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
        #[cfg(unix)]
        {
            let backing = owner
                .state
                .lock()
                .expect("Source worker state")
                .backing
                .clone();
            if let SourceBacking::Growing(backing) = backing {
                if backing
                    .production
                    .files
                    .lock()
                    .await
                    .retire_payload()
                    .await
                    .is_err()
                {
                    tracing::warn!(target: "plurxd::transcode", "Source payload cache retirement remains incomplete after physical/body settlement");
                }
            }
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
/// Why a Source refuses a prepared HLS recipe, decided only from the
/// Source's own prepared request — the player's real caps through the shared
/// planner, never a B claim — and its own scanned file facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
///
/// No Dolby Vision or HDR shape is refused here: preservation, the Profile 7
/// conversion, a compatible-base encode, a Profile 5 encode (whose pixel
/// proof the Source runs as its own work) and native subtitles beside any
/// range are delivered exactly as local playback delivers them.
pub(crate) enum SourceDeliveryRefusal {
    /// A shape this Source does not build: a burn beside native subtitles, a
    /// burn of a downloaded sidecar or of a track the scan does not hold.
    Unsupported,
}

/// A burn always encodes, whatever the copy ask said: burn pixels need an
/// encoder, exactly as Local's VOD recipe treats a copy that burns.
pub(crate) fn source_recipe_is_encoded(request: &crate::transcode::SessionRequest) -> bool {
    matches!(request.kind, SessionKind::Transcode { .. }) || request.subtitle_burn.is_some()
}

pub(crate) fn source_delivery_refusal(
    request: &crate::transcode::SessionRequest,
    file: &plurx_core::domain::MediaFile,
    (native, _): (bool, Option<i64>),
) -> Option<SourceDeliveryRefusal> {
    if let Some(index) = request.subtitle_burn {
        let embedded = usize::try_from(index)
            .ok()
            .and_then(|ordinal| file.subtitle_streams.get(ordinal))
            .is_some();
        if native || !embedded || file.downloaded_subtitle(index).is_some() {
            return Some(SourceDeliveryRefusal::Unsupported);
        }
    }
    None
}

fn source_native_presentation(
    facts: crate::vodserve::VodHlsFacts,
    probe: Option<&str>,
) -> (plurx_core::domain::MediaFile, HlsContext) {
    if let Some(encoding) = facts.encoding.as_ref() {
        let file = encoded_vod_presentation_file(
            facts.file,
            encoding.options.target_height,
            encoding.delivered_output_grade(),
            Some(encoding.plan.output_contract()),
        );
        let mut codecs = super::ladder::transcoded_hls_codecs_for_plan(&encoding.plan);
        if file.audio_streams.is_empty() {
            codecs.truncate(codecs.find(',').unwrap_or(codecs.len()));
        }
        let context = HlsContext {
            codec_facts: Some(super::response::FrozenHlsCodecFacts::encoded(
                &encoding.plan,
            )),
            bandwidth: encoding.plan.output_contract().output_bandwidth(),
            file_id: file.id,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs,
            supplemental_codecs: encoding.processed_dv_supplemental(),
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
                dual_layer_as_copied: false,
            },
            probe,
        );
        let context = HlsContext {
            codec_facts: Some(super::response::FrozenHlsCodecFacts::audio(
                facts.audio_delivery.as_ref(),
                !facts.file.audio_streams.is_empty(),
                facts.aac,
            )),
            bandwidth: None,
            file_id: facts.file.id,
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            codecs,
            supplemental_codecs,
            frame_rate: frozen_video_frame_rate(probe),
        };
        (facts.file, context)
    }
}
