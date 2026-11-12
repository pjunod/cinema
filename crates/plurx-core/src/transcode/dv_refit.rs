//! Re-fitted Profile 8.1 mapping curves for Profile 7 FEL titles ("F+").
//!
//! The plain P7 → 8.1 conversion ([`super::dvconvert`]) serves the disc's own
//! base-layer bits and drops the enhancement layer, with identity mapping
//! curves. An 8.1 RPU can still carry real curves: up to eight luma pieces of
//! 2nd-order polynomials and a 3rd-order chroma MMR over Y, Cb and Cr. Fitted
//! per scene so that the untouched base layer lands as close as possible to
//! the FEL composition, they recover the part of the enhancement layer that is
//! a function of each pixel's own base-layer value. That costs no encode.
//!
//! The offline result and its protocol are in
//! `tools/dv_processing/diagnostics/fel_quality/RPU-REFIT.md`. Measured there
//! (pooled ΔITP against the composition, model B): Michael5400 3.20 → 1.46,
//! Michael1800 3.19 → 1.34, Beekeeper 1.46 → 0.96, with every frame better
//! than the identity curves. macOS applies the curves. The Lenovo TB322FC's
//! Dolby mobile decoder ignores them and shows what the identity curves show.
//!
//! This module is the arithmetic only, ported from `rpu_refit_eval.py`
//! (`Fit.add` and `Fit.solve_safe`, protocol amendments 3–4) and checked
//! against that script's own numbers on the three fixtures
//! (`tests/fixtures/dv-refit/`):
//! - [`RefitStats`] accumulates samples: a base-layer code triple and the
//!   12-bit composition at the same pixel.
//! - [`RefitStats::solve`] produces the safe fit. Luma follows the monotone
//!   conditional mean, continued by identity slope outside the codes the scene
//!   uses, as eight continuously joined pieces. Chroma is a ridge MMR pulled
//!   toward identity.
//! - [`servable`] checks the fit's shape as it will be written, and is the
//!   only way to make the [`ServableCurves`] that [`apply_to_rpu`] takes.
//! - [`held_out_verdict`] scores the fit against identity on samples it did
//!   not see, exactly from their statistics ([`RefitStats::squared_error`]).
//! - [`RefitCurves::composer`] converts the fit into the RPU's normative domain.
//! - [`apply_to_rpu`] writes the curves into an RPU already converted to 8.1.
//!
//! Decoding the layers, composing them, finding scenes, and deciding which
//! titles get curves belong to the callers.

use dolby_vision::rpu::dovi_rpu::DoviRpu;
use dolby_vision::rpu::rpu_data_mapping::{DoviMMRCurve, DoviMappingMethod, DoviPolynomialCurve};
use serde::{Deserialize, Serialize};

/// Names the fitting method. Stored curves carry it, and anything serving
/// them folds it into the output identity, so a change of method is a change
/// of bytes that caches can see.
pub const FIT_VERSION: &str = "dv-refit-safe-v1";

/// Luma pieces: eight equal-width pieces over the 10-bit base-layer code.
pub const LUMA_PIECES: usize = 8;
/// Pivot deltas of those pieces, as the RPU stores them (pivots 0, 128, …, 896, 1023).
pub const LUMA_PIVOT_DELTAS: [u16; LUMA_PIECES + 1] = [0, 128, 128, 128, 128, 128, 128, 128, 127];
/// Terms of a 3rd-order MMR: a constant and y, u, v, yu, yv, uv, yuv to powers 1–3.
pub const MMR_TERMS: usize = 22;

const CODES: usize = 1024;
/// The fit's domain: 10-bit code / 1023 in, 12-bit value / 4095 out.
const IN_SCALE: f64 = 1023.0;
const OUT_SCALE: f64 = 4095.0;
/// Identity slope of a 10-bit code into 12-bit output.
const IDENTITY_SLOPE: f64 = 4.0;
/// Ridge strength per sample pulling the chroma MMR toward identity (amendment 3).
const RIDGE_PER_SAMPLE: f64 = 1e-5;
/// Floor weight per luma code, as a share of the mean populated count (amendment 3).
const LUMA_FLOOR_SHARE: f64 = 0.01;
/// The RPU's coefficient precision for the fixtures (`coefficient_log2_denom` 23).
const FIT_QUANT: f64 = 8_388_608.0; // 2^23

/// Polynomial degree of each MMR term, in basis order (used for domain conversion).
const MMR_DEGREE: [i32; MMR_TERMS] = [
    0, 1, 1, 1, 2, 2, 2, 3, 2, 2, 2, 4, 4, 4, 6, 3, 3, 3, 6, 6, 6, 9,
];

