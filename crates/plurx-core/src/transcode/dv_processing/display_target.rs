//! Explicit title-qualified target policy. No client/caps or settings routing.
use super::DvContractError;
use crate::scan::dv_title::{
    DvTitleStats, MasteringLuminance, MasteringPrimaries, TitleConstant, TitleEligibility,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub const DISPLAY_TARGET_METHOD: &str = "annex-b2-rgb-fixed-floor-no-expansion-linear-edge-v1";
pub const DISPLAY_TARGET_BUCKETS: [u32; 6] = [400, 600, 1000, 1600, 2000, 4000];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DvDisplayTarget {
    nits: u32,
    mastering: MasteringLuminance,
    max_l1_pq: u16,
    mastering_luminance: TitleConstant<MasteringLuminance>,
    mastering_primaries: TitleConstant<MasteringPrimaries>,
    title_stats_sha256: [u8; 32],
}

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
        if !DISPLAY_TARGET_BUCKETS.contains(&nits) {
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
        if title_nits <= f64::from(nits) {
            return Ok(DvDisplayTargetChoice::Master {
                reason: DvDisplayTargetDecline::TitleFits { nits },
            });
        }
        Ok(DvDisplayTargetChoice::Target(Self {
            nits,
            mastering: stats.mastering_or_default(),
            max_l1_pq: stats.max_l1_pq.expect("eligible title maximum"),
            mastering_luminance: stats.mastering_luminance,
            mastering_primaries: stats.mastering_primaries,
            title_stats_sha256: Sha256::digest(
                serde_json::to_vec(&stats).expect("integer title stats serialize"),
            )
            .into(),
        }))
    }
    pub fn nits(self) -> u32 {
        self.nits
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
    pub fn mastering(self) -> MasteringLuminance {
        self.mastering
    }
    pub fn needs_source_mastering(self) -> bool {
        matches!(self.mastering_primaries, TitleConstant::Unknown)
            || matches!(self.mastering_luminance, TitleConstant::Unknown)
    }

    /// Source fallback must come from the same held, attested video stream.
    /// Missing L9 may use source MDCV; contradictory L9 must never be hidden.
    pub fn output_metadata(
        self,
        source: Option<DvSourceMastering>,
    ) -> Result<crate::fmp4::Hdr10OutputMetadata, DvContractError> {
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
        let minimum = match self.mastering_luminance {
            TitleConstant::Consistent(value) => value.min_1e4_nits,
            TitleConstant::Unknown => {
                source
                    .ok_or(DvContractError::Invalid(
                        "targeted HDR10 mastering minimum unknown (no title L6 or source MDCV)",
                    ))?
                    .min_1e4_nits
            }
            TitleConstant::Inconsistent => {
                return Err(DvContractError::Invalid(
                    "targeted HDR10 title L6 mastering luminance inconsistent",
                ))
            }
        };
        // Eligible target selection already proves L1 peak > bucket. Keep the
        // min explicit: MaxCLL is the scanned title bound, capped by the render.
        let max_cll = pq_nits(self.max_l1_pq).min(f64::from(self.nits)).ceil() as u16;
        crate::fmp4::Hdr10OutputMetadata::new(primaries, self.nits, minimum, max_cll)
            .map_err(|_| DvContractError::Invalid("targeted HDR10 metadata cannot be represented"))
    }
    pub fn output_policy(self) -> String {
        format!("bt2020-pq-target-{}", self.nits)
    }
    pub fn endpoint_args(self) -> [String; 2] {
        [
            self.mastering.min_1e4_nits.to_string(),
            self.mastering.max_nits.to_string(),
        ]
    }
}

/// A source MDCV fallback, captured from the selected held video stream. Its
/// units already match ST 2086; it never substitutes container colour tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DvSourceMastering {
    pub primaries_rgbw: [u16; 8],
    pub min_1e4_nits: u32,
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
        assert!(missing.output_metadata(None).is_err());
        assert_eq!(
            missing
                .output_metadata(Some(source))
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
            known.output_metadata(Some(source)).expect("L6/L9 win"),
            crate::fmp4::Hdr10OutputMetadata::new(p3, 1600, 50, 1600).expect("expected metadata")
        );
        for contradictory in [
            target(TitleConstant::Inconsistent, TitleConstant::Unknown),
            target(TitleConstant::Unknown, TitleConstant::Inconsistent),
        ] {
            assert!(contradictory.output_metadata(Some(source)).is_err());
        }
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
