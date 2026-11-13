//! Serving F+ curves on the copy path (`docs/streaming/FEL-RPU-REFIT-BUILD.md`
//! §3.5): which scene's curves a converted sample's RPU carries.
//!
//! Two exact pieces, and nothing that counts frames:
//! - [`ServedCurveTable`]: one title's scenes, keyed by their first
//!   presentation time in the source stream's time base. Scene *i* covers
//!   `[first_ptsᵢ, first_ptsᵢ₊₁)`; the last one runs to its `end_pts`. A
//!   session pins one table when it starts and converts every fragment with
//!   it, so a refit published mid-playback reaches only later sessions.
//! - [`SourceClock`]: where a track's timeline sits in the source stream.
//!   A sample's presentation time (its fragment's `tfdt`, the durations
//!   before it, its composition offset; [`crate::fmp4::SampleTime`]) maps to
//!   a source pts by rational arithmetic, and a time that is not a whole
//!   source tick is refused rather than rounded.
//!
//! A sample before the first scene, or at or past the last `end_pts`, keeps
//! identity curves; so does a scene the fit left at identity.

use super::dv_refit::{
    servable, ComposerCurves, RefitCurves, ServableCurves, LUMA_PIECES, LUMA_PIVOT_DELTAS,
    MMR_TERMS,
};
use dolby_vision::rpu::dovi_rpu::DoviRpu;
use dolby_vision::rpu::rpu_data_mapping::DoviMappingMethod;

/// Why a curve table or a clock cannot serve.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServeError {
    #[error("the curve table has no scenes")]
    Empty,
    #[error("the curve table's time base {0:?} is not num/den")]
    TimeBase(String),
    #[error("the curve table's scenes do not share one time base")]
    MixedTimeBase,
    #[error("scene {0} does not start after the scene before it, or ends before it starts")]
    Order(usize),
    #[error("scene {0}'s stored curves are not servable: {1}")]
    Unservable(usize, String),
    #[error("the source clock's time base or timescale is zero")]
    Clock,
    #[error("presentation time {0} is not a whole tick of the source time base")]
    NonIntegral(i128),
    #[error("presentation time {0} overflows the source clock")]
    Overflow(i128),
}

/// `num/den`, both positive.
pub fn parse_time_base(text: &str) -> Option<(u32, u32)> {
    let (num, den) = text.split_once('/')?;
    match (num.parse::<u32>(), den.parse::<u32>()) {
        (Ok(n), Ok(d)) if n > 0 && d > 0 => Some((n, d)),
        _ => None,
    }
}

/// One title's served scenes, pinned for a session.
#[derive(Debug, Clone, PartialEq)]
pub struct ServedCurveTable {
    digest: String,
    time_base: (u32, u32),
    starts: Vec<i64>,
    end_pts: i64,
    curves: Vec<Option<ServableCurves>>,
}

