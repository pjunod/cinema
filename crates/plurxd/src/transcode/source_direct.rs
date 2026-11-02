//! Owned Source direct play. The same admission, assignment, lease, body
//! ledger, retirement and settlement as a Source VOD owner, with no producer:
//! the held file is the fence. HTTP callers only wait.
use super::*;
use crate::http::sharing_direct_wire::SourceDirectStart;
#[cfg(target_os = "linux")]
use futures_util::FutureExt;
#[cfg(target_os = "linux")]
use plurx_core::adaptive_resources::runtime::direct_source::{
    DirectSourceApplicationLease, DirectSourceBinding, DirectSourceBody, DirectSourceOwner,
    DirectSourceRequest, DirectSourceRetirementTicket,
};
#[cfg(target_os = "linux")]
use plurx_core::adaptive_resources::{ResourceVector, WorkloadDemand};
#[cfg(target_os = "linux")]
use plurx_core::process::observation::Metric;
#[cfg(target_os = "linux")]
use std::future::Future;
#[cfg(target_os = "linux")]
use std::pin::Pin;
#[cfg(target_os = "linux")]
use std::task::{Context, Poll};

#[cfg(target_os = "linux")]
type SharedTicket = Arc<tokio::sync::Mutex<DirectSourceRetirementTicket>>;
#[cfg(target_os = "linux")]
type DirectWorkerResult = DirectSourceRetirementTicket;
#[cfg(not(target_os = "linux"))]
type DirectWorkerResult = ();

#[cfg(target_os = "linux")]
pub(super) enum SourceDirectRecord {
    Active {
        owner_address: usize,
        worker: tokio::task::JoinHandle<DirectSourceRetirementTicket>,
    },
    Settling {
        restart_admission: Option<SourceRestartAdmission>,
        ticket: SharedTicket,
        worker: tokio::task::JoinHandle<
            Result<(), plurx_core::adaptive_resources::runtime::direct_source::DirectSourceError>,
        >,
    },
    Failed,
}

#[cfg(target_os = "linux")]
struct BudgetedSourceWorker<F> {
    future: Pin<Box<F>>,
    _owner: Arc<SourceViewerInner>,
}
#[cfg(target_os = "linux")]
impl<F: Future> Future for BudgetedSourceWorker<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().future.as_mut().poll(cx)
    }
}

/// A published direct session with no open body and no new byte request for
/// this long retires, the same idle bound the VOD presentation uses. Status
/// renews the lease but is never viewer activity.
pub(crate) const SOURCE_DIRECT_IDLE: Duration = Duration::from_secs(300);