/// Sufficient statistics of one scene's samples.
///
/// The serde layout is what `segment_decode_render --refit-stats` prints for
/// each scene's `fit` and `held_out` (`tools/dv_processing/refit_compose.h`).
/// Deserialising checks it the way [`RefitStats::from_parts`] does, and also
/// the second moments, so a malformed helper line is refused, not solved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RefitStatsParts")]
pub struct RefitStats {
    /// Per 10-bit luma code: sum of composed 12-bit luma, and sample count.
    ysum: Vec<f64>,
    ycnt: Vec<f64>,
    /// Normal equations of the chroma MMR: XᵀX (22×22, row-major) and XᵀY (22×2).
    xtx: Vec<f64>,
    xty: Vec<f64>,
    /// Second moments, for scoring curves on held-out samples
    /// ([`RefitStats::squared_error`]): Σ composed luma² per code, and
    /// Σ (composed chroma / 4095)² for Cb and Cr. Statistics loaded through
    /// [`RefitStats::from_parts`] have none.
    ysq: Vec<f64>,
    csq: Vec<f64>,
    second_moments: bool,
}

/// [`RefitStats`] as serialised, before its checks.
#[derive(Deserialize)]
struct RefitStatsParts {
    ysum: Vec<f64>,
    ycnt: Vec<f64>,
    xtx: Vec<f64>,
    xty: Vec<f64>,
    ysq: Vec<f64>,
    csq: Vec<f64>,
    second_moments: bool,
}

impl TryFrom<RefitStatsParts> for RefitStats {
    type Error = RefitError;

    fn try_from(parts: RefitStatsParts) -> Result<Self, RefitError> {
        let mut stats = Self::from_parts(parts.ysum, parts.ycnt, parts.xtx, parts.xty)?;
        if parts.ysq.len() != CODES
            || parts.csq.len() != 2
            || parts
                .ysq
                .iter()
                .chain(&parts.csq)
                .any(|v| !v.is_finite() || *v < 0.0)
        {
            return Err(RefitError::Shape);
        }
        stats.ysq = parts.ysq;
        stats.csq = parts.csq;
        stats.second_moments = parts.second_moments;
        Ok(stats)
    }
}

impl Default for RefitStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a fit, a conversion or a write was refused.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RefitError {
    #[error("the scene has no samples")]
    Empty,
    #[error("the statistics have the wrong shape")]
    Shape,
    #[error("the fit's linear system is singular")]
    Singular,
    #[error("coefficient {0} is outside the writable range [-64, 63)")]
    Unwritable(f64),
    #[error("the RPU is profile {0}, not a converted 8.1 RPU")]
    NotProfile8(u8),
    #[error("the RPU carries no mapping section (it reuses the previous RPU's)")]
    NoMapping,
    #[error("the RPU's header or NLQ is outside what this writes (fixed-point coefficients, 10-bit base, 12-bit output, no NLQ)")]
    UnsupportedHeader,
}

fn mmr_basis(y: f64, u: f64, v: f64) -> [f64; MMR_TERMS] {
    let t = [y, u, v, y * u, y * v, u * v, y * u * v];
    let mut x = [0.0; MMR_TERMS];
    x[0] = 1.0;
    for p in 0..3 {
        for (k, term) in t.iter().enumerate() {
            x[1 + 7 * p + k] = term.powi(p as i32 + 1);
        }
    }
    x
}

impl RefitStats {
    pub fn new() -> Self {
        Self {
            ysum: vec![0.0; CODES],
            ycnt: vec![0.0; CODES],
            xtx: vec![0.0; MMR_TERMS * MMR_TERMS],
            xty: vec![0.0; MMR_TERMS * 2],
            ysq: vec![0.0; CODES],
            csq: vec![0.0; 2],
            second_moments: true,
        }
    }

    /// Statistics as `rpu_refit_eval.py --dump-fit-stats` writes them, or as
    /// a helper reports them: refused unless every value is finite, counts
    /// are non-negative, and XᵀX's constant term is the sample count (the
    /// constant basis term is 1 for every sample). They carry no second
    /// moments, so they can be fitted but not used as held-out samples.
    pub fn from_parts(
        ysum: Vec<f64>,
        ycnt: Vec<f64>,
        xtx: Vec<f64>,
        xty: Vec<f64>,
    ) -> Result<Self, RefitError> {
        if ysum.len() != CODES
            || ycnt.len() != CODES
            || xtx.len() != MMR_TERMS * MMR_TERMS
            || xty.len() != MMR_TERMS * 2
        {
            return Err(RefitError::Shape);
        }
        let finite = ysum
            .iter()
            .chain(&ycnt)
            .chain(&xtx)
            .chain(&xty)
            .all(|v| v.is_finite());
        let samples: f64 = ycnt.iter().sum();
        if !finite
            || ycnt.iter().any(|&n| n < 0.0)
            || (xtx[0] - samples).abs() > 1e-9 * samples.max(1.0)
        {
            return Err(RefitError::Shape);
        }
        Ok(Self {
            ysum,
            ycnt,
            xtx,
            xty,
            ysq: vec![0.0; CODES],
            csq: vec![0.0; 2],
            second_moments: false,
        })
    }

