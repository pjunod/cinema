use super::*;
use std::collections::HashMap;
use std::sync::Weak;

pub(super) struct GateCell {
    pub(super) drops: Arc<AtomicUsize>,
}
impl Drop for GateCell {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::AcqRel);
    }
}
pub(super) type OriginalGate = tokio::sync::Mutex<GateCell>;
/// Opaque handle to one original mutex. It never exposes Arc or Weak.
#[derive(Clone)]
pub struct BuildGate {
    pub(super) inner: Arc<OriginalGate>,
}
#[must_use = "retain this guard for the protected operation"]
pub struct BuildGuard {
    _guard: tokio::sync::OwnedMutexGuard<GateCell>,
}
#[must_use = "retain this guard for the protected operation"]
pub struct BorrowedBuildGuard<'a> {
    _guard: tokio::sync::MutexGuard<'a, GateCell>,
}
impl BuildGate {
    pub fn lock_owned(self) -> impl Future<Output = BuildGuard> {
        use futures_util::FutureExt;
        self.inner
            .lock_owned()
            .map(|guard| BuildGuard { _guard: guard })
    }
    pub fn try_lock_owned(self) -> Result<BuildGuard, tokio::sync::TryLockError> {
        self.inner
            .try_lock_owned()
            .map(|guard| BuildGuard { _guard: guard })
    }
    pub fn lock(&self) -> impl Future<Output = BorrowedBuildGuard<'_>> {
        use futures_util::FutureExt;
        self.inner
            .lock()
            .map(|guard| BorrowedBuildGuard { _guard: guard })
    }
}
#[derive(Clone)]
pub struct BuildGateDirectory {
    pub(super) inner: Arc<Directory>,
}
impl Default for BuildGateDirectory {
    fn default() -> Self {
        Self {
            inner: Arc::new(Directory {
                id: Uuid::new_v4(),
                state: Mutex::new(DirectoryState {
                    epoch: 0,
                    mode: Mode::Legacy,
                    legacy: HashMap::new(),
                }),
                drops: Arc::new(AtomicUsize::new(0)),
            }),
        }
    }
}
impl BuildGateDirectory {
    pub fn gate(&self, key: &str) -> Result<BuildGate, AdmissionRefusal> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        match &state.mode {
            Mode::Closing(_) => return Err(AdmissionRefusal::Unavailable),
            Mode::Running(client) => {
                if let Some(gate) = client.find_gate(key)? {
                    return Ok(gate);
                }
            }
            Mode::Legacy => (),
        }
        if let Some(gate) = state.legacy.get(key).and_then(Weak::upgrade) {
            return Ok(BuildGate { inner: gate });
        }
        let gate = Arc::new(tokio::sync::Mutex::new(GateCell {
            drops: Arc::clone(&self.inner.drops),
        }));
        state.legacy.insert(key.to_owned(), Arc::downgrade(&gate));
        Ok(BuildGate { inner: gate })
    }
    pub fn prune(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.legacy.retain(|_, gate| gate.strong_count() > 0);
        }
    }
    /// Scalar legacy-directory diagnostics; not an inventory completeness witness.
    pub fn fixture_len(&self) -> usize {
        self.inner
            .state
            .lock()
            .map_or(0, |state| state.legacy.len())
    }
}
pub(super) struct Directory {
    pub(super) id: Uuid,
    pub(super) state: Mutex<DirectoryState>,
    pub(super) drops: Arc<AtomicUsize>,
}
pub(super) struct DirectoryState {
    pub(super) epoch: u64,
    pub(super) mode: Mode,
    pub(super) legacy: HashMap<String, Weak<OriginalGate>>,
}
pub(super) enum Mode {
    Legacy,
    Running(OrdinaryPoolClient),
    Closing(SealIdentity),
}
impl Directory {
    pub(super) fn identity(
        &self,
        support: SupportIdentity,
    ) -> Result<SealIdentity, AdmissionRefusal> {
        let state = self
            .state
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if !matches!(state.mode, Mode::Legacy) {
            return Err(AdmissionRefusal::InvalidTransition);
        }
        Ok(SealIdentity {
            support,
            directory: self.id,
            epoch: state
                .epoch
                .checked_add(1)
                .ok_or(AdmissionRefusal::IdentityExhausted)?,
        })
    }
    pub(super) fn publish(&self, client: OrdinaryPoolClient) -> Result<(), AdmissionRefusal> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if !matches!(state.mode, Mode::Legacy)
            || state.epoch.checked_add(1) != Some(client.payload.seal.epoch)
            || self.id != client.payload.seal.directory
        {
            return Err(AdmissionRefusal::ReceiptMismatch);
        }
        state.mode = Mode::Running(client);
        Ok(())
    }
    pub(super) fn seal(&self, seal: SealIdentity) -> Result<(), AdmissionRefusal> {
        let old = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| AdmissionRefusal::Unavailable)?;
            match &state.mode {
                Mode::Closing(current) if *current == seal => return Ok(()),
                Mode::Running(client) if client.payload.seal == seal => (),
                _ => return Err(AdmissionRefusal::ReceiptMismatch),
            }
            std::mem::replace(&mut state.mode, Mode::Closing(seal))
        };
        drop(old);
        Ok(())
    }
    pub(super) fn reopen(
        &self,
        released: FinalizationSupportReleased,
    ) -> Result<(), AdmissionRefusal> {
        let seal = released
            .collector
            .ok_or(AdmissionRefusal::ReceiptMismatch)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| AdmissionRefusal::Unavailable)?;
        if released.identity != seal.support
            || self.id != seal.directory
            || !matches!(&state.mode, Mode::Closing(current) if *current == seal)
        {
            return Err(AdmissionRefusal::ReceiptMismatch);
        }
        state.epoch = seal.epoch;
        state.mode = Mode::Legacy;
        Ok(())
    }
}
