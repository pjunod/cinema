//! Per-title qualification of the reconstructed Profile 8.1: does this
//! title's enhancement layer add more to the picture than re-encoding loses?
//!
//! The fallback for a Profile 7 FEL title, the plain P7 -> P8.1 conversion,
//! serves the disc's own base-layer bits at full source bitrate. The
//! reconstruction composes the EL into the picture and must re-encode all of
//! it, so it is only the better picture where the EL changes the picture by
//! more than the encode loses. That is a measurable fact about each title
//! (coordinator decision, 2026-10-25; `docs/streaming/FEL-P81-RECONSTRUCTION.md`).
//!
//! A few sample windows are rendered three ways — the helper's FEL master
//! (the reference), the EL-dropped picture, and the composed picture through
//! the route's own encode — and compared with the reference in absolute
//! light (Delta E ITP, ITU-R BT.2124; the equations are those of
//! `tools/dv_quality/metrics.py`) on the active picture inside any letterbox.
//! Bars carry no picture, and Delta E ITP is so steep near 0 nits that bar
//! pixels at 0.00001 vs 0.004 nits score 10 each.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

/// Where the verdict lives: beside the title's other Dolby Vision facts in
/// its probe document. A reprobe or source change rewrites the document and
/// takes the verdict with it.
pub const PROBE_KEY: &str = "plurx_dv_fel_v1";
pub const SCHEMA: u32 = 1;
/// Sample windows per title, spread evenly through it (at (i+1)/(K+1) of the
/// duration). K comes from the window-to-window spread of the EL's gain: on
/// the measured title with the most varied windows one window's gain ranged
/// -26%..+17% with a standard deviation of 15%, so K windows give the title's
/// gain a standard error of 15%/sqrt(K). At K = 4 (7.5%) a title the EL does
/// not help at all would still clear the 10% bar about one time in eleven;
/// at K = 8 (5.3%) about one in thirty-four. A title is measured once, in
/// the background, in about two minutes.
pub const WINDOWS: usize = 8;
/// A title is admitted only on at least this many lit windows. A window whose
/// reference is dark (a fade or a title card) is replaced by its alternate
/// half a spacing later; a title with fewer lit windows than this gets a
/// stored refusal instead of being measured again on every play.
pub const MIN_WINDOWS: usize = 6;
/// Pictures per sample window (the VOD window: 2 s at 24 fps).
pub const WINDOW_FRAMES: u32 = 48;
/// One sample every 32 pixels in each direction (8 100 per 4K frame, about
/// 3 million per title): the measurement tools' sampling.
pub const LATTICE_STRIDE: usize = 32;
/// 16-bit PQ code below which a whole row or column is letterbox (~0.03 nits).
pub const ACTIVE_THRESHOLD: u16 = 1024;
/// The served picture's mean Delta E ITP must be at least this much smaller
/// than the EL-dropped picture's.
pub const MIN_MEAN_GAIN: f64 = 0.10;
/// Two standard deviations (2 sigma) of the served p99 across repeated
/// encodes of the same window. hevc_vaapi on radeonsi is deterministic: three
/// encodes of each of four windows (two titles) were byte-identical, so
/// sigma = 0 and the served p99 may not exceed the EL-dropped p99 at all.
pub const P99_NOISE_TOLERANCE: f64 = 0.0;

const PQ_M1: f64 = 2610.0 / 16384.0;
const PQ_M2: f64 = 2523.0 / 32.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 128.0;
const PQ_C3: f64 = 2392.0 / 128.0;
const RGB_TO_LMS: [[f64; 3]; 3] = [
    [1688.0, 2146.0, 262.0],
    [683.0, 2951.0, 462.0],
    [99.0, 309.0, 3688.0],
];
const LMS_P_TO_ICTCP: [[f64; 3]; 3] = [
    [2048.0, 2048.0, 0.0],
    [6610.0, -13613.0, 7003.0],
    [17933.0, -17390.0, -543.0],
];

/// Normalized PQ code [0,1] to absolute cd/m2.
pub fn pq_eotf(code: f64) -> f64 {
    let p = code.clamp(0.0, 1.0).powf(1.0 / PQ_M2);
    10000.0 * ((p - PQ_C1).max(0.0) / (PQ_C2 - PQ_C3 * p)).powf(1.0 / PQ_M1)
}

