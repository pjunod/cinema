//! Complete-title Dolby Vision metadata, stored in the source's probe document.
//!
//! This is not a window sample or a count of RPU NAL units. The scanner accounts
//! for every display frame the decoder would output, judging each access unit
//! by the renderer pipeline's own rules without decoding it, and reaches clean
//! EOF.
//! The probe publisher owns the source-revision fence and replaces the complete
//! probe document atomically; these facts must never be copied to a new source.

use serde::{Deserialize, Serialize};

pub const PROBE_FIELD: &str = "plurx_dv_title_v1";
pub const SCHEMA: u32 = 1;

/// A title constant cannot be selected by taking the last frame's value.
/// Missing metadata anywhere leaves it unknown; conflicting observations leave
/// it inconsistent. Neither can supply an endpoint or mastering declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum TitleConstant<T> {
    Unknown,
    Consistent(T),
    Inconsistent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasteringLuminance {
    /// L6 minimum mastering luminance, in units of 0.0001 cd/m².
    pub min_1e4_nits: u32,
    /// L6 maximum mastering luminance, in whole cd/m². Never L6 MaxCLL.
    pub max_nits: u32,
}

/// Raw L9 representation, retained for the later HDR10 metadata writer. A
/// preset index is not a BT.2020 declaration. Custom coordinates are the
/// original integer metadata units, not a guessed conversion to SEI units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasteringPrimaries {
    pub index: u8,
    pub custom_xy: Option<[u16; 8]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DvTitleStats {
    pub schema: u32,
    /// True only after successful demux EOF over every access unit, without a
    /// refusal.
    pub complete: bool,
    pub decoded_frames: u64,
    pub valid_l1_frames: u64,
    pub missing_l1_frames: u64,
    pub invalid_l1_frames: u64,
    /// Maximum valid L1 max_pq in original 12-bit PQ code units.
    pub max_l1_pq: Option<u16>,
    pub mastering_luminance: TitleConstant<MasteringLuminance>,
    pub mastering_primaries: TitleConstant<MasteringPrimaries>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleEligibility {
    Eligible,
    Incomplete,
    MissingL1,
    InvalidL1,
    InvalidAggregate,
}

impl TitleEligibility {
    /// The eventual session owner names this decline once for the whole
    /// session; individual windows cannot choose a different output policy.
    pub fn master_reason(self) -> Option<&'static str> {
        match self {
            Self::Eligible => None,
            Self::Incomplete => {
                Some("complete-title Dolby Vision L1 scan unavailable; master grade sent")
            }
            Self::MissingL1 => Some(
                "Dolby Vision L1 missing on a title frame; master grade sent for the whole session",
            ),
            Self::InvalidL1 => Some(
                "Dolby Vision L1 invalid on a title frame; master grade sent for the whole session",
            ),
            Self::InvalidAggregate => {
                Some("invalid complete-title Dolby Vision scan; master grade sent")
            }
        }
    }
}

impl DvTitleStats {
    pub fn eligibility(self) -> TitleEligibility {
        let total = self
            .valid_l1_frames
            .checked_add(self.missing_l1_frames)
            .and_then(|n| n.checked_add(self.invalid_l1_frames));
        if self.schema != SCHEMA
            || total != Some(self.decoded_frames)
            || self.max_l1_pq.is_some_and(|code| code > 4095)
            || (self.valid_l1_frames > 0) != self.max_l1_pq.is_some()
            || matches!(self.mastering_luminance,
                TitleConstant::Consistent(value)
                if value.max_nits == 0 || value.max_nits > 10_000
                    || value.min_1e4_nits > 10_000
                    || u64::from(value.min_1e4_nits) >= u64::from(value.max_nits) * 10_000)
            || matches!(self.mastering_primaries,
                TitleConstant::Consistent(value)
                if (value.index == 255) != value.custom_xy.is_some())
        {
            return TitleEligibility::InvalidAggregate;
        }
        if !self.complete || self.decoded_frames == 0 {
            return TitleEligibility::Incomplete;
        }
        if self.invalid_l1_frames != 0 {
            return TitleEligibility::InvalidL1;
        }
        if self.missing_l1_frames != 0 {
            return TitleEligibility::MissingL1;
        }
        TitleEligibility::Eligible
    }

