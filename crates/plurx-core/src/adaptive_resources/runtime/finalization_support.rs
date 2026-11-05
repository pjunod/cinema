//! Fixture-qualified support custody. Production construction waits for the
//! actual finalizer pool/frame audit. No native role or entry authority exists.
#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "No production profile constructor exists before the actual finalizer storage and frame audit."
    )
)]

use super::*;
use crate::adaptive_resources::finalization_support::{
    FinalizationSupportProfile, SupportIdentity,
};
use crate::adaptive_resources::owner_inventory::{
    OwnerPhase, SupportInventoryMarker, SupportInventoryOwner,
};
use crate::adaptive_resources::FinalizationSupportPhase;
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// This inline runtime slot is independent of the dynamic state being closed.
/// The same runtime pin and census token survive a state/payload destructor panic.
pub(super) struct SupportSlot {
    inner: SupportCustody,
}
impl SupportSlot {
    pub(super) fn new() -> Self {
        Self {
            inner: SupportCustody::Empty,
        }
    }
}

enum SupportCustody {
    Empty,
    Held {
        identity: SupportIdentity,
        owner: SupportInventoryOwner,
        runtime_pin: Arc<NodeResourceRuntime>,
        phase: FinalizationSupportPhase,
        state: Option<Arc<SupportState>>,
        destroyed: Option<FinalizationSupportClosed>,
        allocation_possible: bool,
    },
}

struct SupportState {
    identity: SupportIdentity,
    inner: Mutex<Counts>,
    published: Notify,
}
struct Counts {
    sealed: bool,
    joined: bool,
    failed: bool,
    task: Option<JoinHandle<()>>,
    storage: Option<Box<dyn StoredPayload>>,
}

trait StoredPayload: Send + Sync {
    fn unique(&self) -> bool;
    fn destroy(self: Box<Self>);
}
struct OwnedPayload<T> {
    original: Arc<SupportPayload<T>>,
}
impl<T: Send + Sync + 'static> StoredPayload for OwnedPayload<T> {
    fn unique(&self) -> bool {
        Arc::strong_count(&self.original) == 1 && Arc::weak_count(&self.original) == 0
    }
    fn destroy(self: Box<Self>) {
        let Self { original } = *self;
        let actual = Arc::try_unwrap(original).unwrap_or_else(|_| {
            unreachable!("sealed payload uniqueness was checked before state destruction")
        });
        // With no exposed Weak, try_unwrap has already freed the Arc allocation.
        drop(actual);
    }
}
struct SupportPayload<T> {
    actual: T,
}

/// Opaque aliases cannot expose Arc::downgrade, unwrap or mutable payload access.
/// Core retains the original Arc and performs its real final destruction.
pub(super) struct SupportPayloadRef<T>(Arc<SupportPayload<T>>);
impl<T> Clone for SupportPayloadRef<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}
impl<T> Deref for SupportPayloadRef<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0.actual
    }
}

/// Unique external shutdown authority. The original standing task cannot use
/// this facade to join itself. Its compact shutdown frame belongs to the caller's
/// root bookkeeping, not to the support allocation that it destroys.
pub(super) struct FinalizationSupportLease {
    runtime: Arc<NodeResourceRuntime>,
    identity: SupportIdentity,
    armed: bool,
    marker: SupportFailureMarker,
}
impl Drop for FinalizationSupportLease {
    fn drop(&mut self) {
        if self.armed {
            // No census, support-slot or ledger lock from Drop/unwind.
            self.marker.mark();
        }
    }
}

#[derive(Clone)]
struct SupportFailureMarker {
    ledger: Arc<AtomicBool>,
    census: SupportInventoryMarker,
}
impl SupportFailureMarker {
    fn mark(&self) {
        self.ledger.store(true, Ordering::Release);
        self.census.mark_unknown();
    }
}
struct DestructionPending {
    marker: SupportFailureMarker,
    armed: bool,
}
impl Drop for DestructionPending {
    fn drop(&mut self) {
        if self.armed {
            self.marker.mark();
        }
    }
}

