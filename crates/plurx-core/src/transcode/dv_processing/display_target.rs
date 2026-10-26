//! Explicit title-qualified target policy. No client/caps or settings routing.
use super::DvContractError;
use crate::scan::dv_title::{
    DvTitleStats, MasteringLuminance, MasteringPrimaries, TitleConstant, TitleEligibility,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub const DISPLAY_TARGET_METHOD: &str = "dolby-closed-form-cm4-cm29-v5";
pub const DISPLAY_TARGET_BUCKETS: [u32; 6] = [400, 600, 1000, 1600, 2000, 4000];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DvDisplayTarget {
    nits: u32,
    sdr: bool,
    sdr_floor_resolved: bool,
    mastering: MasteringLuminance,
    max_l1_pq: u16,
    mastering_luminance: TitleConstant<MasteringLuminance>,
    mastering_primaries: TitleConstant<MasteringPrimaries>,
    resolved_primaries: Option<[u16; 8]>,
    title_stats_sha256: [u8; 32],
}

/// Closed-form Dolby display management selected per frame (helper `dv_dm.h`): what its
/// `display_target_frame` row must report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DvDisplayManagement {
    pub cm4: bool,
    pub curve: &'static str,
    pub trim_targets: usize,
    pub detail_weight: f64,
}
impl DvDisplayManagement {
    /// Whether the frame's mapping applied a creative trim (an L8 or L2 trim was selected).
    pub fn creative_trims(&self) -> bool {
        !self.curve.ends_with("-l1-only")
    }
}
/// Whether served output applies each family's creative trims (helper `DV_TARGET_TRIMS_CM4` /
/// `DV_TARGET_TRIMS_CM29`). Both families' trims are accepted by pixel comparison against the
/// reference renderer on real titles. Changing either needs a new `DISPLAY_TARGET_METHOD`.
pub const DISPLAY_TARGET_CREATIVE_TRIMS_CM4: bool = true;
pub const DISPLAY_TARGET_CREATIVE_TRIMS_CM29: bool = true;
/// The helper uses at most this many resolvable L8 trims per frame (`dv_dm.h`); the parser itself
/// accepts at most five L8 blocks per RPU.
const DM_MAX_L8_TRIMS: usize = 32;
/// L8 target display indices the helper resolves without an L10 (`dv_dm_preset`).
const DM_PRESET_TARGETS: [u8; 7] = [1, 27, 28, 37, 38, 48, 49];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DvDisplayTargetChoice {
    Target(DvDisplayTarget),
    Master { reason: DvDisplayTargetDecline },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DvDisplayTargetDecline {
    TitleUnknown,
    TitleIneligible(TitleEligibility),
    TitleFits { nits: u32 },
}
impl DvDisplayTargetDecline {
    pub fn reason(self) -> String {
        match self {
            Self::TitleUnknown => {
                "complete-title Dolby Vision L1 scan unavailable; master grade sent".into()
            }
            Self::TitleIneligible(eligibility) => eligibility
                .master_reason()
                .unwrap_or("invalid complete-title Dolby Vision scan; master grade sent")
                .into(),
            Self::TitleFits { nits } => {
                format!("this title already fits a {nits}-nit display; master grade sent")
            }
        }
    }
}
impl DvDisplayTarget {
    /// Caller must obtain stats from the current source's fenced probe document.
    /// This pure decision never changes a live session or selects a client route.
    pub fn for_title(
        nits: u32,
        stats: Option<DvTitleStats>,
    ) -> Result<DvDisplayTargetChoice, DvContractError> {
        Self::choose(nits, false, stats)
    }
    /// SDR conversion is required even when the source peak fits 100 nits:
    /// returning the HDR master would preserve the wrong transfer and gamut.
    pub fn for_sdr_title(
        stats: Option<DvTitleStats>,
    ) -> Result<DvDisplayTargetChoice, DvContractError> {
        Self::choose(100, true, stats)
    }
    fn choose(
        nits: u32,
        sdr: bool,
        stats: Option<DvTitleStats>,
    ) -> Result<DvDisplayTargetChoice, DvContractError> {
        if !(sdr && nits == 100 || !sdr && DISPLAY_TARGET_BUCKETS.contains(&nits)) {
            return Err(DvContractError::Invalid("display target bucket"));
        }
        let Some(stats) = stats else {
            return Ok(DvDisplayTargetChoice::Master {
                reason: DvDisplayTargetDecline::TitleUnknown,
            });
        };
        let eligibility = stats.eligibility();
        if eligibility != TitleEligibility::Eligible {
            return Ok(DvDisplayTargetChoice::Master {
                reason: DvDisplayTargetDecline::TitleIneligible(eligibility),
            });
        }
        let title_nits = pq_nits(stats.max_l1_pq.expect("eligible title has a maximum"));
        if !sdr && title_nits <= f64::from(nits) {
            return Ok(DvDisplayTargetChoice::Master {
                reason: DvDisplayTargetDecline::TitleFits { nits },
            });
        }
        Ok(DvDisplayTargetChoice::Target(Self {
            nits,
            sdr,
            sdr_floor_resolved: false,
            mastering: stats.mastering_or_default(),
            max_l1_pq: stats.max_l1_pq.expect("eligible title maximum"),
            mastering_luminance: stats.mastering_luminance,
            mastering_primaries: stats.mastering_primaries,
            resolved_primaries: None,
            title_stats_sha256: Sha256::digest(
                serde_json::to_vec(&stats).expect("integer title stats serialize"),
            )
            .into(),
        }))
    }
    pub fn nits(self) -> u32 {
        self.nits
    }
    pub fn is_sdr(self) -> bool {
        self.sdr
    }
    pub fn validate_rpu(self, raw: &[u8]) -> Result<(u16, u16), DvContractError> {
        use dolby_vision::rpu::{dovi_rpu::DoviRpu, extension_metadata::blocks::ExtMetadataBlock};
        let invalid = || {
            DvContractError::Invalid(
                "display-target L1 invalid; whole-session master fallback required",
            )
        };
        let rpu = DoviRpu::parse_unspec62_nalu(raw).map_err(|_| invalid())?;
        let dm = rpu.vdr_dm_data.as_ref().ok_or_else(invalid)?;
        let Some(ExtMetadataBlock::Level1(l1)) = dm.get_block(1) else {
            return Err(invalid());
        };
        if l1.min_pq > l1.avg_pq || l1.avg_pq > l1.max_pq || l1.max_pq > self.max_l1_pq {
            return Err(invalid());
        }
        Ok((l1.max_pq, l1.avg_pq))
    }
    /// The exact display-management selection the helper (`dv_dm.h`) must report for this RPU at
    /// this bucket, with creative trims as served (see [`dv_display_management`]).
    pub fn display_management(self, raw: &[u8]) -> Result<DvDisplayManagement, DvContractError> {
        self.display_management_with(
            raw,
            DISPLAY_TARGET_CREATIVE_TRIMS_CM4,
            DISPLAY_TARGET_CREATIVE_TRIMS_CM29,
        )
    }
    /// As [`Self::display_management`], with each family's creative trims applied or not.
    pub fn display_management_with(
        self,
        raw: &[u8],
        cm4_trims: bool,
        cm29_trims: bool,
    ) -> Result<DvDisplayManagement, DvContractError> {
        dv_display_management(
            raw,
            self.nits,
            self.mastering.max_nits,
            cm4_trims,
            cm29_trims,
        )
    }
    pub fn mastering(self) -> MasteringLuminance {
        self.mastering
    }
    pub fn needs_source_mastering(self) -> bool {
        !self.sdr
            && (matches!(self.mastering_primaries, TitleConstant::Unknown)
                || matches!(self.mastering_luminance, TitleConstant::Unknown))
    }

    pub fn needs_source_floor(self) -> bool {
        matches!(self.mastering_luminance, TitleConstant::Unknown)
    }
    /// Resolve only the SDR mapping floor. BT.709 export primaries are defined
    /// by the output policy; missing source HDR mastering primaries are irrelevant.
    pub fn resolve_sdr_floor(
        mut self,
        source_minimum: Option<u32>,
    ) -> Result<Self, DvContractError> {
        if !self.sdr {
            return Err(DvContractError::Invalid(
                "SDR floor requested for HDR target",
            ));
        }
        let original = self;
        self.mastering.min_1e4_nits = match self.mastering_luminance {
            TitleConstant::Consistent(value) => value.min_1e4_nits,
            TitleConstant::Unknown => source_minimum.unwrap_or(50),
            TitleConstant::Inconsistent => {
                return Err(DvContractError::Invalid(
                    "targeted SDR title L6 mastering minimum inconsistent",
                ))
            }
        };
        if self.mastering.min_1e4_nits > 10_000 {
            return Err(DvContractError::Invalid(
                "targeted SDR minimum exceeds renderer range",
            ));
        }
        self.sdr_floor_resolved = true;
        if original.sdr_floor_resolved && original != self {
            return Err(DvContractError::Invalid(
                "targeted SDR floor changed after policy freeze",
            ));
        }
        Ok(self)
    }
    pub fn resolved_sdr_floor(self) -> Result<u32, DvContractError> {
        if !self.sdr || !self.sdr_floor_resolved {
            return Err(DvContractError::Invalid(
                "targeted SDR floor has not been resolved",
            ));
        }
        Ok(self.mastering.min_1e4_nits)
    }
    /// Resolve once, before export/helper policy or rendition identity is frozen.
    /// The renderer and output declarations consume this same effective floor.
    /// Missing L6 uses trusted source MDCV, then the established renderer floor;
    /// contradictory title facts never use fallback values.
    pub fn resolve_source(
        mut self,
        source: Option<DvSourceMastering>,
    ) -> Result<Self, DvContractError> {
        if self.sdr {
            return self.resolve_sdr_floor(source.map(|source| source.min_1e4_nits));
        }
        let original = self;
        if source.is_some_and(|value| value.min_1e4_nits > 10_000) {
            return Err(DvContractError::Invalid(
                "targeted HDR10 source minimum exceeds renderer range",
            ));
        }
        let primaries =
            match self.mastering_primaries {
                TitleConstant::Consistent(value) => l9_primaries(value)?,
                TitleConstant::Unknown => source
                    .ok_or(DvContractError::Invalid(
                        "targeted HDR10 mastering primaries unknown (no title L9 or source MDCV)",
                    ))?
                    .primaries_rgbw,
                TitleConstant::Inconsistent => {
                    return Err(DvContractError::Invalid(
                        "targeted HDR10 title L9 mastering primaries inconsistent",
                    ))
                }
            };
        self.mastering.min_1e4_nits = match self.mastering_luminance {
            TitleConstant::Consistent(value) => value.min_1e4_nits,
            TitleConstant::Unknown => source.map_or(50, |value| value.min_1e4_nits),
            TitleConstant::Inconsistent => {
                return Err(DvContractError::Invalid(
                    "targeted HDR10 title L6 mastering luminance inconsistent",
                ))
            }
        };
        self.resolved_primaries = Some(primaries);
        self.output_metadata()?;
        if original.resolved_primaries.is_some() && original != self {
            return Err(DvContractError::Invalid(
                "targeted HDR10 source mastering changed after policy freeze",
            ));
        }
        Ok(self)
    }

    /// No independent fallback here: metadata must describe the exact resolved
    /// target already supplied to helper execution and semantic/cache identity.
    pub fn output_metadata(self) -> Result<crate::fmp4::Hdr10OutputMetadata, DvContractError> {
        if self.sdr {
            return Err(DvContractError::Invalid(
                "SDR export cannot carry HDR10 mastering metadata",
            ));
        }
        let primaries = self.resolved_primaries.ok_or(DvContractError::Invalid(
            "targeted HDR10 source mastering has not been resolved",
        ))?;
        // Eligible target selection proves L1 peak > bucket. Retain the min
        // explicitly so the static CLL bound follows the same title policy.
        let max_cll = pq_nits(self.max_l1_pq).min(f64::from(self.nits)).ceil() as u16;
        crate::fmp4::Hdr10OutputMetadata::new(
            primaries,
            self.nits,
            self.mastering.min_1e4_nits,
            max_cll,
        )
        .map_err(|_| DvContractError::Invalid("targeted HDR10 metadata cannot be represented"))
    }
    pub fn output_policy(self) -> String {
        if self.sdr {
            "bt709-gamma24-target-100".into()
        } else {
            format!("bt2020-pq-target-{}", self.nits)
        }
    }
    pub fn endpoint_args(self) -> [String; 2] {
        [
            self.mastering.min_1e4_nits.to_string(),
            self.mastering.max_nits.to_string(),
        ]
    }
}

/// The display-management selection the helper (`dv_dm.h`) makes for this RPU at `nits`, for a
/// title mastered at `title_peak` nits: CM v4 when the RPU carries CM v4.0 metadata (L254) or L8
/// trims, otherwise CM v2.9. With trims, CM v4 uses every L8 whose target display is a preset or
/// is described by an L10. CM v2.9 uses the L2 authored for the bucket peak; otherwise each trim
/// field is interpolated linearly in target PQ between the bracketing L2 targets, with neutral
/// trims at PQ 0 and at the mastering peak (measured against the reference renderer). At or above
/// the mastering peak both families map untrimmed.
fn dv_display_management(
    raw: &[u8],
    nits: u32,
    title_peak: u32,
    cm4_trims: bool,
    cm29_trims: bool,
) -> Result<DvDisplayManagement, DvContractError> {
    use dolby_vision::rpu::{dovi_rpu::DoviRpu, extension_metadata::blocks::ExtMetadataBlock};
    let invalid = || DvContractError::Invalid("display-target RPU unparseable");
    let rpu = DoviRpu::parse_unspec62_nalu(raw).map_err(|_| invalid())?;
    let dm = rpu.vdr_dm_data.as_ref().ok_or_else(invalid)?;
    let l8: Vec<u8> = dm
        .level_blocks_iter(8)
        .filter_map(|block| match block {
            ExtMetadataBlock::Level8(x) => Some(x.target_display_index),
            _ => None,
        })
        .collect();
    let tmax = pq_code_f64(f64::from(nits));
    let smast = pq_code_f64(f64::from(title_peak));
    let untrimmed = |cm4: bool| DvDisplayManagement {
        cm4,
        curve: if cm4 { "cm4-l1-only" } else { "cm29-l1-only" },
        trim_targets: 0,
        detail_weight: 0.0,
    };
    if dm.get_block(254).is_some() || !l8.is_empty() {
        if !cm4_trims {
            return Ok(untrimmed(true));
        }
        let described = |index: u8| {
            dm.level_blocks_iter(10).any(|block| {
                matches!(block, ExtMetadataBlock::Level10(x) if x.target_display_index == index)
            })
        };
        let used = l8
            .iter()
            .filter(|index| DM_PRESET_TARGETS.contains(index) || described(**index))
            .count()
            .min(DM_MAX_L8_TRIMS);
        // At or above the mastering peak the trims have faded out.
        if used == 0 || tmax >= smast {
            return Ok(untrimmed(true));
        }
        return Ok(DvDisplayManagement {
            cm4: true,
            curve: "cm4-l8-interpolated",
            trim_targets: used,
            detail_weight: 0.0,
        });
    }
    if !cm29_trims {
        return Ok(untrimmed(false));
    }
    // Only the MS weight of the interpolated trim is reported (`detail_weight`); the helper
    // interpolates every decoded field the same way.
    let want = (tmax * 4095.0 + 0.5).floor() as u32;
    let (mut exact, mut lo, mut lo_ms, mut hi, mut hi_ms) = (None, -1.0, 0.0, 2.0, 0.0);
    let mut l2_trims = 0;
    for block in dm.level_blocks_iter(2) {
        let ExtMetadataBlock::Level2(x) = block else {
            continue;
        };
        l2_trims += 1;
        let ms = if x.ms_weight < 0 {
            0.0
        } else {
            (f64::from(x.ms_weight) - 2048.0) / 2048.0
        };
        let code = u32::from(x.target_max_pq);
        let q = f64::from(x.target_max_pq) / 4095.0;
        if exact.is_none() && code + 1 >= want && code <= want + 1 {
            exact = Some(ms);
        }
        if q < tmax && q > lo {
            (lo, lo_ms) = (q, ms);
        }
        if q > tmax && q < hi {
            (hi, hi_ms) = (q, ms);
        }
    }
    // An L2 target between this display and the mastering peak; without one (and without a lower
    // target) every trim has faded out.
    let (upper, lower) = (hi <= 1.0 && hi < smast, lo >= 0.0);
    let (curve, weight) = if tmax >= smast {
        ("cm29-l1-only", 0.0)
    } else if let Some(ms) = exact {
        ("cm29-l2-exact", ms)
    } else if lower || upper {
        if !upper {
            (hi, hi_ms) = (smast, 0.0);
        }
        if !lower {
            (lo, lo_ms) = (0.0, 0.0);
        }
        let w: f64 = ((hi - tmax) / (hi - lo)).clamp(0.0, 1.0);
        ("cm29-l2-interpolated", w * lo_ms + (1.0 - w) * hi_ms)
    } else {
        ("cm29-l1-only", 0.0)
    };
    Ok(DvDisplayManagement {
        cm4: false,
        curve,
        trim_targets: if curve == "cm29-l1-only" { 0 } else { l2_trims },
        detail_weight: weight,
    })
}

/// A source MDCV fallback, captured from the selected held video stream. Its
/// units already match ST 2086; it never substitutes container colour tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DvSourceMastering {
    pub primaries_rgbw: [u16; 8],
    pub min_1e4_nits: u32,
}

