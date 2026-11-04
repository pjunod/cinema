use super::*;
use crate::adaptive_resources::{ResourceVector, WorkloadDemand};
use crate::process::observation::Metric;
use std::sync::mpsc;
use std::time::Duration;

fn complete_fixture(inventory: &OwnerInventory) {
    inventory
        .install_audited_coverage(AuditedAdapterCoverage {
            families: OwnerFamilies::ALL_REQUIRED,
        })
        .expect("fixture audited adapters");
}

fn binding(reservation: u64) -> ReservationBinding {
    ReservationBinding {
        ledger: 1,
        reservation,
        allocation_epoch: 1,
    }
}

// Test-only proof construction deliberately does not claim native execution.
fn physical_fixture(binding: ReservationBinding) -> PhysicalCloseReceipt {
    PhysicalCloseReceipt {
        binding,
        #[cfg(target_os = "linux")]
        _recovered_r: None,
    }
}

fn ledger_fixture() -> ResourceLedger {
    ResourceLedger::new(
        1,
        Metric::Known(ResourceVector {
            handles: 1024,
            ..ResourceVector::default()
        }),
    )
    .expect("ledger fixture")
}

fn demand() -> WorkloadDemand {
    WorkloadDemand {
        resources: Metric::Known(ResourceVector {
            handles: 1,
            ..ResourceVector::default()
        }),
        limit_epoch: 1,
        trial: false,
    }
}

#[test]
fn closing_owner_survives_serving_reference_removal_and_unknown_held_is_sticky() {
    let inventory = OwnerInventory::new();
    complete_fixture(&inventory);
    let mut producer = inventory
        .register_runtime(binding(1), OwnerFamily::CaptureSink, OwnerKind::Producer)
        .expect("original capture");
    producer.observe_phase(OwnerPhase::Active).expect("entry");
    producer
        .observe_phase(OwnerPhase::Closing)
        .expect("closing after serving removal");
    let snapshot = inventory.snapshot().expect("snapshot");
    assert_eq!(snapshot.records().count(), 1);
    assert_eq!(
        snapshot
            .records()
            .next()
            .expect("hidden closing owner")
            .phase,
        OwnerPhase::Closing
    );
    producer
        .observe_phase(OwnerPhase::UnknownHeld)
        .expect("uncertain fsync/join");
    assert_eq!(
        producer.observe_phase(OwnerPhase::Active),
        Err(InventoryError::InvalidPhase)
    );
    drop(producer);
    let snapshot = inventory.snapshot().expect("held snapshot");
    assert_eq!(snapshot.records().count(), 1);
    assert!(snapshot.completeness().unknown_held);
    assert_eq!(
        inventory.with_current_complete_snapshot(&snapshot, |_| ()),
        Err(InventoryError::Incomplete)
    );
}

#[test]
fn dropped_original_owner_is_never_observed_as_absent() {
    let inventory = OwnerInventory::new();
    let owner = inventory
        .register_original(
            Uuid::from_u128(1),
            OwnerFamily::SourceReader,
            OwnerKind::Sink,
        )
        .expect("original unbuffered response");
    drop(owner);
    let snapshot = inventory.snapshot().expect("snapshot");
    assert_eq!(snapshot.records().count(), 1);
    assert_eq!(
        snapshot.records().next().expect("original record").phase,
        OwnerPhase::UnknownHeld
    );
}

#[test]
fn unbridged_family_refuses_even_when_every_observed_registry_is_empty() {
    let inventory = OwnerInventory::new();
    inventory
        .install_audited_coverage(AuditedAdapterCoverage {
            families: OwnerFamilies(
                OwnerFamilies::ALL_REQUIRED.0 & !(1 << OwnerFamily::CaptureSink as u8),
            ),
        })
        .expect("all except captures fixture");
    let snapshot = inventory.snapshot().expect("snapshot");
    assert_eq!(snapshot.records().count(), 0);
    assert!(snapshot
        .completeness()
        .unbridged
        .contains(OwnerFamily::CaptureSink));
    let mut called = false;
    assert_eq!(
        inventory.with_current_complete_snapshot(&snapshot, |_| called = true),
        Err(InventoryError::Incomplete)
    );
    assert!(!called);
}

