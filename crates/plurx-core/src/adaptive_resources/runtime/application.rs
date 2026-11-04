//! Count actual application payload destruction and retain the original task.
//! Registration before M is bounded startup custody, not census coverage.
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

const MAX_PAYLOADS: usize = 4;

#[derive(Debug)]
pub struct ApplicationError;

/// A fixed four-payload, one-original-task startup group. Clones identify the
/// same group; sealing at M binding permanently refuses new registrations.
#[derive(Clone)]
pub struct PendingApplicationGroup {
    pub(super) state: Arc<ApplicationState>,
}
pub(super) struct ApplicationState {
    #[cfg(target_os = "linux")]
    identity: uuid::Uuid,
    inner: Mutex<Counts>,
    changed: Notify,
}
struct Counts {
    registered: usize,
    live: usize,
    sealed: bool,
    started: bool,
    joined: bool,
    failed: bool,
    task: Option<JoinHandle<()>>,
    inline_bytes: usize,
    task_bytes: usize,
    #[cfg(target_os = "linux")]
    binding: Option<Binding>,
}
#[cfg(target_os = "linux")]
struct Binding {
    reference: crate::adaptive_resources::owner_inventory::ApplicationReference,
    lease: super::finite_lifetime::FiniteApplicationLease,
}
impl Default for PendingApplicationGroup {
    fn default() -> Self {
        Self::new()
    }
}
impl PendingApplicationGroup {
    pub fn new() -> Self {
        Self {
            state: Arc::new(ApplicationState {
                #[cfg(target_os = "linux")]
                identity: uuid::Uuid::new_v4(),
                inner: Mutex::new(Counts {
                    registered: 0,
                    live: 0,
                    sealed: false,
                    started: false,
                    joined: false,
                    failed: false,
                    task: None,
                    inline_bytes: 0,
                    task_bytes: 0,
                    #[cfg(target_os = "linux")]
                    binding: None,
                }),
                changed: Notify::new(),
            }),
        }
    }
    #[cfg(target_os = "linux")]
    pub(super) fn identity(&self) -> uuid::Uuid {
        self.state.identity
    }

    pub fn wrap<T>(&self, payload: T) -> Result<ApplicationPayload<T>, ApplicationError> {
        let mut counts = self.state.inner.lock().map_err(|_| ApplicationError)?;
        if counts.sealed || counts.failed || counts.registered == MAX_PAYLOADS {
            return Err(ApplicationError);
        }
        counts.registered += 1;
        counts.live += 1;
        Ok(ApplicationPayload {
            payload: Some(payload),
            state: Arc::clone(&self.state),
        })
    }
    /// Own the actual pinned inline F. Ready is not destruction: its count is
    /// released only after the original boxed F's destructor returns normally.
    pub fn frame<F: Future>(&self, future: F) -> Result<ApplicationFrame<F>, ApplicationError> {
        let future = self.wrap(Box::pin(future))?;
        let mut counts = self.state.inner.lock().map_err(|_| ApplicationError)?;
        counts.inline_bytes = counts
            .inline_bytes
            .checked_add(std::mem::size_of::<F>())
            .ok_or(ApplicationError)?;
        Ok(ApplicationFrame { future })
    }
    pub fn spawn_original<F>(&self, future: F) -> Result<OriginalUnitTask, ApplicationError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let executor = tokio::runtime::Handle::try_current().map_err(|_| ApplicationError)?;
        {
            let mut counts = self.state.inner.lock().map_err(|_| ApplicationError)?;
            if counts.started || counts.sealed || counts.failed {
                return Err(ApplicationError);
            }
            counts.started = true;
        }
        // A closed executor can synchronously drop F. Never hold the group
        // mutex across spawn; the original F cannot run before publication.
        let original = run_registered(Arc::clone(&self.state), future);
        let frame_bytes = std::mem::size_of_val(&original);
        let task = executor.spawn(original);
        let mut counts = self.state.inner.lock().unwrap_or_else(|poisoned| {
            let mut counts = poisoned.into_inner();
            counts.failed = true;
            #[cfg(target_os = "linux")]
            if let Some(binding) = &counts.binding {
                binding.reference.hold_unknown();
            }
            counts
        });
        counts.task = Some(task);
        counts.task_bytes = frame_bytes;
        drop(counts);
        self.state.changed.notify_waiters();
        Ok(OriginalUnitTask {
            state: Arc::clone(&self.state),
        })
    }
    /// Structural bookkeeping only; the adapter separately charges actual F,
    /// rich payloads and original heartbeat. Private executor overhead remains
    /// unknown and this API never claims a complete producer memory bound.
    pub fn requested_heap_bytes(&self) -> Option<usize> {
        let counts = self.state.inner.lock().ok()?;
        std::mem::size_of::<ApplicationState>()
            .checked_add(2 * std::mem::size_of::<usize>())?
            .checked_add(counts.inline_bytes)?
            .checked_add(counts.task_bytes)
    }
}
async fn run_registered<F: Future<Output = ()>>(state: Arc<ApplicationState>, future: F) {
    loop {
        let registered = state.changed.notified();
        tokio::pin!(registered);
        registered.as_mut().enable();
        {
            let Ok(counts) = state.inner.lock() else {
                return;
            };
            if counts.failed {
                return;
            }
            if counts.task.is_some() {
                break;
            }
        }
        registered.await;
    }
    future.await;
}
impl ApplicationState {
    fn hold_unknown(&self) {
        let mut counts = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        counts.failed = true;
        #[cfg(target_os = "linux")]
        if let Some(binding) = &counts.binding {
            binding.reference.hold_unknown();
        }
    }
}

