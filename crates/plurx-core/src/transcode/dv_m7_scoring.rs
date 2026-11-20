//! Scalar four-model F+ versus NP1 scoring for the measured M7 representation.
//!
//! The caller binds frame ordinals, clocks and NAL custody and checks the video
//! declarations and raster. Samples come from the same lattice site in fresh
//! software-decoded ten-bit planar 4:2:0 pictures, with half-resolution EL,
//! BT.2020 NCL/PQ video-range TopLeft declarations and the frozen M7 tap
//! positions. The F+ arm uses the same coded base picture as the source.
//! The reference is the software model of the FEL signal at that site.
//!
//! Model transforms follow the diagnostic four_model_audit.py; the F+ bridge
//! and reference mask follow served_refit_eval.py and m7_pair_compare.py.

use dolby_vision::rpu::{
    dovi_rpu::DoviRpu, rpu_data_mapping::DoviMappingMethod, vdr_dm_data::VdrDmData,
};

use super::{
    dv_fel_qualification::{delta_e_itp, pq_eotf, rgb48_to_ictcp},
    dv_processing::DvParsedMetadata,
    dv_refit::{ComposerCurves, ServableCurves},
    dv_refit_serve::written_curves,
};

const MAX_RPU_BYTES: usize = 4098;
const RPU_NAL_HEADER: [u8; 2] = [0x7c, 0x01];
const HPE_INVERSE: [[f64; 3]; 3] = [
    [3.06441879, -2.16597676, 0.10155818],
    [-0.65612108, 1.78554118, -0.12943749],
    [0.01736321, -0.04725154, 1.03004253],
];
const Y_WEIGHTS: [f64; 3] = [0.2627, 0.678, 0.0593];
/// The bands are left-inclusive and right-exclusive, with a final open band.
pub const LUMINANCE_BAND_UPPER_NITS: [f64; 4] = [0.1, 1.0, 10.0, 100.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// Continuous BL and residual, affine conversion, PQ/HPE round trip.
    A,
    /// Rounded/clipped 12-bit composition, signed Q13 integer conversion.
    B,
    /// Rounded/clipped 12-bit composition, floating-point affine conversion.
    C,
    /// Model C's affine values with a PQ/HPE round trip before RGB16.
    D,
}

impl Model {
    pub const ALL: [Self; 4] = [Self::A, Self::B, Self::C, Self::D];
}