    /// One sample: the base layer's 10-bit (Y, Cb, Cr) at a pixel and the
    /// 12-bit composition (Y, Cb, Cr) at the same pixel, paired as the
    /// composer pairs them.
    pub fn add(&mut self, base: [u16; 3], composed: [u16; 3]) {
        let base = base.map(|c| c.min(1023));
        let composed = composed.map(|c| c.min(4095));
        let code = usize::from(base[0]);
        self.ysum[code] += f64::from(composed[0]);
        self.ycnt[code] += 1.0;
        self.ysq[code] += f64::from(composed[0]).powi(2);
        let x = mmr_basis(
            f64::from(base[0]) / IN_SCALE,
            f64::from(base[1]) / IN_SCALE,
            f64::from(base[2]) / IN_SCALE,
        );
        let y = [
            f64::from(composed[1]) / OUT_SCALE,
            f64::from(composed[2]) / OUT_SCALE,
        ];
        for i in 0..MMR_TERMS {
            let row = &mut self.xtx[i * MMR_TERMS..(i + 1) * MMR_TERMS];
            for (j, cell) in row.iter_mut().enumerate() {
                *cell += x[i] * x[j];
            }
            self.xty[2 * i] += x[i] * y[0];
            self.xty[2 * i + 1] += x[i] * y[1];
        }
        self.csq[0] += y[0] * y[0];
        self.csq[1] += y[1] * y[1];
    }

    /// Pools another scene's statistics into this one.
    pub fn merge(&mut self, other: &RefitStats) {
        for (a, b) in self
            .ysum
            .iter_mut()
            .chain(self.ycnt.iter_mut())
            .chain(self.xtx.iter_mut())
            .chain(self.xty.iter_mut())
            .chain(self.ysq.iter_mut())
            .chain(self.csq.iter_mut())
            .zip(
                other
                    .ysum
                    .iter()
                    .chain(&other.ycnt)
                    .chain(&other.xtx)
                    .chain(&other.xty)
                    .chain(&other.ysq)
                    .chain(&other.csq),
            )
        {
            *a += *b;
        }
        self.second_moments &= other.second_moments;
    }

    pub fn samples(&self) -> f64 {
        self.ycnt.iter().sum()
    }

    /// Squared error of `curves` on these samples, in 12-bit codes², from the
    /// sufficient statistics alone: Σ(c − f(b))² = Σc² − 2f(b)Σc + n·f(b)²
    /// per luma code, and yᵀy − 2βᵀXᵀy + βᵀXᵀXβ for the chroma MMR. `None`
    /// when the statistics carry no second moments.
    pub fn squared_error(&self, curves: &RefitCurves) -> Option<SquaredError> {
        if !self.second_moments {
            return None;
        }
        let mut luma = [0.0; LUMA_PIECES];
        let mut luma_samples = [0.0; LUMA_PIECES];
        for code in 0..CODES {
            let n = self.ycnt[code];
            if n == 0.0 {
                continue;
            }
            let f = curves.luma_at(code);
            let piece = code / 128;
            luma[piece] += (self.ysq[code] - 2.0 * f * self.ysum[code] + n * f * f).max(0.0);
            luma_samples[piece] += n;
        }
        let mut chroma = 0.0;
        for ch in 0..2 {
            let beta: Vec<f64> = (0..MMR_TERMS).map(|i| curves.mmr[i][ch]).collect();
            let cross: f64 = (0..MMR_TERMS).map(|i| beta[i] * self.xty[2 * i + ch]).sum();
            let quad: f64 = (0..MMR_TERMS)
                .map(|i| {
                    beta[i]
                        * (0..MMR_TERMS)
                            .map(|j| self.xtx[i * MMR_TERMS + j] * beta[j])
                            .sum::<f64>()
                })
                .sum();
            chroma += (self.csq[ch] - 2.0 * cross + quad).max(0.0) * OUT_SCALE * OUT_SCALE;
        }
        Some(SquaredError {
            luma,
            luma_samples,
            chroma,
            samples: self.samples(),
        })
    }

