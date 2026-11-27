use super::super::*;
use crate::process::observation::Metric;

fn capacity() -> ResourceCapacity {
    ResourceCapacity {
        cpu_ns_per_second: Metric::Known(100),
        memory_bytes: Metric::Known(100),
        handles: Metric::Known(100),
        kernel_tasks: Metric::Known(100),
        processes: Metric::Known(100),
        hardware_slots: Metric::Known(100),
        tuner_slots: Metric::Known(100),
        scratch_bytes: Metric::Known(100),
    }
}

fn producer(cpu: u64, memory: u64) -> WorkloadDemand {
    WorkloadDemand {
        resources: Metric::Known(ResourceVector {
            cpu_ns_per_second: cpu,
            peak_memory_bytes: memory,
            handles: 1,
            processes: 1,
            ..ResourceVector::default()
        }),
        limit_epoch: 1,
        trial: false,
    }
}

const CLAIM: MemoryClaim = MemoryClaim::UnknownProducerPeak;

/// A spawned, then reaping, producer: the state a closing generation is in
/// when its close receipt arrives.
fn running_predecessor(ledger: &ResourceLedger, cpu: u64) -> CleanupAuthority {
    let family = ledger.new_family().expect("family");
    let roles = [FamilyRoleDemand {
        role: FamilyRole::Video,
        demand: producer(cpu, cpu),
        retained: None,
    }];
    let (mut held, cleanup) = ledger
        .reserve_families_claiming(
            &[FamilyDemand {
                identity: family,
                roles: &roles,
            }],
            Some(&[CLAIM]),
        )
        .expect("predecessor")
        .pop()
        .expect("one role");
    held.mark_spawn_possible().expect("spawned");
    drop(held);
    cleanup.mark_reaping().expect("closing");
    cleanup
}

fn receipt(cleanup: &CleanupAuthority) -> PhysicalCloseReceipt {
    PhysicalCloseReceipt {
        binding: cleanup.binding,
        #[cfg(target_os = "linux")]
        _recovered_r: None,
    }
}

fn rebind(
    ledger: &ResourceLedger,
    predecessor: &CleanupAuthority,
    receipt: &PhysicalCloseReceipt,
    cpu: u64,
) -> Result<(ResourceReservation, CleanupAuthority), AdmissionRefusal> {
    let family = ledger.new_family().expect("successor family");
    let roles = [FamilyRoleDemand {
        role: FamilyRole::Video,
        demand: producer(cpu, cpu),
        retained: None,
    }];
    ledger.rebind_closed_successor(
        predecessor,
        receipt,
        &[FamilyDemand {
            identity: family,
            roles: &roles,
        }],
        CLAIM,
    )
}

fn used_cpu(ledger: &ResourceLedger) -> (u64, usize) {
    let snapshot = ledger.snapshot().expect("snapshot");
    (snapshot.used.cpu_ns_per_second, snapshot.held)
}

#[test]
fn a_no_larger_successor_takes_the_closed_charge_in_one_transaction() {
    let ledger = ResourceLedger::with_capacity(1, capacity()).expect("ledger");
    let old = running_predecessor(&ledger, 60);
    let proof = receipt(&old);
    // A second unknown-peak producer could never be admitted beside it, so
    // only a rebind (not a fresh admission) can succeed here.
    let (successor, cleanup) = rebind(&ledger, &old, &proof, 60).expect("rebind");
    assert_eq!(used_cpu(&ledger), (60, 1), "exactly one live charge");
    assert!(
        old.close(proof).is_err(),
        "the predecessor's charge is gone; its receipt refunds nothing twice"
    );
    assert_eq!(used_cpu(&ledger), (60, 1));
    // A restart before the successor's journal record recovers nothing for
    // it: its still-ordinary reservation drops as a clean refund.
    drop((successor, cleanup));
    assert_eq!(used_cpu(&ledger), (0, 0));
}

#[test]
fn a_larger_successor_is_admitted_for_only_the_difference() {
    let ledger = ResourceLedger::with_capacity(1, capacity()).expect("ledger");
    let old = running_predecessor(&ledger, 60);
    let proof = receipt(&old);
    // 60 + 90 would exceed the cap; 90 alone fits.
    let (successor, _cleanup) = rebind(&ledger, &old, &proof, 90).expect("larger rebind");
    assert_eq!(used_cpu(&ledger), (90, 1));
    drop(successor);
    assert_eq!(used_cpu(&ledger), (0, 0));
}

#[test]
fn a_refused_successor_changes_nothing_and_the_predecessor_refunds_normally() {
    let ledger = ResourceLedger::with_capacity(1, capacity()).expect("ledger");
    let old = running_predecessor(&ledger, 60);
    let proof = receipt(&old);
    assert_eq!(
        rebind(&ledger, &old, &proof, 120).err(),
        Some(AdmissionRefusal::HardLimit(ResourceDimension::Cpu)),
        "a typed capacity refusal"
    );
    assert_eq!(
        used_cpu(&ledger),
        (60, 1),
        "the predecessor keeps its charge"
    );
    old.close(proof).expect("ordinary close");
    assert_eq!(used_cpu(&ledger), (0, 0), "a whole refund");
}

#[test]
fn a_predecessor_already_closed_leaves_a_clean_refund_and_no_successor() {
    // A restart or crash between the close and the rebind: the close
    // refunded the predecessor, and no successor charge appears.
    let ledger = ResourceLedger::with_capacity(1, capacity()).expect("ledger");
    let old = running_predecessor(&ledger, 60);
    let binding = old.binding;
    old.close(receipt(&old)).expect("close first");
    let stale = PhysicalCloseReceipt {
        binding,
        #[cfg(target_os = "linux")]
        _recovered_r: None,
    };
    assert_eq!(
        rebind(&ledger, &old, &stale, 60).err(),
        Some(AdmissionRefusal::UnknownReservation)
    );
    assert_eq!(used_cpu(&ledger), (0, 0));
}

#[test]
fn only_a_spawned_closing_predecessor_with_its_own_receipt_can_be_rebound() {
    let ledger = ResourceLedger::with_capacity(1, capacity()).expect("ledger");
    let family = ledger.new_family().expect("family");
    let roles = [FamilyRoleDemand {
        role: FamilyRole::Video,
        demand: producer(10, 10),
        retained: None,
    }];
    let (never_spawned, cleanup) = ledger
        .reserve_families_claiming(
            &[FamilyDemand {
                identity: family,
                roles: &roles,
            }],
            Some(&[CLAIM]),
        )
        .expect("reserved")
        .pop()
        .expect("one role");
    assert_eq!(
        rebind(&ledger, &cleanup, &receipt(&cleanup), 10).err(),
        Some(AdmissionRefusal::InvalidTransition),
        "a never-spawned reservation is released by its own handle"
    );
    drop(never_spawned);
    let old = running_predecessor(&ledger, 10);
    let other = running_predecessor_in_other_ledger();
    assert_eq!(
        rebind(&ledger, &old, &receipt(&other), 10).err(),
        Some(AdmissionRefusal::ReceiptMismatch)
    );
    assert_eq!(used_cpu(&ledger), (10, 1));
}

fn running_predecessor_in_other_ledger() -> CleanupAuthority {
    let ledger = ResourceLedger::with_capacity(1, capacity()).expect("other ledger");
    running_predecessor(&ledger, 1)
}
