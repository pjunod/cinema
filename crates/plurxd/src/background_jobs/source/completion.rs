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
    store: Arc<dyn Store>,
    state: Mutex<State>,
    changed: tokio::sync::Notify,
    /// Closes registration (the owner's loss token).
    cancel: CancellationToken,
    /// The exact owner finished and retired its claim. Without it (a dropped
    /// owner) the ledger never publishes proof, matching the legacy join.
    authorize: CancellationToken,
    /// Boot tick when the owner's ledger started; no child launched before it
    /// can have received this owner's source.
    started_tick: u64,
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
            store: Arc::clone(&store),
            state: Mutex::new(State::default()),
            changed: tokio::sync::Notify::new(),
            cancel,
            authorize,
            started_tick: crate::source_custody::boot_tick(),
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
        let first = !state.enrolled;
        state.enrolled = true;
        state.operations += 1;
        if !state.files.iter().any(|held| Arc::ptr_eq(held, &file)) {
            state.files.push(Arc::clone(&file));
        }
        drop(state);
        if first {
            if let Err(error) =
                crate::source_custody::record_background_source(&self.store, &self.receipt, &file)
            {
                // Only weakens a later dead-owner census, never live proof.
                tracing::warn!(custody_id = %self.receipt.custody_id, %error,
                    "opened source identity not recorded beside its witness");
            }
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
    /// Resolves once nothing but this actor references the ledger: the owner
    /// was dropped without `finish`, which never settles custody.
    async fn abandoned(self: &Arc<Self>) {
        while Arc::strong_count(self) > 1 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    async fn drive(self: Arc<Self>, store: Arc<dyn Store>) {
        tokio::select! {
            () = self.cancel.cancelled() => {}
            () = self.abandoned() => return,
        }
        if !self.owns_settlement() {
            return;
        }
        // Physical closure alone is not enough: the owner retires its claim
        // first, exactly as the legacy explicit join did.
        tokio::select! {
            () = self.authorize.cancelled() => {}
            () = self.abandoned() => return,
        }
        loop {
            let ready = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.closed = true;
                if state.veto {
                    tracing::warn!(custody_id = %self.receipt.custody_id,
                        "untracked source capability: live completion refuses; custody stays charged");
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
                break;
            }
            // Arc<File> holders do not issue notifications when they close.
            tokio::select! { () = self.changed.notified() => {}, () = tokio::time::sleep(Duration::from_millis(25)) => {} }
        }
        // Every registered in-process holder is closed. An unknown launch (any
        // unmediated daemon child, or none tracked at all on this backend)
        // could still have handed a child this source, so wait until every
        // daemon descendant started since then has physically exited.
        let since = match crate::source_custody::live_background_join(&store, &self.receipt) {
            crate::source_custody::LiveJoin::Clean => None,
            crate::source_custody::LiveJoin::EscapedSince(tick) => Some(tick),
            crate::source_custody::LiveJoin::Unwitnessed => Some(self.started_tick),
            crate::source_custody::LiveJoin::Refused(reason) => {
                tracing::warn!(custody_id = %self.receipt.custody_id, reason,
                    "live source completion refused; custody stays charged");
                return;
            }
        };
        if let Some(since) = since {
            if !self.descendants_exited(since).await {
                return;
            }
        }
        crate::background_jobs::settle_joined_source_custody(&store, self.receipt.clone()).await;
    }
    #[cfg(target_os = "linux")]
    async fn descendants_exited(&self, since: u64) -> bool {
        if !crate::source_custody::process_tree_complete() {
            tracing::warn!(custody_id = %self.receipt.custody_id,
                "daemon is neither PID 1 nor a subreaper; launched children cannot be proven exited");
            return false;
        }
        let waiting = tokio::time::Instant::now();
        let mut reported = false;
        loop {
            match crate::source_custody::live_descendants_since(since) {
                Ok(live) if live.is_empty() => return true,
                Ok(live) if !reported && waiting.elapsed() >= Duration::from_secs(60) => {
                    reported = true;
                    tracing::warn!(custody_id = %self.receipt.custody_id, ?live,
                        "source custody waits for children launched while it was live");
                }
                Ok(_) => {}
                Err(error) if !reported => {
                    reported = true;
                    tracing::warn!(custody_id = %self.receipt.custody_id, %error,
                        "descendant census unavailable; source custody stays charged");
                }
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    #[cfg(not(target_os = "linux"))]
    async fn descendants_exited(&self, _since: u64) -> bool {
        tracing::warn!(custody_id = %self.receipt.custody_id,
            "launched children cannot be proven exited on this platform");
        false
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