    /// The safe fit (protocol amendments 3–4), quantised to the RPU's 2^-23.
    pub fn solve(&self) -> Result<RefitCurves, RefitError> {
        let populated: Vec<usize> = (0..CODES).filter(|&c| self.ycnt[c] > 0.0).collect();
        if populated.is_empty() {
            return Err(RefitError::Empty);
        }
        let target = self.luma_target(&populated);

        // Eight pieces fitted jointly by weighted least squares, joined (C0)
        // half a code before each pivot: an equality-constrained system solved
        // through its KKT matrix.
        let mean_count =
            populated.iter().map(|&c| self.ycnt[c]).sum::<f64>() / populated.len() as f64;
        let floor = LUMA_FLOOR_SHARE * mean_count;
        const N: usize = 3 * LUMA_PIECES;
        const M: usize = LUMA_PIECES - 1;
        let mut kkt = vec![0.0; (N + M) * (N + M)];
        let mut rhs = vec![0.0; N + M];
        for (c, &t) in target.iter().enumerate() {
            let w = self.ycnt[c] + floor;
            let piece = c / 128;
            let xn = c as f64 / IN_SCALE;
            let g = [1.0, xn, xn * xn];
            for (a, ga) in g.iter().enumerate() {
                for (b, gb) in g.iter().enumerate() {
                    kkt[(3 * piece + a) * (N + M) + 3 * piece + b] += w * ga * gb;
                }
                rhs[3 * piece + a] += w * ga * t / OUT_SCALE;
            }
        }
        for k in 1..LUMA_PIECES {
            let xb = (128.0 * k as f64 - 0.5) / IN_SCALE;
            let row = [1.0, xb, xb * xb];
            for (a, &value) in row.iter().enumerate() {
                let left = 3 * (k - 1) + a;
                let right = 3 * k + a;
                kkt[(N + k - 1) * (N + M) + left] = value;
                kkt[(N + k - 1) * (N + M) + right] = -value;
                kkt[left * (N + M) + N + k - 1] = value;
                kkt[right * (N + M) + N + k - 1] = -value;
            }
        }
        let solution = solve_dense(kkt, rhs, N + M)?;
        let mut luma = [[0.0; 3]; LUMA_PIECES];
        for (p, piece) in luma.iter_mut().enumerate() {
            for (a, coef) in piece.iter_mut().enumerate() {
                *coef = quantise(solution[3 * p + a]);
            }
        }

        // Chroma: ridge regression toward identity (Cb ← u, Cr ← v).
        let lambda = RIDGE_PER_SAMPLE * self.samples();
        let identity = IDENTITY_SLOPE * IN_SCALE / OUT_SCALE;
        let mut mmr = [[0.0; 2]; MMR_TERMS];
        for (ch, ident_term) in [(0usize, 2usize), (1, 3)] {
            let mut a = self.xtx.clone();
            for i in 0..MMR_TERMS {
                a[i * MMR_TERMS + i] += lambda;
            }
            let mut b: Vec<f64> = (0..MMR_TERMS).map(|i| self.xty[2 * i + ch]).collect();
            b[ident_term] += lambda * identity;
            let x = solve_dense(a, b, MMR_TERMS)?;
            for i in 0..MMR_TERMS {
                mmr[i][ch] = quantise(x[i]);
            }
        }
        Ok(RefitCurves { luma, mmr })
    }

    /// Monotone (isotonic) conditional mean of composed luma per code, linear
    /// between populated codes and continued by identity slope beyond them.
    fn luma_target(&self, populated: &[usize]) -> Vec<f64> {
        // Pool-adjacent-violators, count-weighted.
        let mut blocks: Vec<(f64, f64, usize)> = Vec::with_capacity(populated.len());
        for &c in populated {
            let w = self.ycnt[c];
            blocks.push((self.ysum[c], w, 1));
            while blocks.len() > 1 {
                let n = blocks.len();
                if blocks[n - 2].0 / blocks[n - 2].1 > blocks[n - 1].0 / blocks[n - 1].1 {
                    let last = blocks.pop().expect("two blocks");
                    let prev = blocks.last_mut().expect("one block");
                    prev.0 += last.0;
                    prev.1 += last.1;
                    prev.2 += last.2;
                } else {
                    break;
                }
            }
        }
        let iso: Vec<f64> = blocks
            .iter()
            .flat_map(|&(s, w, n)| std::iter::repeat_n(s / w, n))
            .collect();
        let (lo, hi) = (populated[0], populated[populated.len() - 1]);
        let mut target = vec![0.0; CODES];
        let mut next = 0usize;
        for (c, value) in target.iter_mut().enumerate() {
            *value = if c < lo {
                IDENTITY_SLOPE * c as f64 + (iso[0] - IDENTITY_SLOPE * lo as f64)
            } else if c > hi {
                IDENTITY_SLOPE * c as f64 + (iso[iso.len() - 1] - IDENTITY_SLOPE * hi as f64)
            } else {
                while populated[next + 1..].first().is_some_and(|&p| p <= c) {
                    next += 1;
                }
                if populated[next] == c {
                    iso[next]
                } else {
                    let (c0, c1) = (populated[next], populated[next + 1]);
                    let f = (c - c0) as f64 / (c1 - c0) as f64;
                    iso[next] + f * (iso[next + 1] - iso[next])
                }
            };
            *value = value.clamp(0.0, OUT_SCALE);
        }
        target
    }
}

fn quantise(v: f64) -> f64 {
    (v * FIT_QUANT).round() / FIT_QUANT
}

