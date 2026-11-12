use super::*;
use dolby_vision::rpu::ConversionMode;
use serde_json::Value;

/// The window statistics and the safe fit `rpu_refit_eval.py --dump-fit-stats`
/// wrote for each fixture (all 48 frames, stride-8 lattice), after the
/// protocol's amendments 3–4. The script's numbers are the reference.
fn fixture(name: &str) -> Value {
    let text = match name {
        "michael5400" => include_str!("../../../tests/fixtures/dv-refit/michael5400.json"),
        "michael1800" => include_str!("../../../tests/fixtures/dv-refit/michael1800.json"),
        "beekeeper1800" => include_str!("../../../tests/fixtures/dv-refit/beekeeper1800.json"),
        other => panic!("no fixture {other}"),
    };
    serde_json::from_str(text).expect("fixture JSON")
}

fn floats(v: &Value) -> Vec<f64> {
    match v {
        Value::Array(items) => items.iter().flat_map(floats).collect(),
        Value::Number(n) => vec![n.as_f64().expect("f64")],
        other => panic!("not a number: {other}"),
    }
}

fn stats(f: &Value) -> RefitStats {
    RefitStats::from_parts(
        floats(&f["ysum"]),
        floats(&f["ycnt"]),
        floats(&f["xtx"]),
        floats(&f["xty"]),
    )
    .expect("fixture shape")
}

const FIXTURES: [&str; 3] = ["michael5400", "michael1800", "beekeeper1800"];

#[test]
fn the_safe_fit_matches_the_offline_reference_on_every_fixture() {
    for name in FIXTURES {
        let f = fixture(name);
        let curves = stats(&f).solve().expect("fit");
        let want_luma = floats(&f["fp_safe"]["poly"]);
        let want_mmr = floats(&f["fp_safe"]["mmr"]);
        let got_luma: Vec<f64> = curves.luma.iter().flatten().copied().collect();
        let got_mmr: Vec<f64> = curves.mmr.iter().flatten().copied().collect();
        // Both sides are quantised to 2^-23; a last-place flip between two
        // solvers is the only difference allowed.
        for (i, (g, w)) in got_luma.iter().zip(&want_luma).enumerate() {
            assert!(
                (g - w).abs() <= 2.0 / FIT_QUANT,
                "{name} luma[{i}]: {g} vs {w}"
            );
        }
        for (i, (g, w)) in got_mmr.iter().zip(&want_mmr).enumerate() {
            assert!(
                (g - w).abs() <= 2.0 / FIT_QUANT,
                "{name} mmr[{i}]: {g} vs {w}"
            );
        }

        let composer = curves.composer();
        let want_composer_luma = floats(&f["composer_domain"]["luma"]);
        let want_composer_chroma = floats(&f["composer_domain"]["chroma"]);
        let got_composer_luma: Vec<f64> = composer.luma.iter().flatten().copied().collect();
        let got_composer_chroma: Vec<f64> = composer.chroma.iter().flatten().copied().collect();
        for (g, w) in got_composer_luma.iter().zip(&want_composer_luma) {
            assert!((g - w).abs() <= 1e-6, "{name} composer luma {g} vs {w}");
        }
        for (g, w) in got_composer_chroma.iter().zip(&want_composer_chroma) {
            assert!((g - w).abs() <= 1e-6, "{name} composer chroma {g} vs {w}");
        }

        let d = curves.diagnostics();
        let want = &f["diagnostics"];
        assert!(d.luma_monotone, "{name}: the safe luma curve rises");
        assert_eq!(Some(d.luma_monotone), want["luma_monotone"].as_bool());
        for (got, key) in [
            (
                d.luma_max_abs_minus_identity,
                "luma_max_abs_minus_identity_codes12",
            ),
            (
                d.luma_max_excess_step_at_pivots,
                "luma_max_excess_step_at_pivots_codes12",
            ),
            (
                d.chroma_max_abs_minus_identity_on_grid,
                "chroma_max_abs_minus_identity_codes12_on_grid",
            ),
            (d.max_abs_coefficient, "max_abs_coefficient"),
        ] {
            let w = want[key].as_f64().expect(key);
            assert!(
                (got - w).abs() <= 1e-3 + 1e-6 * w.abs(),
                "{name} {key}: {got} vs {w}"
            );
        }
        assert!(
            d.luma_max_excess_step_at_pivots < 0.01,
            "{name}: pieces join without a step"
        );
    }
}

