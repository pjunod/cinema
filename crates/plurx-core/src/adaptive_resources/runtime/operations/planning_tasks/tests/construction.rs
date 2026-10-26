//! Original constructor exposure/unwind regressions; compile-only until review.
//! The synchronous fixture gate delays exposure, not native/journal outcomes.
use super::*;
use std::collections::BTreeMap;
use std::sync::{mpsc, Condvar, Mutex, OnceLock};
use std::time::Duration;

struct Checkpoint {
    selected: mpsc::SyncSender<OwnedPreparation>,
    proceed: Mutex<bool>,
    ready: Condvar,
    unwind: bool,
}
fn gates() -> &'static Mutex<BTreeMap<u64, Arc<Checkpoint>>> {
    static GATES: OnceLock<Mutex<BTreeMap<u64, Arc<Checkpoint>>>> = OnceLock::new();
    GATES.get_or_init(|| Mutex::new(BTreeMap::new()))
}
struct Gate {
    ledger: u64,
    checkpoint: Arc<Checkpoint>,
}
impl Gate {
    fn install(
        runtime: &NodeResourceRuntime,
        unwind: bool,
    ) -> (Self, mpsc::Receiver<OwnedPreparation>) {
        let (selected, receiver) = mpsc::sync_channel(1);
        let checkpoint = Arc::new(Checkpoint {
            selected,
            proceed: Mutex::new(false),
            ready: Condvar::new(),
            unwind,
        });
        let ledger = runtime.ledger.id;
        assert!(gates()
            .lock()
            .expect("constructor fixture registry")
            .insert(ledger, Arc::clone(&checkpoint))
            .is_none());
        (Self { ledger, checkpoint }, receiver)
    }
    fn release(&self) {
        *self
            .checkpoint
            .proceed
            .lock()
            .expect("constructor fixture gate") = true;
        self.checkpoint.ready.notify_all();
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        gates()
            .lock()
            .expect("constructor fixture registry")
            .remove(&self.ledger);
        self.release(); // assertion failure cannot strand the original constructor
    }
}
pub(in crate::adaptive_resources::runtime::operations::planning_tasks) fn after_private_install(
    owner: &OwnedPreparation,
) {
    let checkpoint = gates()
        .lock()
        .expect("constructor fixture registry")
        .get(&owner.ledger)
        .cloned();
    let Some(checkpoint) = checkpoint else {
        return;
    };
    checkpoint
        .selected
        .send(owner.clone())
        .expect("original constructor selection");
    let mut proceed = checkpoint.proceed.lock().expect("constructor fixture gate");
    while !*proceed {
        let (next, elapsed) = checkpoint
            .ready
            .wait_timeout(proceed, Duration::from_secs(10))
            .expect("constructor fixture gate wait");
        proceed = next;
        assert!(
            !elapsed.timed_out(),
            "constructor exposure fixture watchdog expired"
        );
    }
    drop(proceed);
    assert!(
        !checkpoint.unwind,
        "fixture unwind after the original private owner exists"
    );
}

fn assert_complete_private_payload(bundle: &Bundle) -> (Uuid, [Uuid; 7]) {
    assert_eq!(bundle.roles.len(), 2);
    assert!(
        !bundle.installed,
        "the actual queued prepare has not been polled"
    );
    let p = &bundle.roles[0];
    let tasks = p
        .planning_tasks
        .as_ref()
        .expect("original pure tasks installed before exposure");
    assert!(tasks.inputs.is_some() && tasks.source_spec.is_some());
    assert!(!tasks.entered && !tasks.unknown);
    assert!(tasks.probe.is_none() && tasks.engine.is_none() && tasks.source.is_none());
    assert!(p.sources.is_empty() && p.roots.is_empty());
    let m = &bundle.roles[1];
    let metadata = m
        .planning_metadata
        .as_ref()
        .expect("original M fence installed before exposure");
    assert!(Arc::ptr_eq(
        m.finite_app
            .as_ref()
            .expect("same actual application fence"),
        &metadata.release.application
    ));
    assert!(!metadata
        .release
        .cleanup_requested
        .load(std::sync::atomic::Ordering::Acquire));
    let ids = [
        tasks.root_observations[0].operation_id,
        tasks.root_observations[1].operation_id,
        tasks.root_observations[2].operation_id,
        tasks.root_observations[3].operation_id,
        tasks.probe_task_observation.operation_id,
        tasks.engine_task_observation.operation_id,
        tasks.settlement_operation_id,
    ];
    assert!(ids.iter().all(|id| *id != Uuid::nil()));
    assert_eq!(
        ids.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        ids.len()
    );
    (bundle.id, ids)
}

