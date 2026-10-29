//! Original planning-task ownership regressions. Behavioral execution waits
//! for final review; the joined child uses actual installed Linux tools.
use super::*;
use crate::adaptive_resources::runtime::planning_probe::plan;
use crate::process::fixed_probe::{DecodeFactCache, ProbeStreamSelection};
use crate::resource_custody::InstallationContext;
use std::time::Duration;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

async fn prepared(root: &std::path::Path) -> (Arc<NodeResourceRuntime>, OwnedPreparation) {
    prepared_config(root, true).await
}

async fn prepared_config(
    root: &std::path::Path,
    resolve: bool,
) -> (Arc<NodeResourceRuntime>, OwnedPreparation) {
    let runtime = fixture_runtime(root);
    let request = fixture_request(
        root,
        "/never-entered/ffprobe".into(),
        "/never-entered/ffmpeg".into(),
    );
    let original = plan::prepare(&runtime, request, deadline()).unwrap();
    let preparation = plan::original_preparation_for_test(&original);
    // Low-level settlement fixtures retain the original preparation directly;
    // dropping its actual public owner requests cleanup but cannot refund.
    drop(original);
    assert_eq!(
        preparation.state.lock().await.roles[1]
            .typed_members
            .as_ref()
            .unwrap()
            .capacity(),
        1,
        "M survivor allocation must not depend on collection growth"
    );
    if resolve {
        runtime
            .resolve_preparation(&preparation, deadline())
            .await
            .unwrap();
    }
    (runtime, preparation)
}

fn fixture_runtime(root: &std::path::Path) -> Arc<NodeResourceRuntime> {
    let owner = super::super::super::daemon_owner("fixture-node".into(), "fixture-cluster".into());
    let runtime = Arc::new(
        NodeResourceRuntime::open(super::super::super::tests::options(
            root,
            owner,
            InstallationContext::FirstInstallation,
        ))
        .unwrap(),
    );
    let bound = u64::MAX / 4;
    runtime
        .update_capacity(
            1,
            Metric::Known(ResourceVector {
                cpu_ns_per_second: bound,
                peak_memory_bytes: bound,
                handles: bound,
                processes: bound,
                hardware_slots: bound,
                tuner_slots: bound,
                scratch_bytes: bound,
            }),
        )
        .unwrap();
    runtime
}

fn fixture_request(
    root: &std::path::Path,
    probe_program: String,
    ffmpeg_program: String,
) -> FixedPlanningProbeRequest {
    let source_path = root.join("never-opened.mp4");
    let metadata = std::fs::metadata(&source_path).ok();
    let scanner_size = metadata
        .as_ref()
        .map_or(0, |metadata| metadata.len() as i64);
    let scanner_mtime = metadata
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs() as i64);
    FixedPlanningProbeRequest {
        source: FixedPlanningSourceRequest {
            path: source_path,
            media_file_id: "fixture-media".into(),
            scanner_size,
            scanner_mtime,
        },
        recipe_source_binding: "fixture-recipe".into(),
        probe_program_locator: probe_program,
        cache: Arc::new(DecodeFactCache::new()),
        catalog: crate::transcode::DecodeCatalogMetadata::new(
            None,
            None,
            crate::domain::DolbyVisionFacts::default(),
        )
        .unwrap(),
        selected_stream: ProbeStreamSelection::FirstPlayable,
        work: crate::process::ChildWork::background("planning ownership fixture"),
        ffmpeg_program,
        application_metadata_bytes: 4096,
    }
}

#[tokio::test]
async fn uncertain_planning_repeats_one_durable_hold_and_keeps_p_and_m_charged() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner) = prepared(root.path()).await;
    let charge = runtime.accounting().unwrap().used;
    {
        let mut bundle = owner.state.lock().await;
        bundle.roles[0].planning_tasks.as_mut().unwrap().unknown = true;
        runtime
            .retain_planning_uncertainty(bundle.roles[0].binding)
            .unwrap();
    }
    assert!(matches!(
        runtime.finish_planning_native(&owner, deadline()).await,
        Err(OperationError::State)
    ));
    let sequence = {
        let bundle = owner.state.lock().await;
        assert_eq!(bundle.roles[0].phase, Phase::UnknownHeld);
        assert!(bundle.roles.iter().all(|role| !role.refunded));
        bundle.roles[0].sequence
    };
    for _ in 0..3 {
        assert!(matches!(
            runtime.finish_planning_native(&owner, deadline()).await,
            Err(OperationError::State)
        ));
        let bundle = owner.state.lock().await;
        assert_eq!(
            bundle.roles[0].sequence, sequence,
            "no second Hold operation"
        );
        assert!(bundle.roles[0].transition.is_none());
    }
    assert_eq!(runtime.accounting().unwrap().used, charge);
    assert_eq!(
        runtime
            .read_planning_ownership_diagnostics()
            .unwrap()
            .native_roles_held,
        1
    );
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole runtime owner")
        .drain()
        .unwrap();
}