#[test]
fn a_composition_equal_to_the_base_layer_fits_identity_curves() {
    let mut s = RefitStats::new();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..200_000 {
        let base = [
            64 + (next() % 877) as u16,
            448 + (next() % 129) as u16,
            448 + (next() % 129) as u16,
        ];
        s.add(base, base.map(|c| 4 * c));
    }
    let curves = s.solve().expect("fit");
    let composer = curves.composer();
    for y in (0..1024u16).step_by(7) {
        for (u, v) in [(512u16, 512u16), (480, 540), (560, 470)] {
            let out = composer.reshape([y, u, v]);
            for (got, want) in out.iter().zip([4 * y, 4 * u, 4 * v]) {
                assert!(
                    (i32::from(*got) - i32::from(want)).abs() <= 1,
                    "identity data maps ({y},{u},{v}) to {out:?}"
                );
            }
        }
    }
}

#[test]
fn sparse_and_non_monotone_scenes_still_give_a_rising_continuous_curve() {
    // A scene that uses a handful of luma codes, with conditional means that
    // fall as the code rises: the unconstrained fit's failure case.
    let mut s = RefitStats::new();
    for (code, composed, n) in [
        (200u16, 900u16, 500),
        (210, 820, 400),
        (650, 2700, 50),
        (655, 2500, 30),
    ] {
        for _ in 0..n {
            s.add([code, 512, 512], [composed, 2048, 2048]);
        }
    }
    let d = s.solve().expect("fit").diagnostics();
    assert!(d.luma_monotone, "{d:?}");
    assert!(d.luma_max_excess_step_at_pivots < 0.01, "{d:?}");
    assert!(d.max_abs_coefficient < 63.0, "{d:?}");
}

#[test]
fn an_empty_scene_is_refused() {
    assert_eq!(RefitStats::new().solve(), Err(RefitError::Empty));
}

#[test]
fn merging_scenes_is_adding_their_samples() {
    let (mut a, mut b, mut both) = (RefitStats::new(), RefitStats::new(), RefitStats::new());
    for i in 0..500u16 {
        let base = [100 + i, 500 + i % 30, 520 - i % 25];
        let comp = [4 * base[0] + i % 9, 4 * base[1], 4 * base[2] + 3];
        if i % 3 == 0 {
            a.add(base, comp)
        } else {
            b.add(base, comp)
        }
        both.add(base, comp);
    }
    a.merge(&b);
    assert_eq!(a.samples(), both.samples());
    // Summation order differs, so the sums agree to rounding, not to the bit.
    let (merged, single) = (a.solve().expect("fit"), both.solve().expect("fit"));
    for (m, s) in merged
        .luma
        .iter()
        .flatten()
        .chain(merged.mmr.iter().flatten())
        .zip(
            single
                .luma
                .iter()
                .flatten()
                .chain(single.mmr.iter().flatten()),
        )
    {
        assert!((m - s).abs() <= 2.0 / FIT_QUANT, "{m} vs {s}");
    }
}

fn converted_fixture_rpu() -> DoviRpu {
    let mut rpu =
        DoviRpu::parse_unspec62_nalu(&crate::testfixtures::p7_rpu()).expect("fixture parses");
    rpu.convert_with_mode(ConversionMode::To81)
        .expect("converts");
    rpu
}

fn join(int: i64, frac: u64, denom: u64) -> f64 {
    int as f64 + frac as f64 / (1u64 << denom) as f64
}