/// Gaussian elimination with partial pivoting; `a` is `n×n` row-major.
fn solve_dense(mut a: Vec<f64>, mut b: Vec<f64>, n: usize) -> Result<Vec<f64>, RefitError> {
    let scale = a
        .iter()
        .fold(0.0f64, |m, v| m.max(v.abs()))
        .max(f64::MIN_POSITIVE);
    for col in 0..n {
        let pivot = (col..n)
            .max_by(|&i, &j| a[i * n + col].abs().total_cmp(&a[j * n + col].abs()))
            .expect("non-empty range");
        if a[pivot * n + col].abs() <= scale * 1e-15 {
            return Err(RefitError::Singular);
        }
        if pivot != col {
            for k in 0..n {
                a.swap(col * n + k, pivot * n + k);
            }
            b.swap(col, pivot);
        }
        for row in col + 1..n {
            let f = a[row * n + col] / a[col * n + col];
            if f != 0.0 {
                for k in col..n {
                    a[row * n + k] -= f * a[col * n + k];
                }
                b[row] -= f * b[col];
            }
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let s: f64 = (row + 1..n).map(|k| a[row * n + k] * x[k]).sum();
        x[row] = (b[row] - s) / a[row * n + row];
    }
    Ok(x)
}

/// Squared error of a set of curves on a set of samples, in 12-bit codes².
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SquaredError {
    /// Per luma piece, and the samples in it.
    pub luma: [f64; LUMA_PIECES],
    pub luma_samples: [f64; LUMA_PIECES],
    /// Cb and Cr together.
    pub chroma: f64,
    pub samples: f64,
}

impl SquaredError {
    pub fn total(&self) -> f64 {
        self.luma.iter().sum::<f64>() + self.chroma
    }
}

/// How the fitted curves did on held-out samples against the identity
/// curves today's conversion serves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeldOutVerdict {
    pub identity: SquaredError,
    pub fitted: SquaredError,
    /// 1 − fitted/identity over everything: the share of the identity
    /// serve's squared code error the curves remove.
    pub gain: f64,
}

/// A luma piece is judged on its own once it has this many held-out samples.
const PIECE_JUDGED_FROM: f64 = 1000.0;
/// …and may not be worse than identity by more than this share there.
const PIECE_WORSE_ALLOWED: f64 = 0.05;
/// Slack per sample, in 12-bit codes², for a part (chroma, a luma piece)
/// that identity already serves exactly: rounding the fit to 2^-23 and the
/// ridge's pull cost a fraction of a code, and that must not refuse a scene
/// whose gain is elsewhere.
const SLACK_PER_SAMPLE: f64 = 0.25;

/// Scores `curves` and the identity curves on held-out samples (samples
/// the fit did not see). Refused when the curves do no better than identity
/// overall, or worse on chroma or on any luma piece with enough samples
/// (beyond 5% and a quarter of a code² per sample), because a fit can only
/// be trusted where it is shown to beat what it replaces.
pub fn held_out_verdict(
    curves: &RefitCurves,
    held_out: &RefitStats,
) -> Result<HeldOutVerdict, Unservable> {
    let identity_curves = RefitCurves::identity();
    let (identity, fitted) = match (
        held_out.squared_error(&identity_curves),
        held_out.squared_error(curves),
    ) {
        (Some(i), Some(f)) if i.samples > 0.0 => (i, f),
        _ => return Err(Unservable::NoHeldOutSamples),
    };
    if fitted.total() >= identity.total() {
        return Err(Unservable::WorseThanIdentity("overall"));
    }
    if fitted.chroma
        > identity.chroma * (1.0 + PIECE_WORSE_ALLOWED) + SLACK_PER_SAMPLE * 2.0 * identity.samples
    {
        return Err(Unservable::WorseThanIdentity("chroma"));
    }
    for p in 0..LUMA_PIECES {
        if identity.luma_samples[p] >= PIECE_JUDGED_FROM
            && fitted.luma[p]
                > identity.luma[p] * (1.0 + PIECE_WORSE_ALLOWED)
                    + SLACK_PER_SAMPLE * identity.luma_samples[p]
        {
            return Err(Unservable::WorseThanIdentity("a luma piece"));
        }
    }
    let gain = 1.0 - fitted.total() / identity.total();
    Ok(HeldOutVerdict {
        identity,
        fitted,
        gain,
    })
}