fn pq_inverse_eotf(nits: f64) -> f64 {
    let p = (nits.clamp(0.0, 10000.0) / 10000.0).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * p) / (1.0 + PQ_C3 * p)).powf(PQ_M2)
}

fn matrix(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    let row = |r: &[f64; 3]| (r[0] * v[0] + r[1] * v[1] + r[2] * v[2]) / 4096.0;
    [row(&m[0]), row(&m[1]), row(&m[2])]
}

/// Absolute BT.2020 RGB (cd/m2) to ICtCp.
pub fn rgb_nits_to_ictcp(rgb: [f64; 3]) -> [f64; 3] {
    let lms = matrix(&RGB_TO_LMS, rgb);
    matrix(&LMS_P_TO_ICTCP, lms.map(pq_inverse_eotf))
}

/// BT.2124: Ct is half-scaled exactly once.
pub fn delta_e_itp(reference: [f64; 3], candidate: [f64; 3]) -> f64 {
    let d = [
        candidate[0] - reference[0],
        candidate[1] - reference[1],
        candidate[2] - reference[2],
    ];
    720.0 * (d[0] * d[0] + (d[1] / 2.0) * (d[1] / 2.0) + d[2] * d[2]).sqrt()
}

static PQ_NITS: LazyLock<Vec<f64>> = LazyLock::new(|| {
    (0..=u16::MAX)
        .map(|code| pq_eotf(f64::from(code) / 65535.0))
        .collect()
});

/// One 16-bit PQ R'G'B' sample to ICtCp.
pub fn rgb48_to_ictcp(sample: [u16; 3]) -> [f64; 3] {
    rgb_nits_to_ictcp(sample.map(|code| PQ_NITS[usize::from(code)]))
}

/// A frame's sample lattice: every `LATTICE_STRIDE`th pixel from a raw
/// little-endian rgb48 frame, starting half a stride in.
#[derive(Debug, Clone, PartialEq)]
pub struct Lattice {
    pub columns: usize,
    pub rows: usize,
    pub samples: Vec<[u16; 3]>,
}

impl Lattice {
    pub fn from_rgb48(frame: &[u8], width: usize, height: usize) -> Option<Self> {
        if frame.len() != width * height * 6 || width < LATTICE_STRIDE || height < LATTICE_STRIDE {
            return None;
        }
        let offset = LATTICE_STRIDE / 2;
        let columns = (width - offset).div_ceil(LATTICE_STRIDE);
        let rows = (height - offset).div_ceil(LATTICE_STRIDE);
        let mut samples = Vec::with_capacity(columns * rows);
        for y in (offset..height).step_by(LATTICE_STRIDE) {
            for x in (offset..width).step_by(LATTICE_STRIDE) {
                let at = (y * width + x) * 6;
                let word = |i: usize| u16::from_le_bytes([frame[at + i], frame[at + i + 1]]);
                samples.push([word(0), word(2), word(4)]);
            }
        }
        Some(Self {
            columns,
            rows,
            samples,
        })
    }
}

/// Where the `WINDOWS` sample windows start, in milliseconds: slot `i` at
/// (i+1)/(K+1) of the title, and its alternate half a spacing later, used
/// when the primary's reference is dark.
pub fn sample_positions(duration_ms: i64, count: usize) -> Vec<[i64; 2]> {
    if duration_ms <= 0 || count == 0 {
        return Vec::new();
    }
    let slots = count as i128 + 1;
    (0..count as i128)
        .map(|i| {
            let at = |twice: i128| (i128::from(duration_ms) * twice / (2 * slots)) as i64;
            [at(2 * (i + 1)), at(2 * (i + 1) + 1)]
        })
        .collect()
}

/// The window's brightest reference picture (largest sum of samples), whose
/// letterbox is the window's: a fade-in's first frame says nothing about it.
pub fn brightest(frames: &[Lattice]) -> Option<&Lattice> {
    frames.iter().max_by_key(|frame| {
        frame
            .samples
            .iter()
            .map(|rgb| u64::from(*rgb.iter().max().unwrap_or(&0)))
            .sum::<u64>()
    })
}