#[test]
fn overflow_refuses_without_evicting_and_remains_unknown_after_a_known_close() {
    let inventory = OwnerInventory::new();
    complete_fixture(&inventory);
    let ledger = ledger_fixture();
    let (reservation, _cleanup) = ledger.reserve(demand()).expect("original reservation");
    let first_binding = reservation.binding;
    let mut first = inventory
        .register_runtime(first_binding, OwnerFamily::BufferedVod, OwnerKind::Producer)
        .expect("first");
    let _others: Vec<_> = (1..MAX_INVENTORY_OWNERS)
        .map(|id| {
            inventory
                .register_original(
                    Uuid::from_u128(id as u128),
                    OwnerFamily::SourceReader,
                    OwnerKind::Sink,
                )
                .expect("fixed slot")
        })
        .collect();
    assert!(matches!(
        inventory.register_original(
            Uuid::from_u128(1000),
            OwnerFamily::CaptureSink,
            OwnerKind::Sink
        ),
        Err(InventoryError::Full)
    ));
    assert_eq!(
        inventory
            .snapshot()
            .expect("full snapshot")
            .records()
            .count(),
        MAX_INVENTORY_OWNERS
    );
    drop(reservation); // Exact fixture reservation never entered native work.
    first
        .close_after_finalized_receipt(&physical_fixture(first_binding), &ledger)
        .expect("known fixture close");
    let snapshot = inventory.snapshot().expect("after close");
    assert_eq!(snapshot.records().count(), MAX_INVENTORY_OWNERS - 1);
    assert!(snapshot.completeness().overflowed);
    assert_eq!(
        inventory.with_current_complete_snapshot(&snapshot, |_| ()),
        Err(InventoryError::Incomplete)
    );
}

#[test]
fn owner_mutation_and_reservation_attempt_invalidate_prepared_snapshots() {
    let inventory = OwnerInventory::new();
    complete_fixture(&inventory);
    let before = inventory.snapshot().expect("before");
    let mut owner = inventory
        .register_runtime(binding(1), OwnerFamily::BufferedVod, OwnerKind::Producer)
        .expect("owner");
    assert_eq!(
        inventory.with_current_complete_snapshot(&before, |_| ()),
        Err(InventoryError::StaleSnapshot)
    );
    let before_phase = inventory.snapshot().expect("before phase");
    owner
        .observe_phase(OwnerPhase::Active)
        .expect("native entry");
    assert_eq!(
        inventory.with_current_complete_snapshot(&before_phase, |_| ()),
        Err(InventoryError::StaleSnapshot)
    );
    let current = inventory.snapshot().expect("current");
    assert_eq!(
        inventory.with_current_complete_snapshot(&current, |_| "ledger refusal"),
        Ok("ledger refusal")
    );
    assert_eq!(
        inventory.with_current_complete_snapshot(&current, |_| ()),
        Err(InventoryError::StaleSnapshot)
    );
}

#[test]
fn snapshot_from_another_inventory_and_exhausted_epochs_cannot_authorize_reservation() {
    let inventory = OwnerInventory::new();
    let other = OwnerInventory::new();
    complete_fixture(&inventory);
    complete_fixture(&other);
    let foreign = other.snapshot().expect("foreign snapshot");
    assert_eq!(
        inventory.with_current_complete_snapshot(&foreign, |_| ()),
        Err(InventoryError::WrongInventory)
    );
    inventory.state.lock().expect("fixture state").epoch = u64::MAX;
    let last = inventory.snapshot().expect("last epoch snapshot");
    let owner = inventory
        .register_runtime(binding(1), OwnerFamily::NativeCleanup, OwnerKind::Residency)
        .expect("held owner despite exhausted epoch");
    let exhausted = inventory.snapshot().expect("exhausted snapshot");
    assert!(exhausted.completeness().epoch_exhausted);
    assert_eq!(
        inventory.with_current_complete_snapshot(&last, |_| ()),
        Err(InventoryError::Incomplete)
    );
    drop(owner);
}