impl ServedCurveTable {
    /// Build from the store's already checked, quantised serving curves.
    pub fn from_servable<'a>(
        digest: impl Into<String>,
        scenes: impl IntoIterator<Item = (i64, i64, &'a str, Option<&'a ServableCurves>)>,
    ) -> Result<Self, ServeError> {
        let mut time_base: Option<(&str, (u32, u32))> = None;
        let mut starts = Vec::new();
        let mut curves = Vec::new();
        let mut end_pts = i64::MIN;
        for (index, (first_pts, end, base, fitted)) in scenes.into_iter().enumerate() {
            match time_base {
                None => {
                    let parsed = parse_time_base(base).ok_or_else(|| ServeError::TimeBase(base.into()))?;
                    time_base = Some((base, parsed));
                }
                Some((known, _)) if known != base => return Err(ServeError::MixedTimeBase),
                Some(_) => {}
            }
            if end <= first_pts || starts.last().is_some_and(|&last| first_pts <= last) {
                return Err(ServeError::Order(index));
            }
            starts.push(first_pts);
            end_pts = end;
            curves.push(fitted.cloned());
        }
        let (_, time_base) = time_base.ok_or(ServeError::Empty)?;
        Ok(Self { digest: digest.into(), time_base, starts, end_pts, curves })
    }
    /// Builds the table from stored scenes, in presentation order:
    /// `(first_pts, end_pts, time_base, curves)`. Stored curves are checked
    /// with [`servable`] again here; one that fails refuses the whole table,
    /// because the digest names the curves as published and serving a
    /// different set under it would make the identity lie.
    pub fn new<'a>(
        digest: impl Into<String>,
        scenes: impl IntoIterator<Item = (i64, i64, &'a str, Option<&'a RefitCurves>)>,
    ) -> Result<Self, ServeError> {
        let mut time_base: Option<(&str, (u32, u32))> = None;
        let mut starts = Vec::new();
        let mut curves = Vec::new();
        let mut end_pts = i64::MIN;
        for (index, (first_pts, end, base, fitted)) in scenes.into_iter().enumerate() {
            match time_base {
                None => {
                    let parsed =
                        parse_time_base(base).ok_or_else(|| ServeError::TimeBase(base.into()))?;
                    time_base = Some((base, parsed));
                }
                Some((known, _)) if known != base => return Err(ServeError::MixedTimeBase),
                Some(_) => {}
            }
            if end <= first_pts || starts.last().is_some_and(|&last| first_pts <= last) {
                return Err(ServeError::Order(index));
            }
            starts.push(first_pts);
            end_pts = end;
            curves.push(
                fitted
                    .map(|fit| {
                        servable(fit).map_err(|why| ServeError::Unservable(index, why.to_string()))
                    })
                    .transpose()?,
            );
        }
        let (_, time_base) = time_base.ok_or(ServeError::Empty)?;
        Ok(Self {
            digest: digest.into(),
            time_base,
            starts,
            end_pts,
            curves,
        })
    }

    /// The published curve digest this table serves under.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// `(num, den)` of the source time base the scenes are keyed in.
    pub fn time_base(&self) -> (u32, u32) {
        self.time_base
    }

    /// Whether any scene serves fitted curves.
    pub fn fitted(&self) -> bool {
        self.curves.iter().any(Option::is_some)
    }

    /// The curves for a frame presenting at `pts` (source time base), or
    /// `None` for identity.
    pub fn curves_at(&self, pts: i64) -> Option<&ServableCurves> {
        if pts >= self.end_pts {
            return None;
        }
        let after = self.starts.partition_point(|&start| start <= pts);
        let scene = after.checked_sub(1)?;
        self.curves[scene].as_ref()
    }
}

/// Where a track's timeline sits in the source stream: track tick
/// `track_ticks` presents at source pts `source_pts`. Established by the
/// caller from an exact observation, never from a running count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceClock {
    pub track_timescale: u32,
    pub track_ticks: i128,
    pub source_pts: i64,
    pub source_time_base: (u32, u32),
}

impl SourceClock {
    /// Bind a production fragment's first decode time to a candidate index
    /// row. The source packet origin was verified when that fitted index was
    /// built. This calculation uses the row's decode displacement, never a
    /// floating-point seek estimate or a frame count.
    pub fn for_index_landing(
        index: &crate::segplan::FragmentIndex,
        row: usize,
        production_dts: u64,
    ) -> Result<Self, ServeError> {
        let origin = index.dv_refit_origin.ok_or(ServeError::Clock)?;
        let first = index.rows.first().ok_or(ServeError::Clock)?;
        let landed = index.rows.get(row).ok_or(ServeError::Clock)?;
        let clock = Self {
            track_timescale: index.timescale,
            track_ticks: i128::from(production_dts),
            source_pts: origin.first_dts,
            source_time_base: (origin.time_base_num, origin.time_base_den),
        };
        let offset = i128::from(landed.dts) - i128::from(first.dts);
        let denominator = i128::from(index.timescale) * i128::from(origin.time_base_num);
        let numerator = offset.checked_mul(i128::from(origin.time_base_den))
            .ok_or(ServeError::Overflow(offset))?;
        if denominator == 0 || numerator % denominator != 0 {
            return Err(ServeError::NonIntegral(offset));
        }
        let source_dts = i128::from(origin.first_dts).checked_add(numerator / denominator)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or(ServeError::Overflow(offset))?;
        Ok(Self { source_pts: source_dts, ..clock })
    }