#[tokio::test]
async fn published_planning_constructor_returns_same_complete_owner_while_original_bundle_is_locked(
) {
    let root = tempfile::tempdir().expect("original constructor fixture");
    let runtime = fixture_runtime(root.path());
    let baseline = runtime.accounting().expect("actual baseline").used;
    let request = fixture_request(
        root.path(),
        "/never-entered/ffprobe".into(),
        "/never-entered/ffmpeg".into(),
    );
    let (gate, selected) = Gate::install(&runtime, false);
    let constructor = {
        let runtime = Arc::clone(&runtime);
        tokio::task::spawn_blocking(move || {
            runtime.prepare_fixed_planning_probe(request, deadline())
        })
    };
    let original = selected
        .recv_timeout(Duration::from_secs(10))
        .expect("actual constructor reached exposure");
    let bundle = original.state.lock().await;
    let identity = assert_complete_private_payload(&bundle);
    assert_eq!(
        runtime
            .operations
            .lock()
            .expect("actual book")
            .bundles
            .len(),
        1
    );
    gate.release();
    let owner = tokio::time::timeout(Duration::from_secs(10), constructor)
        .await
        .expect("constructor must not re-lock the exposed bundle")
        .expect("constructor thread")
        .expect("same complete original owner despite real lock contention");
    assert_eq!(assert_complete_private_payload(&bundle), identity);
    drop(bundle);
    drop(gate);
    let mut cleanup = owner.into_cleanup();
    drop(original);
    cleanup
        .finish_retirement(deadline())
        .await
        .expect("same original no-task graph and M close");
    assert!(cleanup.known_closed());
    // The completed selector still owns a runtime Arc until it is dropped.
    drop(cleanup);
    assert_eq!(
        runtime.accounting().expect("known original refund").used,
        baseline
    );
    assert!(runtime
        .operations
        .lock()
        .expect("actual book")
        .bundles
        .is_empty());
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole runtime owner")
        .drain()
        .expect("custody shutdown");
}

#[tokio::test]
async fn constructor_unwind_retains_same_original_cleanup_without_native_lookup_or_replacement() {
    let root = tempfile::tempdir().expect("original constructor unwind fixture");
    let runtime = fixture_runtime(root.path());
    let baseline = runtime.accounting().expect("actual baseline").used;
    let request = fixture_request(
        root.path(),
        "/never-entered/ffprobe".into(),
        "/never-entered/ffmpeg".into(),
    );
    let (gate, selected) = Gate::install(&runtime, true);
    let constructor = {
        let runtime = Arc::clone(&runtime);
        tokio::task::spawn_blocking(move || {
            runtime.prepare_fixed_planning_probe(request, deadline())
        })
    };
    let original = selected
        .recv_timeout(Duration::from_secs(10))
        .expect("actual constructor reached exposure");
    let (identity, closed) = {
        let bundle = original.state.lock().await;
        let identity = assert_complete_private_payload(&bundle);
        let closed = Arc::clone(
            &bundle.roles[1]
                .planning_metadata
                .as_ref()
                .expect("original M")
                .release
                .known_closed,
        );
        (identity, closed)
    };
    gate.release();
    match constructor.await {
        Err(error) => assert!(error.is_panic(), "actual constructor unwind"),
        Ok(_) => panic!("injected exposure unwind must not return an owner"),
    }
    assert!(
        !closed.load(std::sync::atomic::Ordering::Acquire),
        "Drop only requests cleanup"
    );
    {
        let bundle = original.state.lock().await;
        let m = bundle.roles[1]
            .planning_metadata
            .as_ref()
            .expect("same original M");
        assert!(m
            .release
            .cleanup_requested
            .load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(bundle.id, identity.0);
        assert_eq!(
            bundle.roles[0]
                .planning_tasks
                .as_ref()
                .expect("same pure payload")
                .settlement_operation_id,
            identity.1[6]
        );
    }
    drop(gate);
    drop(original);
    let until = deadline();
    while !closed.load(std::sync::atomic::Ordering::Acquire) {
        assert!(
            Instant::now() < until,
            "original maintenance did not close the retained constructor"
        );
        runtime.maintain_planning_cleanup(until).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(
        runtime.accounting().expect("original known close").used,
        baseline
    );
    assert!(runtime
        .operations
        .lock()
        .expect("actual book")
        .bundles
        .is_empty());
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole runtime owner")
        .drain()
        .expect("custody shutdown");
}

#[tokio::test]
async fn original_constructor_capacity_refusal_keeps_exact_before_reservation_provenance() {
    let root = tempfile::tempdir().expect("original constructor refusal fixture");
    let runtime = fixture_runtime(root.path());
    let mut capacity = runtime
        .accounting()
        .expect("actual capacity fixture")
        .capacity;
    capacity.cpu_ns_per_second = Metric::Known(0);
    runtime
        .ledger
        .update_capacity(2, capacity)
        .expect("deny actual ledger CPU admission");
    let baseline = runtime.accounting().expect("actual baseline").used;
    let request = fixture_request(
        root.path(),
        "/never-entered/ffprobe".into(),
        "/never-entered/ffmpeg".into(),
    );
    let error = match runtime.prepare_fixed_planning_probe(request, deadline()) {
        Err(error) => error,
        Ok(_) => panic!("zero hard CPU must refuse before any P/M reservation"),
    };
    assert!(matches!(error.0, OperationError::BeforeReservation(_)));
    assert_eq!(runtime.accounting().expect("actual refusal").used, baseline);
    assert!(runtime
        .operations
        .lock()
        .expect("actual book")
        .bundles
        .is_empty());
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole runtime owner")
        .drain()
        .expect("custody shutdown");
}
