use super::*;

fn test_directory() -> tempfile::TempDir {
    let parent = crate::testfixtures::fixture_dir();
    std::fs::create_dir_all(&parent).expect("creating the isolated aggregation fixture parent");
    tempfile::tempdir_in(parent).expect("creating an isolated aggregation fixture")
}

fn sample(nits: f64, served: f64, candidate: f64) -> [ModelSampleScore; MODELS] {
    [ModelSampleScore {
        reference_luminance_nits: nits,
        served_fplus_delta_e_itp: served,
        candidate_delta_e_itp: candidate,
    }; MODELS]
}

fn open(
    directory: &Path,
    shape: &[WindowShape],
    margin: f64,
) -> Result<Aggregator, AggregationError> {
    Aggregator::new(
        directory,
        shape,
        FrozenMargin::new(margin)?,
        Aggregator::required_spill_bytes(shape)?,
    )
}

#[test]
fn pooled_percentiles_select_original_samples_at_global_ranks() {
    let directory = test_directory();
    let shape = [WindowShape {
        frames: 1,
        sites_per_frame: 4,
    }; 2];
    let mut run = open(directory.path(), &shape, 0.25).unwrap();
    run.push_frame(
        &[
            sample(0.0, 1.0, 1.0),
            sample(0.1, 2.0, 1.0),
            sample(10.0, 10.0, 9.0),
            sample(100.0, 100.0, 101.0),
        ],
        || false,
    )
    .unwrap();
    run.push_frame(
        &[
            sample(0.099, 3.0, 1.0),
            sample(0.9, 4.0, 4.0),
            sample(10.0, 20.0, 18.0),
            sample(100.0, 200.0, 192.0),
        ],
        || false,
    )
    .unwrap();
    let report = run.finish(|| false).unwrap();
    assert_eq!(report.disposition, Disposition::Satisfied);
    let [first, second] = report.windows.as_slice() else {
        panic!("two evaluation windows");
    };
    let stats = report.pooled_statistics[0];
    assert_eq!(first.statistics[0].served_fplus.full.p99, 10.0);
    assert_eq!(second.statistics[0].served_fplus.full.p99, 20.0);
    assert_eq!(stats.served_fplus.full.count, 8);
    assert_eq!(stats.served_fplus.full.mean, 42.5);
    assert_eq!(stats.served_fplus.full.p95, 100.0);
    assert_eq!(stats.served_fplus.full.p99, 100.0);
    assert_eq!(stats.candidate.full.mean, 40.875);
    let bands = [Some(2), Some(2), None, Some(2), Some(2)];
    assert_eq!(stats.served_fplus.bands.map(|s| s.map(|s| s.count)), bands);
    assert_eq!(stats.candidate.bands.map(|s| s.map(|s| s.count)), bands);
    assert_eq!(stats.served_fplus.bands[4].unwrap().p99, 100.0);
    assert_eq!(stats.candidate.bands[4].unwrap().p99, 101.0);
    assert_eq!(stats.paired.candidate_closer_count, 5);
    assert_eq!(stats.paired.candidate_worse_count, 1);
    assert_eq!(stats.paired.ties, 2);
    assert_eq!(stats.paired.advantage.mean, 1.625);
    assert_eq!(stats.paired.advantage.min, -1.0);
    assert_eq!(stats.paired.advantage.max, 8.0);
    assert_eq!(stats.paired.advantage.p99, 2.0);
}

#[test]
fn percentiles_have_distinct_zero_based_ranks() {
    let mut accumulator = Accumulator::new(101);
    for value in 0..=100 {
        accumulator.observe(f64::from(value));
    }
    let stats = accumulator.finish().unwrap().unwrap();
    assert_eq!(stats.p95, 95.0);
    assert_eq!(stats.p99, 99.0);
}

