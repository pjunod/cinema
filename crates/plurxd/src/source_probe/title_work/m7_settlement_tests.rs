use super::{complete_title, m7_settlement, JobSettlement, TitleDemand, TitleOutput};
use crate::source_probe::m7_title::Evaluated;
use plurx_core::store::dv_m7_outcomes::M7OutcomeKind;

const IDENTITY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

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
        let Err(JobSettlement::Yield {
            error_code,
            checkpoint,
            not_before_ms,
        }) = m7_settlement(
            &aggregate,
            Err(EvaluationError::Deferred(
                "fixture refused busy scratch".into(),
            )),
            IDENTITY,
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
    // Every ending publishes with its record; a skipped evaluation records
    // nothing, and an interrupted one yields whatever it concluded.
    let kind = |evaluation| {
        m7_settlement(&aggregate, evaluation, IDENTITY, false, 0)
            .unwrap()
            .map(|outcome| {
                assert_eq!(outcome.identity, IDENTITY);
                (outcome.outcome, outcome.gate)
            })
    };
    assert_eq!(kind(Ok(Evaluated::Skipped)), None);
    assert_eq!(
        kind(Ok(Evaluated::NotApplicable)),
        Some((M7OutcomeKind::NotApplicable, None))
    );
    assert_eq!(
        kind(Ok(Evaluated::Result)),
        Some((M7OutcomeKind::Result, None))
    );
    assert_eq!(
        kind(Err(EvaluationError::Unsupported(
            "unsupported fixture geometry".into()
        ))),
        Some((
            M7OutcomeKind::Unsupported,
            Some("unsupported fixture geometry".into())
        ))
    );
    assert_eq!(
        kind(Err(EvaluationError::Inconclusive(
            "authored rate out of band".into()
        ))),
        Some((
            M7OutcomeKind::Inconclusive,
            Some("authored rate out of band".into())
        ))
    );
    assert!(matches!(
        m7_settlement(&aggregate, Ok(Evaluated::Result), IDENTITY, true, 0),
        Err(JobSettlement::Yield { .. })
    ));
}

#[tokio::test]
async fn retained_fel_head_rebuild_skips_m7_and_preserves_fenced_title_settlement() {
    use super::super::m7_title::EvaluationError;
    use plurx_core::scan::dv_title::{DvTitleStats, TitleConstant, TitleElKind};
    use std::cell::Cell;

    let demand = TitleDemand {
        file_id: 1,
        probe_revision: 2,
        source_size: 3,
        source_mtime: 4,
        probe_json: "{}".into(),
        queued_job_id: Some("native-fel-head-job".into()),
        active_job_id: None,
        playback_requested: false,
        playback_recipes: Vec::new(),
        scanner_digest: Some("scanner-fixture".into()),
        scanner_generation: 1,
        aggregate_current: true,
        fel_head_target: Some("title-node".into()),
        fel_head_recipe: None,
    };
    let aggregate = TitleOutput {
        file_id: demand.file_id,
        probe_revision: demand.probe_revision,
        source_size: demand.source_size,
        source_mtime: demand.source_mtime,
        scanner_digest: demand.scanner_digest.clone().unwrap(),
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

    for (interrupted, fence_accepts) in [(false, true), (false, false), (true, true)] {
        let published = Cell::new(false);
        let outcome = complete_title(
            &demand,
            aggregate.clone(),
            IDENTITY,
            async { panic!("an already-settled head must not start M7 evaluation") },
            || interrupted,
            |output, m7| {
                let published = &published;
                let expected = &aggregate;
                async move {
                    published.set(true);
                    assert_eq!(m7, None, "a head rebuild keeps the recorded outcome");
                    assert_eq!(output.probe_revision, expected.probe_revision);
                    assert_eq!(output.stats, expected.stats);
                    Ok(fence_accepts)
                }
            },
            || panic!("no publication failure was injected"),
        )
        .await;
        assert_eq!(published.get(), !interrupted);
        match (interrupted, fence_accepts, outcome) {
            (false, true, Ok(())) => {}
            (false, false, Err(JobSettlement::Stop { error_code })) => {
                assert_eq!(error_code, "title_publication_fenced");
            }
            (true, _, Err(JobSettlement::Yield { error_code, .. })) => {
                assert_eq!(error_code.as_deref(), Some("title_interrupted"));
            }
            (_, _, outcome) => panic!("unexpected settlement {outcome:?}"),
        }
    }

    let ordinary = TitleDemand {
        fel_head_target: None,
        queued_job_id: Some("ordinary-title-job".into()),
        ..demand
    };
    let outcome = complete_title(
        &ordinary,
        aggregate.clone(),
        IDENTITY,
        async {
            Err(EvaluationError::Deferred(
                "fixture refused busy scratch".into(),
            ))
        },
        || false,
        |_, _| async { panic!("deferred M7 must yield before fenced publication") },
        || panic!("ordinary maintenance has not reached publication"),
    )
    .await;
    let Err(JobSettlement::Yield {
        error_code,
        checkpoint,
        ..
    }) = outcome
    else {
        panic!("ordinary maintenance must propagate an M7 deferral");
    };
    assert_eq!(error_code.as_deref(), Some("m7_retry_pending"));
    let checkpoint = checkpoint.unwrap();
    let restored: TitleOutput =
        serde_json::from_value(checkpoint["fel_title_aggregate"].clone()).unwrap();
    assert_eq!(restored.probe_revision, aggregate.probe_revision);
    assert_eq!(restored.stats, aggregate.stats);

    // A refused evaluation is published with the aggregate, in one command.
    let published = Cell::new(false);
    let outcome = complete_title(
        &ordinary,
        aggregate.clone(),
        IDENTITY,
        async {
            Err(EvaluationError::Unsupported(
                "current F+ outcome has no fitted scenes".into(),
            ))
        },
        || false,
        |output, m7| {
            let published = &published;
            async move {
                published.set(true);
                assert_eq!(output.probe_revision, 2);
                let m7 = m7.expect("the refusal is recorded");
                assert_eq!(m7.outcome, M7OutcomeKind::Unsupported);
                assert_eq!(m7.identity, IDENTITY);
                assert_eq!(
                    m7.gate.as_deref(),
                    Some("current F+ outcome has no fitted scenes")
                );
                Ok(true)
            }
        },
        || panic!("no publication failure was injected"),
    )
    .await;
    assert!(published.get());
    assert!(outcome.is_ok());
}
