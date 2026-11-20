use super::{m7_settlement, JobSettlement, TitleOutput};

#[test]
fn m7_transient_failure_yields_with_the_complete_title_aggregate() {
    use super::super::m7_title::EvaluationError;
    use plurx_core::scan::dv_title::{DvTitleStats, TitleConstant, TitleElKind};

    let aggregate = TitleOutput {
        file_id: 1,
        probe_revision: 2,
        source_size: 3,
        source_mtime: 4,
        scanner_digest: "scanner-fixture".into(),
        stats: DvTitleStats {
            schema: 1,
            complete: true,
            decoded_frames: 48,
            valid_l1_frames: 48,
            missing_l1_frames: 0,
            invalid_l1_frames: 0,
            max_l1_pq: Some(800),
            mastering_luminance: TitleConstant::Unknown,
            mastering_primaries: TitleConstant::Unknown,
            el_kind: TitleConstant::Consistent(TitleElKind::Fel),
        },
    };
    for (interrupted, reason, delay) in [
        (false, "m7_retry_pending", 30_000),
        (true, "title_interrupted", 5_000),
    ] {
        let Some(JobSettlement::Yield {
            error_code,
            checkpoint,
            not_before_ms,
        }) = m7_settlement(
            &aggregate,
            Err(EvaluationError::Deferred(
                "fixture refused busy scratch".into(),
            )),
            interrupted,
            42,
        )
        else {
            panic!("transient M7 failure must leave the title queued");
        };
        assert_eq!(error_code.as_deref(), Some(reason));
        let checkpoint = checkpoint.unwrap();
        let restored: TitleOutput =
            serde_json::from_value(checkpoint["fel_title_aggregate"].clone()).unwrap();
        assert_eq!(restored.stats, aggregate.stats);
        assert_eq!(restored.probe_revision, aggregate.probe_revision);
        assert_eq!(not_before_ms, 42 + delay);
    }
    assert!(m7_settlement(&aggregate, Ok(()), false, 0).is_none());
    assert!(m7_settlement(
        &aggregate,
        Err(EvaluationError::Unsupported(
            "unsupported fixture geometry".into()
        )),
        false,
        0,
    )
    .is_none());
    assert!(m7_settlement(
        &aggregate,
        Err(EvaluationError::Inconclusive(
            "authored rate out of band".into()
        )),
        false,
        0,
    )
    .is_none());
}