#[test]
fn failing_model_takes_precedence_over_missing_band() {
    let directory = test_directory();
    let shape = [WindowShape {
        frames: 2,
        sites_per_frame: 2,
    }];
    for fail_dark in [false, true] {
        let mut run = open(directory.path(), &shape, 0.5).unwrap();
        let shadow = sample(0.02, 10.0, 1.0);
        let mut dark = sample(0.5, 10.0, 1.0);
        // One model's reference misses shadow in the second frame.
        let mut second_shadow = shadow;
        second_shadow[1].reference_luminance_nits = 1.0;
        // One other model regresses in required dark p99 while gaining
        // enough over the full window to satisfy the mean margin.
        if fail_dark {
            dark[2].candidate_delta_e_itp = 11.0;
        }
        run.push_frame(&[shadow, dark], || false).unwrap();
        run.push_frame(&[second_shadow, dark], || false).unwrap();
        let report = run.finish(|| false).unwrap();
        let checks = report.windows[0].checks;
        assert_eq!(checks[0].largest_frame_shadow_p99, Check::Passed);
        assert_eq!(checks[1].largest_served_fplus_frame_shadow_p99, Some(10.0));
        assert_eq!(checks[1].largest_frame_shadow_p99, Check::MissingBand);
        assert_eq!(checks[2].mean_gain.check, Check::Passed);
        if fail_dark {
            assert_eq!(checks[2].dark_p99, Check::Failed);
            assert_eq!(report.disposition, Disposition::Refused);
        } else {
            assert_eq!(checks[2].dark_p99, Check::Passed);
            assert_eq!(report.disposition, Disposition::Inconclusive);
        }
    }
}

#[test]
fn window_mean_must_pass_even_if_pooled_mean_gains() {
    let directory = test_directory();
    let shape = [WindowShape {
        frames: 1,
        sites_per_frame: 2,
    }; 2];
    let mut run = open(directory.path(), &shape, 0.5).unwrap();
    run.push_frame(&[sample(0.0, 1.0, 1.0), sample(0.5, 1.0, 1.0)], || false)
        .unwrap();
    run.push_frame(&[sample(0.0, 4.0, 1.0), sample(0.5, 4.0, 1.0)], || false)
        .unwrap();
    let report = run.finish(|| false).unwrap();
    assert_eq!(report.windows[0].checks[0].mean_gain.check, Check::Failed);
    assert_eq!(report.pooled_mean_gain[0].check, Check::Passed);
    assert_eq!(report.disposition, Disposition::Refused);
}

#[test]
fn refuses_budget_truncated_or_incomplete_capture_and_cancellation() {
    let directory = test_directory();
    let shape = [WindowShape {
        frames: 1,
        sites_per_frame: 1,
    }];
    let budget = Aggregator::required_spill_bytes(&shape).unwrap();
    assert_eq!(budget, 96);
    let full_window = [WindowShape {
        frames: 48,
        sites_per_frame: 503_040,
    }];
    assert_eq!(
        Aggregator::required_spill_bytes(&full_window).unwrap(),
        2_318_008_320
    );
    let full_raster = [WindowShape {
        frames: 48,
        sites_per_frame: 518_400,
    }];
    assert_eq!(
        Aggregator::required_spill_bytes(&full_raster).unwrap(),
        2_388_787_200
    );
    let oversized = [WindowShape {
        sites_per_frame: 518_401,
        ..full_raster[0]
    }];
    assert!(matches!(
        Aggregator::required_spill_bytes(&oversized),
        Err(AggregationError::UnsupportedShape)
    ));
    assert!(matches!(
        Aggregator::new(
            directory.path(),
            &shape,
            FrozenMargin::new(0.0).unwrap(),
            budget - 1,
        ),
        Err(AggregationError::SpillBudget { .. })
    ));

    let mut run = open(directory.path(), &shape, 0.0).unwrap();
    assert!(matches!(
        run.push_frame(&[], || false),
        Err(AggregationError::Coverage)
    ));
    assert!(matches!(
        run.finish(|| false),
        Err(AggregationError::Coverage)
    ));
    assert!(matches!(
        open(directory.path(), &shape, 0.0)
            .unwrap()
            .finish(|| false),
        Err(AggregationError::Coverage)
    ));

    let scores = [sample(0.0, 1.0, 1.0)];
    let mut run = open(directory.path(), &shape, 0.0).unwrap();
    run.push_frame(&scores, || false).unwrap();
    run.spill.flush().unwrap();
    run.spill.get_ref().set_len(budget - 1).unwrap();
    assert!(matches!(
        run.finish(|| false),
        Err(AggregationError::SpillCoverage)
    ));

    let mut run = open(directory.path(), &shape, 0.0).unwrap();
    assert!(matches!(
        run.push_frame(&[sample(f64::NAN, 1.0, 1.0)], || false),
        Err(AggregationError::InvalidScore)
    ));
    assert!(matches!(
        run.push_frame(&scores, || false),
        Err(AggregationError::Coverage)
    ));

    let mut run = open(directory.path(), &shape, 0.0).unwrap();
    run.push_frame(&scores, || false).unwrap();
    assert!(matches!(
        run.finish(|| true),
        Err(AggregationError::Cancelled)
    ));
}
