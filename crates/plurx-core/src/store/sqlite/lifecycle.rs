//! Private ownership of SQLite operations and actual connection closure.
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;

use crate::error::StoreError;

#[derive(Default)]
struct Operations {
    closing: bool,
    active: usize,
}

#[derive(Default)]
pub(super) struct ConnectionLifecycle {
    operations: Mutex<Operations>,
    idle: tokio::sync::Notify,
    pub(super) close_owner: Mutex<()>,
}
impl ConnectionLifecycle {
    pub(super) fn admit(self: &Arc<Self>) -> Result<Operation, StoreError> {
        let mut state = self
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closing {
            return Err(StoreError::Task("SQLite store is closing or closed".into()));
        }
        state.active = state
            .active
            .checked_add(1)
            .ok_or_else(|| StoreError::Task("SQLite operation ownership overflow".into()))?;
        Ok(Operation(Arc::clone(self)))
    }
    pub(super) fn begin_close(&self) {
        self.operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closing = true;
    }
    pub(super) async fn wait_idle(&self) {
        loop {
            let idle = self.idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            if self
                .operations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active
                == 0
            {
                return;
            }
            idle.await;
        }
    }
    #[cfg(test)]
    pub(super) fn is_closing(&self) -> bool {
        self.operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closing
    }
}

/// Moved into the actual blocking worker, never retained only by its waiter.
pub(super) struct Operation(Arc<ConnectionLifecycle>);
impl Drop for Operation {
    fn drop(&mut self) {
        let mut state = self
            .0
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active -= 1;
        if state.active == 0 {
            self.0.idle.notify_waiters();
        }
    }
}

pub(super) struct ConnectionSlot {
    pub(super) connection: Mutex<Option<Connection>>,
}
impl ConnectionSlot {
    pub(super) fn new(connection: Connection) -> Self {
        Self {
            connection: Mutex::new(Some(connection)),
        }
    }
    /// No operation can still be using this slot when its connection is
    /// consumed. A rusqlite close refusal returns the owned connection to the
    /// slot; it cannot turn into successful shutdown evidence.
    pub(super) fn close(&self) -> Result<(), StoreError> {
        let mut slot = self
            .connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(connection) = slot.take() else {
            return Ok(());
        };
        match connection.close() {
            Ok(()) => Ok(()),
            Err((connection, error)) => {
                *slot = Some(connection);
                Err(error.into())
            }
        }
    }
    #[cfg(test)]
    pub(super) fn lock(&self) -> std::sync::LockResult<ConnectionGuard<'_>> {
        match self.connection.lock() {
            Ok(guard) => Ok(ConnectionGuard(guard)),
            Err(error) => Err(std::sync::PoisonError::new(ConnectionGuard(
                error.into_inner(),
            ))),
        }
    }
    #[cfg(test)]
    pub(super) fn is_poisoned(&self) -> bool {
        self.connection.is_poisoned()
    }
}

pub(super) struct ConnectionGuard<'a>(pub(super) MutexGuard<'a, Option<Connection>>);
impl std::ops::Deref for ConnectionGuard<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.0.as_ref().expect("an open connection slot")
    }
}
impl std::ops::DerefMut for ConnectionGuard<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        self.0.as_mut().expect("an open connection slot")
    }
}

#[cfg(test)]
mod tests;