/// The file a direct owner may serve, frozen by current Source authority at
/// preparation. Only the HTTP preparation constructs it; nothing on the wire
/// can. Every open re-proves the same object without following links.
pub(crate) struct SourceDirectFile {
    file: plurx_core::domain::MediaFile,
    #[cfg(not(target_os = "linux"))]
    object_version: Option<String>,
    length: u64,
    mime: &'static str,
    recipe_json: String,
    #[cfg(target_os = "linux")]
    admitted: std::sync::OnceLock<Arc<DirectSourceOwner>>,
    #[cfg(target_os = "linux")]
    _application_lease: Option<DirectSourceApplicationLease>,
}
impl SourceDirectFile {
    pub(crate) fn new(
        file: plurx_core::domain::MediaFile,
        object_version: Option<String>,
        length: u64,
        mime: &'static str,
        recipe_json: String,
    ) -> Self {
        #[cfg(target_os = "linux")]
        let _ = object_version;
        Self {
            file,
            #[cfg(not(target_os = "linux"))]
            object_version,
            length,
            mime,
            recipe_json,
            #[cfg(target_os = "linux")]
            admitted: std::sync::OnceLock::new(),
            #[cfg(target_os = "linux")]
            _application_lease: None,
        }
    }
    #[cfg(target_os = "linux")]
    fn admitted(&self) -> Result<Arc<DirectSourceOwner>, SourceWorkerError> {
        self.admitted
            .get()
            .cloned()
            .ok_or(SourceWorkerError::Unavailable)
    }
    #[cfg(target_os = "linux")]
    fn install_admitted(&mut self, owner: DirectSourceOwner, lease: DirectSourceApplicationLease) {
        assert!(
            self.admitted.set(Arc::new(owner)).is_ok(),
            "fresh direct file owner"
        );
        self._application_lease = Some(lease);
    }
    #[cfg(target_os = "linux")]
    pub(super) fn admitted_unchanged(&self) -> bool {
        self.admitted().is_ok_and(|owner| owner.unchanged().is_ok())
    }
    #[cfg(not(target_os = "linux"))]
    async fn fence(&self) -> Result<crate::fragment_index_cluster::SourceFence, SourceWorkerError> {
        let fence = crate::fragment_index_cluster::open_source_playback_fence(
            &self.file,
            Some(
                self.object_version
                    .as_deref()
                    .ok_or(SourceWorkerError::Unavailable)?,
            ),
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

/// One admitted direct byte open. Linux owns the original core body lease;
/// unsupported platforms retain the existing independent legacy descriptor.
pub(crate) enum SourceDirectPayload {
    #[cfg(target_os = "linux")]
    Admitted {
        owner: Arc<DirectSourceOwner>,
        body: DirectSourceBody,
    },
    #[cfg(not(target_os = "linux"))]
    Legacy(std::fs::File),
}
pub(crate) struct SourceDirectOpened {
    payload: SourceDirectPayload,
    length: u64,
    mime: &'static str,
    guard: SourceResponseGuard,
}
impl SourceDirectOpened {
    /// The transport moves both guards into its actual Body, including empty
    /// HEAD and 416 responses until their response head has been assembled.
    pub(crate) fn into_parts(
        self,
    ) -> (SourceDirectPayload, u64, &'static str, SourceResponseGuard) {
        (self.payload, self.length, self.mime, self.guard)
    }
}

impl SourceViewerActor {
    pub(crate) fn is_direct(&self) -> bool {
        self.0.direct
    }
    async fn wait_direct_ready(
        &self,
        deadline: Instant,
    ) -> Result<SourceDirectStart, SourceWorkerError> {
        if !self.0.direct {
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
        let direct = self.0.direct_file().ok_or(SourceWorkerError::Unavailable)?;
        let guard = self.reserve_direct_body(&start)?;
        #[cfg(not(target_os = "linux"))]
        let mut guard = guard;
        #[cfg(target_os = "linux")]
        direct
            .admitted()?
            .unchanged()
            .map_err(|_| SourceWorkerError::Unavailable)?;
        #[cfg(not(target_os = "linux"))]
        {
            guard.source = Some(direct.fence().await?);
        }
        let proof = self.0.gate.current_owned(&self.0.assignment).await?;
        let route = self.0.gate.renew_with(&self.0.assignment, &proof).await?;
        if route.session_id != start.session_id
            || route.publication_ready_at_ms != 0
            || route.response_json
                != serde_json::to_string(&start).map_err(|_| SourceWorkerError::Unavailable)?
            || self.retirement_requested()
            || {
                #[cfg(target_os = "linux")]
                {
                    !direct.admitted_unchanged()
                }
                #[cfg(not(target_os = "linux"))]
                {
                    guard
                        .source
                        .as_ref()
                        .is_none_or(|source| !source.unchanged())
                }
            }
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
        if !self.0.direct {
            return Err(SourceWorkerError::Unsupported);
        }
        let direct = self.0.direct_file().ok_or(SourceWorkerError::Unavailable)?;
        let start = self
            .0
            .state
            .lock()
            .expect("Source worker state")
            .direct_start
            .clone()
            .ok_or(SourceWorkerError::Unavailable)??;
        let guard = self.reserve_direct_body(&start)?;
        #[cfg(not(target_os = "linux"))]
        let mut guard = guard;
        let initial = self.0.gate.current_owned(&self.0.assignment).await?;
        initial
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| SourceWorkerError::Unavailable)?;
        #[cfg(target_os = "linux")]
        let admitted = direct.admitted()?;
        #[cfg(target_os = "linux")]
        admitted
            .unchanged()
            .map_err(|_| SourceWorkerError::Unavailable)?;
        #[cfg(not(target_os = "linux"))]
        let fence = direct.fence().await?;
        #[cfg(not(target_os = "linux"))]
        let file = fence
            .handle
            .try_clone()
            .map_err(|_| SourceWorkerError::Unavailable)?;
        #[cfg(not(target_os = "linux"))]
        {
            guard.source = Some(fence);
        }
        // The open can park on a cold disk. Re-observe real membership and the
        // current binding after it, then couple delivery to a lease renewal.
        let proof = self.0.gate.current_owned(&self.0.assignment).await?;
        if Instant::now() >= deadline
            || proof
                .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
                .is_err()
            || self.retirement_requested()
            || {
                #[cfg(target_os = "linux")]
                {
                    admitted.unchanged().is_err()
                }
                #[cfg(not(target_os = "linux"))]
                {
                    guard
                        .source
                        .as_ref()
                        .is_none_or(|source| !source.unchanged())
                }
            }
        {
            return Err(SourceWorkerError::Unavailable);
        }
        let route = self.0.gate.renew_with(&self.0.assignment, &proof).await?;
        if route.session_id != start.session_id || {
            #[cfg(target_os = "linux")]
            {
                admitted.unchanged().is_err()
            }
            #[cfg(not(target_os = "linux"))]
            {
                guard
                    .source
                    .as_ref()
                    .is_none_or(|source| !source.unchanged())
            }
        } {
            return Err(SourceWorkerError::Unavailable);
        }
        self.0
            .state
            .lock()
            .expect("Source worker state")
            .direct_activity = Some(Instant::now());
        Ok(SourceDirectOpened {
            payload: {
                #[cfg(target_os = "linux")]
                {
                    SourceDirectPayload::Admitted {
                        body: admitted
                            .begin_body()
                            .map_err(|_| SourceWorkerError::Unavailable)?,
                        owner: admitted,
                    }
                }
                #[cfg(not(target_os = "linux"))]
                {
                    SourceDirectPayload::Legacy(file)
                }
            },
            length: direct.length,
            mime: direct.mime,
            guard,
        })
    }
}

impl TranscodeManager {
    #[cfg(target_os = "linux")]
    async fn finish_source_direct_ticket(
        ticket: SharedTicket,
    ) -> Result<(), plurx_core::adaptive_resources::runtime::direct_source::DirectSourceError> {
        ticket
            .lock()
            .await
            .finish_retirement(Instant::now() + Duration::from_secs(305))
            .await
    }

    /// Existing manager maintenance joins the actual original Source worker
    /// before dropping its rich actor and application lease. Only then may a
    /// compact private core ticket perform typed resource settlement/refund.
    #[cfg(target_os = "linux")]
    pub(crate) fn reconcile_source_direct_tasks(self: &Arc<Self>) {
        enum Transition {
            None,
            ToSettling {
                owner_address: usize,
                ticket: DirectSourceRetirementTicket,
            },
            RetrySettle,
            Remove,
            Failed {
                owner_address: usize,
            },
        }
        // Same lock order as start_source_direct_worker: registry first.
        let mut registry = self.source_workers.entries.lock().expect("Source workers");
        let mut tasks = self
            .source_workers
            .direct_tasks
            .lock()
            .expect("Source direct tasks");
        for slot in tasks.iter_mut() {
            let transition = match slot.as_mut() {
                Some(SourceDirectRecord::Active {
                    owner_address,
                    worker,
                }) if worker.is_finished() => match worker.now_or_never() {
                    Some(Ok(ticket)) => Transition::ToSettling {
                        owner_address: *owner_address,
                        ticket,
                    },
                    Some(Err(_)) => Transition::Failed {
                        owner_address: *owner_address,
                    },
                    None => Transition::None,
                },
                Some(SourceDirectRecord::Settling { worker, .. }) if worker.is_finished() => {
                    match worker.now_or_never() {
                        Some(Ok(Ok(()))) => Transition::Remove,
                        Some(_) => Transition::RetrySettle,
                        None => Transition::None,
                    }
                }
                _ => Transition::None,
            };
            match transition {
                Transition::None => {}
                Transition::ToSettling {
                    owner_address,
                    ticket,
                } => {
                    drop(slot.take());
                    // Move the SAME factory admission before the rich owner
                    // leaves the registry. No await or new admission occurs.
                    let restart_admission = registry
                        .iter()
                        .find(|owner| Arc::as_ptr(owner) as usize == owner_address)
                        .and_then(|owner| {
                            let (admission, application) = {
                                let mut state = owner.state.lock().expect("Source worker state");
                                (state.restart_admission.take(), state.direct.take())
                            };
                            // The cached actor retains kind/reply metadata, not
                            // this redundant application Arc. Real aliases can
                            // still keep the SAME core ticket's lease held.
                            drop(application);
                            admission
                        });
                    registry.retain(|owner| Arc::as_ptr(owner) as usize != owner_address);
                    let ticket = Arc::new(tokio::sync::Mutex::new(ticket));
                    let worker =
                        tokio::spawn(Self::finish_source_direct_ticket(Arc::clone(&ticket)));
                    *slot = Some(SourceDirectRecord::Settling {
                        restart_admission,
                        ticket,
                        worker,
                    });
                }
                Transition::RetrySettle => {
                    if let Some(SourceDirectRecord::Settling { ticket, worker, .. }) = slot {
                        *worker =
                            tokio::spawn(Self::finish_source_direct_ticket(Arc::clone(ticket)));
                    }
                }
                Transition::Remove => {
                    // This is the positive join of the SAME ticket finalizer,
                    // after original native closure, permitted Source disposition and R release.
                    if let Some(SourceDirectRecord::Settling {
                        restart_admission, ..
                    }) = slot
                    {
                        if let Some(admission) = restart_admission.take() {
                            admission.known_settled();
                        }
                    }
                    *slot = None;
                }
                Transition::Failed { owner_address } => {
                    // Panic/JoinUnknown cannot imply native or Store closure.
                    // Leave the original actor registered and its charge held.
                    if let Some(owner) = registry
                        .iter()
                        .find(|owner| Arc::as_ptr(owner) as usize == owner_address)
                    {
                        owner.state.lock().expect("Source worker state").settled =
                            Some(Err(SourceWorkerError::Unresolved));
                        owner.changed.notify_waiters();
                    }
                    *slot = Some(SourceDirectRecord::Failed);
                }
            }
        }
    }

    /// Read-only custody diagnostics for the exclusive actual Direct fixture.
    /// Counts and is_finished never grant native/typed closure or a refund.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn fixture_source_direct_restart_custody(&self) -> (usize, usize, usize) {
        let registry = self.source_workers.entries.lock().expect("Source workers");
        let tasks = self
            .source_workers
            .direct_tasks
            .lock()
            .expect("Source direct tasks");
        let mut active = 0;
        let mut settling = 0;
        let mut finished = 0;
        for record in tasks.iter().flatten() {
            match record {
                SourceDirectRecord::Active { owner_address, .. } => {
                    active += usize::from(registry.iter().any(|owner| {
                        Arc::as_ptr(owner) as usize == *owner_address
                            && owner
                                .state
                                .lock()
                                .expect("Source worker state")
                                .restart_admission
                                .is_some()
                    }));
                }
                SourceDirectRecord::Settling {
                    restart_admission,
                    worker,
                    ..
                } => {
                    settling += usize::from(restart_admission.is_some());
                    finished += usize::from(worker.is_finished());
                }
                SourceDirectRecord::Failed => {}
            }
        }
        (active, settling, finished)
    }

    #[cfg(target_os = "linux")]
    fn source_direct_application_bytes(
        assignment: &SourceDispatchAssignment,
        ingress: &SourceIngressAdmissionPermission,
        prepared: &crate::http::shared_source_playback::direct::PreparedSourceDirect,
    ) -> Option<u64> {
        use std::mem::{align_of, size_of};
        fn arc_layout<T>() -> Option<usize> {
            let alignment = align_of::<T>().max(align_of::<usize>());
            size_of::<T>()
                .checked_add(2 * size_of::<usize>())?
                .checked_add(3 * (alignment - 1))
        }
        fn frame_size<A, B, C, D, F: Future>(_: impl Fn(A, B, C, D) -> F) -> usize {
            size_of::<F>()
        }
        let worker_frame = frame_size(TranscodeManager::run_source_direct_owner_registered);
        let finalizer_frame = frame_size::<SharedTicket, (), (), (), _>(|ticket, (), (), ()| {
            TranscodeManager::finish_source_direct_ticket(ticket)
        });
        let assignment_heap = assignment
            .requested_heap_bytes()?
            .checked_add(ingress.requested_heap_bytes()?)?;
        let direct_heap = prepared.persistent_direct_heap_bytes()?;
        let maximum_mime = crate::http::sharing_direct_wire::DIRECT_MIMES
            .iter()
            .map(|mime| mime.len())
            .max()
            .unwrap_or(0);
        // The registered actor, its ingress authority and the published
        // direct Start retain at most these application-owned copies. The
        // original worker frame is measured from its actual async function.
        let start_strings = 36usize.checked_add(maximum_mime)?.checked_mul(4)?;
        let bytes = arc_layout::<SourceViewerInner>()?
            .checked_add(arc_layout::<SourceDirectFile>()?)?
            .checked_add(arc_layout::<SourceProducerAuthority>()?)?
            .checked_add(arc_layout::<resource::SourceResourceHookOwner>()?)?
            .checked_add(arc_layout::<SourceResponseGuard>()?)?
            .checked_add(assignment_heap)?
            .checked_add(direct_heap)?
            .checked_add(start_strings)?
            .checked_add(worker_frame)?
            .checked_add(finalizer_frame)?
            .checked_add(arc_layout::<
                tokio::sync::Mutex<DirectSourceRetirementTicket>,
            >()?)?
            .checked_add(size_of::<
                BudgetedSourceWorker<Pin<Box<dyn Future<Output = ()> + Send>>>,
            >())?
            .checked_add(size_of::<Arc<SourceViewerInner>>())?
            .checked_add(size_of::<Option<SourceDirectRecord>>())?;
        u64::try_from(bytes).ok()
    }

    #[cfg(target_os = "linux")]
    async fn open_admitted_source_direct(
        &self,
        direct: &SourceDirectFile,
        deadline: Instant,
    ) -> Result<(), SourceWorkerError> {
        let held = direct.admitted()?;
        held.open(deadline)
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?;
        if held.length().map_err(|_| SourceWorkerError::Unresolved)? != direct.length {
            return Err(SourceWorkerError::Unavailable);
        }
        held.unchanged().map_err(|_| SourceWorkerError::Unavailable)
    }
    /// Registry insertion precedes every await, into the same bounded registry
    /// as VOD owners, so direct sessions consume the same Source slots.
    pub(crate) async fn start_source_direct_worker(
        self: &Arc<Self>,
        state: Arc<crate::state::AppState>,
        assignment: SourceDispatchAssignment,
        activation: SourceSessionWriteAuthority,
        ingress: SourceIngressAdmissionPermission,
        prepared: crate::http::shared_source_playback::direct::PreparedSourceDirect,
        deadline: Instant,
    ) -> Result<SourceViewerActor, SourceWorkerNoAdmission> {
        if !Arc::ptr_eq(&self.store, &state.store)
            || !prepared.matches_assignment(&assignment)
            || !activation.assignment().same_identity(&assignment)
            || !ingress.assignment().same_identity(&assignment)
            || ingress.registry_boot_id() != state.sharing.accepted_drivers.boot_id()
        {
            return Err(SourceWorkerNoAdmission::refused(
                &assignment,
                SourceWorkerError::Conflict,
            ));
        }
        if ingress
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .is_err()
        {
            return Err(SourceWorkerNoAdmission::refused(
                &assignment,
                SourceWorkerError::Unavailable,
            ));
        }
        if let Some(owner) = self
            .source_workers
            .entries
            .lock()
            .expect("Source workers")
            .iter()
            .find(|owner| owner.assignment.same_identity(&assignment))
        {
            return Ok(SourceViewerActor(Arc::clone(owner)));
        }
        let restart_admission = state.serving.try_restart_admission().await.ok_or_else(|| {
            SourceWorkerNoAdmission::refused(&assignment, SourceWorkerError::Unavailable)
        })?;
        #[cfg(target_os = "linux")]
        let (runtime, epoch, application_owner_bytes, media_file_id, path_len) = {
            let runtime = state.resource_runtime.as_ref().ok_or_else(|| {
                SourceWorkerNoAdmission::refused(&assignment, SourceWorkerError::Unavailable)
            })?;
            let epoch = runtime
                .refresh_admission_capacity(deadline)
                .await
                .map_err(|_error| {
                    #[cfg(test)]
                    eprintln!("original Source direct capacity refresh refused: {_error:?}");
                    SourceWorkerNoAdmission::refused(&assignment, SourceWorkerError::Unavailable)
                })?;
            let application_owner_bytes =
                Self::source_direct_application_bytes(&assignment, &ingress, &prepared)
                    .ok_or_else(|| {
                        SourceWorkerNoAdmission::refused(&assignment, SourceWorkerError::Capacity)
                    })?;
            let file = prepared.source_file();
            let media_file_id = file.id.to_string();
            let path_len = file
                .path
                .to_str()
                .ok_or_else(|| {
                    SourceWorkerNoAdmission::refused(&assignment, SourceWorkerError::Unavailable)
                })?
                .len();
            (
                runtime,
                epoch,
                application_owner_bytes,
                media_file_id,
                path_len,
            )
        };
        let registry_boot_id = state.sharing.accepted_drivers.boot_id();
        let mut registry = self.source_workers.entries.lock().expect("Source workers");
        if let Some(owner) = registry
            .iter()
            .find(|owner| owner.assignment.same_identity(&assignment))
        {
            return Ok(SourceViewerActor(Arc::clone(owner)));
        }
        if registry.len() >= 8 {
            return Err(SourceWorkerNoAdmission::refused(
                &assignment,
                SourceWorkerError::Capacity,
            ));
        }
        #[cfg(target_os = "linux")]
        let mut direct_tasks = self
            .source_workers
            .direct_tasks
            .lock()
            .expect("Source direct tasks");
        #[cfg(target_os = "linux")]
        let direct_slot = direct_tasks
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or_else(|| {
                SourceWorkerNoAdmission::refused(&assignment, SourceWorkerError::Capacity)
            })?;
        registry.try_reserve_exact(1).map_err(|_| {
            SourceWorkerNoAdmission::refused(&assignment, SourceWorkerError::Capacity)
        })?;
        #[cfg(target_os = "linux")]
        let (admitted, application_lease, restart_admission) = {
            const CHUNK: usize = 64 * 1024;
            const CHUNKS: usize = 64;
            let memory = DirectSourceRequest::minimum_memory_bytes(
                path_len,
                // Core retains this original String and rechecks its capacity.
                // Formatting a small ID may allocate more bytes than its len.
                media_file_id.capacity(),
                64,
                CHUNK,
                CHUNKS,
                application_owner_bytes,
            )
            .ok_or_else(|| {
                SourceWorkerNoAdmission::refused(&assignment, SourceWorkerError::Capacity)
            })?;
            // The opaque constructor can fail after R. From this boundary
            // generic failure retains uncertainty instead of claiming no R.
            let restart_admission = SourceRestartAdmission::new(restart_admission);
            #[cfg(test)]
            let media_id_layout = (media_file_id.len(), media_file_id.capacity());
            let result = runtime.prepare_direct_source(
                DirectSourceRequest {
                    binding: DirectSourceBinding::Sharing(assignment.clone()),
                    path: prepared.source_file().path.clone(),
                    media_file_id,
                    scanner_size: prepared.source_file().size,
                    scanner_mtime: prepared.source_file().mtime,
                    demand: WorkloadDemand {
                        resources: Metric::Known(ResourceVector {
                            cpu_ns_per_second: 0,
                            peak_memory_bytes: memory,
                            handles: 1,
                            processes: 0,
                            hardware_slots: 0,
                            tuner_slots: 0,
                            scratch_bytes: 0,
                        }),
                        limit_epoch: epoch,
                        trial: false,
                    },
                    max_bodies: 64,
                    chunk_bytes: CHUNK,
                    outstanding_chunks: CHUNKS,
                    application_owner_bytes,
                },
                deadline,
            );
            let (admitted, application_lease) = match result {
                Ok(admitted) => admitted,
                Err(error) => {
                    #[cfg(test)]
                    eprintln!("original Source direct constructor refused: {error:?}; media_id_len={}, media_id_capacity={}, demanded_memory={memory}", media_id_layout.0, media_id_layout.1);
                    // Only the exact typed pre-reservation tag authorizes this
                    // release. Generic constructor errors may already own R.
                    if error.definitely_before_reservation() {
                        restart_admission.known_settled();
                    }
                    return Err(SourceWorkerNoAdmission::refused(
                        &assignment,
                        SourceWorkerError::Unavailable,
                    ));
                }
            };
            (admitted, application_lease, restart_admission)
        };
        #[cfg(not(target_os = "linux"))]
        let restart_admission = SourceRestartAdmission::new(restart_admission);
        let direct_file = prepared.into_direct_file();
        #[cfg(target_os = "linux")]
        let mut direct_file = direct_file;
        #[cfg(target_os = "linux")]
        direct_file.install_admitted(admitted, application_lease);
        let owner = Arc::new(SourceViewerInner {
            control_target_duration_ms: 0,
            control_hooks: Default::default(),
            status_hooks: Default::default(),
            resource_hooks: Arc::new(Default::default()),
            assignment,
            manager: Arc::downgrade(self),
            gate: Arc::new(SourceProducerAuthority {
                store: Arc::clone(&self.store),
                membership: state.membership.clone(),
                master: Arc::clone(&state.sharing.key),
                registry_boot_id,
                ingress,
            }),
            retirement_deadline: Default::default(),
            direct: true,
            state: std::sync::Mutex::new(SourceViewerState {
                direct: Some(Arc::new(direct_file)),
                restart_admission: Some(restart_admission),
                start: None,
                retirement_requested: false,
                retirement_mode: crate::sharing_connection_custody::DriverCloseMode::Drain,
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
        let actor = SourceViewerActor(Arc::clone(&owner));
        #[cfg(target_os = "linux")]
        {
            let owner_address = Arc::as_ptr(&owner) as usize;
            let future = Arc::clone(self).run_source_direct_owner_registered(
                Some(Arc::clone(&owner)),
                Some(state),
                deadline,
            );
            let worker = tokio::spawn(BudgetedSourceWorker {
                future: Box::pin(future),
                _owner: Arc::clone(&owner),
            });
            *direct_slot = Some(SourceDirectRecord::Active {
                owner_address,
                worker,
            });
        }
        #[cfg(not(target_os = "linux"))]
        {
            let manager = Arc::clone(self);
            let task_owner = Arc::clone(&owner);
            tokio::spawn(Box::pin(async move {
                manager
                    .run_source_direct_owner(task_owner, state, deadline)
                    .await;
            }));
        }
        registry.push(owner);
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
        #[cfg(target_os = "linux")]
        direct
            .admitted()?
            .unchanged()
            .map_err(|_| SourceWorkerError::Unavailable)?;
        #[cfg(not(target_os = "linux"))]
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
        #[cfg(target_os = "linux")]
        direct
            .admitted()?
            .unchanged()
            .map_err(|_| SourceWorkerError::Unavailable)?;
        Ok(start)
    }

    #[cfg(target_os = "linux")]
    async fn run_source_direct_owner_registered(
        self: Arc<Self>,
        owner: Option<Arc<SourceViewerInner>>,
        state: Option<Arc<crate::state::AppState>>,
        deadline: Instant,
    ) -> DirectWorkerResult {
        self.run_source_direct_owner(
            owner.expect("registered Source direct actor"),
            state.expect("registered Source direct state"),
            deadline,
        )
        .await
    }

    async fn run_source_direct_owner(
        self: Arc<Self>,
        owner: Arc<SourceViewerInner>,
        state: Arc<crate::state::AppState>,
        deadline: Instant,
    ) -> DirectWorkerResult {
        let actor = SourceViewerActor(Arc::clone(&owner));
        let direct = owner.direct_file().expect("direct Source owner");
        let mut unowned_existing = true;
        let start = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            Box::pin(async {
                #[cfg(target_os = "linux")]
                self.open_admitted_source_direct(&direct, deadline).await?;
                self.publish_source_direct(&owner, &direct, &mut unowned_existing)
                    .await
            }),
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
                            _ => {owner.state.lock().expect("Source retirement cause").retirement_mode=crate::sharing_connection_custody::DriverCloseMode::Revoke;break},
                        }
                    }
                }
            }
        }
        actor.request_retirement();
        #[cfg(target_os = "linux")]
        {
            let admitted = direct.admitted().expect("registered Source direct owner");
            // First seal every original native read/body/FD and retain the
            // same private core ticket. No typed refund occurs here.
            let ticket = loop {
                match admitted
                    .prepare_retirement(Instant::now() + Duration::from_secs(305))
                    .await
                {
                    Ok(ticket) => break ticket,
                    Err(_) => {
                        owner.state.lock().expect("Source worker state").settled =
                            Some(Err(SourceWorkerError::Unresolved));
                        owner.changed.notify_waiters();
                        tokio::select! {
                            _ = owner.changed.notified() => {},
                            _ = tokio::time::sleep(SOURCE_SETTLEMENT_RETRY) => {},
                        }
                    }
                }
            };
            // A known native close permits the existing Store route/Source
            // settlement while the actor is still retained and charged. A
            // Store error keeps this exact actor and core ticket; neither
            // ingress loss nor timeout can manufacture a resource refund.
            let mut physical = None;
            let mut preparations = SourcePreparationSettlements::default();
            loop {
                let mode = owner
                    .state
                    .lock()
                    .expect("Source worker state")
                    .retirement_mode;
                // No route activation occurred under this owner. There is
                // no Source route it may release, but the SAME sealed native
                // ticket must still reach typed settlement after worker join.
                if unowned_existing {
                    break;
                }
                let result = if crate::http::shared_source_playback::settle_ingress_custody(
                    &state,
                    &owner.assignment,
                    Instant::now() + Duration::from_secs(305),
                    mode,
                )
                .await
                .is_err()
                {
                    Err(SourceSettlementFault::Transient)
                } else {
                    self.settle_source_owner(&owner, &mut None, &mut physical, &mut preparations)
                        .await
                };
                if result.is_ok() {
                    break;
                }
                owner.state.lock().expect("Source worker state").settled =
                    Some(Err(SourceWorkerError::Unresolved));
                owner.changed.notify_waiters();
                tokio::select! {
                    _ = owner.changed.notified() => {},
                    _ = tokio::time::sleep(SOURCE_SETTLEMENT_RETRY) => {},
                }
            }
            {
                let mut current = owner.state.lock().expect("Source worker state");
                current.settled = Some(if unowned_existing {
                    Err(SourceWorkerError::Unresolved)
                } else {
                    Ok(())
                });
                current.finished = true;
            }
            owner.changed.notify_waiters();
            ticket
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.finish_source_owner(
                &owner,
                &state,
                shutdown,
                unowned_existing,
                None,
                SourcePreparationSettlements::default(),
            )
            .await;
        }
    }
}
