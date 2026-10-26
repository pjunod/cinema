//! Lifetime carriers for existing admitted subtitle workers, never admission or closure proof.
use crate::transcode::FragmentAdmission;
use plurx_core::process::bounded::{self, OriginalWorkKeeper};
use std::{future::Future, sync::Arc};

/// Scope only an already acquired original admission. Download/discovery with
/// no admission stays unadmitted; spawned workers explicitly capture this Arc.
pub(crate) async fn scope<T>(
    admission: Option<&Arc<FragmentAdmission>>,
    operation: impl Future<Output = T>,
) -> T {
    if let Some(admission) = admission {
        let keeper: Arc<dyn OriginalWorkKeeper> = admission.clone();
        bounded::with_original_work_keeper(keeper, operation).await
    } else {
        operation.await
    }
}

// Field order is intentional: captured resources (including unpolled closure
// inputs or an unclaimed File result) drop BEFORE the same original keeper.
// This inline value adds no task, permit or Box. Its requested value storage
// and Arc clone are not a bound on Tokio/allocator/native live peaks.
struct Retained<T> {
    value: T,
    _keeper: Option<Arc<dyn OriginalWorkKeeper>>,
}
impl<T> Retained<T> {
    fn new(value: T) -> Self {
        Self {
            value,
            _keeper: bounded::original_keeper(),
        }
    }
    fn map<U>(self, operation: impl FnOnce(T) -> U) -> Retained<U> {
        let Self { value, _keeper } = self;
        Retained {
            value: operation(value),
            _keeper,
        }
    }
    fn accept(self) -> T {
        // The awaiting original scope still owns its admission. Do not retain
        // the CPU permit forever in an accepted cached Model or Engine.
        self.value
    }
}

/// Replace an EXISTING blocking launch, synchronously owning its whole closure
/// before queueing it. A dropped waiter cannot release the keeper while that
/// closure or its unclaimed FD-bearing result remains in the original job.
pub(crate) fn spawn_blocking<F, T>(
    operation: F,
) -> impl Future<Output = Result<T, tokio::task::JoinError>> + Send
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let task = spawn_retained(operation);
    async move { task.await.map(Retained::accept) }
}
fn spawn_retained<F, T>(operation: F) -> tokio::task::JoinHandle<Retained<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let input = Retained::new(operation);
    tokio::task::spawn_blocking(move || input.map(|operation| operation()))
}

#[cfg(all(test, unix))]
mod tests;