/// The lattice rectangle inside any letterbox or pillarbox of a reference
/// frame: rows and columns holding a sample above `ACTIVE_THRESHOLD`.
/// `None` for an all-dark frame.
pub fn active_region(reference: &Lattice) -> Option<(usize, usize, usize, usize)> {
    let lit = |x: usize, y: usize| {
        reference.samples[y * reference.columns + x]
            .iter()
            .any(|code| *code > ACTIVE_THRESHOLD)
    };
    let rows: Vec<usize> = (0..reference.rows)
        .filter(|y| (0..reference.columns).any(|x| lit(x, *y)))
        .collect();
    let (top, bottom) = (*rows.first()?, *rows.last()? + 1);
    let columns: Vec<usize> = (0..reference.columns)
        .filter(|x| (top..bottom).any(|y| lit(*x, y)))
        .collect();
    Some((*columns.first()?, top, *columns.last()? + 1, bottom))
}

/// Delta E ITP of every active-picture sample of one candidate frame.
pub fn active_deltas(
    reference: &Lattice,
    candidate: &Lattice,
    region: (usize, usize, usize, usize),
    out: &mut Vec<f64>,
) -> bool {
    if reference.columns != candidate.columns || reference.rows != candidate.rows {
        return false;
    }
    let (x0, y0, x1, y1) = region;
    for y in y0..y1.min(reference.rows) {
        for x in x0..x1.min(reference.columns) {
            let at = y * reference.columns + x;
            out.push(delta_e_itp(
                rgb48_to_ictcp(reference.samples[at]),
                rgb48_to_ictcp(candidate.samples[at]),
            ));
        }
    }
    true
}

/// Mean and 99th percentile (sorted index floor(0.99 (n-1))).
pub fn mean_p99(deltas: &mut [f64]) -> Option<(f64, f64)> {
    if deltas.is_empty() || deltas.iter().any(|d| !d.is_finite()) {
        return None;
    }
    deltas.sort_by(f64::total_cmp);
    let mean = deltas.iter().sum::<f64>() / deltas.len() as f64;
    Some((mean, deltas[(0.99 * (deltas.len() - 1) as f64) as usize]))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowScore {
    pub start_ms: i64,
    pub served_mean: f64,
    pub served_p99: f64,
    pub dropped_mean: f64,
    pub dropped_p99: f64,
    pub samples: u64,
}

/// One title's measured answer, keyed to the source it measured and the
/// renderer and recipe that would serve it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FelVerdict {
    pub schema: u32,
    pub identity: String,
    pub source_size: i64,
    pub source_mtime: i64,
    pub admitted: bool,
    pub served_mean: f64,
    pub dropped_mean: f64,
    pub served_p99: f64,
    pub dropped_p99: f64,
    pub p99_tolerance: f64,
    pub measured_at_ms: i64,
    pub windows: Vec<WindowScore>,
}

/// Admit reconstruction only if the served picture's mean Delta E ITP is at
/// least `MIN_MEAN_GAIN` smaller than the EL-dropped picture's and its p99 is
/// no worse than the EL-dropped p99 plus `P99_NOISE_TOLERANCE`.
pub fn admits(served_mean: f64, dropped_mean: f64, served_p99: f64, dropped_p99: f64) -> bool {
    [served_mean, dropped_mean, served_p99, dropped_p99]
        .iter()
        .all(|value| value.is_finite() && *value >= 0.0)
        && served_mean <= dropped_mean * (1.0 - MIN_MEAN_GAIN)
        && served_p99 <= dropped_p99 + P99_NOISE_TOLERANCE
}

impl FelVerdict {
    /// Pool every window's active-picture samples into one title verdict.
    pub fn from_windows(
        identity: String,
        source_size: i64,
        source_mtime: i64,
        measured_at_ms: i64,
        windows: Vec<WindowScore>,
        served: &mut [f64],
        dropped: &mut [f64],
    ) -> Option<Self> {
        if windows.is_empty() || served.len() != dropped.len() {
            return None;
        }
        let (served_mean, served_p99) = mean_p99(served)?;
        let (dropped_mean, dropped_p99) = mean_p99(dropped)?;
        let round = |v: f64| (v * 10_000.0).round() / 10_000.0;
        Some(Self {
            schema: SCHEMA,
            identity,
            source_size,
            source_mtime,
            admitted: windows.len() >= MIN_WINDOWS
                && admits(served_mean, dropped_mean, served_p99, dropped_p99),
            served_mean: round(served_mean),
            dropped_mean: round(dropped_mean),
            served_p99: round(served_p99),
            dropped_p99: round(dropped_p99),
            p99_tolerance: P99_NOISE_TOLERANCE,
            measured_at_ms,
            windows,
        })
    }