/// No unwrap, mutable dereference or cloned close capability. An escaped Arc
/// owns this whole wrapper, so its original rich T remains counted.
pub struct ApplicationPayload<T> {
    payload: Option<T>,
    state: Arc<ApplicationState>,
}
impl<T> Deref for ApplicationPayload<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.payload
            .as_ref()
            .expect("live counted application payload")
    }
}
struct DropPending<'a>(Option<&'a ApplicationState>);
impl Drop for DropPending<'_> {
    fn drop(&mut self) {
        if let Some(state) = self.0 {
            state.hold_unknown();
        }
    }
}
impl<T> Drop for ApplicationPayload<T> {
    fn drop(&mut self) {
        let mut pending = DropPending(Some(&self.state));
        drop(self.payload.take());
        let Ok(mut counts) = self.state.inner.lock() else {
            return;
        };
        let Some(live) = counts.live.checked_sub(1) else {
            return;
        };
        counts.live = live;
        pending.0 = None;
    }
}
pub struct ApplicationFrame<F: Future> {
    future: ApplicationPayload<Pin<Box<F>>>,
}
impl<F: Future> Future for ApplicationFrame<F> {
    type Output = F::Output;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let state = Arc::clone(&self.future.state);
        let mut pending = DropPending(Some(&state));
        let result = self
            .future
            .payload
            .as_mut()
            .expect("original pinned application future")
            .as_mut()
            .poll(cx);
        pending.0 = None;
        result
    }
}

