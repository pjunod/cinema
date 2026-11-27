//! One head-established normalization contract for cold, indexed and missing
//! range copies. It does not issue whole-source parameter-constancy proof.
use plurx_core::{
    domain::MediaFile,
    fmp4::{self, Fragment, Init, PromotionInputs, TrimmedFragment},
    transcode::CopyVideoOptions,
};

#[derive(Debug)]
pub(crate) enum CopyNormalizationError {
    Init(String),
    ContractChanged,
    ParameterSetsChanged,
    Conversion(String),
    Failed,
}
impl std::fmt::Display for CopyNormalizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Init(reason) => write!(f, "copy init: {reason}"),
            Self::ContractChanged => f.write_str("copy_init_contract_changed"),
            Self::ParameterSetsChanged => f.write_str("copy parameter sets changed"),
            Self::Conversion(reason) => write!(f, "copy conversion: {reason}"),
            Self::Failed => f.write_str("copy normalization already failed"),
        }
    }
}

pub(crate) struct CopyNormalizationContract {
    promotion: PromotionInputs,
    raw_parameters: PromotionInputs,
    init_contract_sha256: String,
    convert: bool,
}
impl CopyNormalizationContract {
    /// The caller binds the bounded head census to the same held source object
    /// as every later generation. An artificial one-packet census is never an
    /// index row or complete inspection. Capture promotion BEFORE conversion.
    pub(crate) fn from_head(
        file: &MediaFile,
        video: CopyVideoOptions,
        head_init: &Init,
        head_fragment: &Fragment,
    ) -> Result<Self, CopyNormalizationError> {
        let mut raw_parameters = PromotionInputs::from_fragment(head_fragment, head_init);
        let mut promotion = raw_parameters.clone();
        if video.converts_dolby_vision() {
            promotion.dolby_vision = Some(
                crate::fragindex::converted_dolby_vision_record(file)
                    .map_err(CopyNormalizationError::Conversion)?,
            );
        } else if video.leaves_a_stale_dolby_vision_record(file) {
            promotion.strip_dolby_vision = true;
        }
        // HDR metadata copied from the bounded head is frozen for this output.
        // Later in-band sequence CLL stays in samples. This is not the legacy
        // whole-title maximum scan and never sets indexed_hdr10_metadata.
        raw_parameters.hdr10_sei.clear();
        let init_contract_sha256 = served_init_contract_sha256(head_init, &promotion)?;
        Ok(Self {
            promotion,
            raw_parameters,
            init_contract_sha256,
            convert: video.converts_dolby_vision(),
        })
    }

    pub(crate) fn promotion(&self) -> &PromotionInputs {
        &self.promotion
    }
    pub(crate) fn init_contract_sha256(&self) -> &str {
        &self.init_contract_sha256
    }

    pub(crate) fn begin(
        &self,
        canonical_init: Init,
        av_init: Init,
    ) -> Result<CopyNormalizers, CopyNormalizationError> {
        let canonical = self.pipe(canonical_init, None)?;
        // The AV output carries audio too, and its audio may add file-type
        // brands the video-only head and canonical outputs lack. It must name
        // the canonical output's video, not repeat the persisted bytes.
        let companion = fmp4::copy_video_companion_init_sha256(&canonical.served_init)
            .map_err(|error| CopyNormalizationError::Init(error.to_string()))?;
        let av = self.pipe(av_init, Some(&companion))?;
        Ok(CopyNormalizers {
            canonical,
            av,
            raw_parameters: self.raw_parameters.clone(),
            failed: false,
        })
    }