#[tokio::test]
async fn incomplete_planning_cannot_refund_one_missing_native_closure() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner) = prepared(root.path()).await;
    let charge = runtime.accounting().unwrap().used;
    let sequence = {
        let mut bundle = owner.state.lock().await;
        bundle.roles[0].planning_tasks.as_mut().unwrap().entered = true;
        bundle.roles[0].sequence
    };
    assert!(matches!(
        runtime.finish_planning_native(&owner, deadline()).await,
        Err(OperationError::State)
    ));
    let bundle = owner.state.lock().await;
    assert_eq!(bundle.roles[0].sequence, sequence);
    assert_eq!(bundle.roles[0].phase, Phase::Prepared);
    assert!(bundle.roles.iter().all(|role| !role.refunded));
    assert_eq!(runtime.accounting().unwrap().used, charge);
    drop(bundle);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole runtime owner")
        .drain()
        .unwrap();
}

#[tokio::test]
async fn expired_planning_lock_preserves_original_observation_and_settlement_ids() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner) = prepared(root.path()).await;
    let charge = runtime.accounting().unwrap().used;
    let mut bundle = owner.state.lock().await;
    let tasks = bundle.roles[0].planning_tasks.as_mut().unwrap();
    tasks.unknown = true;
    let operations: Vec<_> = tasks
        .root_observations
        .iter()
        .map(|o| o.operation_id)
        .chain([
            tasks.probe_task_observation.operation_id,
            tasks.engine_task_observation.operation_id,
            tasks.settlement_operation_id,
        ])
        .collect();
    runtime
        .retain_planning_uncertainty(bundle.roles[0].binding)
        .unwrap();
    assert!(matches!(
        runtime.finish_planning_native(&owner, Instant::now()).await,
        Err(OperationError::Deadline)
    ));
    drop(bundle);
    assert!(matches!(
        runtime.finish_planning_native(&owner, deadline()).await,
        Err(OperationError::State)
    ));
    let bundle = owner.state.lock().await;
    let tasks = bundle.roles[0].planning_tasks.as_ref().unwrap();
    let resumed: Vec<_> = tasks
        .root_observations
        .iter()
        .map(|o| o.operation_id)
        .chain([
            tasks.probe_task_observation.operation_id,
            tasks.engine_task_observation.operation_id,
            tasks.settlement_operation_id,
        ])
        .collect();
    assert_eq!(resumed, operations);
    assert_eq!(bundle.roles[0].phase, Phase::UnknownHeld);
    assert_eq!(runtime.accounting().unwrap().used, charge);
    drop(bundle);
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole runtime owner")
        .drain()
        .unwrap();
}

async fn assert_only_m_survives(runtime: &NodeResourceRuntime, owner: &OwnedPreparation) {
    let bundle = owner.state.lock().await;
    assert!(bundle.roles[0].refunded);
    assert!(!bundle.roles[1].refunded);
    assert!(bundle.roles[0].planning_tasks.is_none());
    assert!(bundle.roles[0].sources.is_empty());
    assert_eq!(
        bundle.roles[0].members.capacity(),
        0,
        "P manifest backing cannot survive on M's structural budget"
    );
    assert!(bundle.roles[0].typed_members.is_none());
    assert!(bundle.prepare.acknowledged);
    let metadata = bundle.roles[1]
        .planning_metadata
        .as_ref()
        .expect("original M owner survives");
    assert_eq!(
        runtime.accounting().unwrap().used,
        ResourceVector {
            peak_memory_bytes: metadata.plan.retained_memory_bytes,
            ..ResourceVector::default()
        }
    );
}

