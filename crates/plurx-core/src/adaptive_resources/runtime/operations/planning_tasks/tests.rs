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
