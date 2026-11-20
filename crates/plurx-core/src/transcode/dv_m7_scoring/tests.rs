use super::*;
use crate::transcode::dv_refit::{apply_to_rpu, servable, RefitCurves};
use dolby_vision::rpu::ConversionMode;

#[test]
fn signed_composition_and_quantization_boundaries() {
    assert_eq!(signed_q13_floor(-8193), -2);
    assert_eq!(signed_q13_floor(-8192), -1);
    assert_eq!(signed_q13_floor(-1), -1);
    assert_eq!(signed_q13_floor(8191), 0);
    assert_eq!(signed_q13_floor(8192), 1);
    assert_eq!(continuous_residual(-1), -0.5 / 4096.0);
    assert_eq!(continuous_residual(0), 0.0);
    assert_eq!(continuous_residual(1), 0.5 / 4096.0);
    assert_eq!(
        ModelInput::from_residual([256; 3], [-2, -1, 1]).composed,
        [1023, 1024, 1025]
    );
    assert_eq!(
        ModelInput::from_residual([0, 1023, 1023], [-512, 3, 511]).composed,
        [0, 4095, 4095]
    );
    let fitted = ModelInput::from_served([256; 3], [1023, 1024, 1025]);
    assert_eq!(fitted.centered, [-2, 0, 1]);
    assert_eq!(
        ModelInput::from_residual(fitted.base, fitted.centered).composed,
        fitted.composed
    );
    assert_eq!(quantize_rgb([-0.25, 0.5, 1.25]), Some([0, 32768, 65535]));
    assert_eq!(quantize_rgb([0.0, f64::NAN, 1.0]), None);
    assert_eq!(dot([1e16, -1e16, 1.0], [1.0; 3]), 1.0);
}

#[test]
fn four_model_rgb16_vectors_include_superwhite_before_pq() {
    // Values computed from the analytic equations in 75-digit decimal
    // arithmetic at synthetic sample points, with exact integer Q13 for B.
    let color = ColorTransform::new(DmCoefficients::CALIBRATED);
    let vectors = [
        (
            ModelInput::from_residual([128, 500, 540], [-1, 1, -2]),
            [
                [7764, 3770, 3147],
                [7779, 3768, 3170],
                [7781, 3770, 3171],
                [7781, 3770, 3171],
            ],
        ),
        (
            ModelInput::from_residual([925, 512, 512], [85, -20, 77]),
            [
                [65535, 65302, 65373],
                [65535, 65242, 65336],
                [65535, 65257, 65351],
                [65535, 65258, 65352],
            ],
        ),
        (
            ModelInput::from_residual([256, 514, 514], [-2, -2, 2]),
            [
                [14602, 14228, 14571],
                [14611, 14216, 14583],
                [14615, 14220, 14586],
                [14614, 14220, 14587],
            ],
        ),
    ];
    for (input, expected) in vectors {
        for (model, expected) in Model::ALL.into_iter().zip(expected) {
            assert_eq!(color.rgb(model, input), Ok(expected), "{model:?}");
        }
    }
    assert!(pq_decode_lower_clamped(1.01) > 1.0);
}