    /// The source pts of a sample presenting at `presentation` track ticks:
    /// `source_pts + (presentation − track_ticks) · den / (timescale · num)`,
    /// refused unless it is a whole source tick.
    pub fn source_pts(&self, presentation: i128) -> Result<i64, ServeError> {
        let (num, den) = self.source_time_base;
        if num == 0 || den == 0 || self.track_timescale == 0 {
            return Err(ServeError::Clock);
        }
        let offset = presentation
            .checked_sub(self.track_ticks)
            .and_then(|ticks| ticks.checked_mul(i128::from(den)))
            .ok_or(ServeError::Overflow(presentation))?;
        let divisor = i128::from(self.track_timescale) * i128::from(num);
        if offset % divisor != 0 {
            return Err(ServeError::NonIntegral(presentation));
        }
        i128::from(self.source_pts)
            .checked_add(offset / divisor)
            .and_then(|pts| i64::try_from(pts).ok())
            .ok_or(ServeError::Overflow(presentation))
    }
}

/// The curves an RPU's mapping carries, when it has exactly the shape
/// [`super::dv_refit::apply_to_rpu`] writes (eight 2nd-order luma pieces on
/// the fixed pivots, one 3rd-order MMR piece per chroma component); `None`
/// otherwise. What a served fragment's RPUs are checked against.
pub fn written_curves(rpu: &DoviRpu) -> Option<ComposerCurves> {
    let mapping = rpu.rpu_data_mapping.as_ref()?;
    if rpu.header.coefficient_data_type != 0 || mapping.curves.len() < 3 {
        return None;
    }
    let denom = rpu.header.coefficient_log2_denom;
    if denom > 32 {
        return None;
    }
    let join = |int: i64, frac: u64| int as f64 + frac as f64 / (1u64 << denom) as f64;
    let luma = &mapping.curves[0];
    if luma.pivots != LUMA_PIVOT_DELTAS || luma.mapping_idc != DoviMappingMethod::Polynomial {
        return None;
    }
    let poly = luma.polynomial.as_ref()?;
    let mut out = ComposerCurves {
        luma: [[0.0; 3]; LUMA_PIECES],
        chroma: [[0.0; MMR_TERMS]; 2],
    };
    for (p, piece) in out.luma.iter_mut().enumerate() {
        if *poly.poly_order_minus1.get(p)? != 1 || poly.poly_coef.get(p)?.len() != 3 {
            return None;
        }
        for (k, slot) in piece.iter_mut().enumerate() {
            *slot = join(poly.poly_coef_int[p][k], poly.poly_coef[p][k]);
        }
    }
    for (ch, component) in out.chroma.iter_mut().enumerate() {
        let curve = &mapping.curves[ch + 1];
        let mmr = curve.mmr.as_ref()?;
        if curve.mapping_idc != DoviMappingMethod::MMR || *mmr.mmr_order_minus1.first()? != 2 {
            return None;
        }
        component[0] = join(*mmr.mmr_constant_int.first()?, *mmr.mmr_constant.first()?);
        for order in 0..3 {
            for term in 0..7 {
                component[1 + 7 * order + term] = join(
                    *mmr.mmr_coef_int.first()?.get(order)?.get(term)?,
                    *mmr.mmr_coef.first()?.get(order)?.get(term)?,
                );
            }
        }
    }
    Some(out)
}

/// Whether two curve sets agree to within half a step of 2^-23 (the
/// precision [`super::dv_refit::apply_to_rpu`] writes), plus float noise.
pub fn curves_match(written: &ComposerCurves, served: &ComposerCurves) -> bool {
    let tolerance = 0.5 / f64::from(1u32 << 23) + 1e-12;
    let close = |a: &[f64], b: &[f64]| a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tolerance);
    written
        .luma
        .iter()
        .zip(&served.luma)
        .all(|(a, b)| close(a, b))
        && written
            .chroma
            .iter()
            .zip(&served.chroma)
            .all(|(a, b)| close(a, b))
}

#[cfg(test)]
#[path = "dv_refit_serve_tests.rs"]
mod tests;