#[test]
fn curves_written_into_a_real_rpu_read_back_exactly() {
    let servable = servable(&stats(&fixture("michael1800")).solve().expect("fit")).expect("safe");
    let curves = servable.curves().clone();
    let mut rpu = converted_fixture_rpu();
    apply_to_rpu(&mut rpu, &servable).expect("writable");
    let bytes = rpu.write_hevc_unspec62_nalu().expect("writes");
    let back = DoviRpu::parse_unspec62_nalu(&bytes).expect("reads back");
    assert_eq!(back.dovi_profile, 8);
    let d = back.header.coefficient_log2_denom;
    assert_eq!(d, 23);
    // Rounding to the nearest 2^-23: half a step, plus float noise.
    let tolerance = 0.5 / (1u64 << d) as f64 + 1e-12;
    let mapping = back.rpu_data_mapping.as_ref().expect("mapping");
    assert_eq!(mapping.curves[0].pivots, LUMA_PIVOT_DELTAS.to_vec());
    let poly = mapping.curves[0]
        .polynomial
        .as_ref()
        .expect("luma polynomial");
    for (p, piece) in curves.luma.iter().enumerate() {
        assert_eq!(poly.poly_order_minus1[p], 1);
        for (k, want) in piece.iter().enumerate() {
            let got = join(poly.poly_coef_int[p][k], poly.poly_coef[p][k], d);
            assert!(
                (got - want).abs() <= tolerance,
                "luma piece {p} coef {k}: {got} vs {want}"
            );
        }
    }
    for (ch, want) in curves.chroma.iter().enumerate() {
        let mmr = mapping.curves[ch + 1].mmr.as_ref().expect("chroma MMR");
        assert_eq!(mmr.mmr_order_minus1[0], 2);
        let constant = join(mmr.mmr_constant_int[0], mmr.mmr_constant[0], d);
        assert!((constant - want[0]).abs() <= tolerance);
        for order in 0..3 {
            for term in 0..7 {
                let got = join(
                    mmr.mmr_coef_int[0][order][term],
                    mmr.mmr_coef[0][order][term],
                    d,
                );
                assert!((got - want[1 + 7 * order + term]).abs() <= tolerance);
            }
        }
    }
    // The rest of the RPU survives: the DM metadata is the converted RPU's own.
    let original = converted_fixture_rpu();
    assert_eq!(
        format!("{:?}", back.vdr_dm_data),
        format!("{:?}", original.vdr_dm_data),
        "only the mapping changes"
    );
}

#[test]
fn an_unwritable_coefficient_leaves_the_rpu_untouched() {
    let mut curves = stats(&fixture("beekeeper1800"))
        .solve()
        .expect("fit")
        .composer();
    curves.chroma[1][21] = 70.0;
    // Built directly: servable() would refuse these curves before any RPU saw them.
    let curves = ServableCurves(curves);
    let mut rpu = converted_fixture_rpu();
    let before = rpu.write_hevc_unspec62_nalu().expect("writes");
    assert_eq!(
        apply_to_rpu(&mut rpu, &curves),
        Err(RefitError::Unwritable(70.0))
    );
    assert_eq!(rpu.write_hevc_unspec62_nalu().expect("writes"), before);
}

#[test]
fn only_a_converted_rpu_takes_curves() {
    let mut p7 = DoviRpu::parse_unspec62_nalu(&crate::testfixtures::p7_rpu()).expect("parses");
    let curves = servable(&stats(&fixture("michael5400")).solve().expect("fit")).expect("safe");
    assert_eq!(
        apply_to_rpu(&mut p7, &curves),
        Err(RefitError::NotProfile8(7))
    );
}

#[test]
fn every_fixture_fit_is_servable_and_identity_is_too() {
    for name in FIXTURES {
        servable(&stats(&fixture(name)).solve().expect("fit")).expect(name);
    }
    servable(&RefitCurves::identity()).expect("identity");
}

#[test]
fn servable_refuses_falling_flat_and_wild_curves() {
    let mut falling = RefitCurves::identity();
    falling.luma[3] = [0.6, -0.2, 0.0];
    assert_eq!(servable(&falling), Err(Unservable::NotMonotone));
    let mut flat = RefitCurves::identity();
    flat.luma[2] = [flat.luma_at(256) / OUT_SCALE, 0.0, 0.0];
    assert!(matches!(
        servable(&flat),
        Err(Unservable::NotMonotone) | Err(Unservable::Flattens(_))
    ));
    let mut wild = RefitCurves::identity();
    wild.mmr[21][0] = 0.5;
    assert!(matches!(
        servable(&wild),
        Err(Unservable::ChromaDeparture(_))
    ));
}

