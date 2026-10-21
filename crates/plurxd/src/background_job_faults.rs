//! Test-only loss of successful Store replies, scoped to one async caller.
//! The write actually commits before its acknowledgement is discarded.
use plurx_core::error::StoreError;
use std::future::Future;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

pub(crate) const CLAIM: u8 = 1;
pub(crate) const RENEW: u8 = 2;
pub(crate) const PUBLISH: u8 = 4;
pub(crate) const COMPLETE: u8 = 8;
struct Replies {
    pending: AtomicU8,
    dropped: AtomicU8,
}
tokio::task_local! { static REPLIES: Arc<Replies>; }

pub(crate) fn after_commit<T>(point: u8, result: Result<T, StoreError>) -> Result<T, StoreError> {
    if result.is_ok()
        && REPLIES
            .try_with(|replies| {
                let drop = replies.pending.fetch_and(!point, Ordering::SeqCst) & point != 0;
                if drop {
                    replies.dropped.fetch_or(point, Ordering::SeqCst);
                }
                drop
            })
            .unwrap_or(false)
    {
        return Err(StoreError::Task(
            "injected lost Store acknowledgement".into(),
        ));
    }
    result
}

pub(crate) async fn dropping<F: Future>(points: u8, future: F) -> (F::Output, u8) {
    let replies = Arc::new(Replies {
        pending: AtomicU8::new(points),
        dropped: AtomicU8::new(0),
    });
    let result = REPLIES.scope(Arc::clone(&replies), future).await;
    (result, replies.dropped.load(Ordering::SeqCst))
}

/// Test-only barriers for observing the real library worker's lifecycle. Pause
/// at `BeforeScan`, outside the select arms; pausing at `QueueLoss`, `DomainLoss`
/// or `Pressure` stalls an arm that has already been selected. The task-local
/// deliberately does not flow into spawned heartbeat tasks; those transitions
/// remain covered by storage schedules or explicitly unchecked.
#[cfg(test)]
pub(crate) mod worker {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::{mpsc, oneshot, Mutex};

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub(crate) enum Point {
        Claimed,
        DomainUnavailable,
        DomainAcquired,
        Bound,
        Processing,
        BeforeScan,
        QueueLoss,
        DomainLoss,
        Pressure,
        WorkJoined,
        BeforeCompletion,
        CompletionReconciled,
        ReleaseStarted,
        ReleaseFinished,
        Finished,
        AdmissionDropped,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct Event {
        pub(crate) worker: String,
        pub(crate) point: Point,
        pub(crate) signals: Option<Signals>,
    }

    #[derive(Debug, Clone)]
    pub(crate) struct Signals {
        pub(crate) queue_loss: tokio_util::sync::CancellationToken,
        pub(crate) domain_loss: tokio_util::sync::CancellationToken,
    }

    pub(crate) struct Scope {
        worker: String,
        observe: mpsc::UnboundedSender<Event>,
        pause: Mutex<Option<(Point, oneshot::Receiver<()>)>>,
        force_pressure: AtomicBool,
    }

    tokio::task_local! { static CURRENT: Arc<Scope>; }

    pub(crate) fn scope(
        worker: impl Into<String>,
        observe: mpsc::UnboundedSender<Event>,
        pause: Option<(Point, oneshot::Receiver<()>)>,
        force_pressure: bool,
    ) -> Arc<Scope> {
        Arc::new(Scope {
            worker: worker.into(),
            observe,
            pause: Mutex::new(pause),
            force_pressure: AtomicBool::new(force_pressure),
        })
    }

    pub(crate) async fn at(point: Point) {
        at_with_signals(point, None).await;
    }

    pub(crate) async fn at_with_signals(point: Point, signals: Option<Signals>) {
        let Some(scope) = CURRENT.try_with(Arc::clone).ok() else {
            return;
        };
        let _ = scope.observe.send(Event {
            worker: scope.worker.clone(),
            point,
            signals,
        });
        let pause = {
            let mut pause = scope.pause.lock().await;
            if pause
                .as_ref()
                .is_some_and(|(selected, _)| *selected == point)
            {
                pause.take().map(|(_, receiver)| receiver)
            } else {
                None
            }
        };
        if let Some(receiver) = pause {
            let _ = receiver.await;
        }
    }

    pub(crate) fn force_pressure() -> bool {
        CURRENT
            .try_with(|scope| scope.force_pressure.load(Ordering::Acquire))
            .unwrap_or(false)
    }

    pub(crate) fn set_force_pressure(scope: &Scope, forced: bool) {
        scope.force_pressure.store(forced, Ordering::Release);
    }

    pub(crate) async fn run<F: std::future::Future>(scope: Arc<Scope>, future: F) -> F::Output {
        CURRENT.scope(scope, future).await
    }
}