#[test]
fn matched_models_score_one_reference_and_use_its_band_for_both_arms() {
    let mut served_curves = ComposerCurves {
        luma: [[127.0 / 4096.0, 0.0, 0.0]; 8],
        chroma: [[0.0; 22]; 2],
    };
    // luma on the second interval at the pivot, plus Q12 half-code ties
    // and clipping at higher pivots.
    served_curves.luma[1][0] = 510.5 / 4096.0;
    served_curves.luma[2][0] = -0.125;
    served_curves.luma[3][0] = 1.125;
    served_curves.chroma[0][0] = 2000.0 / 4096.0;
    served_curves.chroma[1][0] = 2161.0 / 4096.0;
    assert_eq!(served_curves.reshape([127, 500, 540]), [127, 2000, 2161]);
    assert_eq!(served_curves.reshape([128, 500, 540]), [511, 2000, 2161]);
    assert_eq!(served_curves.reshape([256, 500, 540])[0], 0);
    assert_eq!(served_curves.reshape([384, 500, 540])[0], 4095);
    let scorer = FrameScorer {
        served_curves,
        color: ColorTransform::new(DmCoefficients::CALIBRATED),
    };
    let sample = LatticeSample {
        source_bl: [128, 500, 540],
        source_el: [511, 513, 510],
        candidate: [223, 508, 536],
    };
    let scores = scorer.score_sample(sample).unwrap();
    // Independent 65-digit decimal BT.2124 calculations on quantized RGB.
    let expected = [
        (0.19387753992207118, 1.0002317089256714, 76.33352771019135),
        (0.1945774902563459, 1.034942612082369, 76.12432618252686),
        (0.19474480854313775, 1.0447927200390856, 76.14031547844678),
        (0.19474480854313775, 1.0447927200390856, 76.13979556762531),
    ];
    for (score, (nits, fplus, candidate)) in scores.into_iter().zip(expected) {
        assert!((score.reference_luminance_nits - nits).abs() < 1e-9);
        assert!((score.served_fplus_delta_e_itp - fplus).abs() < 1e-9);
        assert!((score.candidate_delta_e_itp - candidate).abs() < 1e-9);
        assert!((score.paired_advantage() - (fplus - candidate)).abs() < 1e-9);
        assert_eq!(score.luminance_band(), 1);
    }
    assert_eq!(
        ModelSampleScore {
            reference_luminance_nits: 0.1,
            ..scores[0]
        }
        .luminance_band(),
        1
    );
    assert_eq!(
        scorer.score_sample(LatticeSample {
            source_el: [1024, 513, 510],
            ..sample
        }),
        Err(ScoreError::InvalidSampleCode)
    );
}

#[test]
fn frame_requires_both_served_mappings_and_the_authored_candidate_color() {
    let mut source = DoviRpu::parse_unspec62_nalu(include_bytes!(
        "../../../tests/fixtures/dv-runtime/frame-000.nal"
    ))
    .unwrap();
    let mapping = source.rpu_data_mapping.as_mut().unwrap();
    mapping.set_empty_p81_mapping();
    let nlq = mapping.nlq.as_mut().unwrap();
    nlq.nlq_offset = [512; 3];
    nlq.vdr_in_max_int = [0; 3];
    nlq.vdr_in_max = [1_048_576; 3];
    nlq.linear_deadzone_slope_int = [0; 3];
    nlq.linear_deadzone_slope = [2048; 3];
    nlq.linear_deadzone_threshold_int = [0; 3];
    nlq.linear_deadzone_threshold = [0; 3];
    source.vdr_dm_data.as_mut().unwrap().set_p81_coeffs();
    source.modified = true;
    let source_nal = source.write_hevc_unspec62_nalu().unwrap();

    source.convert_with_mode(ConversionMode::To81).unwrap();
    let candidate = source.write_hevc_unspec62_nalu().unwrap();
    let pinned = servable(&RefitCurves::identity()).unwrap();
    let mut served = source.clone();
    apply_to_rpu(&mut served, &pinned).unwrap();
    let served = served.write_hevc_unspec62_nalu().unwrap();
    let paired = FrameScorer::from_rpus(&source_nal, &served, &served, &pinned, &candidate)
        .expect("verified paired sources");
    let baseline = FrameScorer::for_calibration(&source_nal, &served, &served, &pinned)
        .expect("verified baseline sources");
    let sample = LatticeSample {
        source_bl: [256, 481, 591],
        source_el: [520, 510, 512],
        candidate: [236, 501, 529],
    };
    assert_eq!(
        baseline
            .baseline_errors(sample.source_bl, sample.source_el)
            .unwrap(),
        paired
            .score_sample(sample)
            .unwrap()
            .map(|score| score.served_fplus_delta_e_itp),
    );
    assert_eq!(
        FrameScorer::for_calibration(&source_nal, &served, &candidate, &pinned).err(),
        Some(ScoreError::UnboundServedCurves),
    );
    assert_eq!(
        FrameScorer::from_rpus(&source_nal, &served, &candidate, &pinned, &candidate).err(),
        Some(ScoreError::UnboundServedCurves)
    );
    assert_eq!(
        FrameScorer::from_rpus(&candidate, &served, &served, &pinned, &candidate).err(),
        Some(ScoreError::UnsupportedSourceRpu)
    );
    source.vdr_dm_data.as_mut().unwrap().rgb_to_lms_coef8 += 1;
    let wrong_color = source.write_hevc_unspec62_nalu().unwrap();
    assert_eq!(
        FrameScorer::from_rpus(&source_nal, &served, &served, &pinned, &wrong_color).err(),
        Some(ScoreError::UnsupportedCandidateRpu)
    );
}