#[test]
fn reshape_matches_the_offline_decoder_semantics_exactly() {
    let f = fixture("michael1800");
    let luma = floats(&f["composer_domain"]["luma"]);
    let chroma = floats(&f["composer_domain"]["chroma"]);
    let mut curves = ComposerCurves {
        luma: [[0.0; 3]; LUMA_PIECES],
        chroma: [[0.0; MMR_TERMS]; 2],
    };
    for (i, v) in luma.iter().enumerate() {
        curves.luma[i / 3][i % 3] = *v;
    }
    for (i, v) in chroma.iter().enumerate() {
        curves.chroma[i / MMR_TERMS][i % MMR_TERMS] = *v;
    }
    let vectors: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/dv-refit/reshape-michael1800.json"
    ))
    .expect("vectors");
    let bases = vectors["base"].as_array().expect("bases");
    let wants = vectors["composed"].as_array().expect("composed");
    assert!(bases.len() > 500);
    for (b, w) in bases.iter().zip(wants) {
        let b = floats(b);
        let w = floats(w);
        let base = [b[0] as u16, b[1] as u16, b[2] as u16];
        let want = [w[0] as u16, w[1] as u16, w[2] as u16];
        assert_eq!(curves.reshape(base), want, "{base:?}");
    }
}

#[test]
fn add_builds_the_same_normal_equations_as_a_direct_sum() {
    let samples = [
        ([100u16, 500, 520], [410u16, 2010, 2075]),
        ([700, 430, 610], [2790, 1700, 2450]),
        ([33, 512, 512], [140, 2048, 2048]),
        ([1023, 1023, 0], [4095, 4095, 3]),
    ];
    let mut s = RefitStats::new();
    for (b, c) in samples {
        s.add(b, c);
    }
    let mut xtx = vec![0.0; MMR_TERMS * MMR_TERMS];
    let mut xty = vec![0.0; MMR_TERMS * 2];
    for (b, c) in samples {
        let y = f64::from(b[0]) / 1023.0;
        let u = f64::from(b[1]) / 1023.0;
        let v = f64::from(b[2]) / 1023.0;
        // The basis written out by hand: 1, then y u v yu yv uv yuv to the 1st, 2nd and 3rd power.
        let mut x = vec![1.0];
        for p in 1..=3 {
            for t in [y, u, v, y * u, y * v, u * v, y * u * v] {
                x.push(t.powi(p));
            }
        }
        for i in 0..MMR_TERMS {
            for j in 0..MMR_TERMS {
                xtx[i * MMR_TERMS + j] += x[i] * x[j];
            }
            xty[2 * i] += x[i] * f64::from(c[1]) / 4095.0;
            xty[2 * i + 1] += x[i] * f64::from(c[2]) / 4095.0;
        }
    }
    for (a, b) in s.xtx.iter().zip(&xtx).chain(s.xty.iter().zip(&xty)) {
        assert!((a - b).abs() <= 1e-12 * b.abs().max(1.0), "{a} vs {b}");
    }
    assert_eq!(s.ycnt[100], 1.0);
    assert_eq!(s.ysum[700], 2790.0);
}

#[test]
fn squared_error_from_statistics_equals_the_direct_sum() {
    let curves = stats(&fixture("michael5400")).solve().expect("fit");
    let mut s = RefitStats::new();
    let mut direct_luma = 0.0;
    let mut direct_chroma = 0.0;
    for i in 0..3000u32 {
        let b = [
            (64 + i * 7 % 877) as u16,
            (440 + i * 13 % 140) as u16,
            (450 + i * 11 % 130) as u16,
        ];
        let c = [
            (4 * u32::from(b[0]) + i % 23) as u16,
            (4 * u32::from(b[1]) + i % 17) as u16,
            (4 * u32::from(b[2])) as u16,
        ];
        s.add(b, c);
        direct_luma += (f64::from(c[0]) - curves.luma_at(usize::from(b[0]))).powi(2);
        let x = mmr_basis(
            f64::from(b[0]) / 1023.0,
            f64::from(b[1]) / 1023.0,
            f64::from(b[2]) / 1023.0,
        );
        for ch in 0..2 {
            let fit: f64 = (0..MMR_TERMS)
                .map(|k| x[k] * curves.mmr[k][ch])
                .sum::<f64>()
                * 4095.0;
            direct_chroma += (f64::from(c[ch + 1]) - fit).powi(2);
        }
    }
    let e = s.squared_error(&curves).expect("second moments");
    let luma: f64 = e.luma.iter().sum();
    assert!(
        (luma - direct_luma).abs() <= 1e-6 * direct_luma,
        "{luma} vs {direct_luma}"
    );
    assert!(
        (e.chroma - direct_chroma).abs() <= 1e-6 * direct_chroma,
        "{} vs {direct_chroma}",
        e.chroma
    );
    assert!(
        stats(&fixture("michael5400"))
            .squared_error(&curves)
            .is_none(),
        "loaded statistics have no second moments"
    );
}