/// A selector, never the JoinHandle. Core retains the same original handle
/// through cancellation and failed join; no retry starts replacement work.
pub struct OriginalUnitTask {
    state: Arc<ApplicationState>,
}
impl OriginalUnitTask {
    pub fn wait(&self) -> OriginalUnitTaskWaiter {
        OriginalUnitTaskWaiter {
            state: Arc::clone(&self.state),
        }
    }
    pub fn abort(&self) {
        if let Ok(counts) = self.state.inner.lock() {
            if let Some(task) = &counts.task {
                task.abort();
            }
        }
    }
}
pub struct OriginalUnitTaskWaiter {
    state: Arc<ApplicationState>,
}
impl Future for OriginalUnitTaskWaiter {
    type Output = Result<(), ApplicationError>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Ok(mut counts) = self.state.inner.lock() else {
            return Poll::Ready(Err(ApplicationError));
        };
        if counts.failed {
            return Poll::Ready(Err(ApplicationError));
        }
        if counts.joined {
            return Poll::Ready(Ok(()));
        }
        let Some(task) = counts.task.as_mut() else {
            return Poll::Ready(Err(ApplicationError));
        };
        match Pin::new(task).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(())) => {
                counts.task = None;
                counts.joined = true;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(_)) => {
                // Retain the SAME failed original handle, never poll it again.
                counts.failed = true;
                #[cfg(target_os = "linux")]
                if let Some(binding) = &counts.binding {
                    binding.reference.hold_unknown();
                }
                Poll::Ready(Err(ApplicationError))
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl PendingApplicationGroup {
    pub(super) fn bind(
        &self,
        runtime: Arc<super::NodeResourceRuntime>,
        finite: &Arc<super::finite_lifetime::FiniteAppState>,
        bytes: usize,
        register: impl FnOnce() -> Result<
            crate::adaptive_resources::owner_inventory::ApplicationReference,
            ApplicationError,
        >,
    ) -> Result<RegisteredApplication, ApplicationError> {
        let mut counts = self.state.inner.lock().map_err(|_| ApplicationError)?;
        if counts.sealed
            || counts.binding.is_some()
            || counts.failed
            || counts.registered != MAX_PAYLOADS
            || !counts.started
            || (counts.task.is_none() && !counts.joined)
        {
            return Err(ApplicationError);
        }
        let (lease, reference) =
            finite.take_registered_application(bytes, Arc::clone(&self.state), register)?;
        counts.sealed = true;
        counts.binding = Some(Binding { reference, lease });
        Ok(RegisteredApplication {
            state: Arc::clone(&self.state),
            runtime,
        })
    }
}
/// Compact custody, independent of all counted payloads. Drop has no closure
/// authority: the finite owner retains the same group until positive finish.
#[cfg(target_os = "linux")]
pub struct RegisteredApplication {
    state: Arc<ApplicationState>,
    runtime: Arc<super::NodeResourceRuntime>,
}
#[cfg(target_os = "linux")]
impl Drop for RegisteredApplication {
    fn drop(&mut self) {
        let mut counts = self.state.inner.lock().unwrap_or_else(|p| p.into_inner());
        if counts.binding.is_some() {
            counts.failed = true;
            counts
                .binding
                .as_ref()
                .expect("same original binding")
                .reference
                .hold_unknown();
        }
    }
}
#[cfg(target_os = "linux")]
impl RegisteredApplication {
    pub fn finish(&self) -> Result<(), ApplicationError> {
        let lease = self
            .runtime
            .affected_owners
            .with_runtime_mutation(|inventory| {
                let mut counts = self.state.inner.lock().map_err(|_| ApplicationError)?;
                if counts.failed || !counts.sealed || counts.live != 0 || !counts.joined {
                    return Err(ApplicationError);
                }
                let Some(binding) = counts.binding.as_mut() else {
                    return Ok(None);
                };
                let closed = RegisteredApplicationClosed {
                    reference: binding.reference.identity(),
                };
                inventory
                    .finish_application_close(&mut binding.reference, closed.into())
                    .map_err(|_| ApplicationError)?;
                Ok(counts.binding.take().map(|binding| binding.lease))
            })
            .map_err(|_| ApplicationError)??;
        // No census lock while destroying even the compact finite selector.
        drop(lease);
        Ok(())
    }
}
#[cfg(target_os = "linux")]
pub(in crate::adaptive_resources) struct RegisteredApplicationClosed {
    reference: uuid::Uuid,
}
#[cfg(target_os = "linux")]
impl RegisteredApplicationClosed {
    pub(in crate::adaptive_resources) fn reference(&self) -> uuid::Uuid {
        self.reference
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cancelled_waiter_retains_same_original_unit_task_until_positive_join() {
        let group = PendingApplicationGroup::new();
        let (release, wait) = tokio::sync::oneshot::channel();
        let original = group
            .spawn_original(async {
                let _ = wait.await;
            })
            .expect("original task");
        let identity = group
            .state
            .inner
            .lock()
            .expect("group")
            .task
            .as_ref()
            .expect("same handle")
            .id();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), original.wait())
                .await
                .is_err()
        );
        assert_eq!(
            group
                .state
                .inner
                .lock()
                .expect("group")
                .task
                .as_ref()
                .expect("retained original")
                .id(),
            identity
        );
        release.send(()).expect("original task still receives");
        original.wait().await.expect("same original joined");
        assert!(group.state.inner.lock().expect("group").joined);
    }
    #[tokio::test]
    async fn failed_original_unit_task_never_becomes_a_successful_retry() {
        let group = PendingApplicationGroup::new();
        let original = group
            .spawn_original(std::future::pending())
            .expect("original task");
        original.abort();
        assert!(original.wait().await.is_err());
        assert!(original.wait().await.is_err());
        let counts = group.state.inner.lock().expect("group");
        assert!(counts.failed && !counts.joined && counts.task.is_some());
    }
    #[tokio::test]
    async fn ready_inline_future_remains_counted_until_original_f_destruction() {
        struct ReadyPayload(Vec<u8>);
        impl Future for ReadyPayload {
            type Output = ();
            fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
                assert_eq!(self.0.len(), 1024);
                Poll::Ready(())
            }
        }
        let group = PendingApplicationGroup::new();
        let mut frame = Box::pin(
            group
                .frame(ReadyPayload(vec![7; 1024]))
                .expect("original inline F"),
        );
        frame.as_mut().await;
        assert_eq!(
            group.state.inner.lock().expect("group").live,
            1,
            "Ready is not F destruction"
        );
        drop(frame);
        assert_eq!(group.state.inner.lock().expect("group").live, 0);
    }
    #[test]
    fn destructor_panic_keeps_the_actual_payload_count_held() {
        struct Panics;
        impl Drop for Panics {
            fn drop(&mut self) {
                panic!("original payload drop failed");
            }
        }
        let group = PendingApplicationGroup::new();
        let payload = group.wrap(Panics).expect("original payload");
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload))).is_err());
        let counts = group.state.inner.lock().expect("group");
        assert!(counts.failed);
        assert_eq!(counts.live, 1);
    }
    #[test]
    fn original_inline_poll_panic_remains_unknown_after_frame_destruction() {
        let group = PendingApplicationGroup::new();
        let mut frame = Box::pin(
            group
                .frame(async {
                    panic!("original inline pass failed");
                })
                .expect("original inline pass"),
        );
        let mut context = Context::from_waker(std::task::Waker::noop());
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| frame
                .as_mut()
                .poll(&mut context)))
            .is_err()
        );
        drop(frame);
        assert!(group.state.inner.lock().expect("same group").failed);
    }
    #[test]
    fn shutdown_executor_never_polls_unpublished_original_future() {
        let executor = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("executor");
        let handle = executor.handle().clone();
        drop(executor);
        let group = PendingApplicationGroup::new();
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_entered = Arc::clone(&entered);
        let _entered_runtime = handle.enter();
        let original = group
            .spawn_original(async move {
                worker_entered.store(true, std::sync::atomic::Ordering::Release);
            })
            .expect("same shutdown task handle retained");
        drop(original);
        assert!(!entered.load(std::sync::atomic::Ordering::Acquire));
        assert!(group.state.inner.lock().expect("group").task.is_some());
    }
}
