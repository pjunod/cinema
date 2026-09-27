//! Test-only loss of successful Store replies, scoped to one async caller.
//! The write actually commits before its acknowledgement is discarded.
use plurx_core::error::StoreError;
use std::future::Future;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

pub(super) const CLAIM: u8 = 1;
pub(super) const RENEW: u8 = 2;
pub(super) const PUBLISH: u8 = 4;
struct Replies {
    pending: AtomicU8,
    dropped: AtomicU8,
}
tokio::task_local! { static REPLIES: Arc<Replies>; }

pub(super) fn after_commit<T>(point: u8, result: Result<T, StoreError>) -> Result<T, StoreError> {
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

pub(super) async fn dropping<F: Future>(points: u8, future: F) -> (F::Output, u8) {
    let replies = Arc::new(Replies {
        pending: AtomicU8::new(points),
        dropped: AtomicU8::new(0),
    });
    let result = REPLIES.scope(Arc::clone(&replies), future).await;
    (result, replies.dropped.load(Ordering::SeqCst))
}
