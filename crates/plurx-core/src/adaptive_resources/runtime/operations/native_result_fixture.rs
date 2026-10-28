//! Delay delivery of a real result held by its original blocking worker.
//! No native call, outcome, File, owner, receipt or operation is fabricated.
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Kind {
    SourceParentClose,
    BootstrapRoot,
}
type Key = (Kind, Uuid);
#[derive(Default)]
struct State {
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
    key: Key,
    delay: Arc<Delay>,
}
impl Gate {
    pub(super) fn install(kind: Kind, operation: Uuid) -> Self {
        let key = (kind, operation);
        let delay = Arc::new(Delay {
            state: Mutex::new(State::default()),
            release: Condvar::new(),
            reached: tokio::sync::Notify::new(),
        });
        assert!(registry()
            .lock()
            .expect("native result fixture registry")
            .insert(key, Arc::clone(&delay))
            .is_none());
        Self { key, delay }
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
}
impl Drop for Gate {
    fn drop(&mut self) {
        registry()
            .lock()
            .expect("native result fixture registry")
            .remove(&self.key);
        self.delay
            .state
            .lock()
            .expect("native result fixture release")
            .released = true;
        self.delay.release.notify_all();
    }
}

/// Called by the original owner before its existing entry latch is released.
pub(super) fn bind_job(kind: Kind, operation: Uuid, job: tokio::task::Id) {
    let delay = registry()
        .lock()
        .expect("native result fixture registry")
        .get(&(kind, operation))
        .cloned();
    if let Some(delay) = delay {
        let mut state = delay.state.lock().expect("original native job binding");
        assert!(
            state.job.replace(job).is_none(),
            "same slot must not register replacement work"
        );
    }
}

/// Called only after the real native return is locally retained by the SAME
/// blocking worker. It never receives or changes that native result.
pub(super) fn after_result(kind: Kind, operation: Uuid) {
    let delay = registry()
        .lock()
        .expect("native result fixture registry")
        .get(&(kind, operation))
        .cloned();
    if let Some(delay) = delay {
        let mut state = delay.state.lock().expect("original native result state");
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
