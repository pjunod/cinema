//! Inputs to the managed NP1 candidate encoder.
//!
//! The held source acquisition supplies both original BL and Model B TopLeft
//! planes, with an independent frame census. The encoder consumes the composed
//! planes and attaches the ordered regions as AV_FRAME_DATA_REGIONS_OF_INTEREST.
//! Its admitted input, loaded closure, effective options, coded packet census,
//! authored output and readback must be verified by the candidate stage.

use plurx_core::{
    fmp4::Hdr10OutputMetadata,
    transcode::{
        dv_fel_qualification::pq_eotf,
        dv_m7_scoring::{
            aggregation::AGGREGATION_REVISION, capture::FrameEvidence, SCORING_REVISION,
        },
        Rational,
    },
};
use serde::Serialize;
use sha2::{Digest, Sha256};

type Hash = [u8; 32];

const RECIPE_REVISION: u32 = 1;
const INPUT_FORMAT: &str = "plurx-composed-model-b-topleft-v1";
const MODE: &str = "lumazfsp";
const GRID: usize = 16;
const QG_SIZE: u32 = 32;
const QP_OFFSET: i32 = -6;
const QP_RANGE: u32 = 63;
const THRESHOLD_NITS: f64 = 1.0;
const MICRO: i32 = 1_000_000;
const FRAMES: u32 = 48;
const MAX_TARGET_KBPS: u32 = 125_000;
const MIN_TARGET_KBPS: u32 = 83_334;
const BUF_KBIT: u32 = 136_000;
const MAX_ATTEMPTS: usize = 4;
const PREDICTION_OVERHEAD_MBPS: f64 = 0.03;
const MAX_ENCODED_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("unsupported NP1 raster, plane extent or source frame")]
    Input,
    #[error("NP1 input plane hash differs from the accepted acquisition")]
    PlaneHash,
    #[error("NP1 input plane contains a code above ten bits")]
    Sample,
    #[error("unsupported NP1 cadence, HDR metadata or frozen rate control")]
    Recipe,
    #[error("NP1 encoder rate history differs from the frozen proposal sequence")]
    RateHistory,
    #[error("NP1 benefit map cancelled")]
    Cancelled,
}

/// The acquisition owner verifies the origin and held content of each plane
/// before constructing this view. Each byte slice is complete planar
/// yuv420p10le in Y, Cb, Cr order at the full coded raster.
pub(super) struct FullPlanePair<'a> {
    pub source: FrameEvidence,
    pub raster: [u16; 2],
    pub original_bl: &'a [u8],
    pub composed: &'a [u8],
    pub original_bl_sha256: Hash,
    pub composed_sha256: Hash,
}

/// An ordered libavcodec ROI. Keep the supplied rational spelling and list
/// order. libx265 applies overlapping entries in reverse list order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Region {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
    pub qoffset_num: i32,
    pub qoffset_den: u32,
}

#[derive(Debug, Serialize)]
pub(super) struct BenefitMap {
    pub revision: u32,
    pub method: &'static str,
    pub scoring_revision: u32,
    pub source_ordinal: u64,
    pub source_pts: i64,
    pub source_duration: i64,
    pub source_time_base: Rational,
    pub source_access_unit_sha256: Hash,
    pub source_rpu_sha256: Hash,
    pub source_bl_vcl_sha256: Hash,
    pub original_bl_sha256: Hash,
    pub composed_sha256: Hash,
    pub blocks: usize,
    pub selected_blocks: usize,
    pub selected_zero_offset_blocks: usize,
    pub mean_selected_qp_offset: f64,
    pub positive_qp_offset: f64,
    pub regions: Vec<Region>,
}

