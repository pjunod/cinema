//! Explicit title-qualified target policy. No client/caps or settings routing.
use super::DvContractError;
use crate::scan::dv_title::{DvTitleStats, MasteringLuminance, TitleEligibility};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub const DISPLAY_TARGET_METHOD: &str = "annex-b2-rgb-fixed-floor-no-expansion-linear-edge-v1";
pub const DISPLAY_TARGET_BUCKETS: [u32; 6] = [400, 600, 1000, 1600, 2000, 4000];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DvDisplayTarget {
    nits: u32,
    mastering: MasteringLuminance,
    max_l1_pq: u16,
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
        let code = f64::from(stats.max_l1_pq.expect("eligible title has a maximum")) / 4095.0;
        let p = code.powf(32.0 / 2523.0);
        let title_nits = 10000.0
            * ((p - 3424.0 / 4096.0).max(0.0) / (2413.0 / 128.0 - 2392.0 / 128.0 * p))
                .powf(16384.0 / 2610.0);
        if title_nits <= f64::from(nits) {
            return Ok(DvDisplayTargetChoice::Master {
                reason: DvDisplayTargetDecline::TitleFits { nits },
            });
        }
        Ok(DvDisplayTargetChoice::Target(Self {
            nits,
            mastering: stats.mastering_or_default(),
            max_l1_pq: stats.max_l1_pq.expect("eligible title maximum"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::dv_title::{TitleConstant, SCHEMA};
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
    fn target_identity_separates_buckets_and_elision_is_exact_master() {
        use super::super::DvExportPolicy;
        let raster = (1280, 720);
        let master = DvExportPolicy::PqBilinearYuv420p10V1 { raster };
        let mut identities = std::collections::BTreeSet::new();
        for nits in DISPLAY_TARGET_BUCKETS {
            let (target, reason) =
                DvExportPolicy::for_display_target(raster, nits, Some(stats())).unwrap();
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
            .unwrap();
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
                DvDisplayTarget::for_title(nits, Some(stats())).unwrap()
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
                DvDisplayTarget::for_title(1000, stats).unwrap(),
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
            .unwrap(),
            DvDisplayTargetChoice::Master {
                reason: DvDisplayTargetDecline::TitleFits { nits: 1000 }
            }
        );
    }
}
