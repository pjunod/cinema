//! A core-owned original worker and counted rich payload destruction fence.
//! Drop only changes pending state. Census removal needs a successful original
//! join and every wrapped payload's destructor to have returned normally.
use super::*;
use crate::adaptive_resources::owner_inventory::ApplicationReference;
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::task::{Context, Poll};

const MAX_PAYLOADS: usize = 2;

pub(super) struct ApplicationLeaseState {
    state: Mutex<ApplicationLeaseStatus>,
    changed: Notify,
}
struct ApplicationLeaseStatus {
    held: bool,
    wrapped: usize,
    live: usize,
    started: bool,
    joined: bool,
    failed: bool,
    worker: Option<JoinHandle<Result<DirectSourceRetirementTicket, DirectSourceError>>>,
    reference: Option<ApplicationReference>,
    tracked: bool,
}
impl ApplicationLeaseState {
    pub(super) fn new(reference: Option<ApplicationReference>) -> Arc<Self> {
        let tracked = reference.is_some();
        Arc::new(Self {
            state: Mutex::new(ApplicationLeaseStatus {
                held: true,
                wrapped: 0,
                live: 0,
                started: false,
                joined: false,
                failed: false,
                worker: None,
                reference,
                tracked,
            }),
            changed: Notify::new(),
        })
    }
    fn hold_unknown(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.failed = true;
        if let Some(reference) = &state.reference {
            reference.hold_unknown();
        }
        drop(state);
        self.changed.notify_waiters();
    }

    pub(super) fn validate_entry(&self) -> Result<(), DirectSourceError> {
        let state = self.state.lock().map_err(|_| invalid())?;
        if state.tracked
            && (state.held
                || state.wrapped == 0
                || !state.started
                || state.failed
                || (state.worker.is_none() && !state.joined))
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub(super) async fn wait_closed(&self, deadline: Instant) -> Result<(), DirectSourceError> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.state.lock().map_err(|_| invalid())?;
                if state.failed {
                    return Err(invalid());
                }
                if !state.held
                    && (!state.tracked || (state.wrapped != 0 && state.live == 0 && state.joined))
                {
                    return Ok(());
                }
            }
            tokio::time::timeout_at(deadline.into(), notified)
                .await
                .map_err(|_| DirectSourceError(OperationError::Deadline))?;
        }
    }
    pub(super) fn close_reference(
        &self,
        runtime: &NodeResourceRuntime,
    ) -> Result<(), DirectSourceError> {
        runtime
            .affected_owners
            .with_runtime_mutation(|inventory| {
                let mut state = self.state.lock().map_err(|_| invalid())?;
                if state.held
                    || state.failed
                    || (state.tracked && (state.wrapped == 0 || state.live != 0 || !state.joined))
                {
                    return Err(invalid());
                }
                if let Some(reference) = state.reference.as_mut() {
                    let closed = DirectApplicationClosed {
                        reference: reference.identity(),
                    };
                    inventory
                        .finish_application_close(reference, closed.into())
                        .map_err(|_| invalid())?;
                    state.reference = None;
                }
                Ok(())
            })
            .map_err(|_| invalid())?
    }
}

/// Nonclone registration authority. Dropping this seals new payloads, but is
/// never sufficient evidence for application or producer closure.
pub struct DirectSourceApplicationLease {
    state: Arc<ApplicationLeaseState>,
}
impl DirectSourceApplicationLease {
    pub(super) fn new(state: Arc<ApplicationLeaseState>) -> Self {
        Self { state }
    }
    /// Wrap the actual owned value, then place the wrapper inside its Arc.
    /// Audited adapters wrap every charged ownership graph; cloned inner Arcs
    /// require their own tracked payload and native aliases keep their fences.
    pub fn wrap<T>(&self, payload: T) -> Result<DirectSourceApplication<T>, DirectSourceError> {
        let mut state = self.state.state.lock().map_err(|_| invalid())?;
        if !state.held || state.started || state.failed || state.wrapped == MAX_PAYLOADS {
            return Err(invalid());
        }
        state.wrapped += 1;
        state.live += 1;
        Ok(DirectSourceApplication {
            payload: Some(payload),
            state: Some(Arc::clone(&self.state)),
        })
    }
}
impl Drop for DirectSourceApplicationLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.state.lock() {
            state.held = false;
        }
        self.state.changed.notify_waiters();
    }
}

/// Owns the rich payload. There is deliberately no mutable dereference, unwrap
/// or cloned closure capability. A panic in T::drop leaves its count held.
pub struct DirectSourceApplication<T> {
    payload: Option<T>,
    state: Option<Arc<ApplicationLeaseState>>,
}
impl<T> DirectSourceApplication<T> {
    /// Existing non-direct adapters remain explicitly outside this bridge.
    pub fn untracked(payload: T) -> Self {
        Self {
            payload: Some(payload),
            state: None,
        }
    }
}
impl<T> Deref for DirectSourceApplication<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.payload.as_ref().expect("live application payload")
    }
}
struct PayloadDropPending<'a>(Option<&'a ApplicationLeaseState>);
impl Drop for PayloadDropPending<'_> {
    fn drop(&mut self) {
        if let Some(state) = self.0 {
            state.hold_unknown();
        }
    }
}
impl<T> Drop for DirectSourceApplication<T> {
    fn drop(&mut self) {
        let mut pending = PayloadDropPending(self.state.as_deref());
        drop(self.payload.take());
        if let Some(application) = &self.state {
            let Ok(mut state) = application.state.lock() else {
                return;
            };
            if state.live == 0 {
                return;
            }
            state.live -= 1;
            drop(state);
            application.changed.notify_waiters();
        }
        pending.0 = None;
    }
}