/// Scalar, non-clone proof. Only the synchronous actual-storage destructor
/// below (or the sealed never-allocation branch) can construct it.
pub(in crate::adaptive_resources) struct FinalizationSupportClosed {
    identity: SupportIdentity,
}
impl FinalizationSupportClosed {
    pub(in crate::adaptive_resources) fn identity(&self) -> SupportIdentity {
        self.identity
    }
}

fn inventory_error(_: super::super::owner_inventory::InventoryError) -> AdmissionRefusal {
    AdmissionRefusal::Unavailable
}

impl NodeResourceRuntime {
    fn reserve_support(
        self: &Arc<Self>,
        profile: FinalizationSupportProfile,
        epoch: u64,
    ) -> Result<FinalizationSupportLease, AdmissionRefusal> {
        let original = Uuid::new_v4();
        self.affected_owners
            .with_runtime_mutation(|inventory| {
                let mut slot = self
                    .finalization_support
                    .lock()
                    .map_err(|_| AdmissionRefusal::Unavailable)?;
                if !matches!(slot.inner, SupportCustody::Empty) {
                    return Err(AdmissionRefusal::InvalidTransition);
                }
                inventory
                    .preflight_support_owner(original)
                    .map_err(inventory_error)?;
                let (identity, owner) = self.ledger.reserve_finalization_support(
                    profile,
                    self.owner.owner_id,
                    original,
                    epoch,
                    |identity| {
                        inventory
                            .register_support_owner(identity)
                            .map_err(inventory_error)
                    },
                )?;
                // No fallible work between the accounting/census commit and pinning
                // this original runtime's actual journal/daemon-lock lifetime.
                let marker = SupportFailureMarker {
                    ledger: Arc::clone(&self.ledger.support_unknown),
                    census: owner.marker(),
                };
                slot.inner = SupportCustody::Held {
                    identity,
                    owner,
                    runtime_pin: Arc::clone(self),
                    phase: FinalizationSupportPhase::ReservedUnallocated,
                    state: None,
                    destroyed: None,
                    allocation_possible: false,
                };
                Ok(FinalizationSupportLease {
                    runtime: Arc::clone(self),
                    identity,
                    armed: true,
                    marker,
                })
            })
            .map_err(inventory_error)?
    }

    /// The actual-observation production seam has no constructible profile yet.
    #[cfg_attr(
        test,
        allow(
            dead_code,
            reason = "The actual observation seam has no production profile constructor yet; deterministic fixture cases exercise the same locked preflight."
        )
    )]
    async fn reserve_observed_support(
        self: &Arc<Self>,
        profile: FinalizationSupportProfile,
        deadline: std::time::Instant,
    ) -> Result<FinalizationSupportLease, AdmissionRefusal> {
        let epoch = self.refresh_admission_capacity(deadline).await?;
        self.reserve_support(profile, epoch)
    }

    #[cfg(test)]
    fn reserve_fixture_support(
        self: &Arc<Self>,
    ) -> Result<FinalizationSupportLease, AdmissionRefusal> {
        let epoch = self.ledger.snapshot()?.limit_epoch;
        self.reserve_support(FinalizationSupportProfile::fixture(), epoch)
    }
}

