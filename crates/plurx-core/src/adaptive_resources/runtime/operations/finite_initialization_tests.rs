//! Accepted preparation must already retain its fixed application setup.
//! Authored for the complete-candidate batch; compilation does not run these.
use super::*;
use crate::adaptive_resources::{FamilyRoleDemand, WorkloadDemand};
use crate::resource_custody::{InstallationContext, MemberKind, RecoveryMethod};
use std::time::Duration;

fn vector() -> ResourceVector {
    ResourceVector {
        cpu_ns_per_second: 2,
        peak_memory_bytes: 2,
        handles: 6,
        processes: 2,
        hardware_slots: 0,
        tuner_slots: 0,
        scratch_bytes: 2,
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn runtime(root: &Path) -> NodeResourceRuntime {
    let runtime = NodeResourceRuntime::open(super::super::tests::options(
        root,
        daemon_owner(
            "finite-initialization-node".into(),
            "finite-initialization-cluster".into(),
        ),
        InstallationContext::FirstInstallation,
    ))
    .expect("original journal");
    runtime
        .update_capacity(1, Metric::Known(vector()))
        .expect("bounded capacity");
    runtime
}

fn plan() -> TypedGraphPlan {
    TypedGraphPlan {
        attempt_id: None,
        members: vec![TypedMemberPlan::Legacy(PlannedMember {
            member_id: Uuid::new_v4(),
            kind: MemberKind::Root,
            adapter: Adapter::ProcessRoot,
            adapter_version: 1,
            domain: "finite-initialization-fixture".into(),
            locator: "never-entered-root".into(),
            recovery: RecoveryMethod::LiveOnly {
                reason: "unentered preparation fixture".into(),
            },
            owned_by: None,
            depends_on: Vec::new(),
            maximum_cardinality: 1,
            protected_paths: Vec::new(),
        })],
    }
}

fn prepare<T>(
    runtime: &NodeResourceRuntime,
    initialize: impl FnOnce(&OwnedPreparation) -> T,
) -> Result<(OwnedPreparation, T), OperationError> {
    let roles = [FamilyRoleDemand {
        role: FamilyRole::Video,
        demand: WorkloadDemand {
            resources: Metric::Known(vector()),
            limit_epoch: 1,
            trial: false,
        },
        retained: None,
    }];
    runtime.prepare_typed_families_claiming_initialized(
        &[FamilyDemand {
            identity: runtime.new_family()?,
            roles: &roles,
        }],
        vec![plan()],
        None,
        deadline(),
        initialize,
    )
}

#[tokio::test]
async fn initialized_setup_survives_dropped_selector_and_cannot_use_generic_cancel_after_app_release(
) {
    let directory = tempfile::tempdir().expect("durable task scratch");
    let runtime = runtime(directory.path());
    let application = super::super::finite_lifetime::FiniteAppState::with_application_limit(128);
    let lease = application
        .take_application_lease(128)
        .expect("before accepted R");
    let (owner, original_id) = prepare(&runtime, |owner| {
        // The operation-book lock excludes observers throughout setup. The
        // original bundle remains private, with no native acquisition entered.
        assert!(runtime.operations.try_lock().is_err());
        let mut bundle = owner
            .state
            .try_lock()
            .expect("private setup cannot contend");
        assert!(!bundle.installed);
        assert!(bundle.roles[0].armed.is_empty());
        bundle.roles[0].finite_app = Some(Arc::clone(&application));
        owner.id
    })
    .expect("accepted initialized preparation");
    assert_eq!(owner.id, original_id);
    drop(owner);
    assert_eq!(
        runtime.accounting().expect("held original charge").used,
        vector()
    );
    let owner = runtime
        .retained_preparations()
        .expect("original book")
        .pop()
        .expect("same owner");
    assert_eq!(owner.id, original_id);
    {
        let bundle = owner.state.lock().await;
        assert!(Arc::ptr_eq(
            bundle.roles[0].finite_app.as_ref().expect("retained setup"),
            &application
        ));
    }
    drop(lease);
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .expect("original accepted result");
    assert!(
        runtime
            .cancel_preparation(&owner, deadline())
            .await
            .is_err(),
        "generic cancellation has no finite physical/application terminal receipt"
    );
    assert!(
        runtime
            .cancel_preparation(&owner, deadline())
            .await
            .is_err(),
        "retry cannot mint a missing finite receipt"
    );
    assert_eq!(
        runtime
            .accounting()
            .expect("original setup remains charged")
            .used,
        vector()
    );
    assert!(
        runtime.forget_preparation(owner).await.is_err(),
        "dropped application lease alone is no refund or forgotten-owner authority"
    );
    // This setup-only fixture deliberately registered no concrete finite
    // producer/no-root witness. It therefore retains charge until runtime
    // destruction. The real fixed-owner tests exercise dedicated final close.
    runtime.drain().expect("join journal");
}

#[tokio::test]
async fn refused_admission_does_not_initialize_a_replacement_or_return_held_capacity() {
    let directory = tempfile::tempdir().expect("durable task scratch");
    let runtime = runtime(directory.path());
    let (owner, ()) = prepare(&runtime, |_| ()).expect("first complete reservation");
    let initialized = std::cell::Cell::new(false);
    assert!(prepare(&runtime, |_| initialized.set(true)).is_err());
    assert!(!initialized.get());
    assert_eq!(
        runtime
            .retained_preparations()
            .expect("original only")
            .len(),
        1
    );
    assert_eq!(
        runtime
            .accounting()
            .expect("refusal preserves original charge")
            .used,
        vector()
    );
    runtime
        .resolve_preparation(&owner, deadline())
        .await
        .expect("original result");
    runtime
        .cancel_preparation(&owner, deadline())
        .await
        .expect("original known cancel");
    runtime
        .forget_preparation(owner)
        .await
        .expect("original closed");
    runtime.drain().expect("join journal");
}