    /// `companion` is `None` for the canonical output, which must carry the
    /// head's exact contract, and the canonical output's companion digest for
    /// the AV output.
    fn pipe(
        &self,
        raw_init: Init,
        companion: Option<&str>,
    ) -> Result<PipeNormalizer, CopyNormalizationError> {
        let mut served_init = raw_init.clone();
        normalize_init(&mut served_init, &self.promotion)?;
        let (digest, expected) = match companion {
            None => (
                fmp4::copy_video_init_contract_sha256(&served_init),
                self.init_contract_sha256.as_str(),
            ),
            Some(companion) => (
                fmp4::copy_video_companion_init_sha256(&served_init),
                companion,
            ),
        };
        let digest = digest.map_err(|error| CopyNormalizationError::Init(error.to_string()))?;
        if digest != expected {
            return Err(CopyNormalizationError::ContractChanged);
        }
        // The live copy engine has no candidate index row for its rebased
        // fragment clock. It deliberately keeps v1 identity curves; F+ is
        // selected only by an indexed VOD rendition whose landing is proved.
        let converter = if self.convert {
            Some(
                crate::dvpipe::Converter::for_init(&served_init)
                    .map_err(|error| CopyNormalizationError::Conversion(error.to_string()))?,
            )
        } else {
            None
        };
        Ok(PipeNormalizer {
            raw_init,
            served_init,
            converter,
        })
    }
}

/// The decoder/timing contract of `init` once `promotion` is applied: the one
/// recipe for a live copy's frozen head metadata and for a sealed index's
/// whole-title promotion alike.
pub(crate) fn served_init_contract_sha256(
    init: &Init,
    promotion: &PromotionInputs,
) -> Result<String, CopyNormalizationError> {
    let mut served = init.clone();
    normalize_init(&mut served, promotion)?;
    fmp4::copy_video_init_contract_sha256(&served)
        .map_err(|error| CopyNormalizationError::Init(error.to_string()))
}

fn normalize_init(
    init: &mut Init,
    promotion: &PromotionInputs,
) -> Result<(), CopyNormalizationError> {
    crate::copyseg::sanitize_stale_dolby_brand(init);
    fmp4::promote_from(init, promotion)
        .map(|_| ())
        .map_err(|error| CopyNormalizationError::Init(error.to_string()))
}
struct PipeNormalizer {
    raw_init: Init,
    served_init: Init,
    converter: Option<crate::dvpipe::Converter>,
}
pub(crate) struct CopyNormalizers {
    canonical: PipeNormalizer,
    av: PipeNormalizer,
    raw_parameters: PromotionInputs,
    failed: bool,
}
impl CopyNormalizers {
    pub(crate) fn canonical_init(&self) -> &Init {
        &self.canonical.served_init
    }
    pub(crate) fn av_init(&self) -> &Init {
        &self.av.served_init
    }
    /// Normalize one pair. The canonical twin is kept without its slice
    /// data, which neither its parameter sets nor the conversion read: the
    /// same conversion runs on what is kept, so its sizes after conversion
    /// are its own, and a refusal of it fails the pair as it always did.
    pub(crate) fn normalize_pair(
        &mut self,
        canonical: &mut TrimmedFragment,
        av: &mut Fragment,
    ) -> Result<(), CopyNormalizationError> {
        if self.failed {
            return Err(CopyNormalizationError::Failed);
        }
        let kept = canonical.kept_mut();
        let result = match normalize_fragment(&mut self.canonical, kept, &self.raw_parameters) {
            Ok(()) => normalize_fragment(&mut self.av, av, &self.raw_parameters),
            // A conversion refusal names a byte offset in its sample, and the
            // canonical one counts only what was kept. The AV twin refuses
            // the same sample at its real offset, so that is the one reported.
            Err(CopyNormalizationError::Conversion(canonical)) => Err(
                match normalize_fragment(&mut self.av, av, &self.raw_parameters) {
                    Err(av @ CopyNormalizationError::Conversion(_)) => av,
                    _ => CopyNormalizationError::Conversion(canonical),
                },
            ),
            Err(other) => Err(other),
        };
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}
fn normalize_fragment(
    pipe: &mut PipeNormalizer,
    fragment: &mut Fragment,
    expected: &PromotionInputs,
) -> Result<(), CopyNormalizationError> {
    let mut observed = PromotionInputs::from_fragment(fragment, &pipe.raw_init);
    observed.hdr10_sei.clear();
    if !expected.matches_later_parameter_sets(&observed) {
        return Err(CopyNormalizationError::ParameterSetsChanged);
    }
    if let Some(converter) = &mut pipe.converter {
        converter
            .convert(fragment)
            .map_err(|error| CopyNormalizationError::Conversion(error.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