#[test]
fn duplicate_producer_is_rejected_and_shared_viewers_remain_distinct() {
    let inventory = OwnerInventory::new();
    let producer = inventory
        .register_runtime(binding(1), OwnerFamily::BufferedVod, OwnerKind::Producer)
        .expect("producer");
    assert!(matches!(
        inventory.register_runtime(binding(1), OwnerFamily::BufferedVod, OwnerKind::Producer),
        Err(InventoryError::DuplicateIdentity)
    ));
    let _viewer_a = inventory
        .register_reference(&producer, Uuid::from_u128(1), OwnerFamily::BufferedVod)
        .expect("first viewer");
    let _viewer_b = inventory
        .register_reference(&producer, Uuid::from_u128(2), OwnerFamily::BufferedVod)
        .expect("second viewer");
    assert!(matches!(
        inventory.register_reference(&producer, Uuid::from_u128(1), OwnerFamily::BufferedVod),
        Err(InventoryError::DuplicateIdentity)
    ));
    let snapshot = inventory.snapshot().expect("shared snapshot");
    assert_eq!(
        snapshot
            .records()
            .filter(|record| record.kind == OwnerKind::Producer)
            .count(),
        1
    );
    assert_eq!(
        snapshot
            .records()
            .filter(|record| matches!(record.kind, OwnerKind::Reference { .. }))
            .count(),
        2
    );
}

#[test]
fn physical_proof_requires_matching_finalized_ledger_and_preserves_owner_on_failure() {
    let inventory = OwnerInventory::new();
    let ledger = ledger_fixture();
    let (mut reservation, cleanup) = ledger.reserve(demand()).expect("reservation");
    let original = reservation.binding;
    let mut owner = inventory
        .register_runtime(original, OwnerFamily::BufferedVod, OwnerKind::Producer)
        .expect("owner");
    let mut wrong = original;
    wrong.allocation_epoch += 1;
    assert_eq!(
        owner.close_after_finalized_receipt(&physical_fixture(wrong), &ledger),
        Err(InventoryError::ReceiptMismatch)
    );
    let receipt = physical_fixture(original);
    assert_eq!(
        owner.close_after_finalized_receipt(&receipt, &ledger),
        Err(InventoryError::NotFinalized)
    );
    reservation.mark_spawn_possible().expect("fixture entry");
    cleanup
        .close_unpersisted(&receipt)
        .expect("original fixture positive finalization");
    owner
        .close_after_finalized_receipt(&receipt, &ledger)
        .expect("same original finalized receipt");
    assert_eq!(
        inventory
            .snapshot()
            .expect("closed snapshot")
            .records()
            .count(),
        0
    );
    drop(owner);
    assert_eq!(
        inventory
            .snapshot()
            .expect("drop cannot resurrect")
            .records()
            .count(),
        0
    );
}

#[test]
fn original_producer_cannot_close_ahead_of_its_separate_viewer_references() {
    let inventory = OwnerInventory::new();
    let ledger = ledger_fixture();
    let (reservation, _cleanup) = ledger.reserve(demand()).expect("reservation");
    let binding = reservation.binding;
    let mut owner = inventory
        .register_runtime(binding, OwnerFamily::BufferedVod, OwnerKind::Producer)
        .expect("owner");
    let mut viewer = inventory
        .register_reference(&owner, Uuid::from_u128(1), OwnerFamily::BufferedVod)
        .expect("viewer");
    drop(reservation);
    let receipt = physical_fixture(binding);
    assert_eq!(
        owner.close_after_finalized_receipt(&receipt, &ledger),
        Err(InventoryError::ReferencesRemain)
    );
    assert_eq!(
        viewer.close_after_finalized_receipt(&receipt, &ledger),
        Err(InventoryError::ReceiptMismatch)
    );
    assert_eq!(
        owner.close_after_finalized_receipt(&receipt, &ledger),
        Err(InventoryError::ReferencesRemain)
    );
    assert_eq!(
        inventory
            .snapshot()
            .expect("separate consumer still held")
            .records()
            .count(),
        2
    );
}

