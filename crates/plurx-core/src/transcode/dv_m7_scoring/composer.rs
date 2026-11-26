//! Exact source-arm FEL composition for M7 Model B v2.
//!
//! The reference is libplacebo v7.387.0's composer (`pl_shader_dovi_reshape`
//! and `sh_dovi_compose_nlq`), evaluated in exact rational arithmetic rather
//! than the renderer's binary32: each component's code is clamped to its own
//! pivot range and read as code / 2^10, its piece is the last whose lower
//! pivot does not exceed that code, a polynomial or MMR of the RPU's Q23
//! coefficients maps it, the mapped value is clamped to [0, 1], the
//! LINEAR_DZ residual is added and the sum is rounded to 12 bits. Every
//! value is a fraction over 2^113 = 2^23 * 1024^9; with |coefficient| < 2^31
//! each of at most 22 terms stays below 2^121, so `i128` holds the sum
//! exactly. `tools/dv_processing/refit_exact.h` is the same arithmetic for
//! the helper; both replay `tests/fixtures/dv-m7/exact-composer-vectors.json`
//! and `fraction_oracle.py` recomputes it independently. Design:
//! `docs/streaming/M7-MODEL-B-GENERAL-COMPOSITION.md`.

use dolby_vision::rpu::{
    dovi_rpu::DoviRpu,
    rpu_data_mapping::{DoviMappingMethod, DoviNlqMethod, DoviReshapingCurve},
    rpu_data_nlq::{DoviELType, RpuDataNlq},
};