impl FinalizationSupportLease {
    #[cfg(test)]
    fn activate_fixture<T, F>(
        &mut self,
        make_payload: impl FnOnce() -> T,
        make_task: impl FnOnce(SupportPayloadRef<T>) -> F,
    ) -> Result<SupportPayloadRef<T>, AdmissionRefusal>
    where
        T: Send + Sync + 'static,
        F: Future<Output = ()> + Send + 'static,
    {
        let executor =
            tokio::runtime::Handle::try_current().map_err(|_| AdmissionRefusal::Unavailable)?;
        let requested = std::mem::size_of::<SupportState>()
            .checked_add(std::mem::size_of::<SupportPayload<T>>())
            .and_then(|v| v.checked_add(std::mem::size_of::<OwnedPayload<T>>()))
            .and_then(|v| v.checked_add(registered_future_size::<F>()))
            .and_then(|v| v.checked_add(8 * std::mem::size_of::<usize>()))
            .ok_or(AdmissionRefusal::ArithmeticOverflow)?;
        if !FinalizationSupportProfile::fixture().fits_inline(requested) {
            return Err(AdmissionRefusal::HardLimit(
                super::super::ResourceDimension::Memory,
            ));
        }
        self.runtime
            .affected_owners
            .with_runtime_mutation(|inventory| {
                let mut slot = self
                    .runtime
                    .finalization_support
                    .lock()
                    .map_err(|_| AdmissionRefusal::Unavailable)?;
                let SupportCustody::Held {
                    identity,
                    owner,
                    phase,
                    state,
                    allocation_possible,
                    ..
                } = &mut slot.inner
                else {
                    return Err(AdmissionRefusal::UnknownReservation);
                };
                if *identity != self.identity
                    || *phase != FinalizationSupportPhase::ReservedUnallocated
                    || state.is_some()
                {
                    return Err(AdmissionRefusal::InvalidTransition);
                }
                self.runtime.ledger.support_transition(
                    self.identity,
                    *phase,
                    FinalizationSupportPhase::Active,
                    || {
                        inventory
                            .support_phase(owner, self.identity, OwnerPhase::Active)
                            .map_err(inventory_error)
                    },
                )?;
                // Allocation may now become possible. Any subsequent panic/loss
                // leaves the inline runtime pin, row and numeric hold in place.
                *phase = FinalizationSupportPhase::Active;
                *allocation_possible = true;
                Ok(())
            })
            .map_err(inventory_error)??;
        let mut pending = DestructionPending {
            marker: self.marker.clone(),
            armed: true,
        };
        let actual = Arc::new(SupportPayload {
            actual: make_payload(),
        });
        let result = SupportPayloadRef(Arc::clone(&actual));
        let state = Arc::new(SupportState {
            identity: self.identity,
            inner: Mutex::new(Counts {
                sealed: false,
                joined: false,
                failed: false,
                task: None,
                storage: Some(Box::new(OwnedPayload { original: actual })),
            }),
            published: Notify::new(),
        });
        // Pin the original state before a factory or executor can unwind.
        {
            let mut slot = self
                .runtime
                .finalization_support
                .lock()
                .map_err(|_| AdmissionRefusal::Unavailable)?;
            let SupportCustody::Held {
                identity,
                state: original,
                ..
            } = &mut slot.inner
            else {
                return Err(AdmissionRefusal::UnknownReservation);
            };
            if *identity != self.identity || original.is_some() {
                return Err(AdmissionRefusal::ReceiptMismatch);
            }
            *original = Some(Arc::clone(&state));
        }
        let future = run_registered(Arc::clone(&state), make_task(result.clone()));
        // Never hold a support/census/ledger mutex over spawn: an already
        // closed executor can synchronously destroy the submitted future.
        let task = executor.spawn(future);
        state
            .inner
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?
            .task = Some(task);
        state.published.notify_waiters();
        pending.armed = false;
        Ok(result)
    }

    fn original_state(&self) -> Result<Arc<SupportState>, AdmissionRefusal> {
        let slot = self
            .runtime
            .finalization_support
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        match &slot.inner {
            SupportCustody::Held {
                identity,
                state: Some(state),
                ..
            } if *identity == self.identity => Ok(Arc::clone(state)),
            _ => Err(AdmissionRefusal::InvalidTransition),
        }
    }

