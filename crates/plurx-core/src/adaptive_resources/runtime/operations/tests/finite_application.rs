//! Real queued prepare results must never bypass a retained application owner.
//! These are admission/refund boundary fixtures, not native-close evidence.
use super::*;
use crate::adaptive_resources::runtime::finite_lifetime::FiniteAppState;

#[tokio::test]
async fn known_prepare_rollback_preflights_every_original_application_role_before_any_refund() {
    let root = tempfile::tempdir().expect("durable fixture root");
    let runtime = runtime(root.path());
    let owner = prepare(
        &runtime,
        vec![
            plan(),
            GraphPlan {
                attempt_id: None,
                members: Vec::new(),
            },
        ],
        deadline(),
    )
    .expect("original invalid command accepted for actual DB validation");
    let application = FiniteAppState::with_application_limit(128);
    let lease = application
        .take_application_lease(128)
        .expect("original live application");
    {
        let mut bundle = owner.state.lock().await;
        bundle.roles[1].finite_app = Some(Arc::clone(&application));
    }
    assert!(matches!(
        runtime.resolve_preparation(&owner, deadline()).await,
        Err(OperationError::Custody(error)) if error.certainty == CommitCertainty::RolledBack
    ));
    {
        let bundle = owner.state.lock().await;
        assert!(
            bundle.prepare.acknowledged,
            "retain original known rollback result"
        );
        assert!(!bundle.installed);
        assert!(bundle.roles.iter().all(|role| !role.refunded));
        assert!(Arc::ptr_eq(
            bundle.roles[1]
                .finite_app
                .as_ref()
                .expect("original app retained"),
            &application
        ));
    }
    assert_eq!(
        runtime.accounting().expect("all charges retained").used,
        vector(4)
    );
    drop(lease);
    assert!(
        runtime
            .cancel_preparation(&owner, deadline())
            .await
            .is_err(),
        "application release alone is no generic refund authority"
    );
    assert_eq!(
        runtime
            .accounting()
            .expect("still no finite terminal proof")
            .used,
        vector(4)
    );
    drop(owner);
    runtime.drain().expect("join original journal");
}

#[tokio::test]
async fn stale_admission_cannot_partially_cancel_a_family_with_an_original_application_owner() {
    let root = tempfile::tempdir().expect("durable fixture root");
    let runtime = runtime(root.path());
    let owner = prepare(&runtime, vec![plan(), plan()], deadline()).expect("original family");
    let application = FiniteAppState::with_application_limit(128);
    let lease = application
        .take_application_lease(128)
        .expect("original live application");
    {
        let mut bundle = owner.state.lock().await;
        bundle.roles[1].finite_app = Some(Arc::clone(&application));
    }
    runtime
        .update_capacity(2, Metric::Known(vector(100)))
        .expect("actual admission epoch change");
    assert!(matches!(
        runtime.resolve_preparation(&owner, deadline()).await,
        Err(OperationError::Admission(AdmissionRefusal::StaleEpoch))
    ));
    {
        let bundle = owner.state.lock().await;
        assert!(
            bundle.installed,
            "original accepted prepare remains installed"
        );
        assert!(bundle
            .roles
            .iter()
            .all(|role| !role.refunded && role.transition.is_none()));
    }
    assert_eq!(
        runtime
            .accounting()
            .expect("whole original family retained")
            .used,
        vector(4)
    );
    assert!(runtime
        .cancel_preparation(&owner, deadline())
        .await
        .is_err());
    assert_eq!(
        runtime
            .accounting()
            .expect("no partial generic cancellation")
            .used,
        vector(4)
    );
    drop(lease);
    drop(owner);
    runtime.drain().expect("join original journal");
}