#[test]
fn reservation_and_original_registration_share_the_census_mutation_lock() {
    let inventory = OwnerInventory::new();
    complete_fixture(&inventory);
    let ledger = ledger_fixture();
    let snapshot = inventory.snapshot().expect("snapshot");
    let competing = inventory.clone();
    let (start, started) = mpsc::channel();
    let (attempting, attempted) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        started.recv().expect("start competing registration");
        attempting.send(()).expect("attempting registration signal");
        let owner = competing
            .register_original(
                Uuid::from_u128(7),
                OwnerFamily::SourceReader,
                OwnerKind::Sink,
            )
            .expect("competing original");
        done.send(()).expect("completed signal");
        owner
    });
    let original = inventory
        .with_current_complete_snapshot(&snapshot, |admission| {
            start.send(()).expect("start worker");
            attempted
                .recv_timeout(Duration::from_secs(5))
                .expect("competitor reaches registration");
            assert_eq!(
                completed.recv_timeout(Duration::from_millis(25)),
                Err(mpsc::RecvTimeoutError::Timeout)
            );
            assert!(matches!(
                inventory.state.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            let (reservation, cleanup) = ledger
                .reserve(demand())
                .expect("actual ledger reservation inside census lock");
            let owner = admission
                .register_runtime(
                    reservation.binding,
                    OwnerFamily::BufferedVod,
                    OwnerKind::Producer,
                )
                .expect("same transaction original registration");
            (reservation, cleanup, owner)
        })
        .expect("atomic admission");
    completed
        .recv_timeout(Duration::from_secs(5))
        .expect("competitor resumes after transaction");
    let competitor = worker.join().expect("competitor joined");
    assert_eq!(
        inventory
            .snapshot()
            .expect("both originals")
            .records()
            .count(),
        2
    );
    drop((original, competitor));
}

#[test]
fn token_drop_inside_actual_admission_callback_is_nonblocking_and_retains_unknown_owner() {
    let (finished, received) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let inventory = OwnerInventory::new();
        complete_fixture(&inventory);
        let ledger = ledger_fixture();
        let snapshot = inventory.snapshot().expect("prepared census");
        let mut original_reservation = None;
        let result = inventory.with_current_complete_snapshot(&snapshot, |admission| {
            let (reservation, cleanup) = ledger.reserve(demand()).expect("actual original reserve");
            let owner = admission
                .register_runtime(
                    reservation.binding,
                    OwnerFamily::BufferedVod,
                    OwnerKind::Producer,
                )
                .expect("same-lock owner registration");
            original_reservation = Some((reservation, cleanup));
            // Error cleanup runs before the admission mutex is released.
            drop(owner);
            Err::<(), _>("failure after original registration")
        });
        assert_eq!(result, Err(InventoryError::Incomplete));
        let after = inventory.snapshot().expect("post-callback census");
        assert_eq!(after.records().count(), 1);
        assert_eq!(
            after.records().next().expect("original retained").phase,
            OwnerPhase::UnknownHeld
        );
        assert!(after.completeness().unknown_held);
        assert_eq!(
            inventory.with_current_complete_snapshot(&snapshot, |_| ()),
            Err(InventoryError::Incomplete)
        );
        finished.send(()).expect("finite callback completion");
        drop(original_reservation);
    });
    received
        .recv_timeout(Duration::from_secs(5))
        .expect("Drop must not deadlock under the admission mutex");
    worker.join().expect("drop regression joined");
}