    /// Borrow the exact handle; cancellation drops only this waiter/state alias.
    async fn join_original(&mut self) -> Result<(), AdmissionRefusal> {
        if !self.armed {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        let state = self.original_state()?;
        let result = JoinOriginal { state }.await;
        if result.is_err() && result != Err(AdmissionRefusal::InvalidTransition) {
            self.marker.mark();
        }
        result
    }

    /// Synchronous after joins: no large state is captured by the caller's
    /// async frame across destruction/final accounting.
    fn try_close(&mut self) -> Result<(), AdmissionRefusal> {
        if !self.armed {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        let state = self
            .runtime
            .affected_owners
            .with_runtime_mutation(|inventory| {
                let mut slot = self
                    .runtime
                    .finalization_support
                    .lock()
                    .map_err(|_| AdmissionRefusal::Unavailable)?;
                let SupportCustody::Held {
                    identity,
                    owner,
                    phase,
                    state,
                    destroyed,
                    allocation_possible,
                    ..
                } = &mut slot.inner
                else {
                    return Err(AdmissionRefusal::UnknownReservation);
                };
                if *identity != self.identity
                    || self.runtime.ledger.support_unknown.load(Ordering::Acquire)
                {
                    return Err(AdmissionRefusal::ReceiptMismatch);
                }
                inventory
                    .preflight_support_close(owner, self.identity)
                    .map_err(inventory_error)?;
                if *phase != FinalizationSupportPhase::Closing {
                    self.runtime.ledger.support_transition(
                        self.identity,
                        *phase,
                        FinalizationSupportPhase::Closing,
                        || {
                            inventory
                                .support_phase(owner, self.identity, OwnerPhase::Closing)
                                .map_err(inventory_error)
                        },
                    )?;
                    *phase = FinalizationSupportPhase::Closing;
                }
                if destroyed.is_some() {
                    return Ok(None);
                }
                if let Some(state) = state.as_ref() {
                    let mut counts = state
                        .inner
                        .lock()
                        .map_err(|_| AdmissionRefusal::Unavailable)?;
                    counts.sealed = true;
                    if counts.failed || !counts.joined || counts.task.is_some() {
                        return Err(AdmissionRefusal::InvalidTransition);
                    }
                }
                if state.is_none() && *allocation_possible {
                    return Err(AdmissionRefusal::InvalidTransition);
                }
                Ok(state.take())
            })
            .map_err(inventory_error)??;
        let closed = match state {
            Some(state) => match destroy_actual_state(state, self.marker.clone()) {
                Ok(closed) => Some(closed),
                Err((error, state)) => {
                    // Restore exactly the same Arc; the inline Closing barrier
                    // already pins runtime/census/charge independently.
                    let mut slot = match self.runtime.finalization_support.lock() {
                        Ok(slot) => slot,
                        Err(_) => {
                            std::mem::forget(state);
                            self.marker.mark();
                            return Err(AdmissionRefusal::Unavailable);
                        }
                    };
                    if let SupportCustody::Held {
                        identity,
                        state: original,
                        ..
                    } = &mut slot.inner
                    {
                        if *identity == self.identity && original.is_none() {
                            *original = Some(state);
                            return Err(error);
                        }
                    }
                    std::mem::forget(state);
                    self.marker.mark();
                    return Err(AdmissionRefusal::Unavailable);
                }
            },
            None => {
                let slot = self
                    .runtime
                    .finalization_support
                    .lock()
                    .map_err(|_| AdmissionRefusal::Unavailable)?;
                match &slot.inner {
                    SupportCustody::Held {
                        destroyed: Some(_), ..
                    } => None,
                    SupportCustody::Held {
                        identity,
                        allocation_possible: false,
                        ..
                    } if *identity == self.identity => Some(FinalizationSupportClosed {
                        identity: self.identity,
                    }),
                    _ => return Err(AdmissionRefusal::InvalidTransition),
                }
            }
        };
        // Record actual destruction before a final transaction that can refuse.
        {
            let mut slot = self
                .runtime
                .finalization_support
                .lock()
                .map_err(|_| AdmissionRefusal::Unavailable)?;
            let SupportCustody::Held {
                identity,
                destroyed,
                ..
            } = &mut slot.inner
            else {
                return Err(AdmissionRefusal::UnknownReservation);
            };
            if *identity != self.identity {
                return Err(AdmissionRefusal::ReceiptMismatch);
            }
            if let Some(closed) = closed {
                *destroyed = Some(closed);
            }
        }
        let runtime_pin = self
            .runtime
            .affected_owners
            .with_runtime_mutation(|inventory| {
                let mut slot = self
                    .runtime
                    .finalization_support
                    .lock()
                    .map_err(|_| AdmissionRefusal::Unavailable)?;
                let SupportCustody::Held {
                    identity,
                    owner,
                    phase,
                    destroyed,
                    state,
                    ..
                } = &mut slot.inner
                else {
                    return Err(AdmissionRefusal::UnknownReservation);
                };
                if *identity != self.identity
                    || *phase != FinalizationSupportPhase::Closing
                    || destroyed.is_none()
                    || state.is_some()
                {
                    return Err(AdmissionRefusal::ReceiptMismatch);
                }
                inventory
                    .preflight_support_close(owner, self.identity)
                    .map_err(inventory_error)?;
                let closed = destroyed
                    .as_ref()
                    .ok_or(AdmissionRefusal::InvalidTransition)?;
                self.runtime
                    .ledger
                    .finish_finalization_support(closed, || {
                        inventory
                            .finish_support_close(owner, closed)
                            .map_err(inventory_error)
                    })?;
                self.armed = false;
                let old = std::mem::replace(&mut slot.inner, SupportCustody::Empty);
                let SupportCustody::Held { runtime_pin, .. } = old else {
                    unreachable!("matching original closing slot")
                };
                Ok(runtime_pin)
            })
            .map_err(inventory_error)??;
        drop(runtime_pin); // Never destroy even the runtime pin under locks.
        Ok(())
    }
}

/// Infer the actual opaque async return type without constructing a future,
/// payload or Arc. The complete publication wrapper must fit before activation.
fn registered_future_size<F: Future<Output = ()>>() -> usize {
    fn result_size<A, B>(_: fn(Arc<SupportState>, A) -> B) -> usize {
        std::mem::size_of::<B>()
    }
    result_size(run_registered::<F>)
}

async fn run_registered<F: Future<Output = ()>>(state: Arc<SupportState>, future: F) {
    loop {
        let changed = state.published.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if state.inner.lock().is_ok_and(|counts| counts.task.is_some()) {
            break;
        }
        changed.await;
    }
    future.await;
}
struct JoinOriginal {
    state: Arc<SupportState>,
}
impl Future for JoinOriginal {
    type Output = Result<(), AdmissionRefusal>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut counts = match self.state.inner.lock() {
            Ok(value) => value,
            Err(_) => return Poll::Ready(Err(AdmissionRefusal::Unavailable)),
        };
        if counts.failed {
            return Poll::Ready(Err(AdmissionRefusal::Unavailable));
        }
        if counts.joined {
            return Poll::Ready(Ok(()));
        }
        let Some(task) = counts.task.as_mut() else {
            return Poll::Ready(Err(AdmissionRefusal::InvalidTransition));
        };
        if tokio::task::try_id().is_some_and(|current| current == task.id()) {
            return Poll::Ready(Err(AdmissionRefusal::InvalidTransition));
        }
        match Pin::new(task).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(())) => {
                counts.task = None;
                counts.joined = true;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(_)) => {
                counts.failed = true;
                Poll::Ready(Err(AdmissionRefusal::Unavailable))
            }
        }
    }
}