#[tokio::test]
async fn never_entered_planning_resolves_original_dropped_prepare_and_refunds_only_p() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner) = prepared_config(root.path(), false).await;
    let (prepare_operation, settle_operation) = {
        let bundle = owner.state.lock().await;
        assert!(!bundle.installed);
        assert!(bundle.roles[0].sources.is_empty());
        (
            bundle.prepare.id,
            bundle.roles[0]
                .planning_tasks
                .as_ref()
                .unwrap()
                .settlement_operation_id,
        )
    };
    runtime
        .finish_planning_native(&owner, deadline())
        .await
        .expect("same prepare, no source open, known P close");
    assert_only_m_survives(&runtime, &owner).await;
    let charge = runtime.accounting().unwrap().used;
    let settled_sequence = owner.state.lock().await.roles[0].sequence;
    runtime
        .finish_planning_native(&owner, deadline())
        .await
        .expect("same settled P is idempotent");
    assert_eq!(runtime.accounting().unwrap().used, charge);
    let bundle = owner.state.lock().await;
    assert_eq!(bundle.prepare.id, prepare_operation);
    assert_eq!(bundle.roles[0].sequence, settled_sequence);
    assert_eq!(bundle.roles[0].phase, Phase::Settled);
    assert!(bundle.roles[0].transition.is_none());
    assert_ne!(settle_operation, Uuid::nil());
    drop(bundle);
    Arc::try_unwrap(runtime).ok().unwrap().drain().unwrap();
}

#[tokio::test]
async fn never_entered_planning_closes_original_opened_parent_before_p_refund() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("never-opened.mp4"), b"same held source").unwrap();
    let (runtime, owner) = prepared(root.path()).await;
    runtime
        .prepare_planning_inputs(&owner, deadline())
        .await
        .expect("actual original parent open");
    {
        let bundle = owner.state.lock().await;
        let task = bundle.roles[0].planning_tasks.as_ref().unwrap();
        assert!(!task.entered && task.inputs.is_some());
        assert!(!bundle.roles[0]
            .sources
            .get(&task.source_member)
            .unwrap()
            .parent_closed());
    }
    runtime
        .finish_planning_native(&owner, deadline())
        .await
        .expect("checked original parent close plus never-entered child/tasks");
    assert_only_m_survives(&runtime, &owner).await;
    Arc::try_unwrap(runtime).ok().unwrap().drain().unwrap();
}

#[tokio::test]
async fn never_entered_planning_closes_original_parent_even_when_scanner_observation_was_refused() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("never-opened.mp4");
    std::fs::write(&path, b"first").unwrap();
    let (runtime, owner) = prepared(root.path()).await;
    // The original native open retains its File even when its later metadata
    // comparison rejects the changed scanner facts. Cleanup must close it.
    std::fs::write(&path, b"changed after planning").unwrap();
    assert!(runtime
        .prepare_planning_inputs(&owner, deadline())
        .await
        .is_err());
    runtime
        .finish_planning_native(&owner, deadline())
        .await
        .expect("close retained actual File without inventing an observation");
    assert_only_m_survives(&runtime, &owner).await;
    Arc::try_unwrap(runtime).ok().unwrap().drain().unwrap();
}

#[tokio::test]
async fn expired_or_stale_planning_prepare_cannot_generically_refund_p_before_m() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner) = prepared_config(root.path(), false).await;
    let charged = runtime.accounting().unwrap().used;
    assert!(matches!(
        runtime.resolve_preparation(&owner, Instant::now()).await,
        Err(OperationError::Deadline)
    ));
    assert!(runtime
        .cancel_preparation(&owner, deadline())
        .await
        .is_err());
    assert_eq!(runtime.accounting().unwrap().used, charged);
    {
        let bundle = owner.state.lock().await;
        assert!(bundle
            .roles
            .iter()
            .all(|role| !role.refunded && role.phase == Phase::Prepared));
    }
    let bound = u64::MAX / 4;
    runtime
        .update_capacity(
            2,
            Metric::Known(ResourceVector {
                cpu_ns_per_second: bound,
                peak_memory_bytes: bound,
                handles: bound,
                processes: bound,
                hardware_slots: bound,
                tuner_slots: bound,
                scratch_bytes: bound,
            }),
        )
        .unwrap();
    assert!(matches!(
        runtime.resolve_preparation(&owner, deadline()).await,
        Err(OperationError::Admission(AdmissionRefusal::StaleEpoch))
    ));
    assert_eq!(runtime.accounting().unwrap().used, charged);
    runtime
        .finish_planning_native(&owner, deadline())
        .await
        .expect("original cleanup does not grant stale launch eligibility");
    assert_only_m_survives(&runtime, &owner).await;
    Arc::try_unwrap(runtime).ok().unwrap().drain().unwrap();
}