    /// A title with no lit sample window at all: a stored refusal, so it is
    /// not measured again until its source, renderer, recipe or encoder
    /// changes.
    pub fn unmeasurable(
        identity: String,
        source_size: i64,
        source_mtime: i64,
        measured_at_ms: i64,
    ) -> Self {
        Self {
            schema: SCHEMA,
            identity,
            source_size,
            source_mtime,
            admitted: false,
            served_mean: 0.0,
            dropped_mean: 0.0,
            served_p99: 0.0,
            dropped_p99: 0.0,
            p99_tolerance: P99_NOISE_TOLERANCE,
            measured_at_ms,
            windows: Vec::new(),
        }
    }

    /// Why a refused title plays the plain conversion, for its FEL row.
    pub fn refusal(&self) -> String {
        if self.windows.len() < MIN_WINDOWS {
            return format!(
                "too little of this title is lit to measure whether its enhancement layer \
                 survives re-encoding ({} of {} sample windows had picture; {} needed)",
                self.windows.len(),
                WINDOWS,
                MIN_WINDOWS
            );
        }
        format!(
            "the enhancement layer adds less than re-encoding loses for this title \
             (measured ΔE ITP mean {:.2} reconstructed vs {:.2} without the EL, p99 {:.2} vs {:.2})",
            self.served_mean, self.dropped_mean, self.served_p99, self.dropped_p99
        )
    }
}

/// The renderer, the route recipe and the encoder a verdict holds for. A
/// changed renderer (bundle identity), recipe, or encoder (FFmpeg build, VA
/// driver and its version, GPU device) re-qualifies every title: a verdict
/// measured on one GPU or driver never serves on another.
pub fn identity(renderer_identity: &str, recipe: &str, encoder_identity: &str) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            "plurx-dv-fel-qualification-v2",
            renderer_identity,
            recipe,
            encoder_identity,
        ))
        .expect("bounded identity"),
    ))
}

/// The encoder half of [`identity`]: the FFmpeg executable's digest, the VA
/// driver's own description (vendor, Mesa version, GPU), and the render
/// device with its PCI identity.
pub fn encoder_identity(ffmpeg_digest: &str, va_driver: &str, device: &str) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(&("plurx-dv-fel-encoder-v1", ffmpeg_digest, va_driver, device))
            .expect("bounded identity"),
    ))
}

/// The title scan demand binding that asks for one title to be rescanned and
/// re-qualified: never equal to a scanner digest, so the scan worker rebinds
/// the demand to its scanner and runs the title job again.
pub fn requalify_binding(scanner: &str, identity: &str) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(&("plurx-dv-fel-requalify-v1", scanner, identity)).expect("bounded"),
    ))
}

#[derive(Debug, Clone, PartialEq)]
pub enum VerdictState {
    /// No verdict for this source with this renderer and recipe.
    Missing,
    Current(FelVerdict),
}