#[test]
fn held_out_samples_decide_whether_a_fit_is_served() {
    // The fit is learned where the composition sits 6 codes above the base
    // layer; held-out samples from the same scene keep it, samples where the
    // composition equals the base layer refuse it.
    let scene = |offset: u16, phase: u16| {
        let mut s = RefitStats::new();
        for i in 0..20_000u32 {
            let b = [(100 + (i * 37 + u32::from(phase)) % 800) as u16, 512, 512];
            s.add(b, [4 * b[0] + offset, 2048, 2048]);
        }
        s
    };
    let curves = scene(6, 0).solve().expect("fit");
    let kept = held_out_verdict(&curves, &scene(6, 3)).expect("beats identity");
    assert!(kept.gain > 0.9, "{kept:?}");
    assert!(matches!(
        held_out_verdict(&curves, &scene(0, 3)),
        Err(Unservable::WorseThanIdentity(_))
    ));
    assert_eq!(
        held_out_verdict(&curves, &stats(&fixture("michael5400"))),
        Err(Unservable::NoHeldOutSamples)
    );
}

#[test]
fn a_finer_denominator_is_replaced_and_an_rpu_without_mapping_is_left_alone() {
    let curves = servable(&stats(&fixture("michael1800")).solve().expect("fit")).expect("safe");
    let mut rpu = converted_fixture_rpu();
    rpu.header.coefficient_log2_denom = 20;
    rpu.header.coefficient_log2_denom_length = 20;
    apply_to_rpu(&mut rpu, &curves).expect("writable");
    let back = DoviRpu::parse_unspec62_nalu(&rpu.write_hevc_unspec62_nalu().expect("writes"))
        .expect("reads");
    assert_eq!(back.header.coefficient_log2_denom, 23);
    let poly = back.rpu_data_mapping.as_ref().expect("mapping").curves[0]
        .polynomial
        .clone()
        .expect("poly");
    let got = join(poly.poly_coef_int[4][1], poly.poly_coef[4][1], 23);
    assert!((got - curves.curves().luma[4][1]).abs() <= 0.5 / (1u64 << 23) as f64 + 1e-12);

    let mut reuse = converted_fixture_rpu();
    reuse.rpu_data_mapping = None;
    assert_eq!(
        apply_to_rpu(&mut reuse, &curves),
        Err(RefitError::NoMapping)
    );
}

#[test]
fn malformed_statistics_are_refused() {
    let f = fixture("beekeeper1800");
    let (ysum, ycnt, xtx, xty) = (
        floats(&f["ysum"]),
        floats(&f["ycnt"]),
        floats(&f["xtx"]),
        floats(&f["xty"]),
    );
    let mut nan = ysum.clone();
    nan[3] = f64::NAN;
    assert!(RefitStats::from_parts(nan, ycnt.clone(), xtx.clone(), xty.clone()).is_err());
    let mut negative = ycnt.clone();
    negative[10] = -1.0;
    assert!(RefitStats::from_parts(ysum.clone(), negative, xtx.clone(), xty.clone()).is_err());
    let mut inconsistent = xtx.clone();
    inconsistent[0] += 5.0;
    assert!(RefitStats::from_parts(ysum.clone(), ycnt.clone(), inconsistent, xty.clone()).is_err());
    assert!(RefitStats::from_parts(ysum, ycnt, xtx, xty).is_ok());
}

/// One scene exactly as `segment_decode_render --refit-stats` printed it for
/// the Michael5400 window: frames 0–47 of its source, `--area 0 0 32 32`
/// (the offline region of interest; the RPU's L5 says 42 rows top and
/// bottom), stride-8 fit lattice at phase 0 and held-out lattice at phase 4.
fn helper_scene() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/dv-refit/helper-scene-michael5400.json"
    ))
    .expect("helper scene JSON")
}

