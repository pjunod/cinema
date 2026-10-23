//! An application alias drop fence for a finite role. Native closure and
//! durable settlement remain separate, proof-bearing operations.
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;
use tokio::sync::Notify;

pub(super) struct FiniteAppState {
    inner: Mutex<FiniteAppCounts>,
    changed: Notify,
}

struct FiniteAppCounts {
    sealed: bool,
    guards: usize,
    current: Weak<FiniteAppGuard>,
    failed: bool,
}

pub(super) struct FiniteAppGuard {
    state: Arc<FiniteAppState>,
}

impl FiniteAppState {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(FiniteAppCounts {
                sealed: false,
                guards: 0,
                current: Weak::new(),
                failed: false,
            }),
            changed: Notify::new(),
        })
    }

    pub(super) fn new_guard(self: &Arc<Self>) -> Option<Arc<FiniteAppGuard>> {
        let mut inner = self.inner.lock().ok()?;
        if inner.sealed || inner.failed {
            return None;
        }
        if let Some(guard) = inner.current.upgrade() {
            return Some(guard);
        }
        if inner.guards != 0 {
            inner.failed = true;
            return None;
        }
        inner.guards = 1;
        let guard = Arc::new(FiniteAppGuard {
            state: Arc::clone(self),
        });
        inner.current = Arc::downgrade(&guard);
        Some(guard)
    }

    pub(super) fn seal(&self) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        if inner.failed {
            return false;
        }
        inner.sealed = true;
        true
    }

    pub(super) async fn wait_released(&self, deadline: Instant) -> bool {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.inner.lock() {
                Ok(inner) if inner.sealed && !inner.failed && inner.guards == 0 => return true,
                Err(_) => return false,
                _ => {}
            }
            if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), notified)
                .await
                .is_err()
            {
                return false;
            }
        }
    }

    pub(super) fn released(&self) -> bool {
        self.inner
            .lock()
            .is_ok_and(|inner| inner.sealed && !inner.failed && inner.guards == 0)
    }
}

impl Drop for FiniteAppGuard {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.state.inner.lock() {
            if let Some(remaining) = inner.guards.checked_sub(1) {
                inner.guards = remaining;
            } else {
                inner.failed = true;
            }
        }
        self.state.changed.notify_waiters();
    }
}

/// The one state and one final-drop guard are charged in both the original R
/// and its conserving S. A clone of an output or control shares the same guard.
pub(super) fn requested_heap_bytes() -> Option<usize> {
    std::mem::size_of::<FiniteAppState>()
        .checked_add(std::mem::size_of::<FiniteAppGuard>())?
        .checked_add(4 * std::mem::size_of::<usize>())
}