mod joined;
mod partial;
mod resumption;
// Test-only per-original-operation barriers; production has neither registry
// state nor extra awaits/owner fields.
pub(in crate::adaptive_resources::runtime::operations) mod stages;

#[tokio::test]
async fn never_entered_book_conflict_holds_ledger_and_journal_without_refund() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner) = prepared(root.path()).await;
    let charge = runtime.accounting().unwrap().used;
    let binding = {
        let mut bundle = owner.state.lock().await;
        let role = &mut bundle.roles[0];
        // Negative bookkeeping fixture only: an invoked slot contradicts
        // retained unentered inputs. It cannot produce an absence witness.
        let member = role
            .planning_tasks
            .as_ref()
            .unwrap()
            .probe_plan
            .version_root_member_id
            .uuid();
        role.invoked.insert(member);
        role.binding
    };
    assert!(matches!(
        runtime.finish_planning_native(&owner, deadline()).await,
        Err(OperationError::State)
    ));
    let sequence = {
        let bundle = owner.state.lock().await;
        assert_eq!(bundle.roles[0].phase, Phase::UnknownHeld);
        assert!(bundle.roles.iter().all(|role| !role.refunded));
        bundle.roles[0].sequence
    };
    {
        let ledger = runtime.ledger.inner.lock().unwrap();
        assert_eq!(
            ledger.entries.get(&binding.reservation).unwrap().state,
            ReservationState::UnknownHeld
        );
    }
    assert!(matches!(
        runtime.finish_planning_native(&owner, deadline()).await,
        Err(OperationError::State)
    ));
    assert_eq!(owner.state.lock().await.roles[0].sequence, sequence);
    assert_eq!(runtime.accounting().unwrap().used, charge);
    assert_eq!(
        runtime
            .read_planning_ownership_diagnostics()
            .unwrap()
            .uncertain_native_owners,
        1
    );
    Arc::try_unwrap(runtime)
        .ok()
        .expect("sole runtime owner")
        .drain()
        .unwrap();
}

pub(super) mod construction;
mod native_custody;