/// The original handle lives in its bounded application owner, not in this
/// waiter. Dropping/cancelling a waiter never detaches or replaces that handle.
pub struct DirectSourceApplicationWorker {
    state: Arc<ApplicationLeaseState>,
}
impl DirectSourceOwner {
    /// Reacquire a waiter for the SAME retained original handle. This never
    /// spawns or substitutes work after a cancelled maintenance waiter.
    pub fn resume_application_worker(
        &self,
    ) -> Result<DirectSourceApplicationWorker, DirectSourceError> {
        let state = self.inner.application.state.lock().map_err(|_| invalid())?;
        if !state.started || state.joined || state.failed || state.worker.is_none() {
            return Err(invalid());
        }
        Ok(DirectSourceApplicationWorker {
            state: Arc::clone(&self.inner.application),
        })
    }

    pub fn spawn_application_worker<F>(
        &self,
        future: F,
    ) -> Result<DirectSourceApplicationWorker, DirectSourceError>
    where
        F: Future<Output = Result<DirectSourceRetirementTicket, DirectSourceError>>
            + Send
            + 'static,
    {
        let executor = tokio::runtime::Handle::try_current().map_err(|_| invalid())?;
        let future: ApplicationFuture = Box::pin(future);
        let application = &self.inner.application;
        {
            let mut state = application.state.lock().map_err(|_| invalid())?;
            if state.held || state.wrapped == 0 || state.started || state.failed {
                return Err(invalid());
            }
            state.started = true;
        }
        // A closed Tokio executor can synchronously destroy its new future.
        // Never hold the application mutex across spawn or that destruction.
        let worker = executor.spawn(run_registered(Some(Arc::clone(application)), Some(future)));
        let mut state = match application.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.failed = true;
                if let Some(reference) = &state.reference {
                    reference.hold_unknown();
                }
                state
            }
        };
        // Retain the same original handle even after publication uncertainty.
        state.worker = Some(worker);
        drop(state);
        application.changed.notify_waiters();
        Ok(DirectSourceApplicationWorker {
            state: Arc::clone(application),
        })
    }
}
type ApplicationFuture =
    Pin<Box<dyn Future<Output = Result<DirectSourceRetirementTicket, DirectSourceError>> + Send>>;

async fn run_registered(
    application: Option<Arc<ApplicationLeaseState>>,
    future: Option<ApplicationFuture>,
) -> Result<DirectSourceRetirementTicket, DirectSourceError> {
    let application = application.expect("original application worker");
    let future = future.expect("original application future");
    loop {
        let registered = application.changed.notified();
        tokio::pin!(registered);
        registered.as_mut().enable();
        {
            let state = application.state.lock().map_err(|_| invalid())?;
            if state.failed {
                return Err(invalid());
            }
            if state.worker.is_some() {
                break;
            }
        }
        registered.await;
    }
    future.await
}

pub(super) fn worker_frame_bytes() -> usize {
    // This concrete core gate is additional to the daemon's measured boxed F.
    std::mem::size_of_val(&run_registered(None, None))
}

impl DirectSourceApplicationWorker {
    pub fn is_finished(&self) -> bool {
        self.state
            .state
            .lock()
            .is_ok_and(|state| state.worker.as_ref().is_some_and(JoinHandle::is_finished))
    }
}
impl Future for DirectSourceApplicationWorker {
    type Output = Result<DirectSourceRetirementTicket, DirectSourceError>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Ok(mut state) = self.state.state.lock() else {
            return Poll::Ready(Err(invalid()));
        };
        if state.failed || state.joined {
            return Poll::Ready(Err(invalid()));
        }
        let Some(worker) = state.worker.as_mut() else {
            return Poll::Ready(Err(invalid()));
        };
        let result = match Pin::new(worker).poll(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(result) => result,
        };
        match result {
            Ok(Ok(ticket)) if Arc::ptr_eq(&ticket.inner.application, &self.state) => {
                state.worker = None;
                state.joined = true;
                drop(state);
                self.state.changed.notify_waiters();
                Poll::Ready(Ok(ticket))
            }
            _ => {
                // Keep the same completed original handle and full charge.
                // A successor task can never erase this unknown outcome.
                state.failed = true;
                if let Some(reference) = &state.reference {
                    reference.hold_unknown();
                }
                drop(state);
                self.state.changed.notify_waiters();
                Poll::Ready(Err(invalid()))
            }
        }
    }
}

/// Private, nonclone application evidence, minted only after the original
/// worker and all counted rich payloads have positively closed.
pub(in crate::adaptive_resources) struct DirectApplicationClosed {
    reference: Uuid,
}
impl DirectApplicationClosed {
    pub(in crate::adaptive_resources) fn reference(&self) -> Uuid {
        self.reference
    }
}
