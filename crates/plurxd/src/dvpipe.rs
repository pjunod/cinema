//! The Profile 7 → 8.1 conversion, on the far side of the muxer.
//!
//! A converting copy session is the copy session that ships today — one
//! ffmpeg, video and audio from a single open of the file — with plurx
//! rewriting the Dolby Vision RPU NAL units inside the fragments ffmpeg
//! writes:
//!
//! ```text
//!  one ffmpeg  ──▶  this module  ──▶  the fragment reader, unchanged
//!  fMP4 out         rewrite type-62   (the same kind of stream a plain
//!                   NALs to 8.1,      copy produces; only the RPUs
//!                   fix every size    inside it are different)
//!                   that described
//!                   them
//! ```
//!
//! **It used to sit between two ffmpegs, and that was wrong.** The first
//! emitted a raw Annex B elementary stream for the rewriter and the second
//! remuxed it — but ffmpeg's raw HEVC demuxer emits every packet with no
//! timestamps at all, and the muxer then fabricates a decode-order grid:
//! `pts == dts` for every sample, presentation reordering erased, every
//! minigop played in coding order. Measured on an ordinary three-B-frame
//! encode: ~410 display-order inversions in 819 frames, on every GOP, seek or
//! no seek — and closed-GOP sources were affected identically, so it was not a
//! leading-picture problem. The audio misalignment that first exposed it was
//! the mean of that sawtooth, which is why it tracked B-frame structure
//! without being monotonic in it, and why no seek anchor could have repaired
//! it: an anchor translates a timeline, it does not reorder one.
//!
//! Here the timestamps are never reconstructed, only copied — the property the
//! plain copy path has always had, and precisely why that path is correct.
//! There is no second open to align, so there is no alignment to get wrong.
//!
//! **A refusal is not a failure of the stream.** When a fragment cannot be
//! rewritten — an unconvertible RPU, or a `trun` shape whose sizes cannot be
//! corrected — the caller falls back to the delivery that needs no rewrite,
//! rather than serving unrewritten Profile 7 RPUs under a Profile 8.1 label.

use plurx_core::fmp4::{self, Fmp4Error, Fragment, Init};
use plurx_core::transcode::dvconvert::{self, Converted, EnhancementLayer};

/// What one session's conversion observed, folded across its fragments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// RPUs rewritten so far.
    pub rpus: u64,
    /// The source's enhancement layer, from the first RPU that declared one.
    ///
    /// The honest half of the badge: MEL carries no picture detail of its own,
    /// so dropping it is lossless; FEL carries real residual detail, and
    /// dropping it is not.
    pub enhancement_layer: EnhancementLayer,
    /// The profile the first RPU declared.
    pub source_profile: Option<u8>,
}

impl Default for Report {
    fn default() -> Report {
        Report {
            rpus: 0,
            enhancement_layer: EnhancementLayer::None,
            source_profile: None,
        }
    }
}

impl Report {
    /// Fold one fragment's result in.
    ///
    /// The first fragment that saw an RPU decides the source facts, for the
    /// reason [`dvconvert::convert_annex_b`] gives: the badge names what the
    /// title *was*, and a stream spliced from two sources must not have that
    /// answer depend on where the fragment boundaries happened to fall.
    fn absorb(&mut self, batch: Converted) {
        self.rpus += batch.rpus;
        if self.source_profile.is_none() {
            self.source_profile = batch.source_profile;
            self.enhancement_layer = batch.enhancement_layer;
        }
    }
}

/// Why a stream cannot be converted.
#[derive(Debug, Clone)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Rewrites the Dolby Vision RPUs in one session's fragments.
///
/// Holds the two facts a rewrite needs from the init — which track carries the
/// video, and how wide its NAL length prefixes are — so a caller that has seen
/// the init can convert every fragment after it without re-deriving them.
#[derive(Debug)]
pub struct Converter {
    video_track: u32,
    nal_length_size: u8,
    report: Report,
}

impl Converter {
    /// Read what the conversion needs out of the init.
    ///
    /// Refuses an init it cannot describe rather than converting on a guess: a
    /// wrong NAL length width does not fail, it reads picture bytes as a NAL
    /// header and rewrites the middle of a frame.
    pub fn for_init(init: &Init) -> Result<Converter, Refused> {
        let Some(video) = init.video() else {
            return Err(Refused("this init declares no video track".into()));
        };
        if !matches!(video.nal_length_size, 1 | 2 | 4) {
            return Err(Refused(format!(
                "this init declares a {}-byte NAL length prefix, which is not an hvcC value",
                video.nal_length_size
            )));
        }
        Ok(Converter {
            video_track: video.id,
            nal_length_size: video.nal_length_size,
            report: Report::default(),
        })
    }

    /// What the conversion has seen so far.
    pub fn report(&self) -> Report {
        self.report
    }

    /// Rewrite one fragment in place.
    ///
    /// Answers whether anything changed. `false` for a fragment carrying no
    /// RPUs, which is not an error at this layer — whether a source believed
    /// to be Profile 7 producing none at all is a reason to refuse is the
    /// caller's judgement, made with the file's stored facts.
    pub fn convert(&mut self, fragment: &mut Fragment) -> Result<bool, Refused> {
        let width = self.nal_length_size;
        let mut batch = Converted::default();
        let mut scratch = Vec::new();
        let changed = fmp4::rewrite_video_samples(fragment, self.video_track, |sample| {
            let seen = dvconvert::convert_length_prefixed(sample, width, &mut scratch)
                .map_err(|error| Fmp4Error::Unsupported(error.to_string()))?;
            // Folded here rather than after the call, because
            // `rewrite_video_samples` collects every edit before applying any:
            // a refusal on a later sample must leave neither the bytes nor the
            // tally changed, and `batch` is discarded with the error.
            batch.rpus += seen.rpus;
            if batch.source_profile.is_none() {
                batch.source_profile = seen.source_profile;
                batch.enhancement_layer = seen.enhancement_layer;
            }
            Ok(std::mem::take(&mut scratch))
        })
        .map_err(|error| Refused(error.to_string()))?;
        self.report.absorb(batch);
        Ok(changed)
    }
}
