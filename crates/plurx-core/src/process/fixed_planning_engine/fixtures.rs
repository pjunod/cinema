//! Per-original-command diagnostic pause. No receipt or native authority is
//! created here; a child may already have exited when custody is observed.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::Notify;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedPlanningNativeStarted {
    pub pid: u32,
    pub close_operation_id: Uuid,
    pub member_id: Uuid,
}
#[derive(Debug)]
struct State {
    member: Uuid,
    panic_after_native_return: bool,
    started: Mutex<Option<FixedPlanningNativeStarted>>,
    entered: Notify,
    released: AtomicBool,
    release: Notify,
}
fn registry() -> &'static Mutex<BTreeMap<Uuid, Weak<State>>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<Uuid, Weak<State>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}
fn lookup(id: Uuid) -> Option<Arc<State>> {
    registry()
        .lock()
        .expect("original fixture registry")
        .get(&id)?
        .upgrade()
}
/// Retains only fixture synchronization, never a native owner or close proof.
/// Dropping this gate releases its one original command even on assertion panic.
#[derive(Debug)]
pub struct FixedPlanningNativeGate {
    id: Uuid,
    state: Arc<State>,
}
impl FixedPlanningNativeGate {
    pub(crate) fn install(id: Uuid, member: Uuid, panic_after_native_return: bool) -> Option<Self> {
        let mut entries = registry().lock().expect("original fixture registry");
        entries.retain(|_, state| state.strong_count() > 0);
        if entries.contains_key(&id) {
            return None;
        }
        let state = Arc::new(State {
            member,
            panic_after_native_return,
            started: Mutex::new(None),
            entered: Notify::new(),
            released: AtomicBool::new(false),
            release: Notify::new(),
        });
        entries.insert(id, Arc::downgrade(&state));
        Some(Self { id, state })
    }
    pub async fn started(&self) -> FixedPlanningNativeStarted {
        loop {
            let entered = self.state.entered.notified();
            tokio::pin!(entered);
            entered.as_mut().enable();
            if let Some(started) = *self.state.started.lock().expect("native diagnostic") {
                return started;
            }
            entered.await;
        }
    }
    pub fn release(&self) {
        if !self.state.released.swap(true, Ordering::AcqRel) {
            self.state.release.notify_waiters();
        }
    }
}
impl Drop for FixedPlanningNativeGate {
    fn drop(&mut self) {
        self.release();
        registry()
            .lock()
            .expect("original fixture registry")
            .remove(&self.id);
    }
}
pub(crate) async fn after_native_custody(id: Uuid, pid: u32) {
    let Some(state) = lookup(id) else {
        return;
    };
    *state.started.lock().expect("native diagnostic") = Some(FixedPlanningNativeStarted {
        pid,
        close_operation_id: id,
        member_id: state.member,
    });
    state.entered.notify_waiters();
    loop {
        let released = state.release.notified();
        tokio::pin!(released);
        released.as_mut().enable();
        if state.released.load(Ordering::Acquire) {
            break;
        }
        released.await;
    }
}
pub(crate) fn panic_after_native_return(id: Uuid) {
    if lookup(id).is_some_and(|state| state.panic_after_native_return) {
        panic!("original planning engine task lost its returned native summary");
    }
}
