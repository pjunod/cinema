//! Fixture-only delays at real original lifecycle boundaries. No outcome,
//! native owner, operation ID or closure authority is manufactured here.
use super::*;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::adaptive_resources::runtime) enum CopyStartFixtureStage {
    RootRegistered,
    SourceRootObservationQueued,
    RootArmed,
    CleanupAcquisitionsResolved,
    CleanupNativeClosed,
    NeverRootProofReadyToMint,
    NeverRootProofStored,
    CleanupSettlementPrepared,
    TaskArmed,
    TaskRegistered,
    TaskObservationQueued,
    TaskObserved,
    BeforeTaskStartLock,
    OutputWindowFull,
}
struct Delay {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
type GateKey = (u64, CopyStartFixtureStage);
type Gates = BTreeMap<GateKey, Arc<Delay>>;
fn gates() -> &'static Mutex<Gates> {
    static GATES: OnceLock<Mutex<Gates>> = OnceLock::new();
    GATES.get_or_init(|| Mutex::new(BTreeMap::new()))
}
pub(in crate::adaptive_resources::runtime) struct CopyStartFixtureGate {
    key: GateKey,
    delay: Arc<Delay>,
}
impl CopyStartFixtureGate {
    pub(in crate::adaptive_resources::runtime) async fn reached(&self) {
        self.delay.reached.notified().await;
    }
}
impl Drop for CopyStartFixtureGate {
    fn drop(&mut self) {
        gates()
            .lock()
            .expect("copy stage fixture registry")
            .remove(&self.key);
        self.delay.release.notify_one();
    }
}
impl NodeResourceRuntime {
    pub(in crate::adaptive_resources::runtime) fn fixture_pause_copy_start(
        &self,
        stage: CopyStartFixtureStage,
    ) -> CopyStartFixtureGate {
        let key = (self.ledger.id, stage);
        let delay = Arc::new(Delay {
            reached: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        assert!(gates()
            .lock()
            .expect("copy stage fixture registry")
            .insert(key, Arc::clone(&delay))
            .is_none());
        CopyStartFixtureGate { key, delay }
    }
    pub(in crate::adaptive_resources::runtime::operations) async fn fixture_copy_stage(
        &self,
        stage: CopyStartFixtureStage,
    ) {
        let delay = gates()
            .lock()
            .expect("copy stage fixture registry")
            .get(&(self.ledger.id, stage))
            .cloned();
        if let Some(delay) = delay {
            delay.reached.notify_one();
            delay.release.notified().await;
        }
    }
}
