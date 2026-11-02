//! Deterministic fixture barriers, keyed by the original accepted member/op.
//! These hooks never manufacture native/journal outcomes. Engine gating occurs
//! in the SAME registered task, after its original start latch but before any
//! native entry. Enrichment gating delays only acknowledgment observation of
//! an already queued real command. Production compiles out the registry,
//! notifications and additional awaits. The production deadline wrapper owns
//! no new heap storage and never removes an original receiver or proof.
use super::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use tokio::sync::{Notify, Semaphore};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Native,
    Registered,
    BetweenEntries,
    RegisteredPreflight,
}
struct Barrier {
    entered: Notify,
    probe_joined: Notify,
    release: Semaphore,
    visits: AtomicUsize,
}
type StageRegistry = Mutex<BTreeMap<(Uuid, Stage), Arc<Barrier>>>;
fn registry() -> &'static StageRegistry {
    static REGISTRY: OnceLock<StageRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}
fn lookup(id: Uuid, stage: Stage) -> Option<Arc<Barrier>> {
    registry()
        .lock()
        .expect("fixture registry")
        .get(&(id, stage))
        .cloned()
}

pub(in crate::adaptive_resources::runtime::operations) struct Gate {
    id: Uuid,
    stage: Stage,
    barrier: Arc<Barrier>,
}
impl Gate {
    pub(super) fn install(id: Uuid) -> Self {
        Self::install_at(id, Stage::Native)
    }
    pub(super) fn registered(id: Uuid) -> Self {
        Self::install_at(id, Stage::Registered)
    }
    pub(super) fn between_entries(id: Uuid) -> Self {
        Self::install_at(id, Stage::BetweenEntries)
    }
    pub(super) fn registered_preflight(id: Uuid) -> Self {
        Self::install_at(id, Stage::RegisteredPreflight)
    }
    fn install_at(id: Uuid, stage: Stage) -> Self {
        let barrier = Arc::new(Barrier {
            entered: Notify::new(),
            probe_joined: Notify::new(),
            release: Semaphore::new(0),
            visits: AtomicUsize::new(0),
        });
        assert!(registry()
            .lock()
            .expect("fixture registry")
            .insert((id, stage), Arc::clone(&barrier))
            .is_none());
        Self { id, stage, barrier }
    }
    pub(super) async fn entered(&self) {
        self.barrier.entered.notified().await;
    }
    pub(super) async fn probe_joined(&self) {
        self.barrier.probe_joined.notified().await;
    }
    pub(super) fn release(&self) {
        self.barrier.release.add_permits(1);
    }
    pub(super) fn visits(&self) -> usize {
        self.barrier.visits.load(Ordering::SeqCst)
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        registry()
            .lock()
            .expect("fixture registry")
            .remove(&(self.id, self.stage));
        // A failing assertion must not leave the original native job gated.
        self.barrier.release.add_permits(1);
    }
}

pub(in crate::adaptive_resources::runtime::operations) async fn before_engine_native(id: Uuid) {
    if let Some(barrier) = lookup(id, Stage::Native) {
        barrier.visits.fetch_add(1, Ordering::SeqCst);
        barrier.entered.notify_one();
        barrier
            .release
            .acquire()
            .await
            .expect("fixture release")
            .forget();
    }
}
pub(in crate::adaptive_resources::runtime::operations) fn probe_joined(engine_id: Uuid) {
    if let Some(barrier) = lookup(engine_id, Stage::Native) {
        barrier.probe_joined.notify_one();
    }
}
pub(in crate::adaptive_resources::runtime::operations) async fn before_enrichment_ack(
    operation: Uuid,
) {
    if let Some(barrier) = lookup(operation, Stage::Native) {
        barrier.visits.fetch_add(1, Ordering::SeqCst);
        barrier.entered.notify_one();
        barrier
            .release
            .acquire()
            .await
            .expect("fixture release")
            .forget();
    }
}

// These delays observe the real registered handles; they grant no entry.
pub(in crate::adaptive_resources::runtime::operations) async fn before_registered_entry(id: Uuid) {
    pause_entry(id, Stage::Registered).await;
}
pub(in crate::adaptive_resources::runtime::operations) async fn between_registered_entries(
    id: Uuid,
) {
    pause_entry(id, Stage::BetweenEntries).await;
}
async fn pause_entry(id: Uuid, stage: Stage) {
    if let Some(barrier) = lookup(id, stage) {
        barrier.visits.fetch_add(1, Ordering::SeqCst);
        barrier.entered.notify_one();
        barrier
            .release
            .acquire()
            .await
            .expect("fixture entry release")
            .forget();
    }
}

pub(in crate::adaptive_resources::runtime::operations) async fn before_registered_preflight(
    operation: Uuid,
) {
    pause_entry(operation, Stage::RegisteredPreflight).await;
}