/// Why curves may not be served. The scene keeps identity curves.
#[derive(Clone, Debug, thiserror::Error, PartialEq)]
pub enum Unservable {
    #[error("a coefficient is outside the writable range [-64, 63)")]
    Unwritable,
    #[error("the luma curve falls")]
    NotMonotone,
    #[error("the luma curve flattens a gradient (a 32-code span rises {0:.1} codes)")]
    Flattens(f64),
    #[error("the luma curve departs from identity by {0:.0} codes")]
    LumaDeparture(f64),
    #[error("the chroma curve departs from identity by {0:.0} codes")]
    ChromaDeparture(f64),
    #[error("no held-out samples")]
    NoHeldOutSamples,
    #[error("worse than identity on held-out samples ({0})")]
    WorseThanIdentity(&'static str),
}

/// Bounds on the shape of servable curves, checked on the composer-domain
/// coefficients exactly as they will be written. The offline fits'
/// worst cases were 33 (luma) and 80 (chroma) codes from identity
/// (RPU-REFIT.md); these bounds sit well outside them and stop a
/// pathological scene. They bound shape, not error: harm is judged on
/// held-out samples by [`held_out_verdict`].
const LUMA_DEPARTURE_LIMIT: f64 = 256.0;
const CHROMA_DEPARTURE_LIMIT: f64 = 512.0;
/// Any 32-code span of the luma curve must rise at least this share of the
/// identity curve's 128 codes, so a plateau cannot crush a gradient.
const MIN_SPAN_RISE_SHARE: f64 = 0.25;

/// Curves that passed [`servable`]: the only input [`apply_to_rpu`] takes.
#[derive(Clone, Debug, PartialEq)]
pub struct ServableCurves(ComposerCurves);

impl ServableCurves {
    pub fn curves(&self) -> &ComposerCurves {
        &self.0
    }
}

/// Checks the shape of a fit as it will be written: coefficients writable
/// at 2^-23, luma rising everywhere and never flattening a 32-code span,
/// and both curves within bounds of identity. Stored curves are checked
/// again here when they are loaded for serving.
pub fn servable(curves: &RefitCurves) -> Result<ServableCurves, Unservable> {
    let composer = curves.composer();
    if composer
        .luma
        .iter()
        .flatten()
        .chain(composer.chroma.iter().flatten())
        .any(|&v| split(v, 23).is_err())
    {
        return Err(Unservable::Unwritable);
    }
    let mid = 512u16;
    let luma: Vec<f64> = (0..CODES as u16)
        .map(|c| f64::from(composer.reshape([c, mid, mid])[0]))
        .collect();
    if luma.windows(2).any(|w| w[1] < w[0]) {
        return Err(Unservable::NotMonotone);
    }
    let min_rise = MIN_SPAN_RISE_SHARE * IDENTITY_SLOPE * 32.0;
    if let Some(rise) = luma
        .windows(33)
        .map(|w| w[32] - w[0])
        .find(|&r| r < min_rise)
    {
        return Err(Unservable::Flattens(rise));
    }
    let d = curves.diagnostics();
    if d.luma_max_abs_minus_identity > LUMA_DEPARTURE_LIMIT {
        return Err(Unservable::LumaDeparture(d.luma_max_abs_minus_identity));
    }
    if d.chroma_max_abs_minus_identity_on_grid > CHROMA_DEPARTURE_LIMIT {
        return Err(Unservable::ChromaDeparture(
            d.chroma_max_abs_minus_identity_on_grid,
        ));
    }
    Ok(ServableCurves(composer))
}

/// A scene's fitted curves in the fit's domain (code/1023 in, value/4095 out).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RefitCurves {
    /// Per luma piece: c0 + c1·x + c2·x².
    pub luma: [[f64; 3]; LUMA_PIECES],
    /// MMR coefficients per term, for Cb then Cr.
    pub mmr: [[f64; 2]; MMR_TERMS],
}

/// Fit diagnostics, in 12-bit codes: what decides whether curves are safe to serve.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RefitDiagnostics {
    pub luma_max_abs_minus_identity: f64,
    pub luma_monotone: bool,
    pub luma_max_excess_step_at_pivots: f64,
    pub chroma_max_abs_minus_identity_on_grid: f64,
    pub max_abs_coefficient: f64,
}

/// The curves in the RPU's normative domain: base-layer code / 2^10 in,
/// 1.0 = 2^12 out. A decoder sampling a UNORM texture sees code / 1023
/// instead; the two differ by 0.1% of the input, at most about one 10-bit
/// code at peak, and the macOS flat-patch check (median 0.07%) cannot tell
/// them apart. Which one Apple uses is not established.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComposerCurves {
    pub luma: [[f64; 3]; LUMA_PIECES],
    /// Constant then the 21 terms, for Cb then Cr.
    pub chroma: [[f64; MMR_TERMS]; 2],
}

impl RefitCurves {
    /// The curves today's conversion serves: luma and chroma passed through.
    pub fn identity() -> Self {
        let slope = IDENTITY_SLOPE * IN_SCALE / OUT_SCALE;
        let mut mmr = [[0.0; 2]; MMR_TERMS];
        mmr[2][0] = slope;
        mmr[3][1] = slope;
        Self {
            luma: [[0.0, slope, 0.0]; LUMA_PIECES],
            mmr,
        }
    }

