//! Writes Profile 8.1 RPUs whose mapping curves are re-fitted to the FEL composition
//! (protocol-rpu-refit-20261112.json, step 2). Usage: rpu_refit_write PLAN.json
//!
//! PLAN.json: {"frames": [{"src": "<P7 RPU NAL>", "out": "<P8.1 RPU NAL>"}, ...],
//!             "luma": {"pivot_deltas": [u16; 9], "coef": [[c0, c1, c2]; 8]},
//!             "chroma": [{"constant": f, "coef": [[f; 7]; 3]}; 2],
//!             "l1": [min_pq, max_pq, avg_pq]}   (optional, diagnostics only: overrides every frame's L1)
//! Coefficients are in the composer's normalised domain (BL code / 2^bl_bit_depth in,
//! 1.0 = 2^vdr_bit_depth out). Each source RPU is converted to 8.1 (dovi_tool mode 2) and its
//! mapping replaced; everything else (L1/L2 etc.) is kept. Prints, per frame, the curves as
//! parsed back from the written bytes so the caller can check the round trip.
use anyhow::{ensure, Context, Result};
use dolby_vision::rpu::dovi_rpu::DoviRpu;
use dolby_vision::rpu::extension_metadata::blocks::{ExtMetadataBlock, ExtMetadataBlockLevel1};
use dolby_vision::rpu::rpu_data_mapping::{DoviMMRCurve, DoviMappingMethod, DoviPolynomialCurve};
use serde_json::{json, Value};
use std::fs;

fn split(v: f64, denom: u64) -> (i64, u64) {
    // Integer parts outside [-64, 63] are refused: fixed-point decoders are not expected to carry them.
    assert!(v.is_finite() && v >= -64.0 && v < 63.0, "coefficient {v} outside the writable range [-64, 63)");
    let scale = (1u64 << denom) as f64;
    let mut int = v.floor() as i64;
    let mut frac = ((v - int as f64) * scale).round() as u64;
    if frac == 1u64 << denom {
        int += 1;
        frac = 0;
    }
    (int, frac)
}

fn join(int: i64, frac: u64, denom: u64) -> f64 {
    int as f64 + frac as f64 / (1u64 << denom) as f64
}

fn f(v: &Value) -> f64 {
    v.as_f64().expect("number")
}

