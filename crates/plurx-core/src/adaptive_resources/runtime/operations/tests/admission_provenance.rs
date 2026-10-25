//! Authored provenance contracts only; no native execution/qualification.
use super::super::super::finite_vod::FiniteVodError;
use super::*;

#[tokio::test]
async fn actual_preparation_stale_epoch_refusal_precedes_all_new_reservations() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    runtime
        .update_capacity(2, Metric::Known(vector(100)))
        .unwrap();
    let error = FiniteVodError::from(
        prepare(&runtime, vec![plan()], deadline())
            .err()
            .expect("old demand"),
    );
    assert_eq!(
        error.admission_refusal(),
        Some(AdmissionRefusal::StaleEpoch)
    );
    assert!(error.definitely_before_reservation());
    assert_eq!(runtime.accounting().unwrap().held, 0);
    assert_eq!(
        runtime.accounting().unwrap().used,
        ResourceVector::default()
    );
    assert!(runtime.retained_preparations().unwrap().is_empty());
    runtime.drain().unwrap();
}

#[tokio::test]
async fn actual_capacity_refusal_leaves_no_new_reservation_or_prepare_owner() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    runtime
        .update_capacity(1, Metric::Known(vector(1)))
        .unwrap();
    let error = FiniteVodError::from(
        prepare(&runtime, vec![plan()], deadline())
            .err()
            .expect("capacity refusal"),
    );
    assert!(matches!(
        error.admission_refusal(),
        Some(AdmissionRefusal::HardLimit(_))
    ));
    assert!(error.definitely_before_reservation());
    assert_eq!(runtime.accounting().unwrap().held, 0);
    assert_eq!(
        runtime.accounting().unwrap().used,
        ResourceVector::default()
    );
    assert!(runtime.retained_preparations().unwrap().is_empty());
    runtime.drain().unwrap();
}

#[tokio::test]
async fn actual_preparation_draining_refusal_has_explicit_pre_reservation_provenance() {
    let root = tempfile::tempdir().expect("fixture");
    let runtime = runtime(root.path());
    runtime.begin_drain().unwrap();
    let error = FiniteVodError::from(
        prepare(&runtime, vec![plan()], deadline())
            .err()
            .expect("drained preparation"),
    );
    assert!(error.definitely_before_reservation());
    assert_eq!(error.to_string(), "resource runtime draining");
    assert_eq!(runtime.accounting().unwrap().held, 0);
    runtime.drain().unwrap();
}

#[test]
fn generic_rollback_admission_and_draining_remain_uncertain_for_identical_causes() {
    // refund/state.release and accepted start/resolve paths can produce these
    // generic variants. Constructing the same cause does not grant provenance.
    let after_reservation =
        FiniteVodError::from(OperationError::Admission(AdmissionRefusal::StaleEpoch));
    let before = FiniteVodError::from(OperationError::BeforeReservation(
        PreReservationRefusal::Admission(AdmissionRefusal::StaleEpoch),
    ));
    assert_eq!(
        after_reservation.admission_refusal(),
        before.admission_refusal()
    );
    assert_eq!(after_reservation.to_string(), before.to_string());
    assert!(!after_reservation.definitely_before_reservation());
    assert!(before.definitely_before_reservation());
    assert!(!FiniteVodError::from(OperationError::Draining).definitely_before_reservation());
    assert!(!FiniteVodError::from(OperationError::State).definitely_before_reservation());
}