/// ST 2084 inverse EOTF of absolute nits (the helper's `dm_pq_oetf`).
fn pq_code_f64(nits: f64) -> f64 {
    let p = (nits / 10000.0).powf(2610.0 / 16384.0);
    ((3424.0 / 4096.0 + 2413.0 / 128.0 * p) / (1.0 + 2392.0 / 128.0 * p)).powf(2523.0 / 32.0)
}

fn pq_nits(code: u16) -> f64 {
    let p = (f64::from(code) / 4095.0).powf(32.0 / 2523.0);
    10000.0
        * ((p - 3424.0 / 4096.0).max(0.0) / (2413.0 / 128.0 - 2392.0 / 128.0 * p))
            .powf(16384.0 / 2610.0)
}

fn l9_primaries(value: MasteringPrimaries) -> Result<[u16; 8], DvContractError> {
    use dolby_vision::rpu::extension_metadata::PREDEFINED_COLORSPACE_PRIMARIES;
    let invalid = || DvContractError::Invalid("targeted HDR10 L9 primaries cannot be represented");
    let coordinates = if value.index == 255 {
        let custom = value.custom_xy.ok_or_else(invalid)?;
        custom.map(|v| f64::from(v) / 32767.0)
    } else {
        if value.custom_xy.is_some() {
            return Err(invalid());
        }
        *PREDEFINED_COLORSPACE_PRIMARIES
            .get(usize::from(value.index))
            .ok_or_else(invalid)?
    };
    if coordinates.chunks_exact(2).any(|xy| {
        !(0.0..=1.0).contains(&xy[0])
            || !(0.0..=1.0).contains(&xy[1])
            || xy[1] == 0.0
            || xy[0] + xy[1] > 1.00001
    }) {
        return Err(invalid());
    }
    Ok(coordinates.map(|v| (v * 50_000.0).round() as u16))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::dv_title::SCHEMA;

    /// The selection the helper must report, from synthetic RPUs built on a retained fixture.
    #[test]
    fn display_management_selection_follows_the_raw_rpu_exactly() {
        use dolby_vision::rpu::dovi_rpu::DoviRpu;
        use dolby_vision::rpu::extension_metadata::blocks::{
            ExtMetadataBlock, ExtMetadataBlockLevel10, ExtMetadataBlockLevel2,
            ExtMetadataBlockLevel8,
        };
        use dolby_vision::rpu::extension_metadata::{CmV40DmData, DmData};
        let fixture = include_bytes!("../../../tests/fixtures/dv-runtime/p8-piecewise.nal");
        let build = |cm4: bool, blocks: Vec<ExtMetadataBlock>| {
            let mut rpu = DoviRpu::parse_unspec62_nalu(fixture).expect("fixture");
            let dm = rpu.vdr_dm_data.as_mut().expect("display metadata");
            dm.remove_metadata_level(2);
            dm.remove_metadata_level(8);
            dm.remove_metadata_level(10);
            dm.cmv40_metadata = cm4.then(|| DmData::V40(CmV40DmData::new_with_l254_402()));
            for block in blocks {
                dm.add_metadata_block(block).expect("block");
            }
            rpu.modified = true;
            rpu.write_hevc_unspec62_nalu().expect("RPU")
        };
        let l2 = |target_max_pq, ms_weight| {
            ExtMetadataBlock::Level2(ExtMetadataBlockLevel2 {
                target_max_pq,
                ms_weight,
                ..Default::default()
            })
        };
        let l8 = |target_display_index| {
            ExtMetadataBlock::Level8(ExtMetadataBlockLevel8 {
                length: 13,
                target_display_index,
                ..Default::default()
            })
        };
        let target = |nits| match DvDisplayTarget::for_title(nits, Some(stats())).expect("bucket") {
            DvDisplayTargetChoice::Target(target) => target,
            DvDisplayTargetChoice::Master { .. } => panic!("target"),
        };
        let check = |raw: &[u8], nits: u32, expected: (bool, &str, usize, f64)| {
            let dm = target(nits)
                .display_management_with(raw, true, true)
                .expect("selection");
            assert_eq!(
                (dm.cm4, dm.curve, dm.trim_targets, dm.detail_weight),
                expected,
                "{nits}"
            );
        };
        // CM v2.9: exact 12-bit match, else linear in target PQ between the bracketing L2
        // targets, with neutral ones at PQ 0 and at the mastering peak (4000 here); raw
        // ms_weight -1 is 0. The interpolated weights are pinned in test_dv_dm.c too.
        let cm29 = build(false, vec![l2(2081, -1), l2(3079, 1800)]);
        let weight = (1800.0 - 2048.0) / 2048.0;
        assert_eq!(target(1600).mastering().max_nits, 4000);
        check(&cm29, 1000, (false, "cm29-l2-exact", 2, weight));
        let near = |nits: u32, expected: f64| {
            let dm = target(nits)
                .display_management_with(&cm29, true, true)
                .expect("selection");
            assert_eq!(
                (dm.cm4, dm.curve, dm.trim_targets),
                (false, "cm29-l2-interpolated", 2)
            );
            assert!((dm.detail_weight - expected).abs() < 1e-12, "{nits}");
        };
        // (1 - w) * ms(3079) with w = (PQ(3079) - PQ(600)) / (PQ(3079) - PQ(2081)); above 1000,
        // w * ms(3079) with w = (PQ(4000) - PQ(nits)) / (PQ(4000) - PQ(3079))
        near(600, -0.09346839266980962);
        near(1600, -0.07994135942825827);
        near(2000, -0.060392435898949166);
        // below the lowest target: PQ(600) / PQ(3079) of that trim
        let held = target(600)
            .display_management_with(&build(false, vec![l2(3079, 1800)]), true, true)
            .expect("selection");
        assert_eq!(
            (held.cm4, held.curve, held.trim_targets),
            (false, "cm29-l2-interpolated", 1)
        );
        assert!((held.detail_weight - -0.1121395094623157).abs() < 1e-12);
        // A title mastered at 1000 nits: the trims fade to neutral at its peak, not at 4000. The
        // 3079 trim is above PQ(1000) and does not count as a target below the peak; a CM v4 trim
        // at the mastering peak has faded out. Pinned in test_dv_dm.c too.
        let mastered = |nits| {
            let mut stats = stats();
            stats.mastering_luminance = TitleConstant::Consistent(MasteringLuminance {
                min_1e4_nits: 50,
                max_nits: 1000,
            });
            match DvDisplayTarget::for_title(nits, Some(stats)).expect("bucket") {
                DvDisplayTargetChoice::Target(target) => target,
                DvDisplayTargetChoice::Master { .. } => panic!("target"),
            }
        };
        let at_1000 = |raw: &[u8], nits| {
            let dm = mastered(nits)
                .display_management_with(raw, true, true)
                .expect("selection");
            (dm.cm4, dm.curve, dm.trim_targets, dm.detail_weight)
        };
        let (cm4, curve, trims, w) = at_1000(&build(false, vec![l2(2081, 1800)]), 600);
        assert_eq!((cm4, curve, trims), (false, "cm29-l2-interpolated", 1));
        assert!((w - -0.02760024702986694).abs() < 1e-12, "{w}");
        assert_eq!(
            at_1000(&build(false, vec![l2(3079, 1800)]), 600),
            (false, "cm29-l1-only", 0, 0.0)
        );
        // an L2 authored for the mastering peak itself is not applied at it
        assert_eq!(
            at_1000(&build(false, vec![l2(3079, 1800)]), 1000),
            (false, "cm29-l1-only", 0, 0.0)
        );
        assert_eq!(
            at_1000(&build(true, vec![l8(48)]), 1000),
            (true, "cm4-l1-only", 0, 0.0)
        );
        assert_eq!(
            at_1000(&build(true, vec![l8(48)]), 600),
            (true, "cm4-l8-interpolated", 1, 0.0)
        );
        check(&build(false, vec![]), 1000, (false, "cm29-l1-only", 0, 0.0));
        // CM v4: L254 alone selects CM v4; an L8 needs a preset target or an L10 describing it.
        check(&build(true, vec![]), 1000, (true, "cm4-l1-only", 0, 0.0));
        check(
            &build(true, vec![l8(48), l8(99)]),
            1000,
            (true, "cm4-l8-interpolated", 1, 0.0),
        );
        let described = ExtMetadataBlock::Level10(ExtMetadataBlockLevel10 {
            length: 5,
            target_display_index: 99,
            target_max_pq: 2851,
            target_primary_index: 0,
            ..Default::default()
        });
        check(
            &build(true, vec![l8(48), l8(99), described.clone()]),
            1000,
            (true, "cm4-l8-interpolated", 2, 0.0),
        );
        check(
            &build(true, vec![l8(99)]),
            1000,
            (true, "cm4-l1-only", 0, 0.0),
        );
        // Committed fixtures with trims, read by the C control (controls/test_dv_dm.c) through the
        // real parser, so the C and Rust selections are compared on identical bytes. Regenerate
        // with PLURX_WRITE_DM_FIXTURES=1 only when the fixture definition changes.
        let cm4_trims = build(true, vec![l8(48), l8(99), described]);
        for (name, raw, expected) in [
            (
                "cm4-l8-trims.nal",
                &cm4_trims,
                (true, "cm4-l8-interpolated", 2, 0.0),
            ),
            (
                "cm29-l2-trims.nal",
                &cm29,
                (false, "cm29-l2-exact", 2, weight),
            ),
        ] {
            check(raw, 1000, expected);
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tools/dv_processing/controls/tags")
                .join(name);
            if std::env::var_os("PLURX_WRITE_DM_FIXTURES").is_some() {
                std::fs::write(&path, raw).expect("write fixture");
            }
            assert_eq!(
                &std::fs::read(&path).expect("committed fixture"),
                raw,
                "{name}"
            );
        }
        // Served output applies both families' trims; a frame without a usable trim reports none.
        let served = |raw: &[u8]| {
            let dm = target(1000)
                .display_management(raw)
                .expect("served selection");
            (
                dm.cm4,
                dm.curve,
                dm.trim_targets,
                dm.detail_weight,
                dm.creative_trims(),
            )
        };
        assert_eq!(served(&cm29), (false, "cm29-l2-exact", 2, weight, true));
        assert_eq!(
            served(&build(false, vec![])),
            (false, "cm29-l1-only", 0, 0.0, false)
        );
        assert_eq!(
            served(&build(true, vec![l8(48)])),
            (true, "cm4-l8-interpolated", 1, 0.0, true)
        );
        assert_eq!(
            served(&build(true, vec![])),
            (true, "cm4-l1-only", 0, 0.0, false)
        );
    }
    fn stats() -> DvTitleStats {
        DvTitleStats {
            schema: SCHEMA,
            complete: true,
            decoded_frames: 2,
            valid_l1_frames: 2,
            missing_l1_frames: 0,
            invalid_l1_frames: 0,
            max_l1_pq: Some(4095),
            mastering_luminance: TitleConstant::Unknown,
            mastering_primaries: TitleConstant::Unknown,
        }
    }
    #[test]
    fn sdr_resolves_one_floor_without_hdr_primaries_and_freezes_its_identity() {
        let DvDisplayTargetChoice::Target(raw) =
            DvDisplayTarget::for_sdr_title(Some(stats())).expect("SDR target")
        else {
            panic!("target");
        };
        assert!(raw.resolved_sdr_floor().is_err());
        let export = |target| super::super::DvExportPolicy::DisplayTargetYuv420p10V1 {
            raster: (64, 64),
            target,
        };
        assert!(super::super::completed_target_policy(export(raw)).is_err());
        let source = raw
            .resolve_sdr_floor(Some(17))
            .expect("trusted source minimum");
        let fallback = raw.resolve_sdr_floor(None).expect("fixed unknown fallback");
        let policy =
            super::super::completed_target_policy(export(source)).expect("resolved SDR policy");
        assert_eq!(policy.black_1e4_nits, Some(17));
        assert_eq!(policy.peak_millinit, 100_000);
        assert_eq!(source.endpoint_args(), ["17", "4000"]);
        assert_eq!(fallback.endpoint_args(), ["50", "4000"]);
        assert_ne!(
            serde_json::to_value(source).expect("identity"),
            serde_json::to_value(fallback).expect("identity")
        );
        assert_eq!(
            source.resolve_sdr_floor(Some(17)).expect("unchanged"),
            source
        );
        assert!(source.resolve_sdr_floor(Some(50)).is_err());
        assert!(raw.resolve_sdr_floor(Some(10001)).is_err());
        assert!(source.output_metadata().is_err());
        let DvDisplayTargetChoice::Target(known) =
            DvDisplayTarget::for_sdr_title(Some(DvTitleStats {
                mastering_luminance: TitleConstant::Consistent(MasteringLuminance {
                    min_1e4_nits: 13,
                    max_nits: 4000,
                }),
                ..stats()
            }))
            .expect("known L6")
        else {
            panic!("target");
        };
        assert_eq!(
            known
                .resolve_sdr_floor(Some(17))
                .expect("L6 wins")
                .endpoint_args(),
            ["13", "4000"]
        );
    }
    #[test]
    fn sdr_target_never_elides_to_hdr_and_cannot_author_hdr_metadata() {
        let low = DvTitleStats {
            max_l1_pq: Some(0),
            ..stats()
        };
        let DvDisplayTargetChoice::Target(sdr) =
            DvDisplayTarget::for_sdr_title(Some(low)).expect("SDR policy")
        else {
            panic!("SDR conversion must not elide");
        };
        assert!(sdr.is_sdr());
        assert_eq!(sdr.nits(), 100);
        assert_eq!(sdr.output_policy(), "bt709-gamma24-target-100");
        assert!(!sdr.needs_source_mastering());
        assert!(sdr.output_metadata().is_err());
        assert!(matches!(
            DvDisplayTarget::for_sdr_title(None).expect("unknown"),
            DvDisplayTargetChoice::Master {
                reason: DvDisplayTargetDecline::TitleUnknown
            }
        ));
        assert!(matches!(
            DvDisplayTarget::for_sdr_title(Some(DvTitleStats {
                complete: false,
                ..low
            }))
            .expect("incomplete"),
            DvDisplayTargetChoice::Master { .. }
        ));
        assert!(
            DvDisplayTarget::for_title(100, Some(low)).is_err(),
            "100 must never mean HDR PQ"
        );
        let DvDisplayTargetChoice::Target(hdr) =
            DvDisplayTarget::for_title(400, Some(stats())).expect("HDR policy")
        else {
            panic!("HDR target");
        };
        let export = |target| {
            super::super::DvExportPolicy::DisplayTargetYuv420p10V1 {
                raster: (1280, 720),
                target,
            }
            .target_parameters()
        };
        assert_ne!(export(sdr), export(hdr));
    }
    #[test]
    fn target_metadata_uses_exact_l9_units_and_source_fallback_without_masking_conflicts() {
        let p3 = [34000, 16000, 13250, 34500, 7500, 3000, 15635, 16450];
        assert_eq!(
            l9_primaries(MasteringPrimaries {
                index: 0,
                custom_xy: None
            })
            .expect("P3D65"),
            p3
        );
        assert_eq!(
            l9_primaries(MasteringPrimaries {
                index: 2,
                custom_xy: None
            })
            .expect("BT2020"),
            [35400, 14600, 8500, 39850, 6550, 2300, 15635, 16450]
        );
        assert_eq!(
            l9_primaries(MasteringPrimaries {
                index: 255,
                custom_xy: Some([22282, 10485, 8683, 22609, 4915, 1966, 10246, 10780])
            })
            .expect("custom L9"),
            [34001, 15999, 13250, 34500, 7500, 3000, 15635, 16449]
        );
        for index in [6, 7, 8, 9, 254] {
            assert!(
                l9_primaries(MasteringPrimaries {
                    index,
                    custom_xy: None
                })
                .is_err(),
                "negative/unrepresentable/reserved preset {index}"
            );
        }
        assert!(l9_primaries(MasteringPrimaries {
            index: 255,
            custom_xy: Some([65535; 8])
        })
        .is_err());
        let source = DvSourceMastering {
            primaries_rgbw: p3,
            min_1e4_nits: 17,
        };
        let target = |luminance, primaries| {
            let DvDisplayTargetChoice::Target(target) = DvDisplayTarget::for_title(
                1600,
                Some(DvTitleStats {
                    mastering_luminance: luminance,
                    mastering_primaries: primaries,
                    ..stats()
                }),
            )
            .expect("title") else {
                panic!("target");
            };
            target
        };
        let missing = target(TitleConstant::Unknown, TitleConstant::Unknown);
        assert!(missing.needs_source_mastering());
        assert!(missing.resolve_source(None).is_err());
        assert_eq!(
            missing
                .resolve_source(Some(source))
                .expect("resolve source MDCV")
                .output_metadata()
                .expect("source MDCV fallback"),
            crate::fmp4::Hdr10OutputMetadata::new(p3, 1600, 17, 1600).expect("expected metadata")
        );
        let known = target(
            TitleConstant::Consistent(MasteringLuminance {
                min_1e4_nits: 50,
                max_nits: 4000,
            }),
            TitleConstant::Consistent(MasteringPrimaries {
                index: 0,
                custom_xy: None,
            }),
        );
        assert!(!known.needs_source_mastering());
        assert_eq!(
            known
                .resolve_source(Some(source))
                .expect("resolve L6/L9")
                .output_metadata()
                .expect("L6/L9 win"),
            crate::fmp4::Hdr10OutputMetadata::new(p3, 1600, 50, 1600).expect("expected metadata")
        );
        for contradictory in [
            target(TitleConstant::Inconsistent, TitleConstant::Unknown),
            target(TitleConstant::Unknown, TitleConstant::Inconsistent),
        ] {
            assert!(contradictory.resolve_source(Some(source)).is_err());
        }
    }

    #[test]
    fn resolved_floor_unifies_helper_metadata_and_identity_with_renderer_fallback() {
        use super::super::DvExportPolicy;
        let p3 = [34000, 16000, 13250, 34500, 7500, 3000, 15635, 16450];
        let DvDisplayTargetChoice::Target(proposal) = DvDisplayTarget::for_title(
            1600,
            Some(DvTitleStats {
                mastering_primaries: TitleConstant::Consistent(MasteringPrimaries {
                    index: 0,
                    custom_xy: None,
                }),
                ..stats()
            }),
        )
        .expect("title") else {
            panic!("target");
        };
        assert!(
            proposal.output_metadata().is_err(),
            "unresolved proposals cannot author metadata"
        );
        let source = DvSourceMastering {
            primaries_rgbw: p3,
            min_1e4_nits: 17,
        };
        let resolved = proposal
            .resolve_source(Some(source))
            .expect("source MDCV minimum");
        let fallback = proposal
            .resolve_source(None)
            .expect("consistent L9 and established renderer floor");
        assert_eq!(resolved.endpoint_args(), ["17", "4000"]);
        assert_eq!(resolved.mastering().min_1e4_nits, 17);
        assert_eq!(fallback.endpoint_args(), ["50", "4000"]);
        assert_eq!(
            resolved.output_metadata().expect("resolved metadata"),
            crate::fmp4::Hdr10OutputMetadata::new(p3, 1600, 17, 1600).expect("expected metadata")
        );
        assert_eq!(
            fallback.output_metadata().expect("fallback metadata"),
            crate::fmp4::Hdr10OutputMetadata::new(p3, 1600, 50, 1600).expect("expected metadata")
        );
        let export = |target| DvExportPolicy::DisplayTargetYuv420p10V1 {
            raster: (64, 64),
            target,
        };
        assert_ne!(
            export(resolved).target_parameters(),
            export(fallback).target_parameters()
        );
        assert_eq!(
            serde_json::to_value(resolved).expect("serialized policy")["mastering"]["min_1e4_nits"],
            17
        );
        assert_eq!(
            resolved
                .resolve_source(Some(source))
                .expect("same frozen source"),
            resolved
        );
        assert!(
            resolved.resolve_source(None).is_err(),
            "a frozen source floor must not become the default"
        );
        for min_1e4_nits in [10001, u32::MAX] {
            assert!(proposal
                .resolve_source(Some(DvSourceMastering {
                    min_1e4_nits,
                    ..source
                }))
                .is_err());
        }
        let unknown = DvDisplayTarget {
            mastering_primaries: TitleConstant::Unknown,
            ..proposal
        };
        assert!(
            unknown.resolve_source(None).is_err(),
            "missing primaries still refuse"
        );
        let known_l6 = DvDisplayTarget {
            mastering_luminance: TitleConstant::Consistent(MasteringLuminance {
                min_1e4_nits: 50,
                max_nits: 4000,
            }),
            ..proposal
        };
        assert_eq!(
            known_l6
                .resolve_source(Some(source))
                .expect("L6 wins")
                .endpoint_args(),
            ["50", "4000"]
        );
    }

    #[test]
    fn target_identity_separates_buckets_and_elision_is_exact_master() {
        use super::super::DvExportPolicy;
        let raster = (1280, 720);
        let master = DvExportPolicy::PqBilinearYuv420p10V1 { raster };
        let mut identities = std::collections::BTreeSet::new();
        for nits in DISPLAY_TARGET_BUCKETS {
            let (target, reason) = DvExportPolicy::for_display_target(raster, nits, Some(stats()))
                .expect("supported bucket and complete high-peak title");
            assert!(reason.is_none());
            assert_ne!(target.target_parameters(), master.target_parameters());
            assert!(identities.insert(target.target_parameters()));
            assert_eq!(target.output_policy(), format!("bt2020-pq-target-{nits}"));
            let (elided, reason) = DvExportPolicy::for_display_target(
                raster,
                nits,
                Some(DvTitleStats {
                    max_l1_pq: Some(0),
                    ..stats()
                }),
            )
            .expect("supported bucket and complete zero-peak title permit elision");
            assert_eq!(elided, master);
            assert_eq!(elided.target_parameters(), master.target_parameters());
            assert!(matches!(
                reason,
                Some(DvDisplayTargetDecline::TitleFits { .. })
            ));
        }
    }
    #[test]
    fn targeting_requires_every_title_frame_and_exact_buckets() {
        for nits in DISPLAY_TARGET_BUCKETS {
            let DvDisplayTargetChoice::Target(target) =
                DvDisplayTarget::for_title(nits, Some(stats()))
                    .expect("supported bucket and complete high-peak title")
            else {
                panic!("eligible high-peak title");
            };
            assert_eq!(target.nits(), nits);
            assert_eq!(target.endpoint_args(), ["50", "4000"]);
        }
        for nits in [0, 399, 500, 5000, 10000] {
            assert!(DvDisplayTarget::for_title(nits, Some(stats())).is_err());
        }
        for stats in [
            None,
            Some(DvTitleStats {
                complete: false,
                ..stats()
            }),
            Some(DvTitleStats {
                valid_l1_frames: 1,
                missing_l1_frames: 1,
                ..stats()
            }),
            Some(DvTitleStats {
                valid_l1_frames: 1,
                invalid_l1_frames: 1,
                ..stats()
            }),
        ] {
            assert!(matches!(
                DvDisplayTarget::for_title(1000, stats)
                    .expect("supported bucket permits master fallback for unqualified title"),
                DvDisplayTargetChoice::Master { .. }
            ));
        }
        assert_eq!(
            DvDisplayTarget::for_title(
                1000,
                Some(DvTitleStats {
                    max_l1_pq: Some(0),
                    ..stats()
                })
            )
            .expect("supported bucket and complete zero-peak title permit elision"),
            DvDisplayTargetChoice::Master {
                reason: DvDisplayTargetDecline::TitleFits { nits: 1000 }
            }
        );
    }
}
