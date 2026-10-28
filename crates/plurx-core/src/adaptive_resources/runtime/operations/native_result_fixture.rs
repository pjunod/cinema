//! Delay delivery of a real native result held by its original worker.
//! No native call, outcome, File, owner, receipt or operation is fabricated.
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Kind {
    SourceParentClose,
    BootstrapRoot,
    BootstrapTraversal,
    BootstrapChild,
    Publication,
    Sync,
    CleanupRemoveFile,
    CleanupSyncChild,
    CleanupRemoveChild,
    CleanupSyncParent,
    CleanupCloseHandles,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Selector {
    Operation(Uuid),
    NextMember(Uuid),
}
type Key = (Kind, Selector);
#[derive(Default)]
struct State {
    operation: Option<Uuid>,
    job: Option<tokio::task::Id>,
    reached: bool,
    released: bool,
}
struct Delay {
    state: Mutex<State>,
    release: Condvar,
    reached: tokio::sync::Notify,
}
fn registry() -> &'static Mutex<BTreeMap<Key, Arc<Delay>>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<Key, Arc<Delay>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) struct Gate {
    delay: Arc<Delay>,
}
impl Gate {
    fn install_selector(kind: Kind, selector: Selector) -> Self {
        let delay = Arc::new(Delay {
            state: Mutex::new(State::default()),
            release: Condvar::new(),
            reached: tokio::sync::Notify::new(),
        });
        assert!(registry()
            .lock()
            .expect("native result fixture registry")
            .insert((kind, selector), Arc::clone(&delay))
            .is_none());
        Self { delay }
    }
    pub(super) fn install(kind: Kind, operation: Uuid) -> Self {
        Self::install_selector(kind, Selector::Operation(operation))
    }
    /// Select the next matching REAL operation registered by this member.
    /// The operation is bound before the original entry sender is consumed.
    pub(super) fn install_next(kind: Kind, member: Uuid) -> Self {
        Self::install_selector(kind, Selector::NextMember(member))
    }
    pub(super) async fn reached(&self) -> tokio::task::Id {
        self.delay.reached.notified().await;
        let state = self
            .delay
            .state
            .lock()
            .expect("original native result state");
        assert!(state.reached);
        state.job.expect("actual original registered JoinHandle id")
    }
    pub(super) fn identity(&self) -> (Uuid, tokio::task::Id) {
        let state = self
            .delay
            .state
            .lock()
            .expect("original native result identity");
        assert!(state.reached);
        (
            state.operation.expect("original named IO"),
            state.job.expect("original job"),
        )
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        // Remove the member selector and its alias to the actual operation.
        registry()
            .lock()
            .expect("native result fixture registry")
            .retain(|_, candidate| !Arc::ptr_eq(candidate, &self.delay));
        self.delay
            .state
            .lock()
            .expect("native result fixture release")
            .released = true;
        self.delay.release.notify_all();
    }
}

fn bind(delay: &Delay, operation: Uuid, job: tokio::task::Id) {
    let mut state = delay.state.lock().expect("original native job binding");
    assert!(state.operation.replace(operation).is_none());
    assert!(
        state.job.replace(job).is_none(),
        "same slot must not register replacement work"
    );
}
/// Called by the original owner before its existing entry latch is released.
pub(super) fn bind_job(kind: Kind, operation: Uuid, job: tokio::task::Id) {
    let delay = registry()
        .lock()
        .expect("native result fixture registry")
        .get(&(kind, Selector::Operation(operation)))
        .cloned();
    if let Some(delay) = delay {
        bind(&delay, operation, job);
    }
}
/// Bind a member selector to the already registered job and its actual IO.
/// This creates only a fixture lookup alias, never an operation or native owner.
pub(super) fn bind_next(kind: Kind, member: Uuid, operation: Uuid, job: tokio::task::Id) {
    let mut entries = registry().lock().expect("native result fixture registry");
    if let Some(delay) = entries.get(&(kind, Selector::NextMember(member))).cloned() {
        bind(&delay, operation, job);
        assert!(entries
            .insert((kind, Selector::Operation(operation)), delay)
            .is_none());
    }
}
/// Called only after the real native return is locally retained by the SAME
/// worker. It never receives or changes that native result. Native errors are
/// already captured by the caller; the new sites delay positive results only.
pub(super) fn after_result(kind: Kind, operation: Uuid) {
    let delay = registry()
        .lock()
        .expect("native result fixture registry")
        .get(&(kind, Selector::Operation(operation)))
        .cloned();
    if let Some(delay) = delay {
        let mut state = delay.state.lock().expect("original native result state");
        assert_eq!(state.operation, Some(operation));
        assert!(state.job.is_some() && !state.reached);
        state.reached = true;
        delay.reached.notify_one();
        let (state, timeout) = delay
            .release
            .wait_timeout_while(state, Duration::from_secs(30), |state| !state.released)
            .expect("native result fixture wait");
        assert!(
            !timeout.timed_out() && state.released,
            "native result fixture watchdog expired; no closure is inferred"
        );
    }
}
