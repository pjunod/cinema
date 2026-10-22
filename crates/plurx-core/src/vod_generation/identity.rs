use crate::fmp4::{
    collapse_equivalent_hevc_sample_entries, normalize_indexed_hdr10_metadata, promote_from,
    Fmp4Error, Init, PromotionInputs,
};
use sha2::{Digest, Sha256};

/// The two digests plan §2.2's identity check compares, and the inputs that
/// connect them.
///
/// Two, not one, because they detect different things and only one of them can
/// be compared across generations at all.
///
/// - `muxer_init` is ffmpeg's `ftyp`+`moov`, after normalization: scene content-light levels use the
///   stored title-wide values, and repeated decoder-equivalent HEVC descriptions collapse
///   to one ([`collapse_equivalent_hevc_sample_entries`]) — ffmpeg 8 fills
///   them from whichever keyframe led the generation. It is byte-identical across
///   generations including `-ss`-started ones — M0-P0 clause (d), 9/9 — so a
///   mismatch here is real pipeline drift: a different ffmpeg, a different
///   argv, a re-fragmenting upgrade. That is the `producer_failed` case.
/// - `served_init` is what is written to `init.mp4` and what a viewer holds.
///   It is the muxer init plus [`PromotionInputs`], and because those inputs
///   are captured once rather than read from whichever fragment a generation
///   landed on, it is byte-identical across generations *by construction*.
///   Checking it is an assertion, not a test — it cannot fire spuriously, and
///   if it ever does, promotion stopped being a pure function of stored facts.
///
/// Re-promoting and rewriting a stored init is not an option this type offers,
/// deliberately: viewers and already-materialized segments hold the old bytes,
/// and quietly replacing them is the outcome §2.2 exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitIdentity {
    pub muxer_init: String,
    pub served_init: String,
    pub promotion: PromotionInputs,
}

/// Why a generation's init was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitRefused {
    /// The muxer init differs from the rendition's. Real pipeline drift.
    MuxerDrift { stored: String, found: String },
    /// The muxer init matched and the served init did not, which means
    /// promotion is no longer a pure function of the stored inputs. It is here
    /// so that when something does cause it, it is a loud refusal rather than
    /// a viewer receiving different bytes.
    ///
    /// One cause is ordinary and expected, and is worth knowing before an
    /// operator goes hunting a pipeline bug: **version skew inside a
    /// cluster.** [`crate::fmp4::PromotionInputs`] gains fields over
    /// time, each `#[serde(default)]` so a newer node reads an older
    /// rendition correctly. The reverse does not hold — an older node
    /// deserializing a newer rendition silently drops the field it does not
    /// know, promotes without it, and lands here. `strip_dolby_vision` is the
    /// first field with that property.
    ///
    /// The refusal is the right answer: the older node genuinely cannot build
    /// the bytes the rendition promised, and serving what it can build would
    /// hand a viewer holding one generation's playlist a different
    /// generation's media. The remedy is to finish the rollout, not to relax
    /// the check.
    PromotionDrift { stored: String, found: String },
}

impl std::fmt::Display for InitRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitRefused::MuxerDrift { stored, found } => write!(
                f,
                "this generation's muxer init is {found}, and the rendition \
                 was built against {stored} — the video pipeline changed under \
                 a rendition a client already holds a playlist for"
            ),
            InitRefused::PromotionDrift { stored, found } => write!(
                f,
                "promoting the stored inputs produced {found} where the \
                 rendition's served init is {stored}"
            ),
        }
    }
}

impl InitIdentity {
    /// Establish a rendition's identity from its first generation.
    pub fn establish(muxer: &Init, promotion: PromotionInputs) -> Result<InitIdentity, Fmp4Error> {
        let muxer = normalized_muxer(muxer, &promotion)?;
        let mut served = muxer.clone();
        promote_from(&mut served, &promotion)?;
        Ok(InitIdentity {
            muxer_init: digest(&muxer.bytes),
            served_init: digest(&served.bytes),
            promotion,
        })
    }

    /// Build a later generation's served init, refusing rather than serving
    /// bytes that do not match what the rendition promised.
    ///
    /// This is plan §2.2's check. It runs at generation start, against the
    /// muxer init the pipe just emitted, before a single segment is written.
    pub fn served_init_for(&self, muxer: &Init) -> Result<Init, InitRefused> {
        let muxer =
            &normalized_muxer(muxer, &self.promotion).map_err(|_| InitRefused::MuxerDrift {
                stored: self.muxer_init.clone(),
                found: "unreadable".to_owned(),
            })?;
        let found = digest(&muxer.bytes);
        if found != self.muxer_init {
            return Err(InitRefused::MuxerDrift {
                stored: self.muxer_init.clone(),
                found,
            });
        }
        let mut served = muxer.clone();
        // A promotion error here is the same class of answer as a digest
        // mismatch: the stored inputs no longer apply to this init.
        if promote_from(&mut served, &self.promotion).is_err() {
            return Err(InitRefused::PromotionDrift {
                stored: self.served_init.clone(),
                found: "unpromotable".to_owned(),
            });
        }
        let served_digest = digest(&served.bytes);
        if served_digest != self.served_init {
            return Err(InitRefused::PromotionDrift {
                stored: self.served_init.clone(),
                found: served_digest,
            });
        }
        Ok(served)
    }
}

/// The muxer init as the identity digests it (see [`InitIdentity`]). A single
/// description, or descriptions that differ, pass through unchanged, so every
/// identity recorded before this normalization still matches.
fn normalized_muxer(muxer: &Init, promotion: &PromotionInputs) -> Result<Init, Fmp4Error> {
    let mut normalized = muxer.clone();
    collapse_equivalent_hevc_sample_entries(&mut normalized)?;
    if promotion.indexed_hdr10_metadata {
        normalize_indexed_hdr10_metadata(&mut normalized, &promotion.hdr10_sei)?;
    }
    Ok(normalized)
}

fn digest(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes))
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