/// The helper frees the actual Arc allocation (no Weak is exposed), destroys
/// arena/record payloads and State handle/notify fields, then returns scalar
/// identity. Positive join/handle Drop proves original F/output destruction;
/// Tokio task-cell backing may survive via runtime/completion/waker references.
/// That executor allocation remains Unknown and outside this storage proof.
fn destroy_actual_state(
    state: Arc<SupportState>,
    marker: SupportFailureMarker,
) -> Result<FinalizationSupportClosed, (AdmissionRefusal, Arc<SupportState>)> {
    let ready = state.inner.lock().is_ok_and(|counts| {
        counts.sealed
            && counts.joined
            && !counts.failed
            && counts.task.is_none()
            && counts
                .storage
                .as_ref()
                .is_some_and(|payload| payload.unique())
    });
    if !ready {
        return Err((AdmissionRefusal::InvalidTransition, state));
    }
    let state = match Arc::try_unwrap(state) {
        Ok(state) => state,
        Err(state) => return Err((AdmissionRefusal::InvalidTransition, state)),
    };
    let mut pending = DestructionPending {
        marker,
        armed: true,
    };
    let SupportState {
        identity,
        inner,
        // Notify has no Drop implementation or independent owned allocation.
        // Its inline bytes are consumed with this synchronous State destructure.
        published: _,
    } = state;
    let mut counts = inner
        .into_inner()
        .unwrap_or_else(|_| unreachable!("original support mutex was unpoisoned and unique"));
    counts
        .storage
        .take()
        .expect("actual sealed support payload")
        .destroy();
    drop(counts);
    pending.armed = false;
    Ok(FinalizationSupportClosed { identity })
}

#[cfg(all(test, unix))]
mod tests;