#[test]
fn a_helper_scene_deserialises_and_solves_to_the_offline_fit() {
    let scene = helper_scene();
    assert_eq!(scene["kind"], "refit_scene");
    assert_eq!(
        (scene["first_frame"].as_i64(), scene["frames"].as_i64()),
        (Some(0), Some(48))
    );
    assert_eq!(scene["time_base"], "1/1000");
    let fit: RefitStats = serde_json::from_value(scene["fit"].clone()).expect("fit stats");
    let held_out: RefitStats =
        serde_json::from_value(scene["held_out"].clone()).expect("held-out stats");
    // 480 columns × 262 rows of the 3840 × [32, 2128) area, 48 frames, on
    // both lattices.
    assert_eq!(fit.samples(), 48.0 * 480.0 * 262.0);
    assert_eq!(held_out.samples(), 48.0 * 480.0 * 262.0);
    // The same samples the offline script fitted: its per-code luma sums and
    // counts are integers and agree exactly; the normal equations agree to
    // summation order.
    let offline = fixture("michael5400");
    assert_eq!(fit.ycnt, floats(&offline["ycnt"]));
    assert_eq!(fit.ysum, floats(&offline["ysum"]));
    for (a, b) in fit
        .xtx
        .iter()
        .zip(floats(&offline["xtx"]))
        .chain(fit.xty.iter().zip(floats(&offline["xty"])))
    {
        assert!((a - b).abs() <= 1e-11 * b.abs().max(1.0), "{a} vs {b}");
    }
    let curves = fit.solve().expect("fit");
    let want: Vec<f64> = floats(&offline["fp_safe"]["poly"])
        .into_iter()
        .chain(floats(&offline["fp_safe"]["mmr"]))
        .collect();
    let got: Vec<f64> = curves
        .luma
        .iter()
        .flatten()
        .chain(curves.mmr.iter().flatten())
        .copied()
        .collect();
    assert_eq!(got.len(), want.len());
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert!(
            (g - w).abs() <= 2.0 / FIT_QUANT,
            "coefficient {i}: {g} vs {w}"
        );
    }
    // The held-out lattice scores the fit: it beats identity there too.
    let verdict = held_out_verdict(&curves, &held_out).expect("served on held-out samples");
    assert!(verdict.gain > 0.5, "{verdict:?}");
}

#[test]
fn a_malformed_helper_scene_is_refused() {
    let scene = helper_scene();
    let mut short = scene["fit"].clone();
    short["ysq"].as_array_mut().expect("ysq").pop();
    assert!(serde_json::from_value::<RefitStats>(short).is_err());
    let mut negative = scene["fit"].clone();
    negative["csq"][0] = serde_json::json!(-1.0);
    assert!(serde_json::from_value::<RefitStats>(negative).is_err());
    let mut uncounted = scene["fit"].clone();
    uncounted["xtx_upper"][0] = serde_json::json!(1.0);
    assert!(serde_json::from_value::<RefitStats>(uncounted).is_err());
    // Moments the samples cannot have: a luma sum of squares below sum²/n,
    // a sum of squares where no sample landed, Σ target² above the count,
    // a fractional count, an XᵀX entry above the count, and moments under a
    // flag that says there are none.
    let code = scene["fit"]["ycnt"]
        .as_array()
        .expect("ycnt")
        .iter()
        .position(|c| c.as_f64() > Some(10.0))
        .expect("a populated code");
    let empty = scene["fit"]["ycnt"]
        .as_array()
        .expect("ycnt")
        .iter()
        .position(|c| c.as_f64() == Some(0.0))
        .expect("an empty code");
    let mut low = scene["fit"].clone();
    low["ysq"][code] = serde_json::json!(1.0);
    let mut phantom = scene["fit"].clone();
    phantom["ysq"][empty] = serde_json::json!(5.0);
    let mut over = scene["fit"].clone();
    over["csq"][1] = serde_json::json!(1e12);
    let mut fractional = scene["fit"].clone();
    let c = fractional["ycnt"][code].as_f64().expect("count");
    fractional["ycnt"][code] = serde_json::json!(c + 0.5);
    let mut big = scene["fit"].clone();
    big["xtx_upper"][5] = serde_json::json!(1e12);
    let mut unflagged = scene["fit"].clone();
    unflagged["second_moments"] = serde_json::json!(false);
    for (name, bad) in [
        ("low", low),
        ("phantom", phantom),
        ("over", over),
        ("fractional", fractional),
        ("big", big),
        ("unflagged", unflagged),
    ] {
        assert!(serde_json::from_value::<RefitStats>(bad).is_err(), "{name}");
    }
    let mut missing = scene["fit"].clone();
    missing
        .as_object_mut()
        .expect("object")
        .remove("second_moments");
    assert!(serde_json::from_value::<RefitStats>(missing).is_err());
    // What a helper prints round-trips (serde_json's default float parser
    // may land one ulp away).
    let fit: RefitStats = serde_json::from_value(scene["fit"].clone()).expect("fit");
    let again: RefitStats =
        serde_json::from_str(&serde_json::to_string(&fit).expect("serialise")).expect("parse");
    assert_eq!(
        (fit.ycnt.clone(), fit.ysum.clone()),
        (again.ycnt.clone(), again.ysum.clone())
    );
    assert!(again.second_moments);
    for (a, b) in [
        (&fit.xtx, &again.xtx),
        (&fit.xty, &again.xty),
        (&fit.ysq, &again.ysq),
    ] {
        for (x, y) in a.iter().zip(b.iter()) {
            assert!((x - y).abs() <= 1e-15 * x.abs(), "{x} vs {y}");
        }
    }
}