#[test]
fn unwinding_callback_token_drop_neither_relocks_nor_double_panics() {
    let (finished, received) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let inventory = OwnerInventory::new();
        complete_fixture(&inventory);
        let ledger = ledger_fixture();
        let snapshot = inventory.snapshot().expect("prepared census");
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = inventory.with_current_complete_snapshot::<()>(&snapshot, |admission| {
                let (reservation, _cleanup) =
                    ledger.reserve(demand()).expect("actual original reserve");
                let _original_owner = admission
                    .register_runtime(
                        reservation.binding,
                        OwnerFamily::BufferedVod,
                        OwnerKind::Producer,
                    )
                    .expect("same-lock original");
                panic!("injected admission callback unwind");
            });
        }));
        assert!(unwind.is_err());
        assert!(matches!(
            inventory.snapshot(),
            Err(InventoryError::Unavailable)
        ));
        // Poison is never cleared by production. Inspect the retained original
        // and reconcile its marker solely to verify the unwind custody fact.
        let mut state = inventory
            .state
            .locked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inventory.state.reconcile(&mut state);
        assert_eq!(state.records.iter().flatten().count(), 1);
        assert_eq!(
            state
                .records
                .iter()
                .flatten()
                .next()
                .expect("original remains")
                .phase,
            OwnerPhase::UnknownHeld
        );
        finished.send(()).expect("finite unwind completion");
    });
    received
        .recv_timeout(Duration::from_secs(5))
        .expect("unwind Drop must not deadlock or double panic");
    worker.join().expect("unwind regression joined");
}

#[test]
fn disarmed_original_drop_cannot_mark_a_reused_slot_unknown() {
    let inventory = OwnerInventory::new();
    complete_fixture(&inventory);
    let ledger = ledger_fixture();
    let (reservation, _cleanup) = ledger.reserve(demand()).expect("original reserve");
    let binding = reservation.binding;
    let mut owner = inventory
        .register_runtime(binding, OwnerFamily::BufferedVod, OwnerKind::Producer)
        .expect("original owner");
    drop(reservation);
    owner
        .close_after_finalized_receipt(&physical_fixture(binding), &ledger)
        .expect("same original finalized close");
    let replacement = inventory
        .register_original(
            Uuid::from_u128(1),
            OwnerFamily::SourceReader,
            OwnerKind::Sink,
        )
        .expect("reused fixed slot");
    assert_eq!(owner.slot, replacement.slot);
    drop(owner);
    let after = inventory.snapshot().expect("reused slot census");
    assert!(after.completeness().is_complete());
    assert_eq!(
        after.records().next().expect("replacement").phase,
        OwnerPhase::Registered
    );
    drop(replacement);
}

#[test]
fn rejected_admission_result_is_destroyed_after_releasing_the_census_mutex() {
    struct InspectOnDrop {
        inventory: OwnerInventory,
        finished: mpsc::Sender<()>,
    }
    impl Drop for InspectOnDrop {
        fn drop(&mut self) {
            let snapshot = self
                .inventory
                .snapshot()
                .expect("result destructor can inspect census");
            assert!(snapshot.completeness().unknown_held);
            self.finished.send(()).expect("result destructor completed");
        }
    }
    let (finished, received) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let inventory = OwnerInventory::new();
        complete_fixture(&inventory);
        let ledger = ledger_fixture();
        let snapshot = inventory.snapshot().expect("prepared census");
        let mut original_reservation = None;
        let result = inventory.with_current_complete_snapshot(&snapshot, |admission| {
            let (reservation, cleanup) = ledger.reserve(demand()).expect("actual original reserve");
            let owner = admission
                .register_runtime(
                    reservation.binding,
                    OwnerFamily::BufferedVod,
                    OwnerKind::Producer,
                )
                .expect("same-lock original");
            original_reservation = Some((reservation, cleanup));
            drop(owner);
            InspectOnDrop {
                inventory: inventory.clone(),
                finished,
            }
        });
        assert!(matches!(result, Err(InventoryError::Incomplete)));
        drop(original_reservation);
    });
    received
        .recv_timeout(Duration::from_secs(5))
        .expect("rejected result destructor must not run under census mutex");
    worker.join().expect("result drop regression joined");
}
