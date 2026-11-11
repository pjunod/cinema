//! Live exact-owner completion. This never makes an invalidated restart
//! witness replayable, and starts disabled until an audited producer enrolls.
use super::*;
use plurx_core::store::background_jobs_resources::BackgroundSourceCustodyToken;

#[derive(Default)]
struct State {
    enrolled: bool,
    closed: bool,
    veto: bool,
    operations: usize,
    holders: usize,
    raw: usize,
    // These are OWNED original media capabilities, not recovery anchor refs.
    files: Vec<Arc<std::fs::File>>,
}
pub(crate) struct Completion {
    receipt: BackgroundSourceCustodyToken,
    state: Mutex<State>,
    changed: tokio::sync::Notify,
    /// Closes registration (the owner's loss token).
    cancel: CancellationToken,
    /// The exact owner finished and retired its claim. Without it (a dropped
    /// owner) the ledger never publishes proof, matching the legacy join.
    authorize: CancellationToken,
}
pub(crate) struct Operation(Arc<Completion>);
pub(crate) struct Holder(Arc<Completion>);
pub(crate) struct RawRetirement {
    completion: Arc<Completion>,
    joined: bool,
}
impl Completion {
    pub(super) fn start(
        store: Arc<dyn Store>,
        receipt: BackgroundSourceCustodyToken,
        cancel: CancellationToken,
        authorize: CancellationToken,
    ) -> Arc<Self> {
        let owner = Arc::new(Self {
            receipt,
            state: Mutex::new(State::default()),
            changed: tokio::sync::Notify::new(),
            cancel,
            authorize,
        });
        let driving = Arc::clone(&owner);
        tokio::spawn(async move { driving.drive(store).await });
        owner
    }
    pub(super) fn enroll(self: &Arc<Self>, file: Arc<std::fs::File>) -> Result<Operation, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "source completion poisoned")?;
        if state.closed || self.cancel.is_cancelled() {
            return Err("source completion registration closed".into());
        }
        state.enrolled = true;
        state.operations += 1;
        if !state.files.iter().any(|held| Arc::ptr_eq(held, &file)) {
            state.files.push(file);
        }
        Ok(Operation(Arc::clone(self)))
    }
    pub(super) fn file(&self, file: Arc<std::fs::File>) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "source completion poisoned")?;
        if state.closed || self.cancel.is_cancelled() {
            return Err("source completion registration closed".into());
        }
        if state.enrolled && !state.files.iter().any(|held| Arc::ptr_eq(held, &file)) {
            state.files.push(file);
        }
        Ok(())
    }
    pub(super) fn holder(self: &Arc<Self>) -> Result<Option<Holder>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "source completion poisoned")?;
        if state.closed || self.cancel.is_cancelled() {
            return Err("source completion registration closed".into());
        }
        if !state.enrolled {
            return Ok(None);
        }
        state.holders += 1;
        Ok(Some(Holder(Arc::clone(self))))
    }
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(super) fn raw(self: &Arc<Self>) -> Result<Option<RawRetirement>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "source completion poisoned")?;
        if state.closed || self.cancel.is_cancelled() {
            return Err("source completion registration closed".into());
        }
        if !state.enrolled {
            return Ok(None);
        }
        state.raw += 1;
        Ok(Some(RawRetirement {
            completion: Arc::clone(self),
            joined: false,
        }))
    }
    /// Only meaningful after `cancel` fired: registration is then closed, so
    /// enrollment can no longer change. An enrolled ledger (vetoed or not)
    /// owns settlement; the owner must not publish a joined proof itself.
    pub(super) fn owns_settlement(&self) -> bool {
        debug_assert!(self.cancel.is_cancelled());
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        state.enrolled
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn observation(&self) -> (usize, usize, bool) {
        let state = self.state.lock().expect("completion state");
        (state.operations, state.raw, state.veto)
    }
    pub(super) fn veto(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .veto = true;
        self.changed.notify_one();
    }
    async fn drive(self: Arc<Self>, store: Arc<dyn Store>) {
        self.cancel.cancelled().await;
        if !self.owns_settlement() {
            return;
        }
        // Physical closure alone is not enough: the owner retires its claim
        // first, exactly as the legacy explicit join did.
        self.authorize.cancelled().await;
        loop {
            let ready = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.closed = true;
                if state.veto || !state.enrolled {
                    return;
                }
                let ready = state.operations == 0
                    && state.holders == 0
                    && state.raw == 0
                    && state.files.iter().all(|file| Arc::strong_count(file) == 1);
                if ready {
                    // Close the last real descriptors BEFORE publishing proof.
                    state.files.clear();
                }
                ready
            };
            if ready {
                // An OWNED anchor is not a media capability. Require THIS boot's
                // exact witness and reject every unknown escape. Missing state
                // (including after restart) can never establish live proof.
                if crate::source_custody::live_background_join_allowed(&store, &self.receipt) {
                    crate::background_jobs::settle_joined_source_custody(
                        &store,
                        self.receipt.clone(),
                    )
                    .await;
                }
                return;
            }
            // Arc<File> holders do not issue notifications when they close.
            tokio::select! { () = self.changed.notified() => {}, () = tokio::time::sleep(Duration::from_millis(25)) => {} }
        }
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.operations -= 1;
        self.0.changed.notify_one();
    }
}
// Only stored AFTER the actual File/SourceFence in its owning struct. Every
// blocking read retains that same owner Arc, so this is a close notification,
// not an assertion that dropping the initiating operation joined anything.
impl Drop for Holder {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.holders -= 1;
        self.0.changed.notify_one();
    }
}
impl RawRetirement {
    pub(crate) fn spawn<T>(&self, spawn: impl FnOnce() -> T) -> T {
        crate::source_custody::declared_background_spawn(&self.completion.receipt, spawn)
    }
    /// Caller has reaped the child AND joined its source read/supervisor worker.
    pub(crate) fn joined(mut self) {
        self.joined = true;
        let mut state = self
            .completion
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.raw -= 1;
        self.completion.changed.notify_one();
    }
}
impl Drop for RawRetirement {
    fn drop(&mut self) {
        if !self.joined {
            self.completion.veto();
        }
    }
}