const MAX_PIVOTS: usize = 9;
const Q23: i64 = 1 << 23;
const COEFFICIENT_LIMIT: i64 = 1 << 31;
const ONE: i128 = 1 << 113;
const MMR_DEGREE: [u32; 7] = [1, 1, 1, 2, 2, 2, 3];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Pieces {
    /// Per piece `a0 + a1 x + a2 x^2`, unused orders zero.
    Polynomial(Vec<[i64; 3]>),
    /// Per piece the order, the constant and the 7 terms of each order.
    Mmr(Vec<(usize, i64, [[i64; 7]; 3])>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Curve {
    pivots: Vec<u16>,
    pieces: Pieces,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Nlq {
    offset: u16,
    slope: i64,
    threshold: i64,
}

/// One fresh Profile 7 FEL RPU's mapping, admitted only when it is composed
/// exactly. The admission is the helper's `refit_exact_from_dovi` plus the
/// fresh-source header predicate M7 has always applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceComposition {
    curves: [Curve; 3],
    nlq: [Nlq; 3],
}

/// The composed 12-bit codes and, for the continuous Model A, the mapped
/// value in ten-bit code units (1024 * clamped mapped value, rounded once to
/// binary64) and the residual.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Composed {
    pub codes: [u16; 3],
    pub mapped_codes: [f64; 3],
    pub residual: [f64; 3],
    pub mapped_q113: [i128; 3],
}

impl SourceComposition {
    pub fn from_rpu(rpu: &DoviRpu) -> Option<Self> {
        let header = &rpu.header;
        if rpu.dovi_profile != 7
            || rpu.el_type != Some(DoviELType::FEL)
            || header.use_prev_vdr_rpu_flag
            || header.disable_residual_flag
            || !header.vdr_dm_metadata_present_flag
            || header.rpu_type != 2
            || header.rpu_format != 18
            || header.vdr_rpu_profile != 1
            || header.vdr_rpu_level != 0
            || !header.vdr_seq_info_present_flag
            || header.chroma_resampling_explicit_filter_flag
            || header.vdr_rpu_normalized_idc != 1
            || header.reserved_zero_3bits != 0
            || header.spatial_resampling_filter_flag
            || !header.el_spatial_resampling_filter_flag
            || header.coefficient_data_type != 0
            || header.coefficient_log2_denom != 23
            || header.bl_bit_depth_minus8 != 2
            || header.el_bit_depth_minus8 != 2
            || header.vdr_bit_depth_minus8 != 4
            || header.bl_video_full_range_flag
        {
            return None;
        }
        let mapping = rpu.rpu_data_mapping.as_ref()?;
        if mapping.mapping_color_space != 0
            || mapping.mapping_chroma_format_idc != 0
            || mapping.num_x_partitions_minus1 != 0
            || mapping.num_y_partitions_minus1 != 0
            || mapping.nlq_method_idc != Some(DoviNlqMethod::LinearDeadzone)
            || mapping.nlq_num_pivots_minus2 != Some(0)
        {
            return None;
        }
        let nlq = mapping.nlq.as_ref()?;
        let [a, b, c] = &mapping.curves;
        Some(Self {
            curves: [curve(a)?, curve(b)?, curve(c)?],
            nlq: [
                nonbinding(nlq, 0)?,
                nonbinding(nlq, 1)?,
                nonbinding(nlq, 2)?,
            ],
        })
    }

    /// Whether the mapping is the identity the frozen v1 model required.
    pub fn is_identity(&self) -> bool {
        self.curves.iter().all(|curve| {
            curve.pivots == [0, 1023] && curve.pieces == Pieces::Polynomial(vec![[0, Q23, 0]])
        })
    }

    pub fn compose(&self, base: [u16; 3], el: [u16; 3]) -> Composed {
        let code: [i64; 3] = std::array::from_fn(|c| {
            let pivots = &self.curves[c].pivots;
            i64::from(base[c].clamp(pivots[0], pivots[pivots.len() - 1]))
        });
        let mapped: [i128; 3] =
            std::array::from_fn(|c| self.curves[c].mapped(c, code).clamp(0, ONE));
        let residual: [i64; 3] = std::array::from_fn(|c| residual(self.nlq[c], el[c]));
        Composed {
            codes: std::array::from_fn(|c| {
                let v = mapped[c] + (i128::from(residual[c]) << 89) + (1 << 100);
                (v >> 101).clamp(0, 4095) as u16
            }),
            // Rounding the integer numerator once, then scaling by a power
            // of two, keeps identity codes exact (2^103 * code).
            mapped_codes: mapped.map(|m| m as f64 / 2f64.powi(103)),
            residual: residual.map(|r| r as f64 / 16_777_216.0),
            mapped_q113: mapped,
        }
    }
}

impl Curve {
    fn mapped(&self, component: usize, code: [i64; 3]) -> i128 {
        let own = code[component];
        let piece = self.pivots[1..self.pivots.len() - 1]
            .iter()
            .take_while(|&&pivot| own >= i64::from(pivot))
            .count();
        match &self.pieces {
            Pieces::Polynomial(pieces) => {
                let [a0, a1, a2] = pieces[piece].map(i128::from);
                let x = i128::from(own);
                (a0 << 90) + ((a1 * x) << 80) + ((a2 * x * x) << 70)
            }
            Pieces::Mmr(pieces) => {
                let (order, constant, coefficients) = &pieces[piece];
                let [y, u, v] = code.map(i128::from);
                let terms = [y, u, v, y * u, y * v, u * v, y * u * v];
                let mut power = terms;
                let mut sum = i128::from(*constant) << 90;
                for (p, row) in (1u32..).zip(&coefficients[..*order]) {
                    for ((&coefficient, power), (&term, degree)) in
                        row.iter().zip(&mut power).zip(terms.iter().zip(MMR_DEGREE))
                    {
                        sum += (i128::from(coefficient) * *power) << (90 - 10 * degree * p);
                        *power *= term;
                    }
                }
                sum
            }
        }
    }
}

fn coefficient(integer: i64, fraction: u64) -> Option<i64> {
    let fraction = i64::try_from(fraction).ok().filter(|&f| f < Q23)?;
    let value = integer.checked_mul(Q23)?.checked_add(fraction)?;
    (value.abs() < COEFFICIENT_LIMIT).then_some(value)
}

fn curve(curve: &DoviReshapingCurve) -> Option<Curve> {
    let count = usize::try_from(curve.num_pivots_minus2)
        .ok()?
        .checked_add(2)?;
    if count > MAX_PIVOTS || curve.pivots.len() != count {
        return None;
    }
    // libdovi keeps the bitstream's pivot deltas; the composer's pivots are
    // their running sums.
    let mut pivots = Vec::with_capacity(count);
    let mut pivot = 0u32;
    for &delta in &curve.pivots {
        pivot += u32::from(delta);
        if pivot > 1023 || pivots.last().is_some_and(|&last| pivot <= u32::from(last)) {
            return None;
        }
        pivots.push(pivot as u16);
    }
    let pieces = count - 1;
    let pieces = match curve.mapping_idc {
        DoviMappingMethod::Polynomial if curve.mmr.is_none() => {
            let poly = curve.polynomial.as_ref()?;
            if poly.poly_order_minus1.len() != pieces
                || poly.linear_interp_flag.len() != pieces
                || poly.poly_coef_int.len() != pieces
                || poly.poly_coef.len() != pieces
                || poly.linear_interp_flag.iter().any(|&flag| flag)
            {
                return None;
            }
            let mut out = Vec::with_capacity(pieces);
            for i in 0..pieces {
                let order = usize::try_from(poly.poly_order_minus1[i]).ok()? + 1;
                if !(1..=2).contains(&order)
                    || poly.poly_coef_int[i].len() != order + 1
                    || poly.poly_coef[i].len() != order + 1
                {
                    return None;
                }
                let mut piece = [0; 3];
                for (j, slot) in piece.iter_mut().enumerate().take(order + 1) {
                    *slot = coefficient(poly.poly_coef_int[i][j], poly.poly_coef[i][j])?;
                }
                out.push(piece);
            }
            Pieces::Polynomial(out)
        }
        DoviMappingMethod::MMR if curve.polynomial.is_none() => {
            let mmr = curve.mmr.as_ref()?;
            if mmr.mmr_order_minus1.len() != pieces
                || mmr.mmr_constant_int.len() != pieces
                || mmr.mmr_constant.len() != pieces
                || mmr.mmr_coef_int.len() != pieces
                || mmr.mmr_coef.len() != pieces
            {
                return None;
            }
            let mut out = Vec::with_capacity(pieces);
            for i in 0..pieces {
                let order = usize::from(mmr.mmr_order_minus1[i]) + 1;
                if !(1..=3).contains(&order)
                    || mmr.mmr_coef_int[i].len() != order
                    || mmr.mmr_coef[i].len() != order
                {
                    return None;
                }
                let constant = coefficient(mmr.mmr_constant_int[i], mmr.mmr_constant[i])?;
                let mut rows = [[0; 7]; 3];
                for (p, row) in rows.iter_mut().enumerate().take(order) {
                    let (integer, fraction) = (&mmr.mmr_coef_int[i][p], &mmr.mmr_coef[i][p]);
                    if integer.len() != 7 || fraction.len() != 7 {
                        return None;
                    }
                    for (t, slot) in row.iter_mut().enumerate() {
                        *slot = coefficient(integer[t], fraction[t])?;
                    }
                }
                out.push((order, constant, rows));
            }
            Pieces::Mmr(out)
        }
        _ => return None,
    };
    Some(Curve { pivots, pieces })
}

/// The helper's `nlq_clip_is_nonbinding`: the renderer clamps the residual
/// to vdr_in_max, so admit only limits no EL code can reach, with the same
/// 2^-16 reserve.
fn nonbinding(nlq: &RpuDataNlq, c: usize) -> Option<Nlq> {
    let (offset, slope, threshold) = (
        nlq.nlq_offset[c],
        nlq.linear_deadzone_slope[c],
        nlq.linear_deadzone_threshold[c],
    );
    if offset > 1023
        || nlq.linear_deadzone_slope_int[c] != 0
        || nlq.linear_deadzone_threshold_int[c] != 0
        || nlq.vdr_in_max_int[c] > 1
        || slope >= Q23 as u64
        || threshold >= Q23 as u64
        || nlq.vdr_in_max[c] >= Q23 as u64
    {
        return None;
    }
    let distance = u64::from(offset.max(1023 - offset));
    let worst = (2 * distance - 1) * slope + 2 * threshold;
    let limit = 2 * (nlq.vdr_in_max_int[c] * Q23 as u64 + nlq.vdr_in_max[c]);
    (worst == 0 || worst <= 2 * Q23 as u64 && worst + 256 <= limit).then_some(Nlq {
        offset,
        slope: slope as i64,
        threshold: threshold as i64,
    })
}

/// The LINEAR_DZ residual of one EL code as a numerator over 2^24.
fn residual(nlq: Nlq, el: u16) -> i64 {
    let d = i64::from(el) - i64::from(nlq.offset);
    if d == 0 {
        return 0;
    }
    let magnitude = (2 * d.abs() - 1) * nlq.slope + 2 * nlq.threshold;
    magnitude * d.signum()
}

#[cfg(test)]
mod tests;