/// Build the exact diagnostic lumazfsp map on independently acquired planes.
pub(super) fn build_map(
    frame: FullPlanePair<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<BenefitMap, Error> {
    let [width, height] = frame.raster.map(usize::from);
    if !(64..=3840).contains(&width)
        || !(64..=2160).contains(&height)
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || frame.source.pts == i64::MIN
        || frame.source.duration <= 0
        || frame
            .source
            .pts
            .checked_add(frame.source.duration)
            .is_none()
    {
        return Err(Error::Input);
    }
    let pixels = width * height;
    let bytes = 3 * pixels;
    if frame.original_bl.len() != bytes || frame.composed.len() != bytes {
        return Err(Error::Input);
    }
    check_cancelled(&cancelled)?;
    let original_bl_sha256 = hash(frame.original_bl);
    let composed_sha256 = hash(frame.composed);
    if original_bl_sha256 != frame.original_bl_sha256 || composed_sha256 != frame.composed_sha256 {
        return Err(Error::PlaneHash);
    }

    let bw = width.div_ceil(GRID);
    let bh = height.div_ceil(GRID);
    let mut dark_rms = Vec::new();
    let mut blocks = Vec::with_capacity(bw * bh);
    for by in 0..bh {
        check_cancelled(&cancelled)?;
        for bx in 0..bw {
            let mut sum = 0.0;
            let mut squares = 0.0;
            let mut count = 0;
            for y in by * GRID..((by + 1) * GRID).min(height) {
                for x in bx * GRID..((bx + 1) * GRID).min(width) {
                    let luma = y * width + x;
                    let chroma = (y / 2) * (width / 2) + x / 2;
                    let cb = pixels + chroma;
                    let cr = pixels * 5 / 4 + chroma;
                    let [base_y, base_cb, base_cr] =
                        [luma, cb, cr].map(|at| code(frame.original_bl, at));
                    let [y, cb, cr] = [luma, cb, cr].map(|at| code(frame.composed, at));
                    if [base_y, base_cb, base_cr, y, cb, cr]
                        .into_iter()
                        .any(|code| code > 1023)
                    {
                        return Err(Error::Sample);
                    }
                    sum += f64::from(y);
                    count += 1;
                    // Match the C per-pixel sum, with each chroma difference
                    // repeated for the four pixels in its 4:2:0 footprint.
                    let dy = 0.82 * f64::from(i32::from(y) - i32::from(base_y));
                    let du = 1.12 * f64::from(i32::from(cb) - i32::from(base_cb));
                    let dv = 1.70 * f64::from(i32::from(cr) - i32::from(base_cr));
                    squares += dy * dy + du * du + dv * dv;
                }
            }
            let limited_pq = (sum / f64::from(count) - 64.0) / 876.0;
            let rms = (pq_eotf(limited_pq) < THRESHOLD_NITS).then(|| {
                let rms = (squares / f64::from(count)).sqrt();
                dark_rms.push(rms);
                rms
            });
            blocks.push(rms);
        }
    }
    dark_rms.sort_unstable_by(f64::total_cmp);
    let median = match dark_rms.len() {
        0 => 0.0,
        n if n.is_multiple_of(2) => 0.5 * (dark_rms[n / 2 - 1] + dark_rms[n / 2]),
        n => dark_rms[n / 2],
    };

    let mut negative_sum = 0.0;
    let mut zero = 0;
    let offsets: Vec<_> = blocks
        .into_iter()
        .map(|rms| {
            rms.map(|rms| {
                let strength = if rms <= 0.0 || median <= 0.0 {
                    1.0
                } else {
                    (1.0 - (rms / median).log2() / 0.5).clamp(0.0, 1.0)
                };
                let qp = f64::from(QP_OFFSET) * strength;
                negative_sum += qp.abs();
                let micro = micro_qp(qp);
                zero += (micro == 0) as usize;
                micro
            })
        })
        .collect();
    let selected = dark_rms.len();
    let unselected = offsets.len() - selected;
    let positive = if unselected == 0 {
        0.0
    } else {
        (negative_sum / unselected as f64).min(f64::from(QP_OFFSET.abs()))
    };
    let regions = ordered_regions(&offsets, bw, frame.raster, positive, &cancelled)?;
    Ok(BenefitMap {
        revision: RECIPE_REVISION,
        method: MODE,
        scoring_revision: SCORING_REVISION,
        source_ordinal: frame.source.ordinal,
        source_pts: frame.source.pts,
        source_duration: frame.source.duration,
        source_time_base: frame.source.time_base,
        source_access_unit_sha256: frame.source.access_unit_sha256,
        source_rpu_sha256: frame.source.rpu_sha256,
        source_bl_vcl_sha256: frame.source.bl_vcl_sha256,
        original_bl_sha256,
        composed_sha256,
        blocks: offsets.len(),
        selected_blocks: selected,
        selected_zero_offset_blocks: zero,
        mean_selected_qp_offset: if selected == 0 {
            0.0
        } else {
            -negative_sum / selected as f64
        },
        positive_qp_offset: positive,
        regions,
    })
}

fn ordered_regions(
    offsets: &[Option<i32>],
    columns: usize,
    raster: [u16; 2],
    positive: f64,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<Region>, Error> {
    let mut regions = Vec::new();
    for (y, row) in offsets.chunks_exact(columns).enumerate() {
        check_cancelled(cancelled)?;
        let mut x = 0;
        while x < row.len() {
            let Some(offset) = row[x].filter(|&value| value != 0) else {
                // Selected zero offsets still counted in the balance; the
                // compatibility arm lets them fall under the final region.
                x += 1;
                continue;
            };
            let start = x;
            x += 1;
            while row.get(x) == Some(&Some(offset)) {
                x += 1;
            }
            let (num, den) = if offset == QP_OFFSET * MICRO {
                (QP_OFFSET, QP_RANGE)
            } else {
                (offset, QP_RANGE * MICRO as u32)
            };
            // The compatibility map keeps aligned rectangle edges. The
            // encoder's ROI implementation clips them to its edge blocks.
            regions.push(Region {
                left: (start * GRID) as u16,
                top: (y * GRID) as u16,
                right: (x * GRID) as u16,
                bottom: ((y + 1) * GRID) as u16,
                qoffset_num: num,
                qoffset_den: den,
            });
        }
    }
    if positive > 0.0 {
        regions.push(Region {
            left: 0,
            top: 0,
            right: raster[0],
            bottom: raster[1],
            qoffset_num: micro_qp(positive),
            qoffset_den: QP_RANGE * MICRO as u32,
        });
    }
    Ok(regions)
}

fn code(plane: &[u8], word: usize) -> u16 {
    u16::from_le_bytes([plane[2 * word], plane[2 * word + 1]])
}

fn micro_qp(offset: f64) -> i32 {
    // C lround rounds half away from zero.
    (offset * f64::from(MICRO)).round() as i32
}

fn hash(bytes: &[u8]) -> Hash {
    Sha256::digest(bytes).into()
}

fn check_cancelled(cancelled: &impl Fn() -> bool) -> Result<(), Error> {
    if cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

/// ST 2086 primaries in R,G,B,white order, in 1/50000 units; luminance
/// values are in 1/10000 cd/m2. The source observation owns these values.
#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct StaticHdr {
    pub primaries_rgbw: [u16; 8],
    pub max_1e4_nits: u32,
    pub min_1e4_nits: u32,
    pub max_cll_fall: Option<[u16; 2]>,
    pub source_observation_sha256: Hash,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct RatePlan {
    /// Inclusive bounds on authored video packet bits per second, in kb/s.
    pub authored_band_kbps: [u32; 2],
    pub initial_target_kbps: u32,
}

#[derive(Debug, Serialize)]
pub(super) struct FrozenRecipe {
    revision: u32,
    scoring_revision: u32,
    aggregation_revision: u32,
    input_format: &'static str,
    map_method: &'static str,
    grid: usize,
    qg_size: u32,
    qp_offset: i32,
    qp_range: u32,
    threshold_millinits: u32,
    rolloff_half_octaves: u32,
    frames: u32,
    timescale: u32,
    frame_ticks: u32,
    hdr: StaticHdr,
    rate: RatePlan,
    maxrate_kbps: u32,
    buffer_kbit: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EncoderGrid {
    pub timescale: u32,
    pub frame_ticks: u32,
    pub frames: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RoiControl {
    /// Same unmodified recipe as the independent CLI baseline, no ROI.
    None,
    /// Same unmodified recipe, one zero-valued full-frame ROI.
    Zero,
    Candidate,
}

impl FrozenRecipe {
    pub(super) fn new(cadence: Rational, hdr: StaticHdr, rate: RatePlan) -> Result<Self, Error> {
        let [low, high] = rate.authored_band_kbps;
        if !matches!(
            (cadence.numerator(), cadence.denominator()),
            (24, 1) | (24_000, 1001)
        ) || low == 0
            || low > high
            || !admitted_target(rate.initial_target_kbps)
            || hdr
                .max_cll_fall
                .is_some_and(|[cll, fall]| cll == 0 || fall > cll)
        {
            return Err(Error::Recipe);
        }
        Hdr10OutputMetadata::from_source(
            hdr.primaries_rgbw,
            hdr.max_1e4_nits,
            hdr.min_1e4_nits,
            hdr.max_cll_fall,
        )
        .map_err(|_| Error::Recipe)?;
        Ok(Self {
            revision: RECIPE_REVISION,
            scoring_revision: SCORING_REVISION,
            aggregation_revision: AGGREGATION_REVISION,
            input_format: INPUT_FORMAT,
            map_method: MODE,
            grid: GRID,
            qg_size: QG_SIZE,
            qp_offset: QP_OFFSET,
            qp_range: QP_RANGE,
            threshold_millinits: 1000,
            rolloff_half_octaves: 1,
            frames: FRAMES,
            timescale: cadence.numerator(),
            frame_ticks: cadence.denominator(),
            hdr,
            rate,
            maxrate_kbps: MAX_TARGET_KBPS,
            buffer_kbit: BUF_KBIT,
        })
    }

    pub(super) fn encoder_grid(&self) -> EncoderGrid {
        EncoderGrid {
            timescale: self.timescale,
            frame_ticks: self.frame_ticks,
            frames: self.frames,
        }
    }

    pub(super) fn recipe_sha256(&self) -> Hash {
        let mut digest = Sha256::new();
        digest.update(b"plurx.m7.np1.recipe.v1\0");
        digest.update(serde_json::to_vec(self).expect("fixed recipe serializes"));
        digest.finalize().into()
    }

    /// Direct avcodec dictionary. The parent separately sets the encoder
    /// clock to 1/timescale, frame ordinal times frame_ticks, first type I,
    /// progressive yuv420p10le, video-only hvc1 fMP4 with global headers
    /// and +empty_moov+default_base_moof+frag_keyframe+frag_discont.
    pub(super) fn encoder_options(
        &self,
        target_kbps: u32,
        control: RoiControl,
    ) -> Result<Vec<(&'static str, String)>, Error> {
        if !admitted_target(target_kbps)
            || control != RoiControl::Candidate && target_kbps != self.rate.initial_target_kbps
        {
            return Err(Error::Recipe);
        }
        let [rx, ry, gx, gy, bx, by, wx, wy] = self.hdr.primaries_rgbw;
        let max = self.hdr.max_1e4_nits;
        let min = self.hdr.min_1e4_nits;
        let mut x265 = format!(
            "hdr10=1:hdr10-opt=0:aq-mode=2:repeat-headers=1:open-gop=0:bframes=0:level-idc=5.1:high-tier=1:hrd=1:chromaloc=2:scenecut=0:pools=none:frame-threads=1:wpp=0:master-display=G({gx},{gy})B({bx},{by})R({rx},{ry})WP({wx},{wy})L({max},{min})"
        );
        if let Some([cll, fall]) = self.hdr.max_cll_fall {
            x265.push_str(&format!(":max-cll={cll},{fall}"));
        }
        x265.push_str(":cbqpoffs=-9:crqpoffs=-9:qg-size=32");
        if control == RoiControl::Candidate {
            x265.push_str(":cbqpoffs=-9:crqpoffs=-12:aq-strength=0");
        }
        Ok(vec![
            ("preset", "slow".into()),
            ("profile", "main10".into()),
            ("x265-params", x265),
            ("b", format!("{target_kbps}k")),
            ("maxrate", format!("{}k", self.maxrate_kbps)),
            ("bufsize", format!("{}k", self.buffer_kbit)),
            ("bf", "0".into()),
            ("flags", "+cgop".into()),
            ("g", FRAMES.to_string()),
            ("keyint_min", FRAMES.to_string()),
            ("sc_threshold", "0".into()),
            ("color_primaries", "bt2020".into()),
            ("color_trc", "smpte2084".into()),
            ("colorspace", "bt2020nc".into()),
            ("color_range", "tv".into()),
            ("threads", "1".into()),
        ])
    }

    /// The actual authored packet payload rate is checked in integer space.
    /// The encode predictor cannot admit a candidate.
    pub(super) fn authored_rate_in_band(&self, video_packet_bytes: u64) -> bool {
        if !(1..=MAX_ENCODED_BYTES).contains(&video_packet_bytes) {
            return false;
        }
        let bits = u128::from(video_packet_bytes) * 8 * u128::from(self.timescale);
        let denominator = u128::from(self.frames) * u128::from(self.frame_ticks) * 1000;
        let [low, high] = self
            .rate
            .authored_band_kbps
            .map(|v| u128::from(v) * denominator);
        (low..=high).contains(&bits)
    }

    /// Rebuild the guarded secant sequence from measured encoded packet
    /// payloads. A refused or repeated proposal never becomes an attempt.
    pub(super) fn rate_decision(&self, attempts: &[RateAttempt]) -> Result<RateDecision, Error> {
        if attempts.len() > MAX_ATTEMPTS {
            return Err(Error::RateHistory);
        }
        let mut previous = None;
        let mut decision = RateDecision::Encode(self.rate.initial_target_kbps);
        let [low, high] = self.rate.authored_band_kbps.map(|v| f64::from(v) / 1000.0);
        let target = (low + high) / 2.0;
        let duration =
            f64::from(self.frames) * f64::from(self.frame_ticks) / f64::from(self.timescale);
        for (index, attempt) in attempts.iter().enumerate() {
            if decision != RateDecision::Encode(attempt.target_kbps)
                || !(1..=MAX_ENCODED_BYTES).contains(&attempt.encoded_video_packet_bytes)
            {
                return Err(Error::RateHistory);
            }
            let predicted = attempt.encoded_video_packet_bytes as f64 * 8.0 / duration / 1e6
                + PREDICTION_OVERHEAD_MBPS;
            let current_target = f64::from(attempt.target_kbps);
            decision = if (low..=high).contains(&predicted) {
                RateDecision::PredictedInBand {
                    target_kbps: attempt.target_kbps,
                    predicted_mbps: predicted,
                }
            } else if index + 1 == MAX_ATTEMPTS {
                RateDecision::Inconclusive(RateStop::AttemptsExhausted)
            } else {
                let ratio_proposal = current_target * target / predicted;
                let proposed = if let Some((target_before, rate_before)) = previous {
                    let slope = (predicted - rate_before) / (current_target - target_before);
                    if slope > 0.0 {
                        current_target + (target - predicted) / slope
                    } else {
                        ratio_proposal
                    }
                } else {
                    ratio_proposal
                };
                let rounded = proposed.round_ties_even();
                if !(f64::from(MIN_TARGET_KBPS)..=f64::from(MAX_TARGET_KBPS)).contains(&rounded) {
                    RateDecision::Inconclusive(RateStop::OutsideTargetBounds(rounded as i64))
                } else {
                    let next = rounded as u32;
                    if attempts[..=index]
                        .iter()
                        .any(|seen| seen.target_kbps == next)
                    {
                        RateDecision::Inconclusive(RateStop::RepeatedTarget(next))
                    } else {
                        RateDecision::Encode(next)
                    }
                }
            };
            previous = Some((current_target, predicted));
        }
        Ok(decision)
    }
}

fn admitted_target(target: u32) -> bool {
    (MIN_TARGET_KBPS..=MAX_TARGET_KBPS).contains(&target)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) struct RateAttempt {
    pub target_kbps: u32,
    pub encoded_video_packet_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub(super) enum RateDecision {
    Encode(u32),
    PredictedInBand {
        target_kbps: u32,
        predicted_mbps: f64,
    },
    Inconclusive(RateStop),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) enum RateStop {
    OutsideTargetBounds(i64),
    RepeatedTarget(u32),
    AttemptsExhausted,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame<'a>(width: u16, height: u16, bl: &'a [u8], composed: &'a [u8]) -> FullPlanePair<'a> {
        FullPlanePair {
            source: FrameEvidence {
                ordinal: 53,
                pts: -1,
                duration: 42,
                time_base: Rational::new(1, 1000).unwrap(),
                access_unit_sha256: [1; 32],
                rpu_sha256: [2; 32],
                bl_vcl_sha256: [3; 32],
            },
            raster: [width, height],
            original_bl: bl,
            composed,
            original_bl_sha256: hash(bl),
            composed_sha256: hash(composed),
        }
    }

    fn simple_planes(width: usize, height: usize, delta: [u16; 3]) -> (Vec<u8>, Vec<u8>) {
        let mut bl = Vec::new();
        let mut composed = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let dark = y < 16 && x < 48;
                let value = if dark { 80_u16 } else { 500 };
                let original = if dark { value - delta[x / 16] } else { value };
                composed.extend_from_slice(&value.to_le_bytes());
                bl.extend_from_slice(&original.to_le_bytes());
            }
        }
        for _ in 0..width * height / 2 {
            composed.extend_from_slice(&512_u16.to_le_bytes());
            bl.extend_from_slice(&512_u16.to_le_bytes());
        }
        (bl, composed)
    }

    #[test]
    fn selected_zero_blocks_fall_under_balance_and_adjacent_regions_keep_priority() {
        let (bl, composed) = simple_planes(64, 64, [1, 1, 2]);
        let map = build_map(frame(64, 64, &bl, &composed), || false).unwrap();
        assert_eq!(map.method, "lumazfsp");
        assert_eq!(map.revision, 1);
        assert_eq!(map.source_pts, -1);
        assert_eq!(map.source_duration, 42);
        assert_eq!(map.source_access_unit_sha256, [1; 32]);
        assert_eq!(map.source_rpu_sha256, [2; 32]);
        assert_eq!(map.source_bl_vcl_sha256, [3; 32]);
        assert_eq!(map.blocks, 16);
        assert_eq!(map.selected_blocks, 3);
        assert_eq!(map.selected_zero_offset_blocks, 1);
        assert_eq!(map.mean_selected_qp_offset, -4.0);
        assert!((map.positive_qp_offset - 12.0 / 13.0).abs() < 1e-12);
        assert_eq!(
            map.regions,
            [
                Region {
                    left: 0,
                    top: 0,
                    right: 32,
                    bottom: 16,
                    qoffset_num: -6,
                    qoffset_den: 63
                },
                Region {
                    left: 0,
                    top: 0,
                    right: 64,
                    bottom: 64,
                    qoffset_num: 923_077,
                    qoffset_den: 63_000_000
                },
            ]
        );
    }

    #[test]
    fn half_octave_rolloff_keeps_fractional_negative_micro_qp() {
        let (bl, composed) = simple_planes(64, 64, [4, 4, 5]);
        let map = build_map(frame(64, 64, &bl, &composed), || false).unwrap();
        // Independent analytic ratios: median 4, last selected RMS ratio
        // 5/4, and 13 unselected blocks in the frame.
        assert_eq!(map.selected_zero_offset_blocks, 0);
        assert_eq!(map.regions.len(), 3);
        assert_eq!(map.regions[0].right, 32);
        assert_eq!(map.regions[1].left, 32);
        assert_eq!(map.regions[1].right, 48);
        assert_eq!(map.regions[1].qoffset_num, -2_136_863);
        assert_eq!(map.regions[1].qoffset_den, 63_000_000);
        assert_eq!(map.regions[2].qoffset_num, 1_087_451);
        assert_eq!(micro_qp(-0.000_000_5), -1);
        assert_eq!(micro_qp(0.000_000_5), 1);
    }

    #[test]
    fn input_requires_exact_extent_matching_plane_hash_and_unsigned_ten_bit_codes() {
        let (bl, mut composed) = simple_planes(66, 66, [1, 1, 2]);
        let mut evidence = frame(66, 66, &bl, &composed);
        evidence.composed_sha256[0] ^= 1;
        assert!(matches!(
            build_map(evidence, || false),
            Err(Error::PlaneHash)
        ));
        let truncated = &composed[..composed.len() - 2];
        assert!(matches!(
            build_map(frame(66, 66, &bl, truncated), || false),
            Err(Error::Input)
        ));
        assert!(matches!(
            build_map(frame(66, 66, &bl, &composed), || true),
            Err(Error::Cancelled)
        ));
        composed[0..2].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(matches!(
            build_map(frame(66, 66, &bl, &composed), || false),
            Err(Error::Sample)
        ));
    }

    fn recipe(cadence: Rational) -> FrozenRecipe {
        FrozenRecipe::new(
            cadence,
            StaticHdr {
                primaries_rgbw: [34000, 16000, 13250, 34500, 7500, 3000, 15635, 16450],
                max_1e4_nits: 10_000_000,
                min_1e4_nits: 1,
                max_cll_fall: Some([464, 147]),
                source_observation_sha256: [13; 32],
            },
            RatePlan {
                authored_band_kbps: [128_500, 130_000],
                initial_target_kbps: 115_000,
            },
        )
        .unwrap()
    }

    #[test]
    fn encoder_dictionary_retains_frozen_np1_controls_and_gate_options() {
        let recipe = recipe(Rational::new(24_000, 1001).unwrap());
        assert_eq!(
            recipe.encoder_grid(),
            EncoderGrid {
                timescale: 24_000,
                frame_ticks: 1001,
                frames: 48
            }
        );
        let options = recipe
            .encoder_options(121_179, RoiControl::Candidate)
            .unwrap();
        let x265 = options
            .iter()
            .find(|(key, _)| *key == "x265-params")
            .unwrap()
            .1
            .as_str();
        assert_eq!(
            x265,
            concat!(
                "hdr10=1:hdr10-opt=0:aq-mode=2:repeat-headers=1:open-gop=0:bframes=0:",
                "level-idc=5.1:high-tier=1:hrd=1:chromaloc=2:scenecut=0:pools=none:frame-threads=1:wpp=0:",
                "master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):",
                "max-cll=464,147:cbqpoffs=-9:crqpoffs=-9:qg-size=32:cbqpoffs=-9:crqpoffs=-12:aq-strength=0",
            )
        );
        let none = recipe.encoder_options(115_000, RoiControl::None).unwrap();
        let zero = recipe.encoder_options(115_000, RoiControl::Zero).unwrap();
        assert_eq!(none, zero);
        assert!(!none
            .iter()
            .any(|(_, value)| value.contains("aq-strength=0")));
        assert!(recipe
            .encoder_options(125_001, RoiControl::Candidate)
            .is_err());
        assert!(recipe.encoder_options(121_179, RoiControl::None).is_err());
        let mut different = recipe.hdr;
        different.source_observation_sha256[0] ^= 1;
        let different =
            FrozenRecipe::new(Rational::new(24_000, 1001).unwrap(), different, recipe.rate)
                .unwrap();
        assert_ne!(different.recipe_sha256(), recipe.recipe_sha256());
    }

    #[test]
    fn rate_sequence_uses_only_measured_payload_and_refuses_a_target_above_peak() {
        let recipe = recipe(Rational::new(24, 1).unwrap());
        let first = RateAttempt {
            target_kbps: 115_000,
            encoded_video_packet_bytes: 31_750_000,
        };
        let second = RateAttempt {
            target_kbps: 117_010,
            encoded_video_packet_bytes: 31_950_000,
        };
        assert_eq!(
            recipe.rate_decision(&[]).unwrap(),
            RateDecision::Encode(115_000)
        );
        assert_eq!(
            recipe.rate_decision(&[first]).unwrap(),
            RateDecision::Encode(117_010)
        );
        assert_eq!(
            recipe.rate_decision(&[first, second]).unwrap(),
            RateDecision::Encode(120_578)
        );
        let slow = RateAttempt {
            encoded_video_packet_bytes: 25_000_000,
            ..first
        };
        assert_eq!(
            recipe.rate_decision(&[slow]).unwrap(),
            RateDecision::Inconclusive(RateStop::OutsideTargetBounds(148_593))
        );
        assert!(matches!(
            recipe.rate_decision(&[slow, second]),
            Err(Error::RateHistory)
        ));
        let predicted = RateAttempt {
            encoded_video_packet_bytes: 32_305_000,
            ..first
        };
        assert!(matches!(
            recipe.rate_decision(&[predicted]).unwrap(),
            RateDecision::PredictedInBand {
                target_kbps: 115_000,
                ..
            }
        ));
        assert!(recipe.authored_rate_in_band(32_125_000));
        assert!(recipe.authored_rate_in_band(32_500_000));
        assert!(!recipe.authored_rate_in_band(32_124_999));
        assert!(!recipe.authored_rate_in_band(32_500_001));
    }

    #[test]
    fn rate_proposals_stop_on_repetition_or_four_attempts() {
        let cadence = Rational::new(24, 1).unwrap();
        let recipe = recipe(cadence);
        let attempts = [
            RateAttempt {
                target_kbps: 115_000,
                encoded_video_packet_bytes: 31_750_000,
            },
            RateAttempt {
                target_kbps: 117_010,
                encoded_video_packet_bytes: 31_950_000,
            },
            RateAttempt {
                target_kbps: 120_578,
                encoded_video_packet_bytes: 32_115_000,
            },
            RateAttempt {
                target_kbps: 124_687,
                encoded_video_packet_bytes: 32_115_000,
            },
        ];
        assert_eq!(
            recipe.rate_decision(&attempts[..3]).unwrap(),
            RateDecision::Encode(124_687)
        );
        assert_eq!(
            recipe.rate_decision(&attempts).unwrap(),
            RateDecision::Inconclusive(RateStop::AttemptsExhausted)
        );
        let single_rate = FrozenRecipe::new(
            cadence,
            recipe.hdr,
            RatePlan {
                authored_band_kbps: [129_250; 2],
                ..recipe.rate
            },
        )
        .unwrap();
        let repeated = RateAttempt {
            target_kbps: 115_000,
            encoded_video_packet_bytes: 32_305_025,
        };
        assert_eq!(
            single_rate.rate_decision(&[repeated]).unwrap(),
            RateDecision::Inconclusive(RateStop::RepeatedTarget(115_000))
        );
    }
}