/// A nonidentity Profile 7 mapping (eight luma pieces, a third-order MMR per
/// chroma component; `tools/dv_processing/controls/p7_mapping_fixture.h`)
/// composed by the helper's `refit_compose.h` with a zero residual, replayed
/// through [`ComposerCurves::reshape`]: two implementations of the mapped
/// base layer agree code for code. `test_refit_compose` regenerates the file
/// and `tools/dv_processing/test_metadata.py` requires it unchanged.
#[test]
fn the_helper_maps_a_nonidentity_p7_base_layer_as_reshape_does() {
    let v: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/dv-refit/composer-p7-mapping.json"
    ))
    .expect("vectors");
    let luma = floats(&v["luma"]);
    let chroma = floats(&v["chroma"]);
    let mut curves = ComposerCurves {
        luma: [[0.0; 3]; LUMA_PIECES],
        chroma: [[0.0; MMR_TERMS]; 2],
    };
    for (p, piece) in curves.luma.iter_mut().enumerate() {
        piece.copy_from_slice(&luma[3 * p..3 * p + 3]);
    }
    for (ch, terms) in curves.chroma.iter_mut().enumerate() {
        terms.copy_from_slice(&chroma[MMR_TERMS * ch..MMR_TERMS * (ch + 1)]);
    }
    let samples = v["samples"].as_array().expect("samples");
    assert_eq!(samples.len(), 3 * 1024 + 9 * 9 * 9);
    let mut moved = 0;
    for sample in samples {
        let s: Vec<u16> = floats(sample).into_iter().map(|x| x as u16).collect();
        let base = [s[0], s[1], s[2]];
        assert_eq!(curves.reshape(base), [s[3], s[4], s[5]], "base {base:?}");
        moved += usize::from([s[3], s[4], s[5]] != base.map(|c| 4 * c));
    }
    assert!(moved > samples.len() / 2, "the mapping is not identity");
}

/// A whole `--refit-stats` run of the 311-frame Michael5400 clip (no
/// `--frames`, ends at EOF): five scenes, the RPU's own, and the summary.
fn helper_run() -> Vec<&'static str> {
    include_str!("../../../tests/fixtures/dv-refit/helper-run-michael5400.jsonl")
        .lines()
        .collect()
}

