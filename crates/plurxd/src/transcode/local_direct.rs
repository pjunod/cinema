//! One admitted local file owner per raw response. The manager retains the
//! original worker until it joins, then settles the same core owner after its
//! rich application entry and nonclone memory lease have been destroyed.

use super::TranscodeManager;
use futures_util::FutureExt;
use plurx_core::adaptive_resources::runtime::direct_source::{
    DirectSourceApplicationLease, DirectSourceBinding, DirectSourceBody, DirectSourceError,
    DirectSourceOwner, DirectSourceRequest, DirectSourceRetirementTicket,
};
use plurx_core::adaptive_resources::{ResourceVector, WorkloadDemand};
use plurx_core::domain::MediaFile;
use plurx_core::process::observation::Metric;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::task::JoinHandle;

const MAX_LOCAL_DIRECT_OWNERS: usize = 64;
type SharedTicket = Arc<AsyncMutex<DirectSourceRetirementTicket>>;

pub(crate) struct LocalDirectOwners {
    // Fixed inline controller slots do not grow or retain refundable heap.
    entries: Mutex<[Option<LocalDirectRecord>; MAX_LOCAL_DIRECT_OWNERS]>,
}
impl Default for LocalDirectOwners {
    fn default() -> Self {
        Self {
            entries: Mutex::new(std::array::from_fn(|_| None)),
        }
    }
}

enum LocalDirectRecord {
    Active {
        entry: Arc<LocalDirectEntry>,
        worker: JoinHandle<Result<DirectSourceRetirementTicket, DirectSourceError>>,
    },
    Settling {
        ticket: SharedTicket,
        worker: JoinHandle<Result<(), DirectSourceError>>,
    },
}

struct LocalDirectEntry {
    owner: Arc<DirectSourceOwner>,
    body_done: AtomicBool,
    changed: Notify,
    // Last field: destroying this entry releases the application charge only
    // after its other owned payloads. The original worker retains an extra
    // Arc until the actual future allocation has been destroyed.
    _application_lease: DirectSourceApplicationLease,
}

struct BudgetedWorker<F> {
    future: Pin<Box<F>>,
    _entry: Arc<LocalDirectEntry>,
}
impl<F: Future> Future for BudgetedWorker<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().future.as_mut().poll(cx)
    }
}

/// The original response owns this until its Body ends or is dropped.
pub(crate) struct LocalDirectGuard(Arc<LocalDirectEntry>);
impl Drop for LocalDirectGuard {
    fn drop(&mut self) {
        self.0.body_done.store(true, Ordering::Release);
        self.0.changed.notify_waiters();
    }
}

pub(crate) struct LocalDirectOpen {
    pub(crate) owner: Arc<DirectSourceOwner>,
    pub(crate) guard: LocalDirectGuard,
    pub(crate) length: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalDirectFailure {
    Unavailable,
    NotFound,
    LengthMismatch,
}
impl From<()> for LocalDirectFailure {
    fn from(_: ()) -> Self {
        Self::Unavailable
    }
}

impl TranscodeManager {
    fn local_direct_application_bytes() -> Option<u64> {
        use std::mem::{align_of, size_of, size_of_val};
        // ArcInner has two reference counters and the payload. Include
        // conservative field-alignment padding; allocator/Tokio private task
        // headers remain separate framework overhead.
        fn arc_layout<T>() -> Option<usize> {
            let alignment = align_of::<T>().max(align_of::<usize>());
            size_of::<T>()
                .checked_add(2 * size_of::<usize>())?
                .checked_add(3 * (alignment - 1))
        }
        let original = Self::prepare_local_retirement(None);
        let finalizer = Self::finish_local_retirement(None);
        let body_state = size_of::<(
            Arc<DirectSourceOwner>,
            DirectSourceBody,
            LocalDirectGuard,
            u64,
            u64,
        )>();
        let bytes = arc_layout::<LocalDirectEntry>()?
            .checked_add(arc_layout::<AsyncMutex<DirectSourceRetirementTicket>>()?)?
            .checked_add(size_of_val(&original))?
            .checked_add(size_of_val(&finalizer))?
            .checked_add(size_of::<
                BudgetedWorker<Pin<Box<dyn Future<Output = ()> + Send>>>,
            >())?
            .checked_add(body_state)?
            .checked_add(size_of::<LocalDirectOpen>())?;
        u64::try_from(bytes).ok()
    }

    fn spawn_original(
        entry: Arc<LocalDirectEntry>,
    ) -> JoinHandle<Result<DirectSourceRetirementTicket, DirectSourceError>> {
        let future = Self::prepare_local_retirement(Some(Arc::clone(&entry)));
        tokio::spawn(BudgetedWorker {
            future: Box::pin(future),
            _entry: entry,
        })
    }

