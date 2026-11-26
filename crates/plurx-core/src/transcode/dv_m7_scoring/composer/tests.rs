#![expect(
    clippy::unwrap_used,
    reason = "test fixtures intentionally stop at the first invalid setup or assertion result"
)]

use super::*;
use serde_json::Value;

const FRAME: &[u8] = include_bytes!("../../../../tests/fixtures/dv-runtime/frame-000.nal");
const VECTORS: &str = include_str!("../../../../tests/fixtures/dv-m7/exact-composer-vectors.json");

fn split(value: i64) -> (i64, u64) {
    (value.div_euclid(Q23), value.rem_euclid(Q23) as u64)
}

/// The fixture's fresh P7 FEL RPU with `mapping` written into it through
/// libdovi, then parsed back from the bitstream, as M7 receives it.
fn rpu_with(mapping: &Value) -> DoviRpu {
    let mut rpu = DoviRpu::parse_unspec62_nalu(FRAME).unwrap();
    let data = rpu.rpu_data_mapping.as_mut().unwrap();
    for (curve, component) in data
        .curves
        .iter_mut()
        .zip(mapping["components"].as_array().unwrap())
    {
        let pivots: Vec<u16> = component["pivots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_u64().unwrap() as u16)
            .collect();
        curve.num_pivots_minus2 = pivots.len() as u64 - 2;
        curve.pivots = std::iter::once(pivots[0])
            .chain(pivots.windows(2).map(|w| w[1] - w[0]))
            .collect();
        let pieces = component["pieces"].as_array().unwrap();
        if component["method"] == "polynomial" {
            curve.mapping_idc = DoviMappingMethod::Polynomial;
            curve.mmr = None;
            let mut poly = curve.polynomial.take().unwrap_or_default();
            poly.poly_order_minus1.clear();
            poly.linear_interp_flag.clear();
            poly.poly_coef_int.clear();
            poly.poly_coef.clear();
            for piece in pieces {
                let order = piece["order"].as_u64().unwrap();
                poly.poly_order_minus1.push(order - 1);
                poly.linear_interp_flag.push(false);
                poly.poly_coef_int.push(Default::default());
                poly.poly_coef.push(Default::default());
                for value in &piece["coefficients"].as_array().unwrap()[..=order as usize] {
                    let (integer, fraction) = split(value.as_i64().unwrap());
                    poly.poly_coef_int.last_mut().unwrap().push(integer);
                    poly.poly_coef.last_mut().unwrap().push(fraction);
                }
            }
            curve.polynomial = Some(poly);
        } else {
            curve.mapping_idc = DoviMappingMethod::MMR;
            curve.polynomial = None;
            let mut mmr = curve.mmr.take().unwrap_or_default();
            mmr.mmr_order_minus1.clear();
            mmr.mmr_constant_int.clear();
            mmr.mmr_constant.clear();
            mmr.mmr_coef_int.clear();
            mmr.mmr_coef.clear();
            for piece in pieces {
                let rows = piece["coefficients"].as_array().unwrap();
                mmr.mmr_order_minus1.push(rows.len() as u8 - 1);
                let (integer, fraction) = split(piece["constant"].as_i64().unwrap());
                mmr.mmr_constant_int.push(integer);
                mmr.mmr_constant.push(fraction);
                mmr.mmr_coef_int.push(Default::default());
                mmr.mmr_coef.push(Default::default());
                for row in rows {
                    mmr.mmr_coef_int
                        .last_mut()
                        .unwrap()
                        .push(Default::default());
                    mmr.mmr_coef.last_mut().unwrap().push(Default::default());
                    for value in row.as_array().unwrap() {
                        let (integer, fraction) = split(value.as_i64().unwrap());
                        mmr.mmr_coef_int
                            .last_mut()
                            .unwrap()
                            .last_mut()
                            .unwrap()
                            .push(integer);
                        mmr.mmr_coef
                            .last_mut()
                            .unwrap()
                            .last_mut()
                            .unwrap()
                            .push(fraction);
                    }
                }
            }
            curve.mmr = Some(mmr);
        }
    }
    let nlq = data.nlq.as_mut().unwrap();
    for (c, component) in mapping["components"].as_array().unwrap().iter().enumerate() {
        let n = &component["nlq"];
        let maximum = n["vdr_in_max"].as_u64().unwrap();
        nlq.nlq_offset[c] = n["offset"].as_u64().unwrap() as u16;
        nlq.vdr_in_max_int[c] = maximum >> 23;
        nlq.vdr_in_max[c] = maximum & (Q23 as u64 - 1);
        nlq.linear_deadzone_slope_int[c] = 0;
        nlq.linear_deadzone_slope[c] = n["slope"].as_u64().unwrap();
        nlq.linear_deadzone_threshold_int[c] = 0;
        nlq.linear_deadzone_threshold[c] = n["threshold"].as_u64().unwrap();
    }
    rpu.modified = true;
    DoviRpu::parse_unspec62_nalu(&rpu.write_hevc_unspec62_nalu().unwrap()).unwrap()
}

fn mappings() -> Vec<Value> {
    let vectors: Value = serde_json::from_str(VECTORS).unwrap();
    vectors["mappings"].as_array().unwrap().clone()
}

/// The helper's integer composer (refit_exact.h) and this one agree on every
/// vector the helper wrote, codes and mapped numerators alike; the Python
/// rational oracle reproduces the same file independently.
#[test]
fn replays_the_helper_vectors_bit_for_bit() {
    let mut samples = 0;
    for mapping in mappings() {
        let rpu = rpu_with(&mapping);
        let composition = SourceComposition::from_rpu(&rpu)
            .unwrap_or_else(|| panic!("{} is admitted", mapping["name"]));
        assert_eq!(composition.is_identity(), mapping["name"] == "identity");
        for sample in mapping["samples"].as_array().unwrap() {
            let s = sample.as_array().unwrap();
            let code = |i: usize| s[i].as_u64().unwrap() as u16;
            let composed =
                composition.compose([code(0), code(1), code(2)], [code(3), code(4), code(5)]);
            assert_eq!(composed.codes, [code(6), code(7), code(8)], "{sample}");
            for c in 0..3 {
                let mapped: i128 = s[9 + c].as_str().unwrap().parse().unwrap();
                assert_eq!(composed.mapped_q113[c], mapped, "{sample}");
            }
            samples += 1;
        }
    }
    assert_eq!(samples, 6448);
}

/// Identity curves with the frozen NLQ give v1's integer composition and
/// Model A's v1 continuous input, bit for bit, for every BL and EL code.
#[test]
fn identity_is_the_frozen_v1_composition() {
    let identity = mappings()
        .into_iter()
        .find(|m| m["name"] == "identity")
        .unwrap();
    let composition = SourceComposition::from_rpu(&rpu_with(&identity)).unwrap();
    assert!(composition.is_identity());
    for b in 0..=1023u16 {
        for e in 0..=1023u16 {
            let composed = composition.compose([b, 1023 - b, b], [e, e, 1023 - e]);
            let frozen = super::super::ModelInput::from_residual(
                [b, 1023 - b, b],
                [e, e, 1023 - e].map(|code| code as i16 - 512),
            );
            let general = super::super::ModelInput::from_composition(composed);
            assert_eq!(general.composed, frozen.composed);
            assert_eq!(
                general.continuous.map(f64::to_bits),
                frozen.continuous.map(f64::to_bits)
            );
        }
    }
}

#[test]
fn refuses_what_it_does_not_compose() {
    let single = mappings()
        .into_iter()
        .find(|m| m["name"] == "p7-single-mmr-0")
        .unwrap();
    let admitted = rpu_with(&single);
    assert!(SourceComposition::from_rpu(&admitted).is_some());
    let refused = |edit: &dyn Fn(&mut DoviRpu)| {
        let mut rpu = admitted.clone();
        edit(&mut rpu);
        assert!(SourceComposition::from_rpu(&rpu).is_none());
    };
    refused(&|rpu| rpu.header.disable_residual_flag = true);
    refused(&|rpu| rpu.header.coefficient_log2_denom = 22);
    refused(&|rpu| rpu.header.bl_video_full_range_flag = true);
    refused(&|rpu| rpu.header.use_prev_vdr_rpu_flag = true);
    refused(&|rpu| {
        rpu.rpu_data_mapping
            .as_mut()
            .unwrap()
            .num_x_partitions_minus1 = 1
    });
    refused(&|rpu| rpu.rpu_data_mapping.as_mut().unwrap().mapping_color_space = 1);
    refused(&|rpu| rpu.rpu_data_mapping.as_mut().unwrap().nlq_num_pivots_minus2 = Some(1));
    // A repeated pivot, and a pivot past the ten-bit range.
    refused(&|rpu| rpu.rpu_data_mapping.as_mut().unwrap().curves[0].pivots[3] = 0);
    refused(&|rpu| rpu.rpu_data_mapping.as_mut().unwrap().curves[0].pivots[8] = 128);
    // A component carrying pieces of both methods.
    refused(&|rpu| {
        let curves = &mut rpu.rpu_data_mapping.as_mut().unwrap().curves;
        curves[0].mmr = curves[1].mmr.clone();
    });
    refused(&|rpu| {
        let curve = &mut rpu.rpu_data_mapping.as_mut().unwrap().curves[0];
        curve.polynomial.as_mut().unwrap().linear_interp_flag[2] = true;
    });
    // The coefficient bound that keeps the 128-bit sum exact.
    refused(&|rpu| {
        let curve = &mut rpu.rpu_data_mapping.as_mut().unwrap().curves[1];
        curve.mmr.as_mut().unwrap().mmr_coef_int[0][2][6] = 256;
    });
    refused(&|rpu| {
        let curve = &mut rpu.rpu_data_mapping.as_mut().unwrap().curves[2];
        curve.mmr.as_mut().unwrap().mmr_constant_int[0] = -257;
    });
    // A vdr_in_max the farthest EL code reaches, and integer NLQ parts.
    refused(&|rpu| {
        let nlq = rpu.rpu_data_mapping.as_mut().unwrap().nlq.as_mut().unwrap();
        nlq.vdr_in_max[1] = 1023 * 2048 / 2;
    });
    refused(&|rpu| {
        let nlq = rpu.rpu_data_mapping.as_mut().unwrap().nlq.as_mut().unwrap();
        nlq.linear_deadzone_slope_int[0] = 1;
    });
    // Just inside the bound composes without overflow, clamped to one.
    let mut extreme = admitted.clone();
    let mmr = extreme.rpu_data_mapping.as_mut().unwrap().curves[1]
        .mmr
        .as_mut()
        .unwrap();
    mmr.mmr_constant_int[0] = 255;
    mmr.mmr_constant[0] = Q23 as u64 - 1;
    for row in mmr.mmr_coef_int[0].iter_mut() {
        row.iter_mut().for_each(|v| *v = 255);
    }
    for row in mmr.mmr_coef[0].iter_mut() {
        row.iter_mut().for_each(|v| *v = Q23 as u64 - 1);
    }
    let composition = SourceComposition::from_rpu(&extreme).unwrap();
    let composed = composition.compose([1023; 3], [512; 3]);
    assert_eq!((composed.mapped_q113[1], composed.codes[1]), (ONE, 4095));
}