#[test]
fn a_whole_helper_run_parses_and_checks() {
    let lines = helper_run();
    let (summary, scenes) = lines.split_last().expect("lines");
    let scenes: Vec<RefitScene> = scenes
        .iter()
        .map(|l| RefitScene::from_line(l).expect("scene"))
        .collect();
    let summary = RefitSummary::from_line(summary).expect("summary");
    check_refit_run(&scenes, &summary).expect("consistent run");
    assert_eq!(
        (summary.frames, summary.scenes, summary.rpu_scenes),
        (311, 5, 5)
    );
    assert_eq!(
        scenes
            .iter()
            .map(|s| (s.first_frame, s.frames))
            .collect::<Vec<_>>(),
        [(0, 51), (51, 41), (92, 79), (171, 115), (286, 25)]
    );
    assert_eq!((summary.first_pts, summary.end_pts), (125, 13179));
    // Every frame of every scene carries the same L5 lattice: 480 × 260 fit
    // and 480 × 259 held-out samples.
    for s in &scenes {
        assert_eq!(s.fit.samples(), s.frames as f64 * 480.0 * 260.0);
        assert_eq!(s.held_out.samples(), s.frames as f64 * 480.0 * 259.0);
        s.fit.solve().expect("each scene fits");
    }

    // Disagreements with the summary, a gap, a short pooled scene, a bad
    // kind and an over-long line are refused.
    let mut wrong = summary.clone();
    wrong.fit_samples += 1;
    assert!(check_refit_run(&scenes, &wrong).is_err());
    let mut gap = scenes.clone();
    gap[2].first_frame += 1;
    assert!(check_refit_run(&gap, &summary).is_err());
    let mut short = scenes.clone();
    short[4].frames = 20;
    assert!(check_refit_run(&short, &summary).is_err());
    assert!(RefitScene::from_line(lines[5]).is_err());
    assert!(RefitSummary::from_line(lines[0]).is_err());
    let long = format!("{}{}", lines[0], " ".repeat(MAX_REFIT_LINE_BYTES));
    assert_eq!(
        RefitScene::from_line(&long),
        Err(RefitError::Output("line longer than the cap"))
    );
    let mut extra: Value = serde_json::from_str(lines[5]).expect("json");
    extra["degenerate"] = serde_json::json!(true);
    assert!(RefitSummary::from_line(&extra.to_string()).is_err());
}

/// The same nonidentity mapping with a nonzero LINEAR_DZ residual (offset
/// 500, slope and threshold 3000 and 1000 / 2^23): the helper's composition
/// equals the mapped value of [`ComposerCurves::reshape`]'s arithmetic plus
/// sign(d)·((|d| − ½)·S + T)/2^23, rounded at 2^12.
#[test]
fn the_helper_adds_the_residual_to_a_nonidentity_mapping() {
    let v: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/dv-refit/composer-p7-mapping.json"
    ))
    .expect("vectors");
    let luma = floats(&v["luma"]);
    let chroma = floats(&v["chroma"]);
    let nlq: Vec<i64> = floats(&v["residual"]["nlq"])
        .into_iter()
        .map(|x| x as i64)
        .collect();
    let (offset, slope, threshold) = (nlq[0], nlq[1], nlq[2]);
    let samples = v["residual"]["samples"].as_array().expect("samples");
    assert!(samples.len() > 400);
    let mut residuals = 0;
    for sample in samples {
        let s: Vec<i64> = floats(sample).into_iter().map(|x| x as i64).collect();
        let x: Vec<f64> = s[..3].iter().map(|&c| c as f64 / 1024.0).collect();
        let piece = (s[0] as usize / 128).min(LUMA_PIECES - 1);
        let mapped_y =
            luma[3 * piece] + luma[3 * piece + 1] * x[0] + luma[3 * piece + 2] * x[0] * x[0];
        let basis = mmr_basis(x[0], x[1], x[2]);
        let mapped_c: Vec<f64> = (0..2)
            .map(|ch| {
                (0..MMR_TERMS)
                    .map(|i| basis[i] * chroma[MMR_TERMS * ch + i])
                    .sum()
            })
            .collect();
        for (c, mapped) in [mapped_y, mapped_c[0], mapped_c[1]].into_iter().enumerate() {
            let d = s[3 + c] - offset;
            let r = if d == 0 {
                0.0
            } else {
                d.signum() as f64 * ((2 * d.abs() - 1) * slope + 2 * threshold) as f64
                    / 16_777_216.0
            };
            residuals += usize::from(d != 0);
            let want = ((mapped.clamp(0.0, 1.0) + r) * 4096.0 + 0.5)
                .floor()
                .clamp(0.0, 4095.0);
            assert_eq!(s[6 + c] as f64, want, "sample {s:?} component {c}");
        }
    }
    assert!(residuals > samples.len());
}
