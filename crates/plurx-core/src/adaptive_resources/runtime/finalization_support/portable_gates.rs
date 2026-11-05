//! Legacy gate interface on platforms without the native finite collector.
use crate::adaptive_resources::AdmissionRefusal;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard, OwnedMutexGuard};
#[derive(Clone)]
pub struct BuildGate {
    inner: Arc<AsyncMutex<()>>,
}
#[must_use = "retain the guard for the protected operation"]
pub struct BuildGuard {
    _guard: OwnedMutexGuard<()>,
}
#[must_use = "retain the guard for the protected operation"]
pub struct BorrowedBuildGuard<'a> {
    _guard: MutexGuard<'a, ()>,
}
impl BuildGate {
    pub async fn lock_owned(self) -> BuildGuard {
        BuildGuard {
            _guard: self.inner.lock_owned().await,
        }
    }
    pub fn try_lock_owned(self) -> Result<BuildGuard, tokio::sync::TryLockError> {
        self.inner
            .try_lock_owned()
            .map(|guard| BuildGuard { _guard: guard })
    }
    pub async fn lock(&self) -> BorrowedBuildGuard<'_> {
        BorrowedBuildGuard {
            _guard: self.inner.lock().await,
        }
    }
}
#[derive(Clone, Default)]
pub struct BuildGateDirectory {
    entries: Arc<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>>,
}
impl BuildGateDirectory {
    pub fn gate(&self, key: &str) -> Result<BuildGate, AdmissionRefusal> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(inner) = entries.get(key).and_then(Weak::upgrade) {
            return Ok(BuildGate { inner });
        }
        let inner = Arc::new(AsyncMutex::new(()));
        entries.insert(key.to_owned(), Arc::downgrade(&inner));
        Ok(BuildGate { inner })
    }
    pub fn prune(&self) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, gate| gate.strong_count() > 0);
    }
    pub fn fixture_len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}
