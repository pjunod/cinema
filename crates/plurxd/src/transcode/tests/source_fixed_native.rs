//! The selected actor's SAME original P engine task, paused after native
//! custody is installed and before original wait/drain joins. A started PID
//! is diagnostic only: it proves neither continued execution nor closure.
use super::*;

pub(super) async fn exercise(
    state: &Arc<crate::state::AppState>,
    manager: &Arc<TranscodeManager>,
    actor: &SourceViewerActor,
    assignment: &SourceDispatchAssignment,
    mode: u8,
    ingress_monitor: SourceFactoryIngressMonitor,
) {
    let deadline = Instant::now() + Duration::from_secs(45);
    let runtime = manager
        .resource_runtime
        .as_ref()
        .expect("real selected Source runtime");
    let gate = tokio::time::timeout_at(
        deadline.into(),
        manager.source_workers.probe_hooks.fixed_native_gate(),
    )
    .await
    .expect("FAILURE watchdog: original P fixture gate was not installed");
    let started = tokio::time::timeout_at(deadline.into(), gate.started())
        .await
        .expect("FAILURE watchdog: actual original native custody was not reached");
    assert!(started.pid > 0);
    assert!(!started.member_id.is_nil() && !started.close_operation_id.is_nil());
    let charge = runtime.accounting().expect("same original P/M charge");
    assert_eq!(charge.planning_metadata_held, 1);
    assert!(charge.unknown_peak_producers > 0);
    assert!(matches!(
        actor.wait_ready(Instant::now()).await,
        Err(SourceWorkerError::Deadline)
    ));
    assert!(manager.lookup_source_worker(assignment).is_some());
    assert!(manager.vod.session_ids().await.is_empty());
    assert!(state
        .store
        .media_session_route_by_incarnation(&assignment.binding().incarnation_id().to_string())
        .await
        .expect("no preactivation route")
        .is_none());
    assert_eq!(actor.settlement_status(), None);
    assert_eq!(
        manager.admissions.software_in_use(),
        0,
        "fixed P is not a legacy encode permit"
    );
    if mode == 13 {
        state
            .store
            .put_setting(keys::SHARING_ENABLED, "false")
            .await
            .expect("actual policy revocation while original native task is pending");
    }
    gate.release();
    assert!(
        tokio::time::timeout_at(deadline.into(), actor.wait_ready(deadline))
            .await
            .expect("FAILURE watchdog: original Source outcome hung")
            .is_err()
    );
    if mode == 15 {
        // Historical regression name says wait failure. This authentic seam
        // panics the original task AFTER its actual collector returns; loss
        // of the real summary is a JoinError, not a kernel wait syscall error.
        // No native-close receipt is invented or substituted on that path.
        tokio::time::timeout_at(deadline.into(), async {
            loop {
                if runtime
                    .planning_ownership_diagnostics()
                    .is_some_and(|counts| counts.uncertain_native_owners == 1)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("FAILURE watchdog: original task JoinError did not become UnknownHeld");
        let held = runtime
            .accounting()
            .expect("uncertain original charge")
            .used;
        let counts = runtime
            .planning_ownership_diagnostics()
            .expect("complete held original inventory");
        assert_eq!(counts.metadata_owners, 1);
        assert_eq!(counts.native_roles_held, 1);
        assert_eq!(
            counts.original_tasks_pending, 0,
            "the genuine failed original JoinHandle was consumed, not replaced"
        );
        assert_eq!(actor.settlement_status(), None);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), actor.retire())
                .await
                .is_err(),
            "uncertainty must refuse positive Source retirement"
        );
        assert_eq!(actor.settlement_status(), None);
        assert_eq!(
            runtime.accounting().expect("no uncertainty refund").used,
            held
        );
        assert!(actor
            .wait_ready(Instant::now() + Duration::from_secs(1))
            .await
            .is_err());
        assert_eq!(
            runtime
                .accounting()
                .expect("replay does not replace or refund P/M")
                .used,
            held
        );
        assert!(manager.lookup_source_worker(assignment).is_some());
        // Abort only the fixture's accepted transport observer on teardown.
        // This grants no ACK, Source settlement, native absence or reset.
        drop(ingress_monitor);
        return;
    }
    tokio::time::timeout_at(deadline.into(), actor.retire())
        .await
        .expect("FAILURE watchdog: same original native closer did not finish")
        .expect("original P closes before Source SQL release");
    assert!(manager.lookup_source_worker(assignment).is_none());
    assert_eq!(
        state
            .store
            .settle_source_assigned_without_activation(assignment)
            .await
            .expect("exact already-settled Source replay"),
        plurx_core::sharing_source_sessions::SourceReleaseOutcome::ExactReplay
    );
    ingress_monitor.join().await;
}
