//! Exact-operation delayed reply AFTER real settlement commit/result storage.
//! This synchronization creates no command, receipt, native proof or refund.
use super::*;
use std::sync::{Condvar, OnceLock, Weak};
use tokio::sync::Notify;

type Key = (Uuid, Uuid); // actual journal owner, original operation
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SettlementReplyCommitted {
    pub(crate) owner_id: Uuid,
    pub(crate) operation_id: Uuid,
    pub(crate) obligation_id: Uuid,
    pub(crate) graph_id: Uuid,
    pub(crate) sequence: i64,
    pub(crate) phase: Phase,
}
#[derive(Default)]
struct Pause {
    committed: Option<SettlementReplyCommitted>,
    released: bool,
}
#[derive(Default)]
struct State {
    pause: Mutex<Pause>,
    entered: Notify,
    release: Condvar,
}
fn registry() -> &'static Mutex<BTreeMap<Key, Weak<State>>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<Key, Weak<State>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}
fn lookup(key: Key) -> Option<Arc<State>> {
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)?
        .upgrade()
}
/// Fixture synchronization only. Drop releases the SAME journal worker even
/// when a runtime assertion panics or a borrowed ticket waiter is cancelled.
pub(crate) struct SettlementReplyGate {
    key: Key,
    state: Arc<State>,
}
impl SettlementReplyGate {
    pub(super) fn install(owner: Uuid, operation: Uuid) -> Option<Self> {
        let key = (owner, operation);
        let mut entries = registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|_, state| state.strong_count() > 0);
        if entries.contains_key(&key) {
            return None;
        }
        let state = Arc::new(State::default());
        entries.insert(key, Arc::downgrade(&state));
        Some(Self { key, state })
    }
    pub(crate) async fn committed(&self) -> SettlementReplyCommitted {
        loop {
            let entered = self.state.entered.notified();
            tokio::pin!(entered);
            entered.as_mut().enable();
            let committed = self
                .state
                .pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .committed;
            if let Some(committed) = committed {
                return committed;
            }
            entered.await;
        }
    }
    pub(crate) fn release(&self) {
        let mut pause = self
            .state
            .pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pause.released = true;
        self.state.release.notify_all();
    }
}
impl Drop for SettlementReplyGate {
    fn drop(&mut self) {
        self.release();
        registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
    }
}

pub(super) fn after_run(
    owner: Uuid,
    owned: &Arc<Mutex<OwnedOperation>>,
    result: &Result<OperationResult, CustodyError>,
) {
    // run() already returned: its SQLite transaction and original operation
    // guard are gone. Reborrow briefly to select only the exact stored result;
    // this guard MUST leave before registry lookup or the synchronous pause.
    let committed = {
        let Ok(OperationResult::Changed(ack)) = result else {
            return;
        };
        if ack.phase != Phase::Settled {
            return;
        }
        let Ok(operation) = owned.lock() else {
            return;
        };
        let Command::Change {
            key,
            mutation: Mutation::SettleTyped(proof),
        } = &operation.operation.command
        else {
            return;
        };
        if operation.operation.id != ack.operation_id
            || key.operation_id != ack.operation_id
            || key.handle.owner_id != owner
            || key.handle.obligation_id != ack.obligation_id
            || key.handle.graph_id != ack.graph_id
            || proof.settlement.owner_id.uuid() != owner
            || proof.settlement.operation_id.uuid() != ack.operation_id
            || proof.settlement.graph_id.uuid() != ack.graph_id
        {
            return;
        }
        let Some(Ok(OperationResult::Changed(stored))) = &operation.result else {
            return;
        };
        if stored.operation_id != ack.operation_id
            || stored.obligation_id != ack.obligation_id
            || stored.graph_id != ack.graph_id
            || stored.sequence != ack.sequence
            || stored.phase != Phase::Settled
            || operation.queued
        {
            return;
        }
        SettlementReplyCommitted {
            owner_id: owner,
            operation_id: ack.operation_id,
            obligation_id: ack.obligation_id,
            graph_id: ack.graph_id,
            sequence: ack.sequence,
            phase: ack.phase,
        }
    };
    // No DB transaction, book mutex or OwnedOperation mutex spans this gate.
    let Some(state) = lookup((owner, committed.operation_id)) else {
        return;
    };
    let mut pause = state
        .pause
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    pause.committed = Some(committed);
    state.entered.notify_waiters();
    while !pause.released {
        pause = state
            .release
            .wait(pause)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}