    /// The fixed per-title fallback is 4000 / 0.005 nits. L1 min never enters
    /// this value, so a scene change cannot pump the black floor.
    pub fn mastering_or_default(self) -> MasteringLuminance {
        match self.mastering_luminance {
            TitleConstant::Consistent(value)
                if value.max_nits > 0
                    && value.max_nits <= 10_000
                    && value.min_1e4_nits <= 10_000
                    && u64::from(value.min_1e4_nits) < u64::from(value.max_nits) * 10_000 =>
            {
                value
            }
            _ => MasteringLuminance {
                min_1e4_nits: 50,
                max_nits: 4000,
            },
        }
    }

    /// Old or future probe documents are unknown, never eligible by default.
    pub fn from_probe_json(document: &str) -> Option<Self> {
        let document: serde_json::Value = serde_json::from_str(document).ok()?;
        let stats: Self = serde_json::from_value(document.get(PROBE_FIELD)?.clone()).ok()?;
        (stats.schema == SCHEMA).then_some(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete() -> DvTitleStats {
        DvTitleStats {
            schema: SCHEMA,
            complete: true,
            decoded_frames: 3,
            valid_l1_frames: 3,
            missing_l1_frames: 0,
            invalid_l1_frames: 0,
            max_l1_pq: Some(0),
            mastering_luminance: TitleConstant::Unknown,
            mastering_primaries: TitleConstant::Unknown,
        }
    }

    #[test]
    fn title_eligibility_requires_complete_exact_display_frame_coverage() {
        let good = complete();
        assert_eq!(good.eligibility(), TitleEligibility::Eligible);
        assert_eq!(
            DvTitleStats {
                complete: false,
                ..good
            }
            .eligibility(),
            TitleEligibility::Incomplete
        );
        assert_eq!(
            DvTitleStats {
                decoded_frames: 4,
                ..good
            }
            .eligibility(),
            TitleEligibility::InvalidAggregate
        );
        assert_eq!(
            DvTitleStats {
                valid_l1_frames: 2,
                missing_l1_frames: 1,
                ..good
            }
            .eligibility(),
            TitleEligibility::MissingL1
        );
        assert_eq!(
            DvTitleStats {
                valid_l1_frames: 2,
                invalid_l1_frames: 1,
                ..good
            }
            .eligibility(),
            TitleEligibility::InvalidL1
        );
        assert_eq!(
            DvTitleStats {
                valid_l1_frames: u64::MAX,
                missing_l1_frames: 1,
                ..good
            }
            .eligibility(),
            TitleEligibility::InvalidAggregate
        );
        assert_eq!(
            DvTitleStats {
                max_l1_pq: Some(4096),
                ..good
            }
            .eligibility(),
            TitleEligibility::InvalidAggregate
        );
        assert_eq!(
            DvTitleStats {
                max_l1_pq: None,
                ..good
            }
            .eligibility(),
            TitleEligibility::InvalidAggregate
        );
        assert_eq!(
            DvTitleStats {
                decoded_frames: 0,
                valid_l1_frames: 0,
                max_l1_pq: None,
                ..good
            }
            .eligibility(),
            TitleEligibility::Incomplete
        );
    }

    #[test]
    fn title_defaults_are_constant_and_old_probe_documents_remain_unknown() {
        let good = complete();
        assert_eq!(
            good.mastering_or_default(),
            MasteringLuminance {
                min_1e4_nits: 50,
                max_nits: 4000
            }
        );
        assert_eq!(
            DvTitleStats {
                mastering_luminance: TitleConstant::Inconsistent,
                ..good
            }
            .mastering_or_default(),
            good.mastering_or_default()
        );
        assert!(DvTitleStats::from_probe_json("{}").is_none());
        let mut doc = serde_json::json!({ PROBE_FIELD: good });
        assert_eq!(DvTitleStats::from_probe_json(&doc.to_string()), Some(good));
        doc[PROBE_FIELD]["schema"] = serde_json::json!(2);
        assert!(DvTitleStats::from_probe_json(&doc.to_string()).is_none());
    }
}
