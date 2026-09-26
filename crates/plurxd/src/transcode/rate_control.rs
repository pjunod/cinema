use super::*;

/// One atomically published answer to the operator's requested rate control.
///
/// `quality_rc` is for the exact requested override (or each family's
/// candidate default), not a version-derived guess. A session reads this once
/// while its normalized options are built and keeps that effective value for
/// its lifetime.
#[derive(Debug, Clone, Copy)]
pub(super) struct RateControlSnapshot {
    /// `None` means the operator has not chosen a mode, so the encoder
    /// family's evidence-backed code default applies. `Some(Bitrate)` is an
    /// explicit override and must remain VBR even if that family later flips.
    pub(super) requested_mode: Option<RateMode>,
    pub(super) requested_quality: Option<u8>,
    pub(super) quality_rc: QualityRc,
}

pub(super) const RATE_CONTROL_REFRESH: Duration = Duration::from_secs(2);

/// Normalize the complete durable request pair. Missing/empty quality is the
/// valid "use this family's default" value; a present nonempty value that does
/// not fit `u8` is corruption and fails the entire pair back to legacy VBR.
pub(crate) fn normalize_rate_control_request(
    raw_mode: Option<&str>,
    raw_quality: Option<&str>,
) -> (Option<RateMode>, Option<u8>, bool) {
    let mode_text = raw_mode.map(str::trim).filter(|value| !value.is_empty());
    let mode = mode_text.and_then(RateMode::parse);
    let quality_text = raw_quality.map(str::trim).filter(|value| !value.is_empty());
    let quality = quality_text.and_then(|value| value.parse::<u8>().ok());
    let corrupt =
        mode_text.is_some() && mode.is_none() || quality_text.is_some() && quality.is_none();
    if corrupt {
        (Some(RateMode::Bitrate), None, true)
    } else {
        (mode, quality, false)
    }
}

pub(super) enum RateControlValidation {
    Complete(RateControlSnapshot),
    Deferred,
}

#[derive(Clone, Copy)]
pub(super) enum RateControlProbePolicy {
    Boot,
    YieldingBackground,
}

#[derive(Debug)]
pub enum ApplyRateControlError {
    Store(plurx_core::error::StoreError),
    Busy,
}

impl From<plurx_core::error::StoreError> for ApplyRateControlError {
    fn from(error: plurx_core::error::StoreError) -> Self {
        Self::Store(error)
    }
}

impl RateControlSnapshot {
    pub(super) fn bitrate(boot_caps: QualityRc) -> Self {
        Self {
            requested_mode: Some(RateMode::Bitrate),
            requested_quality: None,
            quality_rc: boot_caps,
        }
    }

    pub(super) fn effective_for(self, encoder: Encoder) -> EffectiveRateControl {
        let requested_mode = self
            .requested_mode
            .unwrap_or_else(|| encoder.default_rate_mode());
        if requested_mode == RateMode::Quality && self.quality_rc.supported_by(encoder) {
            EffectiveRateControl::Qvbr {
                quality: self
                    .requested_quality
                    .unwrap_or_else(|| encoder.default_quality()),
            }
        } else {
            EffectiveRateControl::Vbr
        }
    }
}