/// Read a title's verdict from its stored probe document. A verdict for
/// another source size/mtime, renderer or recipe is not this title's verdict.
pub fn verdict_state(
    probe_json: Option<&str>,
    source_size: i64,
    source_mtime: i64,
    identity: &str,
) -> VerdictState {
    probe_json
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .and_then(|document| document.get(PROBE_KEY).cloned())
        .and_then(|value| serde_json::from_value::<FelVerdict>(value).ok())
        .filter(|verdict| {
            verdict.schema == SCHEMA
                && verdict.identity == identity
                && verdict.source_size == source_size
                && verdict.source_mtime == source_mtime
        })
        .map_or(VerdictState::Missing, VerdictState::Current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_e_matches_the_reviewed_python_metrics() {
        // tools/dv_quality/metrics.py, same inputs.
        let ictcp = rgb_nits_to_ictcp([100.0, 50.0, 10.0]);
        for (got, want) in ictcp.iter().zip([
            0.4586408065752814,
            -0.15776046154935652,
            0.11425574015732316,
        ]) {
            assert!((got - want).abs() < 1e-12, "{got} {want}");
        }
        let bar = delta_e_itp(
            rgb_nits_to_ictcp([0.004, 0.005, 0.0034]),
            rgb_nits_to_ictcp([1e-5, 0.0, 2e-5]),
        );
        assert!((bar - 10.314323883388658).abs() < 1e-9, "{bar}");
        assert!((pq_eotf(0.5) - 92.24570899406527).abs() < 1e-9);
        assert_eq!(
            rgb48_to_ictcp([0, 0, 0]),
            rgb_nits_to_ictcp([0.0, 0.0, 0.0])
        );
    }

    #[test]
    fn the_lattice_and_letterbox_follow_the_picture() {
        let (width, height) = (128, 128);
        let mut frame = vec![0u8; width * height * 6];
        // A letterboxed picture: rows 32..96 lit.
        for y in 32..96 {
            for x in 0..width {
                let at = (y * width + x) * 6;
                frame[at..at + 2].copy_from_slice(&30000u16.to_le_bytes());
            }
        }
        let lattice = Lattice::from_rgb48(&frame, width, height).expect("lattice");
        assert_eq!((lattice.columns, lattice.rows), (4, 4));
        // Samples at y = 16, 48, 80, 112: the middle two rows are picture.
        assert_eq!(active_region(&lattice), Some((0, 1, 4, 3)));
        assert!(Lattice::from_rgb48(&frame[1..], width, height).is_none());
        assert_eq!(
            active_region(
                &Lattice::from_rgb48(&vec![0; width * height * 6], width, height)
                    .expect("dark lattice")
            ),
            None
        );
        let mut deltas = Vec::new();
        assert!(active_deltas(&lattice, &lattice, (0, 1, 4, 3), &mut deltas));
        assert_eq!(deltas.len(), 8);
        assert!(deltas.iter().all(|d| *d == 0.0));
    }

    #[test]
    fn admission_needs_a_ten_percent_mean_gain_and_no_worse_p99() {
        // A clearly helpful EL; the measured Beekeeper window (served worse).
        assert!(admits(2.68, 3.27, 7.10, 7.34));
        assert!(!admits(2.90, 1.79, 7.78, 9.60));
        // Better on the mean but worse at p99: refused (sigma is 0).
        assert!(!admits(2.68, 3.27, 7.35, 7.34));
        // Exactly 10% passes; 9.9% does not.
        assert!(admits(9.0, 10.0, 5.0, 5.0));
        assert!(!admits(9.01, 10.0, 5.0, 5.0));
        // p99 within the tolerance passes, beyond it does not.
        assert_eq!(
            P99_NOISE_TOLERANCE, 0.0,
            "2 sigma of byte-identical encodes"
        );
        assert!(admits(1.0, 2.0, 5.0 + P99_NOISE_TOLERANCE, 5.0));
        assert!(!admits(1.0, 2.0, 5.0 + P99_NOISE_TOLERANCE + 0.01, 5.0));
        assert!(!admits(f64::NAN, 2.0, 1.0, 1.0));
        let mut values = [3.0, 1.0, 2.0];
        assert_eq!(mean_p99(&mut values), Some((2.0, 2.0)));
        assert_eq!(mean_p99(&mut []), None);
    }

    #[test]
    fn a_verdict_is_keyed_to_its_source_renderer_and_recipe() {
        let encoder = encoder_identity(
            "ffmpeg-sha",
            "Mesa 26.0.8 (radeonsi)",
            "/dev/dri/renderD128",
        );
        let id = identity("renderer-a", "recipe-1", &encoder);
        assert_ne!(id, identity("renderer-b", "recipe-1", &encoder));
        assert_ne!(id, identity("renderer-a", "recipe-2", &encoder));
        // Another FFmpeg build, VA driver version or GPU re-qualifies.
        for other in [
            encoder_identity(
                "ffmpeg-sha2",
                "Mesa 26.0.8 (radeonsi)",
                "/dev/dri/renderD128",
            ),
            encoder_identity(
                "ffmpeg-sha",
                "Mesa 26.1.0 (radeonsi)",
                "/dev/dri/renderD128",
            ),
            encoder_identity(
                "ffmpeg-sha",
                "Mesa 26.0.8 (radeonsi)",
                "/dev/dri/renderD129",
            ),
        ] {
            assert_ne!(id, identity("renderer-a", "recipe-1", &other));
        }
        assert_ne!(requalify_binding("scanner", &id), "scanner");
        let window = WindowScore {
            start_ms: 1000,
            served_mean: 1.0,
            served_p99: 2.0,
            dropped_mean: 2.0,
            dropped_p99: 2.0,
            samples: 3,
        };
        let verdict = FelVerdict::from_windows(
            id.clone(),
            10,
            20,
            30,
            vec![window; MIN_WINDOWS],
            &mut [1.0, 1.0, 1.0],
            &mut [2.0, 2.0, 2.0],
        )
        .expect("verdict");
        assert!(verdict.admitted);
        let document = serde_json::json!({"streams": [], PROBE_KEY: verdict}).to_string();
        assert_eq!(
            verdict_state(Some(&document), 10, 20, &id),
            VerdictState::Current(verdict.clone())
        );
        // Another source, renderer/recipe or schema: no verdict for this play.
        assert_eq!(
            verdict_state(Some(&document), 11, 20, &id),
            VerdictState::Missing
        );
        assert_eq!(
            verdict_state(Some(&document), 10, 21, &id),
            VerdictState::Missing
        );
        assert_eq!(
            verdict_state(
                Some(&document),
                10,
                20,
                &identity("renderer-b", "recipe-1", &encoder)
            ),
            VerdictState::Missing
        );
        assert_eq!(verdict_state(None, 10, 20, &id), VerdictState::Missing);
        assert_eq!(
            verdict_state(Some("{}"), 10, 20, &id),
            VerdictState::Missing
        );
        let refused = FelVerdict::from_windows(id, 10, 20, 30, vec![], &mut [], &mut []);
        assert!(refused.is_none(), "no windows, no verdict");
        let no = FelVerdict {
            admitted: false,
            served_mean: 3.06,
            dropped_mean: 1.81,
            served_p99: 10.65,
            dropped_p99: 5.98,
            ..verdict
        };
        assert!(
            no.refusal().contains("adds less than re-encoding loses"),
            "{}",
            no.refusal()
        );
    }

    #[test]
    fn dark_windows_fall_back_to_their_alternates_and_too_few_lit_ones_refuse() {
        // Primary at (i+1)/(K+1), alternate half a spacing later.
        assert_eq!(
            sample_positions(9_000, 2),
            vec![[3_000, 4_500], [6_000, 7_500]]
        );
        assert_eq!(sample_positions(7_200_000, WINDOWS).len(), WINDOWS);
        assert!(sample_positions(0, WINDOWS).is_empty());
        // The brightest picture of a fade-in sets the window's letterbox.
        let (width, height) = (128, 128);
        let dark =
            Lattice::from_rgb48(&vec![0u8; width * height * 6], width, height).expect("dark");
        let mut frame = vec![0u8; width * height * 6];
        for y in 32..96 {
            for x in 0..width {
                let at = (y * width + x) * 6;
                frame[at..at + 2].copy_from_slice(&30000u16.to_le_bytes());
            }
        }
        let lit = Lattice::from_rgb48(&frame, width, height).expect("lit");
        let fade = [dark.clone(), dark.clone(), lit];
        assert_eq!(brightest(&fade).and_then(active_region), Some((0, 1, 4, 3)));
        assert_eq!(
            brightest(&[dark.clone(), dark]).and_then(active_region),
            None
        );
        // Fewer than MIN_WINDOWS lit windows never admit, however good.
        let window = WindowScore {
            start_ms: 0,
            served_mean: 0.5,
            served_p99: 1.0,
            dropped_mean: 3.0,
            dropped_p99: 9.0,
            samples: 3,
        };
        let few = FelVerdict::from_windows(
            "id".into(),
            1,
            2,
            3,
            vec![window.clone(); MIN_WINDOWS - 1],
            &mut [0.5; 3],
            &mut [3.0; 3],
        )
        .expect("verdict");
        assert!(!few.admitted);
        assert!(
            few.refusal().contains("too little of this title is lit"),
            "{}",
            few.refusal()
        );
        let enough = FelVerdict::from_windows(
            "id".into(),
            1,
            2,
            3,
            vec![window; MIN_WINDOWS],
            &mut [0.5; 3],
            &mut [3.0; 3],
        )
        .expect("verdict");
        assert!(enough.admitted);
        // No lit window at all is still a stored answer.
        let none = FelVerdict::unmeasurable("id".into(), 1, 2, 3);
        assert!(!none.admitted && none.windows.is_empty());
        assert!(
            none.refusal().contains("0 of 8 sample windows"),
            "{}",
            none.refusal()
        );
    }
}