#[tokio::test]
async fn v7_original_planning_prepare_persists_p_and_metadata_claim_before_native_entry() {
    let root = tempfile::tempdir().unwrap();
    let (runtime, owner) = prepared(root.path()).await;
    let journal =
        rusqlite::Connection::open(root.path().join("resource-custody/obligations.sqlite3"))
            .unwrap();
    assert_eq!(
        journal
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
    let claims = journal
        .prepare("SELECT memory_claim FROM obligations ORDER BY memory_claim")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(claims, ["planning_metadata", "unknown_producer"]);
    let adapters = journal
        .prepare("SELECT adapter FROM members")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<std::collections::BTreeSet<_>, _>>()
        .unwrap();
    assert!(adapters.contains("fixed_probe_metadata"));
    assert!(adapters.contains("fixed_probe_discover_source"));
    assert!(adapters.contains("fixed_probe_discover_task"));
    assert!(adapters.contains("fixed_planning_engine_task"));
    drop(journal);
    runtime
        .finish_planning_native(&owner, deadline())
        .await
        .expect("same never-entered original P closes");
    assert_only_m_survives(&runtime, &owner).await;
    Arc::try_unwrap(runtime).ok().unwrap().drain().unwrap();
}

#[derive(Clone, Copy)]
enum EntryFence {
    Epoch,
    Drain,
    Overage,
    EpochBetweenSiblings,
    DrainAfterEntry,
}

async fn registered_planning_entry_respects_original_fence(fence: EntryFence) {
    let root = tempfile::tempdir().expect("real registered P entry fixture");
    std::fs::write(
        root.path().join("never-opened.mp4"),
        b"same retained source",
    )
    .expect("actual original source");
    let runtime = fixture_runtime(root.path());
    let request = fixture_request(
        root.path(),
        "/never-entered/ffprobe".into(),
        "/never-entered/ffmpeg".into(),
    );
    let original = plan::prepare(&runtime, request, deadline()).expect("actual charged P/M");
    let preparation = plan::original_preparation_for_test(&original);
    runtime
        .prepare_planning_inputs(&preparation, deadline())
        .await
        .expect("actual original source parent acquired");
    let charged = runtime.accounting().expect("full original P/M charge").used;
    let both_refused = matches!(
        fence,
        EntryFence::Epoch | EntryFence::Drain | EntryFence::Overage
    );
    let held_parent = if both_refused {
        let mut bundle = preparation.state.lock().await;
        let role = &mut bundle.roles[0];
        let source_member = role.planning_tasks.as_ref().unwrap().source_member;
        Some(
            role.sources
                .get_mut(&source_member)
                .unwrap()
                .take_probe_lease()
                .expect("actual original parent alias")
                .0,
        )
    } else {
        None
    };
    let (binding, member) = {
        let bundle = preparation.state.lock().await;
        let role = &bundle.roles[0];
        let tasks = role.planning_tasks.as_ref().expect("same original P");
        assert!(!tasks.entered && tasks.probe.is_none() && tasks.engine.is_none());
        assert!(!role.sources[&tasks.source_member].parent_closed());
        (role.binding, tasks.engine_plan.member_id.uuid())
    };
    let gate = match fence {
        EntryFence::EpochBetweenSiblings => stages::Gate::between_entries(member),
        EntryFence::DrainAfterEntry => stages::Gate::install(member),
        _ => stages::Gate::registered(member),
    };
    let engine_native = if matches!(fence, EntryFence::DrainAfterEntry) {
        None
    } else {
        Some(stages::Gate::install(member))
    };
    let mut collect = Some(Box::pin(
        runtime.collect_planning_tasks(&preparation, deadline()),
    ));
    tokio::select! {
        result = collect.as_mut().expect("same borrowed collector") => panic!("original P skipped registered entry boundary: {}", result.is_ok()),
        () = gate.entered() => {},
        () = tokio::time::sleep(Duration::from_secs(15)) => panic!("real original P registration watchdog"),
    }
    // These exercise the actual existing authority controls. The overage case
    // installs an accounting fence, not a fabricated kernel overage witness.
    match fence {
        EntryFence::Epoch | EntryFence::EpochBetweenSiblings => {
            let bound = u64::MAX / 4;
            runtime
                .update_capacity(
                    2,
                    Metric::Known(ResourceVector {
                        cpu_ns_per_second: bound,
                        peak_memory_bytes: bound,
                        handles: bound,
                        processes: bound,
                        hardware_slots: bound,
                        tuner_slots: bound,
                        scratch_bytes: bound,
                    }),
                )
                .expect("actual intervening allocation epoch");
        }
        EntryFence::Drain | EntryFence::DrainAfterEntry => {
            runtime.begin_drain().expect("actual intervening drain");
        }
        EntryFence::Overage => runtime
            .ledger
            .record_native_overage(
                binding,
                Uuid::new_v4(),
                ResourceVector {
                    handles: 1,
                    ..Default::default()
                },
            )
            .expect("actual retained-ledger overage fence"),
    }
    if matches!(fence, EntryFence::DrainAfterEntry) {
        assert_eq!(
            gate.visits(),
            1,
            "same engine passed its original released latch"
        );
        drop(collect.take()); // only waiter; same original handle remains in P
    }
    drop(gate);
    let result = if let Some(collect) = collect.take() {
        collect.await
    } else {
        // A new policy fence refuses new entry, but cannot prevent original
        // handle collection and cleanup under the same prepaid P.
        runtime
            .collect_planning_tasks(&preparation, deadline())
            .await
    };
    // Both branches consumed the selected waiter. End the empty holder's
    // borrow before the original preparation and runtime are drained below.
    drop(collect);
    assert!(
        result.is_err(),
        "failed native work cannot produce a planning report"
    );
    if let Some(engine_native) = engine_native {
        assert_eq!(
            engine_native.visits(),
            0,
            "refused engine never passed its original receiver"
        );
    }
    {
        let mut bundle = preparation.state.lock().await;
        let role = &mut bundle.roles[0];
        let tasks = role.planning_tasks.as_mut().expect("same retained P");
        assert!(tasks.entered && tasks.unknown);
        assert!(
            tasks.probe.is_none() && tasks.engine.is_none(),
            "both original handles joined"
        );
        if both_refused {
            assert!(tasks.probe_failure.is_none() && tasks.engine_failure.is_none());
            let probe = tasks
                .probe_never_entered
                .as_ref()
                .expect("same probe joined after receiver error");
            let engine = tasks
                .engine_never_entered
                .as_ref()
                .expect("same engine joined after receiver error");
            assert!(probe.matches(
                binding,
                tasks.probe_plan.member_id.uuid(),
                tasks.probe_join_operation
            ));
            assert!(engine.matches(
                binding,
                tasks.engine_plan.member_id.uuid(),
                tasks.engine_join_operation
            ));
            assert_ne!(probe.join_operation(), engine.join_operation());
            assert_eq!(
                tasks.native_state(),
                PlanningNativeState::RegisteredNeverEntered
            );
        } else {
            let probe = tasks
                .probe_failure
                .as_ref()
                .expect("original entered probe failure retained");
            assert!(
                !matches!(probe.error, DecodeFactError::Deadline),
                "released sibling ran its actual collector"
            );
            assert!(tasks.probe_never_entered.is_none());
            if matches!(fence, EntryFence::EpochBetweenSiblings) {
                assert!(
                    tasks.engine_never_entered.is_some(),
                    "unreleased sibling retained its distinct joined witness"
                );
                assert!(tasks.engine_failure.is_none());
            } else {
                assert!(tasks.engine_never_entered.is_none());
                assert_eq!(tasks.engine_failure.as_ref().expect("original entered engine failure retained").phase,
                    crate::process::fixed_planning_engine::FixedEngineFailurePhase::BeforeNativeEntry);
            }
            assert_eq!(tasks.native_state(), PlanningNativeState::Uncertain);
        }
        assert!(tasks.probe_closed.is_none() && tasks.engine_closed.is_none());
        assert!(
            tasks.never_entered.is_none(),
            "registered tasks cannot become NeverAcquired"
        );
        assert!(!role.refunded && role.pending_finite_close.is_none());
    }
    let held = runtime.accounting().expect("same full held charge");
    let mut expected = charged;
    if matches!(fence, EntryFence::Overage) {
        expected.handles += 1;
    }
    assert_eq!(held.used, expected);
    assert!(held.unknown_held > 0);
    if matches!(fence, EntryFence::Epoch | EntryFence::Drain) {
        let identities = {
            let bundle = preparation.state.lock().await;
            let tasks = bundle.roles[0].planning_tasks.as_ref().unwrap();
            (
                tasks.settlement_operation_id,
                tasks.probe_never_entered.as_ref().unwrap().join_operation(),
                tasks
                    .engine_never_entered
                    .as_ref()
                    .unwrap()
                    .join_operation(),
            )
        };
        // Task non-entry never stands in for the actual acquired parent close.
        assert!(runtime
            .finish_planning_native(&preparation, deadline())
            .await
            .is_err());
        assert_eq!(runtime.accounting().unwrap().used, expected);
        {
            let bundle = preparation.state.lock().await;
            let role = &bundle.roles[0];
            let tasks = role.planning_tasks.as_ref().unwrap();
            assert!(tasks.registered_unentered_graph.is_some());
            assert!(!role.sources[&tasks.source_member].parent_closed());
            assert!(!role.refunded);
        }
        drop(held_parent);
        let preflight = stages::Gate::registered_preflight(identities.0);
        let mut finish = Box::pin(runtime.finish_planning_native(&preparation, deadline()));
        tokio::select! {
            result = &mut finish => panic!("original registered preflight skipped: {result:?}"),
            () = preflight.entered() => {},
            () = tokio::time::sleep(Duration::from_secs(15)) => panic!("registered closure preflight watchdog"),
        }
        drop(finish); // only the borrower; all original proofs stay in P
        {
            let mut bundle = preparation.state.lock().await;
            let role = &mut bundle.roles[0];
            let tasks = role.planning_tasks.as_ref().unwrap();
            assert!(tasks.registered_unentered_graph.is_some());
            assert_eq!(
                (
                    tasks.settlement_operation_id,
                    tasks.probe_never_entered.as_ref().unwrap().join_operation(),
                    tasks
                        .engine_never_entered
                        .as_ref()
                        .unwrap()
                        .join_operation()
                ),
                identities
            );
            let source_member = tasks.source_member;
            let original_s1 = tasks.observed_source.clone().unwrap();
            let source = role.sources.get_mut(&source_member).unwrap();
            assert!(source.parent_closed());
            // A synchronous rejected audit borrower must restore the SAME
            // acquired metadata, without reopening/re-statting or consuming it.
            assert!(source
                .with_fixed_probe_unentered_observation(|observed| {
                    assert!(
                        matches!(observed, Some(v2::Observed::FixedProbeDiscoverSourceV1 { source })
                    if source.source_association.object_version == original_s1)
                    );
                    Err(OperationError::State)
                })
                .is_err());
            source
                .with_fixed_probe_unentered_observation(|observed| {
                    assert!(
                        matches!(observed, Some(v2::Observed::FixedProbeDiscoverSourceV1 { source })
                    if source.source_association.object_version == original_s1)
                    );
                    Ok(())
                })
                .expect("same metadata restored after rejected borrower");
            assert!(!role.refunded);
        }
        assert_eq!(runtime.accounting().unwrap().used, expected);
        drop(preflight);
        runtime.finish_planning_native(&preparation, deadline()).await
            .expect("same two joined non-entry witnesses plus checked parent close and exact typed journal settlement");
        assert_only_m_survives(&runtime, &preparation).await;
        let sequence = preparation.state.lock().await.roles[0].sequence;
        runtime
            .finish_planning_native(&preparation, deadline())
            .await
            .expect("same P settlement is idempotent");
        assert_eq!(preparation.state.lock().await.roles[0].sequence, sequence);
        drop(original);
        drop(preparation);
        let until = deadline();
        while runtime.accounting().unwrap().used != ResourceVector::default() {
            assert!(
                Instant::now() < until,
                "actual original M/application cleanup did not finish"
            );
            runtime.maintain_planning_cleanup(until).await;
            tokio::task::yield_now().await;
        }
        assert!(runtime.operations.lock().unwrap().bundles.is_empty());
        Arc::try_unwrap(runtime)
            .ok()
            .expect("sole original runtime")
            .drain()
            .expect("original journal drained");
    } else {
        // A mixed entered outcome or independent native overage cannot be
        // discharged by the unopened sibling's exact registered witness.
        assert!(runtime
            .finish_planning_native(&preparation, deadline())
            .await
            .is_err());
        runtime
            .retry_planning_native_failures(&preparation, deadline())
            .await
            .expect("same original failed owners positively drained");
        drop(held_parent);
        assert!(runtime
            .finish_planning_native(&preparation, deadline())
            .await
            .is_err());
        {
            let mut bundle = preparation.state.lock().await;
            let role = &mut bundle.roles[0];
            let tasks = role
                .planning_tasks
                .as_ref()
                .expect("same P remains charged");
            let source_member = tasks.source_member;
            if matches!(fence, EntryFence::Overage) {
                assert!(tasks.registered_unentered_graph.is_some());
                assert!(
                    tasks.probe_never_entered.is_some() && tasks.engine_never_entered.is_some()
                );
                assert!(
                    role.sources[&source_member].parent_closed(),
                    "known original parent may close while independent overage stays held"
                );
            } else {
                let source = role.sources.get_mut(&source_member).unwrap();
                assert!(source.close_parent().await.is_err(), "a generic close cannot override the issued original lease without its exact native witness");
                assert!(
                    !source.parent_closed(),
                    "original parent remains retained with unresolved entered-task custody"
                );
            }
            assert!(!role.refunded);
        }
        assert_eq!(runtime.accounting().unwrap().used, expected);
        drop(original);
        drop(preparation);
        Arc::try_unwrap(runtime)
            .ok()
            .expect("sole original runtime")
            .drain()
            .expect("original journal drained");
        eprintln!(
            "retained unresolved original R1 P fixture custody at {}",
            root.keep().display()
        );
    }
}

#[tokio::test]
async fn registered_planning_latches_refuse_intervening_epoch_and_join_both_originals() {
    registered_planning_entry_respects_original_fence(EntryFence::Epoch).await;
}
#[tokio::test]
async fn registered_planning_latches_refuse_intervening_drain_and_join_both_originals() {
    registered_planning_entry_respects_original_fence(EntryFence::Drain).await;
}
#[tokio::test]
async fn registered_planning_latches_refuse_intervening_overage_and_join_both_originals() {
    registered_planning_entry_respects_original_fence(EntryFence::Overage).await;
}
#[tokio::test]
async fn released_probe_and_refused_engine_keep_both_original_outcomes_and_full_charge() {
    registered_planning_entry_respects_original_fence(EntryFence::EpochBetweenSiblings).await;
}

#[tokio::test]
async fn registered_planning_join_survives_waiter_cancellation_and_later_drain() {
    registered_planning_entry_respects_original_fence(EntryFence::DrainAfterEntry).await;
}