/// The three decoded code triplets at one matched M7 lattice site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LatticeSample {
    pub source_bl: [u16; 3],
    pub source_el: [u16; 3],
    pub candidate: [u16; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelSampleScore {
    pub reference_luminance_nits: f64,
    pub served_fplus_delta_e_itp: f64,
    pub candidate_delta_e_itp: f64,
}

impl ModelSampleScore {
    pub fn luminance_band(self) -> usize {
        LUMINANCE_BAND_UPPER_NITS.partition_point(|&hi| self.reference_luminance_nits >= hi)
    }

    /// Positive when the decoded candidate is closer to the reference.
    pub fn paired_advantage(self) -> f64 {
        self.served_fplus_delta_e_itp - self.candidate_delta_e_itp
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ScoreError {
    #[error("M7 requires a fresh Profile 7 FEL RPU with calibrated color, identity mapping and linear NLQ")]
    UnsupportedSourceRpu,
    #[error("M7 requires actual whole and seek Profile 8.1 mappings that match the pinned production curves and source color")]
    UnboundServedCurves,
    #[error("M7 requires a fresh authored Profile 8.1 RPU with identity mapping and the same frame's source color")]
    UnsupportedCandidateRpu,
    #[error("M7 lattice site contains a decoded code above ten bits")]
    InvalidSampleCode,
    #[error("M7 model {0:?} produced nonfinite RGB")]
    NonfiniteRgb(Model),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DmCoefficients {
    matrix_q13: [i16; 9],
    offsets_q28: [u32; 3],
    lms_q14: [i16; 9],
}

impl DmCoefficients {
    // Same binding as refit_topleft_color: other matrices need their own
    // evidence that the transformed RGB represents BT.2020.
    const CALIBRATED: Self = Self {
        matrix_q13: [9574, 0, 13802, 9574, -1540, -5348, 9574, 17610, 0],
        offsets_q28: [16_777_216, 134_217_728, 134_217_728],
        lms_q14: [7222, 8771, 390, 2654, 12430, 1300, 0, 422, 15962],
    };

    fn from_dm(dm: &VdrDmData) -> Option<Self> {
        if dm.compressed
            || dm.signal_eotf != u16::MAX
            || dm.signal_eotf_param0 != 0
            || dm.signal_eotf_param1 != 0
            || dm.signal_eotf_param2 != 0
            || dm.signal_bit_depth != 12
            || dm.signal_color_space != 0
            || dm.signal_chroma_format != 0
            || dm.signal_full_range_flag != 1
        {
            return None;
        }
        Some(Self {
            matrix_q13: [
                dm.ycc_to_rgb_coef0,
                dm.ycc_to_rgb_coef1,
                dm.ycc_to_rgb_coef2,
                dm.ycc_to_rgb_coef3,
                dm.ycc_to_rgb_coef4,
                dm.ycc_to_rgb_coef5,
                dm.ycc_to_rgb_coef6,
                dm.ycc_to_rgb_coef7,
                dm.ycc_to_rgb_coef8,
            ],
            offsets_q28: [
                dm.ycc_to_rgb_offset0,
                dm.ycc_to_rgb_offset1,
                dm.ycc_to_rgb_offset2,
            ],
            lms_q14: [
                dm.rgb_to_lms_coef0,
                dm.rgb_to_lms_coef1,
                dm.rgb_to_lms_coef2,
                dm.rgb_to_lms_coef3,
                dm.rgb_to_lms_coef4,
                dm.rgb_to_lms_coef5,
                dm.rgb_to_lms_coef6,
                dm.rgb_to_lms_coef7,
                dm.rgb_to_lms_coef8,
            ],
        })
    }

    fn source(raw: &[u8]) -> Option<Self> {
        // libdovi's parser panics on unsupported polynomial interpolation.
        let source = std::panic::catch_unwind(|| DvParsedMetadata::from_raw_rpu(raw))
            .ok()?
            .ok()?;
        let color = Self {
            matrix_q13: source.nonlinear_matrix,
            offsets_q28: source.nonlinear_offsets,
            lms_q14: source.linear_matrix,
        };
        (source.depths == [10, 10, 12]
            && source.coefficient_data_type == 0
            && !source.bl_full_range
            && source.luma_integer == [0, 1]
            && source.luma_fraction == [0; 2]
            && source.chroma_integer == [[0, 1]; 2]
            && source.chroma_fraction == [[0; 2]; 2]
            && source.nlq_offset == [512; 3]
            && source.nlq_slope_integer == [0; 3]
            && source.nlq_slope == [2048; 3]
            && source.nlq_threshold_integer == [0; 3]
            && source.nlq_threshold == [0; 3]
            && source.nlq_max_integer == [0; 3]
            && source.nlq_max_fraction == [1_048_576; 3]
            && color == Self::CALIBRATED)
            .then_some(color)
    }
}

/// Score a frame with its published F+ curves and authored candidate color.
pub struct FrameScorer {
    served_curves: ComposerCurves,
    color: ColorTransform,
}

impl FrameScorer {
    /// The inputs are bare HEVC UNSPEC62 NALs from the same bound source
    /// frame and its whole, seek and candidate publications. The caller
    /// resolves the pinned production table at that frame's source time.
    pub fn from_rpus(
        source: &[u8],
        served_whole: &[u8],
        served_seek: &[u8],
        pinned: &ServableCurves,
        candidate: &[u8],
    ) -> Result<Self, ScoreError> {
        let color = DmCoefficients::source(source).ok_or(ScoreError::UnsupportedSourceRpu)?;
        let served_curves = fitted_curves(served_whole, color)
            .filter(|curves| curves == pinned.curves())
            .ok_or(ScoreError::UnboundServedCurves)?;
        if fitted_curves(served_seek, color).as_ref() != Some(&served_curves) {
            return Err(ScoreError::UnboundServedCurves);
        }
        let candidate =
            profile81_rpu(candidate, color).ok_or(ScoreError::UnsupportedCandidateRpu)?;
        if !candidate.rpu_data_mapping.is_some_and(|mapping| {
            mapping.curves.iter().all(|curve| {
                curve.pivots == [0, 1023]
                    && curve.mapping_idc == DoviMappingMethod::Polynomial
                    && curve.polynomial.as_ref().is_some_and(|poly| {
                        poly.poly_order_minus1 == [0]
                            && poly.linear_interp_flag == [false]
                            && poly.poly_coef_int.len() == 1
                            && poly.poly_coef_int[0].as_slice() == [0, 1]
                            && poly.poly_coef.len() == 1
                            && poly.poly_coef[0].as_slice() == [0, 0]
                    })
            })
        }) {
            return Err(ScoreError::UnsupportedCandidateRpu);
        }
        Ok(Self {
            served_curves,
            color: ColorTransform::new(color),
        })
    }

    /// Return each model's paired errors in [`Model::ALL`] order. Each band is
    /// computed once from that model's RGB16 reference, for use by both arms.
    pub fn score_sample(&self, sample: LatticeSample) -> Result<[ModelSampleScore; 4], ScoreError> {
        let LatticeSample {
            source_bl,
            source_el,
            candidate,
        } = sample;
        if source_bl
            .into_iter()
            .chain(source_el)
            .chain(candidate)
            .any(|code| code > 1023)
        {
            return Err(ScoreError::InvalidSampleCode);
        }
        let reference =
            ModelInput::from_residual(source_bl, source_el.map(|code| code as i16 - 512));
        let fitted = ModelInput::from_served(source_bl, self.served_curves.reshape(source_bl));
        let candidate = ModelInput::from_residual(candidate, [0; 3]);

        let score = |model| -> Result<ModelSampleScore, ScoreError> {
            let rgb = self.color.rgb(model, reference)?;
            let reference = rgb48_to_ictcp(rgb);
            let fitted = rgb48_to_ictcp(self.color.rgb(model, fitted)?);
            let candidate = rgb48_to_ictcp(self.color.rgb(model, candidate)?);
            Ok(ModelSampleScore {
                reference_luminance_nits: dot(
                    rgb.map(|code| pq_eotf(f64::from(code) / 65535.0)),
                    Y_WEIGHTS,
                ),
                served_fplus_delta_e_itp: delta_e_itp(reference, fitted),
                candidate_delta_e_itp: delta_e_itp(reference, candidate),
            })
        };
        Ok([
            score(Model::A)?,
            score(Model::B)?,
            score(Model::C)?,
            score(Model::D)?,
        ])
    }
}

fn profile81_rpu(raw: &[u8], color: DmCoefficients) -> Option<DoviRpu> {
    if raw.len() > MAX_RPU_BYTES || !raw.starts_with(&RPU_NAL_HEADER) {
        return None;
    }
    let rpu = std::panic::catch_unwind(|| DoviRpu::parse_unspec62_nalu(raw))
        .ok()?
        .ok()?;
    let header = &rpu.header;
    if rpu.dovi_profile != 8
        || header.use_prev_vdr_rpu_flag
        || !header.vdr_dm_metadata_present_flag
        || !header.disable_residual_flag
        || header.bl_video_full_range_flag
        || header.bl_bit_depth_minus8 != 2
        || header.vdr_bit_depth_minus8 != 4
        || header.coefficient_data_type != 0
        || header.coefficient_log2_denom != 23
        || DmCoefficients::from_dm(rpu.vdr_dm_data.as_ref()?) != Some(color)
    {
        return None;
    }
    let mapping = rpu.rpu_data_mapping.as_ref()?;
    (mapping.num_x_partitions_minus1 == 0 && mapping.num_y_partitions_minus1 == 0).then_some(rpu)
}

fn fitted_curves(raw: &[u8], color: DmCoefficients) -> Option<ComposerCurves> {
    let rpu = profile81_rpu(raw, color)?;
    let mapping = rpu.rpu_data_mapping.as_ref()?;
    if mapping.curves[1..].iter().any(|curve| {
        curve.pivots != [0, 1023]
            || !curve
                .mmr
                .as_ref()
                .is_some_and(|mmr| mmr.mmr_order_minus1 == [2])
    }) {
        return None;
    }
    let curves = written_curves(&rpu)?;
    // Luma is read from Q23 wire values. The published chroma subset is Q12.
    curves
        .chroma
        .iter()
        .flatten()
        .all(|value| (value * 4096.0).fract() == 0.0)
        .then_some(curves)
}

#[derive(Clone, Copy)]
struct ModelInput {
    base: [u16; 3],
    centered: [i16; 3],
    composed: [u16; 3],
}

impl ModelInput {
    fn from_residual(base: [u16; 3], centered: [i16; 3]) -> Self {
        let composed = std::array::from_fn(|i| {
            let d = centered[i];
            // floor((BL / 1024 + residual) * 4096 + 0.5), exactly.
            (4 * base[i] as i16 + d + (d < 0) as i16).clamp(0, 4095) as u16
        });
        Self {
            base,
            centered,
            composed,
        }
    }

    fn from_served(base: [u16; 3], composed: [u16; 3]) -> Self {
        let centered = std::array::from_fn(|i| {
            let delta = composed[i] as i16 - 4 * base[i] as i16;
            // The signed half-code NLQ formula gives negative deltas a
            // different rounding boundary from positive deltas.
            delta - (delta < 0) as i16
        });
        Self {
            base,
            centered,
            composed,
        }
    }
}

struct ColorTransform {
    dm: DmCoefficients,
    nonlinear: [[f64; 3]; 3],
    offsets_10: [f64; 3],
    offsets_12: [f64; 3],
    linear: [[f64; 3]; 3],
}

impl ColorTransform {
    fn new(dm: DmCoefficients) -> Self {
        let nonlinear = std::array::from_fn(|r| {
            std::array::from_fn(|c| f64::from(dm.matrix_q13[3 * r + c]) / 8192.0)
        });
        let offsets = dm.offsets_q28.map(|v| f64::from(v) / 268_435_456.0);
        let linear = HPE_INVERSE.map(|row| {
            std::array::from_fn(|c| {
                let column = std::array::from_fn(|r| f64::from(dm.lms_q14[3 * r + c]) / 16384.0);
                dot(row, column)
            })
        });
        Self {
            dm,
            nonlinear,
            offsets_10: offsets.map(|v| v * (1024.0 / 1023.0)),
            offsets_12: offsets.map(|v| v * (4096.0 / 4095.0)),
            linear,
        }
    }

    fn rgb(&self, model: Model, input: ModelInput) -> Result<[u16; 3], ScoreError> {
        if model == Model::B {
            return Ok(self.integer_rgb(input.composed));
        }
        let signal = if model == Model::A {
            std::array::from_fn(|i| {
                f64::from(input.base[i]) / 1023.0 + continuous_residual(input.centered[i])
                    - self.offsets_10[i]
            })
        } else {
            std::array::from_fn(|i| f64::from(input.composed[i]) / 4095.0 - self.offsets_12[i])
        };
        let affine = self.nonlinear.map(|row| dot(row, signal));
        let rgb = if model == Model::C {
            affine
        } else {
            let linear = affine.map(pq_decode_lower_clamped);
            self.linear
                .map(|row| pq_encode_lower_clamped(dot(row, linear)))
        };
        quantize_rgb(rgb).ok_or(ScoreError::NonfiniteRgb(model))
    }

    fn integer_rgb(&self, composed: [u16; 3]) -> [u16; 3] {
        let signal: [i64; 3] = std::array::from_fn(|i| {
            (i64::from(composed[i]) << 4) - i64::from(self.dm.offsets_q28[i] >> 12)
        });
        std::array::from_fn(|r| {
            let raw = signal[0] * i64::from(self.dm.matrix_q13[3 * r])
                + signal[1] * i64::from(self.dm.matrix_q13[3 * r + 1])
                + signal[2] * i64::from(self.dm.matrix_q13[3 * r + 2]);
            signed_q13_floor(raw).clamp(0, 65535) as u16
        })
    }
}

fn signed_q13_floor(raw: i64) -> i64 {
    raw >> 13
}

fn continuous_residual(centered: i16) -> f64 {
    let d = f64::from(centered);
    f64::from(centered.signum()) * (d.abs() - 0.5) / 4096.0
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    (left[0] * right[0] + left[1] * right[1]) + left[2] * right[2]
}

const PQ_M1: f64 = 2610.0 / 16384.0;
const PQ_M2: f64 = 2523.0 / 32.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 128.0;
const PQ_C3: f64 = 2392.0 / 128.0;

fn pq_decode_lower_clamped(code: f64) -> f64 {
    let p = code.clamp(0.0, f64::INFINITY).powf(1.0 / PQ_M2);
    ((p - PQ_C1).clamp(0.0, f64::INFINITY) / (PQ_C2 - PQ_C3 * p)).powf(1.0 / PQ_M1)
}

fn pq_encode_lower_clamped(linear: f64) -> f64 {
    let p = linear.clamp(0.0, f64::INFINITY).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * p) / (1.0 + PQ_C3 * p)).powf(PQ_M2)
}

fn quantize_rgb(rgb: [f64; 3]) -> Option<[u16; 3]> {
    rgb.iter()
        .all(|value| value.is_finite())
        .then(|| rgb.map(|value| (value.clamp(0.0, 1.0) * 65535.0 + 0.5).floor() as u16))
}

#[cfg(test)]
mod tests;