    fn luma_at(&self, code: usize) -> f64 {
        let c = self.luma[(code / 128).min(LUMA_PIECES - 1)];
        let x = code as f64 / IN_SCALE;
        (c[0] + c[1] * x + c[2] * x * x) * OUT_SCALE
    }

    pub fn diagnostics(&self) -> RefitDiagnostics {
        let luma: Vec<f64> = (0..CODES).map(|c| self.luma_at(c)).collect();
        let luma_max_abs_minus_identity = luma
            .iter()
            .enumerate()
            .map(|(c, v)| (v - IDENTITY_SLOPE * c as f64).abs())
            .fold(0.0, f64::max);
        let luma_monotone = luma.windows(2).all(|w| w[1] - w[0] >= -1e-6);
        let luma_max_excess_step_at_pivots = (1..LUMA_PIECES)
            .map(|k| {
                let p = 128 * k;
                let step = luma[p] - luma[p - 1];
                let expected = 0.5 * ((luma[p - 1] - luma[p - 2]) + (luma[p + 1] - luma[p]));
                (step - expected).abs()
            })
            .fold(0.0, f64::max);
        let grid: Vec<f64> = (0..17)
            .map(|i| (1023.0 * i as f64 / 16.0).round())
            .collect();
        let mut chroma_max = 0.0f64;
        for &y in &grid {
            for &u in &grid {
                for &v in &grid {
                    let x = mmr_basis(y / IN_SCALE, u / IN_SCALE, v / IN_SCALE);
                    for (ch, input) in [(0usize, u), (1, v)] {
                        let out: f64 = (0..MMR_TERMS).map(|i| x[i] * self.mmr[i][ch]).sum();
                        chroma_max =
                            chroma_max.max((out * OUT_SCALE - IDENTITY_SLOPE * input).abs());
                    }
                }
            }
        }
        let max_abs_coefficient = self
            .luma
            .iter()
            .flatten()
            .chain(self.mmr.iter().flatten())
            .fold(0.0f64, |m, v| m.max(v.abs()));
        RefitDiagnostics {
            luma_max_abs_minus_identity,
            luma_monotone,
            luma_max_excess_step_at_pivots,
            chroma_max_abs_minus_identity_on_grid: chroma_max,
            max_abs_coefficient,
        }
    }

    /// Converts to the normative domain: input x = code/1024 instead of
    /// code/1023, output scaled by 4096 instead of 4095.
    pub fn composer(&self) -> ComposerCurves {
        let k = OUT_SCALE / 4096.0;
        let rs = 1024.0 / IN_SCALE;
        let mut luma = [[0.0; 3]; LUMA_PIECES];
        for (out, c) in luma.iter_mut().zip(&self.luma) {
            *out = [k * c[0], k * c[1] * rs, k * c[2] * rs * rs];
        }
        let mut chroma = [[0.0; MMR_TERMS]; 2];
        for (ch, out) in chroma.iter_mut().enumerate() {
            for i in 0..MMR_TERMS {
                out[i] = k * self.mmr[i][ch] * rs.powi(MMR_DEGREE[i]);
            }
        }
        ComposerCurves { luma, chroma }
    }
}

impl ComposerCurves {
    /// `pl_shader_dovi_reshape`'s arithmetic on one 10-bit (Y, Cb, Cr), with
    /// the code / 1024 input of this domain: the 12-bit composition a
    /// decoder produces from these curves.
    pub fn reshape(&self, base: [u16; 3]) -> [u16; 3] {
        let x = base.map(|c| (f64::from(c) / 1024.0).clamp(0.0, 1023.0 / 1024.0));
        let mut pivots = [0u16; LUMA_PIECES + 1];
        let mut acc = 0u16;
        for (p, d) in pivots.iter_mut().zip(LUMA_PIVOT_DELTAS) {
            acc += d;
            *p = acc;
        }
        let piece = pivots[1..LUMA_PIECES]
            .iter()
            .take_while(|&&p| base[0] >= p)
            .count();
        let c = self.luma[piece];
        let y = c[0] + c[1] * x[0] + c[2] * x[0] * x[0];
        let basis = mmr_basis(x[0], x[1], x[2]);
        let cb: f64 = (0..MMR_TERMS).map(|i| basis[i] * self.chroma[0][i]).sum();
        let cr: f64 = (0..MMR_TERMS).map(|i| basis[i] * self.chroma[1][i]).sum();
        [y, cb, cr].map(|s| (s.clamp(0.0, 1.0) * 4096.0 + 0.5).floor().min(4095.0) as u16)
    }
}

