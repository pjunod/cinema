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
    application_limit: usize,
    application_taken: bool,
    application_live: bool,
}

pub(super) struct FiniteAppGuard {
    state: Arc<FiniteAppState>,
}

/// One charged rich application entry/future. It is deliberately non-clone;
/// its final Drop follows destruction of the owned application payload.
pub struct FiniteApplicationLease {
    state: Arc<FiniteAppState>,
}

impl FiniteAppState {
    pub(super) fn new() -> Arc<Self> {
        Self::with_application_limit(0)
    }

    pub(super) fn with_application_limit(application_limit: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(FiniteAppCounts {
                sealed: false,
                guards: 0,
                current: Weak::new(),
                failed: false,
                application_limit,
                application_taken: false,
                application_live: false,
            }),
            changed: Notify::new(),
        })
    }

    pub(super) fn take_application_lease(
        self: &Arc<Self>,
        requested_bytes: usize,
    ) -> Option<FiniteApplicationLease> {
        let mut inner = self.inner.lock().ok()?;
        if inner.sealed
            || inner.failed
            || inner.application_taken
            || requested_bytes == 0
            || requested_bytes > inner.application_limit
        {
            return None;
        }
        inner.application_taken = true;
        inner.application_live = true;
        Some(FiniteApplicationLease {
            state: Arc::clone(self),
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
                Ok(inner)
                    if inner.sealed
                        && !inner.failed
                        && inner.guards == 0
                        && !inner.application_live =>
                {
                    return true;
                }
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
        self.inner.lock().is_ok_and(|inner| {
            inner.sealed && !inner.failed && inner.guards == 0 && !inner.application_live
        })
    }

    pub(super) fn application_limit_bytes(&self) -> Option<u64> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| u64::try_from(inner.application_limit).ok())
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

impl Drop for FiniteApplicationLease {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.state.inner.lock() {
            if !inner.application_live {
                inner.failed = true;
            } else {
                inner.application_live = false;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn finite_application_slot_survives_selector_drop_until_actual_entry_drop() {
        let state = FiniteAppState::with_application_limit(128);
        let selector = state.new_guard().expect("original selector");
        let entry = state.take_application_lease(128).expect("precharged entry");
        assert!(state.take_application_lease(1).is_none(), "one shot");
        assert!(state.seal());
        drop(selector);
        assert!(!state.released(), "rich entry still owns its charged slot");
        drop(entry);
        assert!(state.released());
        assert!(
            state.new_guard().is_none(),
            "sealed role cannot issue an alias"
        );
    }
}
