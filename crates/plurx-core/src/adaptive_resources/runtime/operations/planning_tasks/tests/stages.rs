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

struct Barrier {
    entered: Notify,
    probe_joined: Notify,
    release: Semaphore,
    visits: AtomicUsize,
}
fn registry() -> &'static Mutex<BTreeMap<Uuid, Arc<Barrier>>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<Uuid, Arc<Barrier>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}
fn lookup(id: Uuid) -> Option<Arc<Barrier>> {
    registry()
        .lock()
        .expect("fixture registry")
        .get(&id)
        .cloned()
}

pub(in crate::adaptive_resources::runtime::operations) struct Gate {
    id: Uuid,
    barrier: Arc<Barrier>,
}
impl Gate {
    pub(super) fn install(id: Uuid) -> Self {
        let barrier = Arc::new(Barrier {
            entered: Notify::new(),
            probe_joined: Notify::new(),
            release: Semaphore::new(0),
            visits: AtomicUsize::new(0),
        });
        assert!(registry()
            .lock()
            .expect("fixture registry")
            .insert(id, Arc::clone(&barrier))
            .is_none());
        Self { id, barrier }
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
            .remove(&self.id);
        // A failing assertion must not leave the original native job gated.
        self.barrier.release.add_permits(1);
    }
}

pub(in crate::adaptive_resources::runtime::operations) async fn before_engine_native(id: Uuid) {
    if let Some(barrier) = lookup(id) {
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
    if let Some(barrier) = lookup(engine_id) {
        barrier.probe_joined.notify_one();
    }
}
pub(in crate::adaptive_resources::runtime::operations) async fn before_enrichment_ack(
    operation: Uuid,
) {
    if let Some(barrier) = lookup(operation) {
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