/// Fixed-point split as the RPU stores it: `int + frac / 2^denom`.
fn split(v: f64, denom: u64) -> Result<(i64, u64), RefitError> {
    if !(v.is_finite() && (-64.0..63.0).contains(&v)) {
        return Err(RefitError::Unwritable(v));
    }
    let scale = (1u64 << denom) as f64;
    let mut int = v.floor() as i64;
    let mut frac = ((v - int as f64) * scale).round() as u64;
    if frac == 1u64 << denom {
        int += 1;
        frac = 0;
    }
    Ok((int, frac))
}

/// Replaces the mapping of an RPU already converted to 8.1 with `curves`:
/// eight 2nd-order luma pieces and one 3rd-order MMR piece per chroma
/// component. Everything else in the RPU (L1, L2, …) is kept. On error the
/// RPU is unchanged.
pub fn apply_to_rpu(rpu: &mut DoviRpu, curves: &ServableCurves) -> Result<(), RefitError> {
    let curves = curves.curves();
    if rpu.dovi_profile != 8 {
        return Err(RefitError::NotProfile8(rpu.dovi_profile));
    }
    let header = &rpu.header;
    if header.coefficient_data_type != 0
        || header.bl_bit_depth_minus8 != 2
        || header.vdr_bit_depth_minus8 != 4
    {
        return Err(RefitError::UnsupportedHeader);
    }
    // An RPU that reuses the previous one's mapping (use_prev_vdr_rpu_flag)
    // carries none of its own; the caller leaves it as it is, and the
    // decoder applies the previous RPU's curves.
    let has_mapping = rpu.rpu_data_mapping.as_ref().ok_or(RefitError::NoMapping)?;
    if has_mapping.nlq.is_some() {
        return Err(RefitError::UnsupportedHeader);
    }
    // The whole mapping is replaced, so its precision is ours to choose:
    // 2^-23, the precision the fit was quantised to. (NLQ, the only other
    // user of the denominator, is absent after To81 on a FEL RPU.)
    let denom = 23u64;
    // Every coefficient is split before anything is written, so a refusal
    // leaves the RPU as it was.
    let mut luma_parts = Vec::with_capacity(LUMA_PIECES);
    for piece in &curves.luma {
        let mut parts = [(0i64, 0u64); 3];
        for (slot, &v) in parts.iter_mut().zip(piece) {
            *slot = split(v, denom)?;
        }
        luma_parts.push(parts);
    }
    let mut chroma_parts = Vec::with_capacity(2);
    for component in &curves.chroma {
        let mut parts = [(0i64, 0u64); MMR_TERMS];
        for (slot, &v) in parts.iter_mut().zip(component) {
            *slot = split(v, denom)?;
        }
        chroma_parts.push(parts);
    }
    rpu.header.coefficient_log2_denom = denom;
    rpu.header.coefficient_log2_denom_length = denom as u32;
    let mapping = rpu.rpu_data_mapping.as_mut().ok_or(RefitError::NoMapping)?;

    let luma = &mut mapping.curves[0];
    luma.num_pivots_minus2 = (LUMA_PIVOT_DELTAS.len() - 2) as u64;
    luma.pivots = LUMA_PIVOT_DELTAS.to_vec();
    luma.mapping_idc = DoviMappingMethod::Polynomial;
    luma.mmr = None;
    let mut poly = DoviPolynomialCurve::default();
    for parts in &luma_parts {
        poly.poly_order_minus1.push(1);
        poly.linear_interp_flag.push(false);
        poly.poly_coef_int.push(Default::default());
        poly.poly_coef.push(Default::default());
        for &(int, frac) in parts {
            poly.poly_coef_int.last_mut().expect("pushed").push(int);
            poly.poly_coef.last_mut().expect("pushed").push(frac);
        }
    }
    luma.polynomial = Some(poly);

    for (component, parts) in chroma_parts.iter().enumerate() {
        let curve = &mut mapping.curves[component + 1];
        curve.num_pivots_minus2 = 0;
        curve.pivots = vec![0, 1023];
        curve.mapping_idc = DoviMappingMethod::MMR;
        curve.polynomial = None;
        let mut mmr = DoviMMRCurve::default();
        mmr.mmr_order_minus1.push(2);
        mmr.mmr_constant_int.push(parts[0].0);
        mmr.mmr_constant.push(parts[0].1);
        mmr.mmr_coef_int.push(Default::default());
        mmr.mmr_coef.push(Default::default());
        for order in 0..3 {
            let ints = mmr.mmr_coef_int.last_mut().expect("pushed");
            ints.push(Default::default());
            let fracs = mmr.mmr_coef.last_mut().expect("pushed");
            fracs.push(Default::default());
            for term in 0..7 {
                let (int, frac) = parts[1 + 7 * order + term];
                mmr.mmr_coef_int.last_mut().expect("pushed")[order].push(int);
                mmr.mmr_coef.last_mut().expect("pushed")[order].push(frac);
            }
        }
        curve.mmr = Some(mmr);
    }
    rpu.modified = true;
    Ok(())
}

#[cfg(test)]
mod tests;