fn main() -> Result<()> {
    let plan: Value = serde_json::from_str(&fs::read_to_string(std::env::args().nth(1).context("PLAN.json")?)?)?;
    let deltas: Vec<u16> = plan["luma"]["pivot_deltas"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as u16).collect();
    ensure!(deltas.len() >= 2 && deltas.len() <= 9);
    // Optional diagnostic L1 override, validated once: three PQ codes in 0..=4095.
    let l1 = match plan.get("l1") {
        None => None,
        Some(v) => {
            let vals = v.as_array().context("l1 must be [min_pq, max_pq, avg_pq]")?;
            ensure!(vals.len() == 3, "l1 must have exactly three values");
            let mut pq = [0u16; 3];
            for (slot, x) in pq.iter_mut().zip(vals) {
                let n = x.as_u64().context("l1 values must be integers")?;
                ensure!(n <= 4095, "l1 value {n} outside 0..=4095");
                *slot = n as u16;
            }
            Some(ExtMetadataBlockLevel1 { min_pq: pq[0], max_pq: pq[1], avg_pq: pq[2] })
        }
    };
    let mut readback = Vec::new();
    for fr in plan["frames"].as_array().unwrap() {
        let src = fs::read(fr["src"].as_str().unwrap())?;
        let mut rpu = DoviRpu::parse_unspec62_nalu(&src)?;
        rpu.convert_with_mode(2u8)?;
        ensure!(rpu.dovi_profile == 8, "conversion did not give profile 8");
        let denom = rpu.header.coefficient_log2_denom;
        let map = rpu.rpu_data_mapping.as_mut().context("no mapping")?;
        // Luma: piecewise 2nd-order polynomials.
        let pieces = deltas.len() - 1;
        let lc = &mut map.curves[0];
        lc.num_pivots_minus2 = (deltas.len() - 2) as u64;
        lc.pivots = deltas.clone();
        lc.mapping_idc = DoviMappingMethod::Polynomial;
        lc.mmr = None;
        let mut poly = DoviPolynomialCurve::default();
        for i in 0..pieces {
            poly.poly_order_minus1.push(1);
            poly.linear_interp_flag.push(false);
            let (mut ints, mut fracs) = (Default::default(), Default::default());
            for k in 0..3 {
                let (a, b) = split(f(&plan["luma"]["coef"][i][k]), denom);
                tinyvec_push(&mut ints, a);
                tinyvec_push(&mut fracs, b);
            }
            poly.poly_coef_int.push(ints);
            poly.poly_coef.push(fracs);
        }
        lc.polynomial = Some(poly);
        // Chroma: one piece, 3rd-order MMR.
        for c in 0..2 {
            let cc = &mut map.curves[c + 1];
            cc.num_pivots_minus2 = 0;
            cc.pivots = vec![0, 1023];
            cc.mapping_idc = DoviMappingMethod::MMR;
            cc.polynomial = None;
            let spec = &plan["chroma"][c];
            let mut m = DoviMMRCurve::default();
            m.mmr_order_minus1.push(2);
            let (ci, cf) = split(f(&spec["constant"]), denom);
            m.mmr_constant_int.push(ci);
            m.mmr_constant.push(cf);
            let (mut oi, mut of) = (Default::default(), Default::default());
            for j in 0..3 {
                let (mut ri, mut rf) = (Default::default(), Default::default());
                for k in 0..7 {
                    let (a, b) = split(f(&spec["coef"][j][k]), denom);
                    tinyvec_push(&mut ri, a);
                    tinyvec_push(&mut rf, b);
                }
                tinyvec_push(&mut oi, ri);
                tinyvec_push(&mut of, rf);
            }
            m.mmr_coef_int.push(oi);
            m.mmr_coef.push(of);
            cc.mmr = Some(m);
        }
        if let Some(l1) = &l1 {
            let dm = rpu.vdr_dm_data.as_mut().context("no DM data")?;
            dm.replace_metadata_block(ExtMetadataBlock::Level1(l1.clone()))?;
        }
        rpu.modified = true;
        let out = rpu.write_hevc_unspec62_nalu()?;
        fs::write(fr["out"].as_str().unwrap(), &out)?;
        // Round trip: parse what was written and report the curves as decoders will read them.
        let back = DoviRpu::parse_unspec62_nalu(&out)?;
        let bm = back.rpu_data_mapping.as_ref().unwrap();
        let d = back.header.coefficient_log2_denom;
        let lp = bm.curves[0].polynomial.as_ref().unwrap();
        let luma: Vec<Vec<f64>> = (0..lp.poly_coef.len()).map(|i| (0..lp.poly_coef[i].len()).map(|k| join(lp.poly_coef_int[i][k], lp.poly_coef[i][k], d)).collect()).collect();
        let chroma: Vec<Value> = (1..3).map(|c| {
            let m = bm.curves[c].mmr.as_ref().unwrap();
            json!({"constant": join(m.mmr_constant_int[0], m.mmr_constant[0], d),
                   "coef": (0..m.mmr_coef[0].len()).map(|j| (0..7).map(|k| join(m.mmr_coef_int[0][j][k], m.mmr_coef[0][j][k], d)).collect::<Vec<f64>>()).collect::<Vec<_>>()})
        }).collect();
        readback.push(json!({"out": fr["out"], "profile": back.dovi_profile, "bytes": out.len(),
            "luma_pivot_deltas": bm.curves[0].pivots, "luma_coef": luma, "chroma": chroma,
            "bl_bit_depth": back.header.bl_bit_depth_minus8 + 8, "vdr_bit_depth": back.header.vdr_bit_depth_minus8 + 8,
            "coefficient_log2_denom": d, "disable_residual_flag": back.header.disable_residual_flag}));
    }
    println!("{}", serde_json::to_string(&json!({"frames": readback}))?);
    Ok(())
}

fn tinyvec_push<T, A: tinyvec::Array<Item = T>>(v: &mut tinyvec::ArrayVec<A>, x: T) {
    v.push(x);
}