    /// Register original worker and memory lease before native entry. Failure
    /// after preparation remains in this manager for same-operation cleanup.
    pub(crate) async fn open_local_direct(
        self: &Arc<Self>,
        state: &crate::state::AppState,
        binding: DirectSourceBinding,
        file: &MediaFile,
        deadline: Instant,
    ) -> Result<LocalDirectOpen, LocalDirectFailure> {
        const CHUNK: usize = 64 * 1024;
        const CHUNKS: usize = 64;
        if file.id <= 0 || Instant::now() >= deadline {
            return Err(LocalDirectFailure::Unavailable);
        }
        let runtime = state.resource_runtime.as_ref().ok_or(())?;
        let epoch = runtime
            .refresh_admission_capacity(deadline)
            .await
            .map_err(|_| ())?;
        let media_file_id = file.id.to_string();
        let path_len = file.path.to_str().ok_or(())?.len();
        let application_owner_bytes = Self::local_direct_application_bytes().ok_or(())?;
        let memory = DirectSourceRequest::minimum_memory_bytes(
            path_len,
            // Retain and charge the same formatted String allocation.
            media_file_id.capacity(),
            1,
            CHUNK,
            CHUNKS,
            application_owner_bytes,
        )
        .ok_or(())?;
        let demand = WorkloadDemand {
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
        };
        // Claim an inline slot and keep its mutex through synchronous core
        // prepare and original task registration. No await separates them.
        let (owner, entry) = {
            let mut entries = self.local_direct_owners.entries.lock().map_err(|_| ())?;
            let slot = entries.iter_mut().find(|slot| slot.is_none()).ok_or(())?;
            let (owner, lease) = runtime
                .prepare_direct_source(
                    DirectSourceRequest {
                        binding,
                        path: file.path.clone(),
                        media_file_id,
                        scanner_size: file.size,
                        scanner_mtime: file.mtime,
                        demand,
                        max_bodies: 1,
                        chunk_bytes: CHUNK,
                        outstanding_chunks: CHUNKS,
                        application_owner_bytes,
                    },
                    deadline,
                )
                .map_err(|_| ())?;
            let owner = Arc::new(owner);
            let entry = Arc::new(LocalDirectEntry {
                owner: Arc::clone(&owner),
                body_done: AtomicBool::new(false),
                changed: Notify::new(),
                _application_lease: lease,
            });
            let worker = Self::spawn_original(Arc::clone(&entry));
            *slot = Some(LocalDirectRecord::Active {
                entry: Arc::clone(&entry),
                worker,
            });
            (owner, entry)
        };
        let guard = LocalDirectGuard(entry);
        if owner.open(deadline).await.is_err() {
            return Err(
                if owner.known_open_error_kind() == Some(std::io::ErrorKind::NotFound) {
                    LocalDirectFailure::NotFound
                } else {
                    LocalDirectFailure::Unavailable
                },
            );
        }
        let length = owner.length().map_err(|_| ())?;
        if length != file.size.max(0) as u64 {
            return Err(LocalDirectFailure::LengthMismatch);
        }
        owner.unchanged().map_err(|_| ())?;
        Ok(LocalDirectOpen {
            owner,
            guard,
            length,
        })
    }

    async fn prepare_local_retirement(
        entry: Option<Arc<LocalDirectEntry>>,
    ) -> Result<DirectSourceRetirementTicket, DirectSourceError> {
        let entry = entry.expect("registered local direct worker");
        loop {
            let notification = entry.changed.notified();
            tokio::pin!(notification);
            notification.as_mut().enable();
            if entry.body_done.load(Ordering::Acquire) {
                break;
            }
            notification.await;
        }
        entry
            .owner
            .prepare_retirement(Instant::now() + Duration::from_secs(305))
            .await
    }

    async fn finish_local_retirement(
        ticket: Option<SharedTicket>,
    ) -> Result<(), DirectSourceError> {
        let ticket = ticket.expect("registered local direct retirement");
        let mut ticket = ticket.lock().await;
        ticket
            .finish_retirement(Instant::now() + Duration::from_secs(305))
            .await
    }

    /// Existing manager maintenance joins only completed original workers.
    /// Unknown results keep the same owner or ticket for another pass.
    pub(super) fn reconcile_local_direct_owners(self: &Arc<Self>) {
        enum Transition {
            None,
            RetryActive,
            ToSettling(DirectSourceRetirementTicket),
            RetrySettle,
            Remove,
        }
        let mut entries = self
            .local_direct_owners
            .entries
            .lock()
            .expect("local direct owners");
        for slot in entries.iter_mut() {
            let transition = match slot.as_mut() {
                Some(LocalDirectRecord::Active { worker, .. }) if worker.is_finished() => {
                    match worker.now_or_never() {
                        Some(Ok(Ok(ticket))) => Transition::ToSettling(ticket),
                        Some(_) => Transition::RetryActive,
                        None => Transition::None,
                    }
                }
                Some(LocalDirectRecord::Settling { worker, .. }) if worker.is_finished() => {
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
                Transition::RetryActive => {
                    if let Some(LocalDirectRecord::Active { entry, worker }) = slot {
                        *worker = Self::spawn_original(Arc::clone(entry));
                    }
                }
                Transition::ToSettling(ticket) => {
                    // Drop the joined task, rich entry and application lease
                    // before the compact ticket may settle/refund the role.
                    drop(slot.take());
                    let ticket = Arc::new(AsyncMutex::new(ticket));
                    let worker =
                        tokio::spawn(Self::finish_local_retirement(Some(Arc::clone(&ticket))));
                    *slot = Some(LocalDirectRecord::Settling { ticket, worker });
                }
                Transition::RetrySettle => {
                    if let Some(LocalDirectRecord::Settling { ticket, worker }) = slot {
                        *worker =
                            tokio::spawn(Self::finish_local_retirement(Some(Arc::clone(ticket))));
                    }
                }
                Transition::Remove => {
                    *slot = None;
                }
            }
        }
    }
}
