use super::*;
use crate::adaptive_resources::{
    FamilyDemand, FamilyRole, FamilyRoleDemand, ResourceVector, WorkloadDemand,
};

const TEST_WORK: ChildWork = ChildWork::background("physical graph custody fixture");

fn vector(value: u64) -> ResourceVector {
    ResourceVector {
        cpu_ns_per_second: value,
        peak_memory_bytes: value,
        handles: value,
        processes: value,
        hardware_slots: value,
        tuner_slots: value,
        scratch_bytes: value,
    }
}

fn ledger() -> ResourceLedger {
    ResourceLedger::new(1, Metric::Known(vector(100))).expect("bounded ledger")
}

fn reservation(ledger: &ResourceLedger, trial: bool) -> ResourceReservation {
    ledger
        .reserve(WorkloadDemand {
            resources: Metric::Known(vector(1)),
            limit_epoch: 1,
            trial,
        })
        .expect("reservation")
        .0
}

fn graph(ledger: &ResourceLedger) -> OwnedPhysicalGraph {
    reservation(ledger, false)
        .into_physical_graph()
        .expect("move exclusive custody")
}

#[tokio::test]
async fn graph_owned_role_borrow_is_counted_once_and_sealed_cleanup_cannot_share() {
    let ledger = ledger();
    let family = ledger.new_family().expect("owner family");
    let successor = ledger.new_family().expect("successor family");
    let demand = WorkloadDemand {
        resources: Metric::Known(vector(1)),
        limit_epoch: 1,
        trial: false,
    };
    let roles = [FamilyRoleDemand {
        role: FamilyRole::Audio,
        demand,
        retained: None,
    }];
    let mut owned = ledger
        .reserve_families(&[FamilyDemand {
            identity: family,
            roles: &roles,
        }])
        .expect("family reservation");
    let (reservation, cleanup) = owned.pop().expect("owned role");
    let mut graph = reservation.into_physical_graph().expect("exclusive graph");
    graph
        .spawn_root(&mut child("sleep"), TEST_WORK)
        .await
        .expect("actual root");
    let roles = [FamilyRoleDemand {
        role: FamilyRole::Audio,
        demand,
        retained: Some(graph.retained_role()),
    }];
    assert!(ledger
        .reserve_families(&[FamilyDemand {
            identity: successor,
            roles: &roles
        }])
        .expect("same retained physical role")
        .is_empty());
    assert_eq!(ledger.snapshot().expect("counted once").used, vector(1));
    cleanup.mark_reaping().expect("seal via cleanup");
    assert!(ledger
        .reserve_families(&[FamilyDemand {
            identity: successor,
            roles: &roles
        }])
        .is_err());
    let sealed = graph.seal();
    assert_eq!(
        sealed.close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
    let roles = [FamilyRoleDemand {
        role: FamilyRole::Audio,
        demand,
        retained: Some(sealed.retained_role()),
    }];
    assert!(ledger
        .reserve_families(&[FamilyDemand {
            identity: successor,
            roles: &roles
        }])
        .is_err());
    assert_eq!(ledger.snapshot().expect("not refunded").held, 1);
}

#[tokio::test]
async fn root_control_rejects_cross_graph_and_waited_root_signal_without_refund() {
    let ledger = ledger();
    let mut first = graph(&ledger);
    let mut second = graph(&ledger);
    let root = first
        .spawn_root(&mut child("sleep"), TEST_WORK)
        .await
        .expect("root");
    let other = second
        .spawn_root(&mut child("sleep"), TEST_WORK)
        .await
        .expect("other root");
    let sealed = first.seal();
    assert!(sealed.start_kill_root(other).await.is_err());
    assert!(sealed.try_wait_root(other).await.is_err());
    assert!(sealed.wait_root(other).await.is_err());
    sealed.start_kill_root(root).await.expect("kill owned root");
    let status = sealed.wait_root(root).await.expect("wait owned root");
    assert_eq!(
        sealed.try_wait_root(root).await.expect("cached status"),
        Some(status)
    );
    assert!(sealed
        .signal_root(root, crate::process::ProcessSignal::Resume)
        .await
        .is_err());
    assert_eq!(ledger.snapshot().expect("still charged").held, 2);
    sealed.close().await;
    second.seal().close().await;
    assert_eq!(
        ledger
            .snapshot()
            .expect("physical uncertainty")
            .unknown_held,
        2
    );
}

#[test]
fn retained_graph_role_rechecks_epoch_and_invalid_ledger_even_without_new_roles() {
    let ledger = ledger();
    let family = ledger.new_family().expect("family");
    let demand = WorkloadDemand {
        resources: Metric::Known(vector(1)),
        limit_epoch: 1,
        trial: false,
    };
    let roles = [FamilyRoleDemand {
        role: FamilyRole::Audio,
        demand,
        retained: None,
    }];
    let (reservation, _) = ledger
        .reserve_families(&[FamilyDemand {
            identity: family,
            roles: &roles,
        }])
        .expect("owned role")
        .pop()
        .expect("one role");
    let graph = reservation.into_physical_graph().expect("graph owner");
    let roles = [FamilyRoleDemand {
        role: FamilyRole::Audio,
        demand,
        retained: Some(graph.retained_role()),
    }];
    ledger
        .update_hard_cap(2, Metric::Known(vector(100)))
        .expect("new epoch");
    assert!(matches!(
        ledger.reserve_families(&[FamilyDemand {
            identity: family,
            roles: &roles
        }]),
        Err(AdmissionRefusal::StaleEpoch)
    ));
    let roles = [FamilyRoleDemand {
        role: FamilyRole::Audio,
        demand: WorkloadDemand {
            limit_epoch: 2,
            ..demand
        },
        retained: Some(graph.retained_role()),
    }];
    assert!(ledger
        .reserve_families(&[FamilyDemand {
            identity: family,
            roles: &roles
        }])
        .expect("fresh view")
        .is_empty());
    ledger
        .inner
        .lock()
        .expect("simulate invalid accounting")
        .invalid = true;
    assert!(matches!(
        ledger.reserve_families(&[FamilyDemand {
            identity: family,
            roles: &roles
        }]),
        Err(AdmissionRefusal::Unavailable)
    ));
    assert_eq!(ledger.snapshot().expect("still counted").held, 1);
}

// The existing portable process fixture re-execs the actual test binary. No
// shell, production helper, display/device or platform provisioning is needed.
fn child(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", "process::tests::child_main", "--nocapture"])
        .env("PLURX_CHILD_MODE", mode)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

#[tokio::test]
async fn no_acquisition_drop_and_close_release_only_their_own_reservation() {
    let ledger = ledger();
    let first = graph(&ledger);
    let other = graph(&ledger);
    let first_identity = first.identity();
    assert_ne!(first_identity.generation, other.identity().generation);
    assert_eq!(
        first_identity.process_instance,
        other.identity().process_instance
    );
    assert_eq!(first_identity.os_boot, other.identity().os_boot);
    drop(first);
    assert_eq!(ledger.snapshot().expect("snapshot").held, 1);
    let sealed = other.seal();
    assert_eq!(sealed.close().await, GraphCloseOutcome::NoAcquisition);
    assert_eq!(sealed.close().await, GraphCloseOutcome::NoAcquisition);
    assert_eq!(ledger.snapshot().expect("snapshot").held, 0);
    assert!(ledger
        .retained_physical_graphs()
        .expect("retained custody")
        .is_empty());
}

#[test]
fn unsupported_containment_refuses_trial_before_launch_and_clears_no_spawn_latch() {
    let ledger = ledger();
    assert!(matches!(
        reservation(&ledger, true).into_physical_graph(),
        Err(AdmissionRefusal::InsufficientEvidence)
    ));
    let snapshot = ledger.snapshot().expect("snapshot");
    assert_eq!(snapshot.held, 0);
    assert!(!snapshot.trial_held);
}

#[test]
fn external_spawn_possible_reservation_cannot_be_retroactively_attached() {
    let ledger = ledger();
    let mut reservation = reservation(&ledger, false);
    reservation
        .mark_spawn_possible()
        .expect("external launch possible");
    assert!(matches!(
        reservation.into_physical_graph(),
        Err(AdmissionRefusal::InvalidTransition)
    ));
    let snapshot = ledger.snapshot().expect("snapshot");
    assert_eq!(snapshot.used, vector(1));
    assert_eq!(snapshot.unknown_held, 1);
    assert!(ledger
        .retained_physical_graphs()
        .expect("no retroactive custody")
        .is_empty());
}

#[tokio::test]
async fn failed_launch_is_not_no_spawn_proof_and_retains_exact_custody() {
    let ledger = ledger();
    let mut graph = graph(&ledger);
    let identity = graph.identity();
    let program = std::env::current_exe()
        .expect("test executable")
        .with_file_name(format!("missing-physical-graph-helper-{}", Uuid::new_v4()));
    assert!(graph
        .spawn_root(&mut Command::new(program), TEST_WORK)
        .await
        .is_err());
    assert!(graph
        .spawn_root(&mut child("sleep"), TEST_WORK)
        .await
        .is_err());
    drop(graph);
    assert_eq!(ledger.snapshot().expect("snapshot").unknown_held, 1);
    let mut retained = ledger.retained_physical_graphs().expect("retained owner");
    assert_eq!(retained.len(), 1);
    let retained = retained.pop().expect("same graph");
    let snapshot = retained.snapshot().await;
    assert_eq!(snapshot.identity, identity);
    assert!(snapshot.acquired && snapshot.sealed);
    assert_eq!(snapshot.roots, 0);
    assert_eq!(
        retained.close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
    assert_eq!(ledger.snapshot().expect("still charged").used, vector(1));
}

#[tokio::test]
async fn root_budget_refuses_before_spawn_and_cleanup_authority_seals_acquisition() {
    let ledger = ledger();
    let mut resources = vector(1);
    resources.processes = 0;
    let (reservation, _) = ledger
        .reserve(WorkloadDemand {
            resources: Metric::Known(resources),
            limit_epoch: 1,
            trial: false,
        })
        .expect("nonprocess bound");
    let mut nonprocess = reservation.into_physical_graph().expect("custody");
    assert!(nonprocess
        .spawn_root(&mut child("sleep"), TEST_WORK)
        .await
        .is_err());
    assert_eq!(
        nonprocess.seal().close().await,
        GraphCloseOutcome::NoAcquisition
    );

    let (reservation, cleanup) = ledger
        .reserve(WorkloadDemand {
            resources: Metric::Known(vector(1)),
            limit_epoch: 1,
            trial: false,
        })
        .expect("one-root bound");
    let mut graph = reservation.into_physical_graph().expect("custody");
    let root = graph
        .spawn_root(&mut child("sleep"), TEST_WORK)
        .await
        .expect("one actual root");
    assert!(graph
        .spawn_root(&mut child("sleep"), TEST_WORK)
        .await
        .is_err());
    cleanup
        .mark_reaping()
        .expect("existing cleanup authority seals graph");
    assert!(graph.spawn_root_writer(root, |_| async {}).await.is_err());
    let sealed = graph.seal();
    assert_eq!(
        sealed.close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
    assert_eq!(sealed.snapshot().await.roots, 1);
    assert_eq!(ledger.snapshot().expect("retained charge").used, vector(1));
}

#[tokio::test]
async fn root_wait_and_real_writer_join_do_not_prove_descendant_extinction() {
    let ledger = ledger();
    let mut graph = graph(&ledger);
    let root = graph
        .spawn_root(&mut child("sleep"), TEST_WORK)
        .await
        .expect("actual root");
    graph
        .spawn_root_writer(root, |pipes| async move {
            drop(pipes);
        })
        .await
        .expect("owned task");
    let sealed = graph.seal();
    assert_eq!(
        sealed.close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
    let snapshot = sealed.snapshot().await;
    assert_eq!(snapshot.roots, 1);
    assert_eq!(snapshot.waited_roots, 1);
    assert_eq!(snapshot.joined_writers, 1);
    assert_eq!(snapshot.abandoned_writers, 0);
    assert_eq!(ledger.snapshot().expect("held").unknown_held, 1);
    assert_eq!(ledger.snapshot().expect("held").used, vector(1));
}

#[tokio::test]
async fn partial_launch_failure_retains_the_actual_child_and_optional_job_for_reap() {
    // Fault the internal post-spawn handoff with a REAL launcher-owned child.
    // This is not Windows attach/resume qualification or an injected success
    // receipt; it checks both partial-error ownership shapes on this host.
    for retain_job in [false, true] {
        let ledger = ledger();
        let graph = graph(&ledger);
        {
            let custody = graph.custody();
            let mut members = custody.members.lock().await;
            begin_acquisition(&ledger, custody, &mut members).expect("before launcher");
            let (child, job) = crate::process::spawn_job_owned(&mut child("sleep"), TEST_WORK)
                .expect("actual child and job");
            let job = if retain_job {
                Some(job)
            } else {
                drop(job);
                None
            };
            let error = retain_launch_failure(
                custody,
                &mut members,
                OwnedSpawnFailure {
                    error: io::Error::other("deliberate post-spawn handoff failure"),
                    child: Some(child),
                    job,
                },
            );
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert_eq!(members.roots.len(), 1);
            assert_eq!(members.roots[0]._job.is_some(), retain_job);
        }
        drop(graph);
        let mut retained = ledger
            .retained_physical_graphs()
            .expect("retained partial launch");
        let retained = retained.pop().expect("same actual owner");
        assert_eq!(
            retained.close().await,
            GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
        );
        assert_eq!(retained.snapshot().await.waited_roots, 1);
        assert_eq!(ledger.snapshot().expect("still held").used, vector(1));
    }
}

#[tokio::test]
async fn dropped_completion_sender_does_not_substitute_for_join_and_cancel_keeps_owner() {
    let ledger = ledger();
    let mut graph = graph(&ledger);
    let root = graph
        .spawn_root(&mut child("sleep"), TEST_WORK)
        .await
        .expect("actual root");
    let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
    let finish = Arc::new(tokio::sync::Notify::new());
    let writer_finish = Arc::clone(&finish);
    graph
        .spawn_root_writer(root, move |pipes| async move {
            // A sender can be dropped long before the task and its handles end.
            drop(sender);
            writer_finish.notified().await;
            drop(pipes);
        })
        .await
        .expect("owned writer");
    assert!(receiver.await.is_err());
    let sealed = graph.seal();
    let identity = sealed.snapshot().await.identity;
    {
        let close = sealed.close();
        tokio::pin!(close);
        // Poll real closure once, then cancel its waiter, without a sleep or a
        // fabricated child-wait result. The writer is provably still blocked.
        std::future::poll_fn(|cx| {
            assert!(close.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }
    drop(sealed);
    let mut retained = ledger
        .retained_physical_graphs()
        .expect("retained after cancellation");
    assert_eq!(retained.len(), 1);
    let retained = retained.pop().expect("retained graph");
    let snapshot = retained.snapshot().await;
    assert_eq!(snapshot.identity, identity);
    assert_eq!(snapshot.writers, 1);
    assert_eq!(snapshot.joined_writers, 0);
    assert_eq!(ledger.snapshot().expect("still held").used, vector(1));
    finish.notify_one();
    assert_eq!(
        retained.close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
    assert_eq!(retained.snapshot().await.joined_writers, 1);
}

#[tokio::test]
async fn task_panic_is_joined_abandonment_not_physical_release() {
    let ledger = ledger();
    let mut graph = graph(&ledger);
    let root = graph
        .spawn_root(&mut child("sleep"), TEST_WORK)
        .await
        .expect("actual root");
    graph
        .spawn_root_writer(root, |pipes| async move {
            drop(pipes);
            panic!("deliberate owned writer failure");
        })
        .await
        .expect("owned writer");
    let sealed = graph.seal();
    assert_eq!(
        sealed.close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
    let snapshot = sealed.snapshot().await;
    assert_eq!(snapshot.joined_writers, 1);
    assert_eq!(snapshot.abandoned_writers, 1);
    assert_eq!(ledger.snapshot().expect("held").unknown_held, 1);
}

#[tokio::test]
async fn root_identity_cannot_cross_graph_and_seal_rejects_late_acquisition() {
    let ledger = ledger();
    let mut first = graph(&ledger);
    let mut second = graph(&ledger);
    let first_root = first
        .spawn_root(&mut child("sleep"), TEST_WORK)
        .await
        .expect("first root");
    let second_root = second
        .spawn_root(&mut child("sleep"), TEST_WORK)
        .await
        .expect("second root");
    assert!(second
        .spawn_root_writer(first_root, |_| async {})
        .await
        .is_err());
    second
        .spawn_root_writer(second_root, |_| async {})
        .await
        .expect("own root");
    assert!(second
        .spawn_root_writer(second_root, |_| async {})
        .await
        .is_err());
    let sealed = second.seal();
    // Private test-only alias exercises the runtime sealing fence as well as
    // the public type-level inability to acquire through SealedPhysicalGraph.
    let mut late = OwnedPhysicalGraph {
        ledger: ledger.clone(),
        custody: Some(Arc::clone(&sealed.custody)),
    };
    assert!(late
        .spawn_root(&mut child("sleep"), TEST_WORK)
        .await
        .is_err());
    assert!(late
        .spawn_root_writer(second_root, |_| async {})
        .await
        .is_err());
    drop(late);
    assert_eq!(
        sealed.close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
    assert_eq!(
        first.seal().close().await,
        GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable)
    );
    assert_eq!(ledger.snapshot().expect("both still held").held, 2);
}

#[test]
fn boot_identity_rejects_missing_nil_malformed_and_oversized_values() {
    let id = b"12345678-1234-4234-8234-123456789abc";
    assert!(matches!(parse_boot_identity(Some(id)), Metric::Known(_)));
    assert!(matches!(
        parse_boot_identity(Some(b"12345678-1234-4234-8234-123456789abc\0")),
        Metric::Known(_)
    ));
    for value in [
        None,
        Some(b"".as_slice()),
        Some(b"00000000-0000-0000-0000-000000000000".as_slice()),
        Some(b"not-an-os-boot".as_slice()),
        Some([b'0'; 65].as_slice()),
    ] {
        assert!(matches!(parse_boot_identity(value), Metric::Unknown(_)));
    }
}
