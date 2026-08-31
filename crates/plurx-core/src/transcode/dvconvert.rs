//! Profile 7 → Profile 8.1 Dolby Vision conversion, one NAL unit at a time.
//!
//! A Profile 7 disc remux carries two layers: a base layer that is
//! HDR10-compatible, and an enhancement layer no consumer decoder takes —
//! there has never been a shipping player for dual-layer Dolby Vision outside
//! of Blu-ray hardware. Profile 8.1 is the same base layer, with each RPU
//! rewritten to describe a single layer. The conversion drops one layer and
//! rewrites a per-frame metadata unit; it re-encodes nothing and loses no
//! picture the client could have seen.
//!
//! What [`ConversionMode::To81`] actually does to an RPU is more than clearing
//! a flag, and it is worth stating because the RPU shrinks visibly and a
//! reader who expected a flag edit would think something was lost. It clears
//! the enhancement-layer references, installs the Profile 8.1 colour
//! coefficients (`set_p81_coeffs`), and — for a full enhancement layer only —
//! drops the NLQ mapping the enhancement layer was the input to
//! (`remove_mapping`). Measured on the FEL fixture in this module's tests: 367
//! bytes in, 150 bytes out, `el_type` FEL → `None`. The base-layer picture is
//! untouched; what shrinks is metadata describing a layer that is no longer
//! there.
//!
//! This is the middle stage of the pipe in PLAYBACK-CAPS-V2-PLAN §4.8:
//!
//! ```text
//!  ffmpeg#1  -c:v copy -bsf:v hevc_mp4toannexb,filter_units=remove_types=63
//!                │  Annex B, base layer + RPU, enhancement layer dropped
//!                ▼
//!  this module   type 62 (RPU) → convert to 8.1, CRC32 recomputed
//!                anything else → passed through byte for byte
//!                │
//!                ▼
//!  ffmpeg#2  -f hevc -i -  -c copy  -tag:v hvc1 → the HLS segmenter
//! ```
//!
//! **Nothing runs this pipe yet.** `convert_annex_b` and
//! [`whole_units_prefix`] have no callers outside these tests, and what
//! [`super::copy_video_args`] renders today for a converting source is the
//! ordinary single-ffmpeg copy — fragmented MP4, which this stage cannot read
//! — plus the marker that reserves its fragment-index identity. Wiring the
//! stage up therefore has to change that argv (Annex B out of ffmpeg#1, and
//! `remove_types=63` to drop the enhancement layer, which is not dropped
//! today), and changing the argv changes the fingerprint. That is the intended
//! order: the marker exists so a converted stream never shares an index with
//! an unconverted one, and re-indexing at the moment the pipe becomes real is
//! correct, not a cost to design around.
//!
//! ffmpeg#2 writes no Dolby Vision configuration record — it copies the one
//! its input container had, and a raw Annex B stream has no container
//! (measured: `docs/PLAYBACK-CAPS-V2-M0.md` §8). The record is written
//! separately by [`crate::fmp4::set_dolby_vision_record`].
//!
//! Three names for the output get confused with each other, so all three at
//! once. The **sample entry** is `hvc1`, not `dvh1`: Profile 8.1 is a
//! backward-compatible enhancement of HDR10, and Apple's ISOBMFF contract
//! requires a compatible stream to keep the base entry — which is what
//! [`super::hevc_copy_tag_for_source`] already answers for any source with a
//! compatible base, so the conversion needs no special case. The
//! **configuration record** is `dvvC`, the profile ≥ 8 spelling, and not the
//! `dvcC` the Profile 7 source arrived with. The **playlist** still advertises
//! `dvh1.08.06`, because `SUPPLEMENTAL-CODECS` is a codec string, not a box
//! name.
//!
//! Note that the plan disagrees about the first of those: §4.8 writes
//! `-tag:v dvh1` and describes the record as living "inside the `dvh1` sample
//! entry", and the string `hvc1` appears nowhere in the plan document. §4.8's
//! only stated correction is `dvcC` → `dvvC`. The code is right and the plan
//! is stale — `hevc_copy_tag_for_source` has answered `hvc1` for any
//! compatible base since long before this milestone, and a converted 8.1
//! stream is exactly that — but the plan says otherwise, so this says so
//! rather than citing it for a claim it does not make.

use dolby_vision::rpu::dovi_rpu::DoviRpu;
use dolby_vision::rpu::rpu_data_nlq::DoviELType;
use dolby_vision::rpu::ConversionMode;

/// The NAL type an RPU travels in (`unspec62`).
const RPU_NAL_TYPE: u8 = 62;

/// What kind of enhancement layer the source carried, read off its first RPU.
///
/// The difference is what the viewer loses, and it is the honest half of the
/// badge: MEL is a minimum enhancement layer that carries no picture detail of
/// its own, so dropping it is lossless; FEL carries real residual detail, and
/// dropping it is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnhancementLayer {
    /// Minimum enhancement layer — near-lossless to drop.
    Minimum,
    /// Full enhancement layer — real detail is lost.
    Full,
    /// The RPU declared no enhancement layer, which is what a Profile 8
    /// source looks like. Nothing to lose.
    None,
}

impl EnhancementLayer {
    /// The clause the session's reasons carry, so a viewer reading the badge
    /// is told what the conversion cost rather than only that it happened.
    pub fn reason(&self) -> &'static str {
        match self {
            EnhancementLayer::Minimum => {
                "the discarded enhancement layer was minimum (MEL): near-lossless"
            }
            EnhancementLayer::Full => {
                "the discarded enhancement layer was full (FEL): its residual detail is lost"
            }
            EnhancementLayer::None => "the source declared no enhancement layer",
        }
    }
}

/// What one conversion pass observed, for the badge and the session's reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Converted {
    /// RPUs rewritten. Zero means the stream carried none, which is not a
    /// Profile 7 source and not something to claim a conversion for.
    pub rpus: u64,
    /// The source's enhancement layer, from the first RPU that declared one.
    pub enhancement_layer: EnhancementLayer,
    /// The source profile the first RPU declared. 7 for the case this exists
    /// for; anything else is worth reporting rather than silently converting.
    pub source_profile: Option<u8>,
}

impl Default for Converted {
    fn default() -> Converted {
        Converted {
            rpus: 0,
            enhancement_layer: EnhancementLayer::None,
            source_profile: None,
        }
    }
}

/// Why a conversion could not be completed.
///
/// Every variant carries two locators. `frame` is how many RPUs were rewritten
/// before this one — so it is the zero-based index of the failing RPU among
/// the units this pass accepted, not a picture's frame number in the source.
/// `offset` is the byte the failing unit's start code sits at inside the
/// buffer this pass was handed, because an ordinal cannot be turned back into
/// bytes on disk and a refusal that cannot be reproduced cannot be diagnosed.
#[derive(Debug, thiserror::Error)]
pub enum DvConvertError {
    #[error("the Dolby Vision RPU at frame {frame} (byte {offset}) could not be read: {detail}")]
    Unreadable {
        frame: u64,
        offset: usize,
        detail: String,
    },
    #[error("the Dolby Vision RPU at frame {frame} (byte {offset}) could not be converted to profile 8.1: {detail}")]
    Unconvertible {
        frame: u64,
        offset: usize,
        detail: String,
    },
    /// The RPU declared a profile this conversion has no correct answer for.
    ///
    /// [`ConversionMode::To81`] converts whatever it is given, and none of its
    /// answers fail loudly:
    ///
    /// - **Profile 5** goes through `p5_to_p81`, which yields a well-formed
    ///   RPU *labelled* 8.1 over an IPT-PQ-C2 base layer. Every frame is the
    ///   wrong colour and nothing downstream can tell, because the conversion
    ///   reported success.
    /// - **Profile 8** is passed through essentially unchanged — and that is
    ///   the subtler trap, because profile 8 covers both 8.1 (HDR10 base) and
    ///   8.4 (HLG base) and *the RPU does not say which*. The distinction
    ///   lives in the container's `dv_bl_signal_compatibility_id`, which this
    ///   stage never sees. An 8.4 RPU accepted here comes out byte-identical
    ///   with `rpus: 1`, and the caller then writes a `dvvC` with
    ///   compatibility id 1 and advertises `dvh1.08.06` over an HLG base:
    ///   every frame decoded through PQ where ARIB STD-B67 was meant.
    ///
    /// So Profile 7 is the only accepted input, and what that costs is one
    /// real stream: a remux spliced from a Profile 7 source and an already-8.1
    /// one — or a P7 file some earlier tool converted halfway — now refuses
    /// where it would previously have converted, because an 8.1 RPU passes
    /// through byte for byte. That is the trade taken deliberately: a mixed
    /// stream is rare and its refusal is loud and diagnosable (the error names
    /// the byte), while an 8.4 source is indistinguishable from an 8.1 one at
    /// this layer and its acceptance is silent and wrong on every frame. A
    /// caller that wants the spliced case back should widen the check with the
    /// container's compatibility id in hand, not by trusting the RPU.
    #[error("the Dolby Vision RPU at frame {frame} (byte {offset}) declares profile {profile}; this converts profile 7 only, because the RPU alone cannot tell an HDR10 base from an HLG one")]
    UnsupportedProfile {
        frame: u64,
        offset: usize,
        profile: u8,
    },
}

/// Rewrite every RPU in an Annex B byte stream to Profile 8.1, leaving every
/// other NAL unit byte for byte as it was.
///
/// `out` is **replaced** by the whole stream, converted; anything it held is
/// discarded, so the same buffer can be reused across calls without carrying
/// the previous chunk's bytes into this one's output.
///
/// **The caller owns framing.** This converts the buffer it is given and
/// nothing else — a NAL unit split across two calls would be handed to the
/// RPU parser in halves and refused. A caller reading from a pipe must cut its
/// buffer at a unit boundary and carry the remainder into the next read;
/// [`whole_units_prefix`] is that cut. What it bounds is the *carried tail* —
/// one NAL unit, never the file, which for a two-hour 4K remux is ~72 GB. The
/// rest of a call's memory is the caller's read buffer and two `Vec`s
/// proportional to it, so the read size is the caller's to choose.
///
/// **A stream with no RPUs converts to itself**, and answers `rpus: 0`. That
/// is not an error: the caller decides whether a source it believed was
/// Profile 7 having no RPUs is a reason to refuse, and it has the file's
/// stored facts to decide with, which this function does not.
///
/// **On any error `out` is left empty**, not holding the units that converted
/// before the failure. A refusal happens partway through by construction — the
/// bad RPU is discovered after its predecessors were written — and a caller
/// that forwarded the buffer on its error path would publish a stream truncated
/// mid-frame, which a segmenter will happily index and a player will happily
/// stall on. The bytes are unrecoverable anyway: the refusal means the stream
/// cannot be converted, not that some prefix of it can.
pub fn convert_annex_b(input: &[u8], out: &mut Vec<u8>) -> Result<Converted, DvConvertError> {
    out.clear();
    out.reserve(input.len());
    match convert_units(input, out) {
        Ok(report) => Ok(report),
        Err(error) => {
            out.clear();
            Err(error)
        }
    }
}

/// The conversion proper. Separate only so its early returns cannot leave a
/// half-converted stream behind — see `convert_annex_b`.
fn convert_units(input: &[u8], out: &mut Vec<u8>) -> Result<Converted, DvConvertError> {
    let mut report = Converted::default();
    for unit in annex_b_units(input) {
        let AnnexBUnit {
            start_code,
            nal,
            offset,
        } = unit;
        if nal_type(nal) != Some(RPU_NAL_TYPE) {
            out.extend_from_slice(start_code);
            out.extend_from_slice(nal);
            continue;
        }
        let mut rpu =
            DoviRpu::parse_unspec62_nalu(nal).map_err(|error| DvConvertError::Unreadable {
                frame: report.rpus,
                offset,
                detail: error.to_string(),
            })?;
        // Refuse before converting, not after. `To81` has an answer for every
        // profile it is handed and none of them fail loudly; the error's own
        // documentation says what each wrong answer looks like on screen.
        if rpu.dovi_profile != 7 {
            return Err(DvConvertError::UnsupportedProfile {
                frame: report.rpus,
                offset,
                profile: rpu.dovi_profile,
            });
        }
        if report.source_profile.is_none() {
            report.source_profile = Some(rpu.dovi_profile);
            report.enhancement_layer = match rpu.el_type {
                Some(DoviELType::MEL) => EnhancementLayer::Minimum,
                Some(DoviELType::FEL) => EnhancementLayer::Full,
                None => EnhancementLayer::None,
            };
        }
        // `To81` is the conversion the plan names; the module doc says what it
        // does beyond clearing the enhancement-layer references. The library
        // writes the NAL type back and re-applies start-code emulation
        // prevention, so what comes out is a complete replacement unit.
        rpu.convert_with_mode(ConversionMode::To81)
            .map_err(|error| DvConvertError::Unconvertible {
                frame: report.rpus,
                offset,
                detail: error.to_string(),
            })?;
        let written =
            rpu.write_hevc_unspec62_nalu()
                .map_err(|error| DvConvertError::Unconvertible {
                    frame: report.rpus,
                    offset,
                    detail: error.to_string(),
                })?;
        out.extend_from_slice(start_code);
        out.extend_from_slice(&written);
        report.rpus += 1;
    }
    Ok(report)
}

/// Rewrite every RPU in a **length-prefixed** sample, the shape a sample has
/// inside an fMP4 fragment.
///
/// The Annex B form above exists for a pipeline that no longer ships: feeding
/// a raw elementary stream between two ffmpegs destroys the video timeline,
/// because ffmpeg's raw HEVC demuxer emits every packet with no timestamps at
/// all and the muxer then fabricates a decode-order grid — `pts == dts` for
/// every sample, presentation reordering erased, every minigop played in
/// coding order. Measured: ~410 display-order inversions in 819 frames on an
/// ordinary three-B-frame encode, on every GOP, seek or no seek.
///
/// So the rewrite moved to the other side of the muxer. One ffmpeg produces
/// the fragmented MP4 it always produced — video and audio from one open, one
/// timeline, timestamps copied rather than reconstructed — and plurx rewrites
/// the RPU NAL units inside the fragments it already parses. There is no
/// second stream to align, which is why there is no longer an alignment
/// question to get wrong.
///
/// `nal_length_size` is the `hvcC` field: 1, 2 or 4. Returns what the sample
/// held, and the caller owns the consequences of the size change — a shorter
/// sample has to be declared shorter, which is the size-fixup chain in
/// [`crate::fmp4`].
pub fn convert_length_prefixed(
    sample: &[u8],
    nal_length_size: u8,
    out: &mut Vec<u8>,
) -> Result<Converted, DvConvertError> {
    out.clear();
    out.reserve(sample.len());
    match convert_length_prefixed_into(sample, nal_length_size, out) {
        Ok(report) => Ok(report),
        Err(error) => {
            out.clear();
            Err(error)
        }
    }
}

fn convert_length_prefixed_into(
    sample: &[u8],
    nal_length_size: u8,
    out: &mut Vec<u8>,
) -> Result<Converted, DvConvertError> {
    let width = usize::from(nal_length_size);
    if !matches!(width, 1 | 2 | 4) {
        return Err(DvConvertError::Unreadable {
            frame: 0,
            offset: 0,
            detail: format!("a {nal_length_size}-byte NAL length prefix is not a legal hvcC value"),
        });
    }
    let mut report = Converted::default();
    let mut at = 0usize;
    while at < sample.len() {
        if at + width > sample.len() {
            return Err(DvConvertError::Unreadable {
                frame: report.rpus,
                offset: at,
                detail: "a NAL length prefix runs past the end of the sample".to_owned(),
            });
        }
        let length = sample[at..at + width]
            .iter()
            .fold(0usize, |acc, byte| (acc << 8) | usize::from(*byte));
        let body = at + width;
        let end = body
            .checked_add(length)
            .filter(|end| *end <= sample.len())
            .ok_or_else(|| DvConvertError::Unreadable {
                frame: report.rpus,
                offset: at,
                detail: format!("a NAL unit declares {length} bytes the sample does not carry"),
            })?;
        let nal = &sample[body..end];

        if nal_type(nal) != Some(RPU_NAL_TYPE) {
            out.extend_from_slice(&sample[at..end]);
            at = end;
            continue;
        }

        let mut rpu =
            DoviRpu::parse_unspec62_nalu(nal).map_err(|error| DvConvertError::Unreadable {
                frame: report.rpus,
                offset: at,
                detail: error.to_string(),
            })?;
        // The same guard the Annex B form applies, for the same reasons: the
        // error type's own documentation says what each wrong answer looks
        // like on screen.
        if rpu.dovi_profile != 7 {
            return Err(DvConvertError::UnsupportedProfile {
                frame: report.rpus,
                offset: at,
                profile: rpu.dovi_profile,
            });
        }
        if report.source_profile.is_none() {
            report.source_profile = Some(rpu.dovi_profile);
            report.enhancement_layer = match rpu.el_type {
                Some(DoviELType::MEL) => EnhancementLayer::Minimum,
                Some(DoviELType::FEL) => EnhancementLayer::Full,
                None => EnhancementLayer::None,
            };
        }
        rpu.convert_with_mode(ConversionMode::To81)
            .map_err(|error| DvConvertError::Unconvertible {
                frame: report.rpus,
                offset: at,
                detail: error.to_string(),
            })?;
        // `write_hevc_unspec62_nalu` emits the Annex B spelling: the NAL with
        // its type byte, ready for a start code. Inside a sample the same
        // bytes take a length prefix instead, so the payload is identical and
        // only the framing differs.
        let written =
            rpu.write_hevc_unspec62_nalu()
                .map_err(|error| DvConvertError::Unconvertible {
                    frame: report.rpus,
                    offset: at,
                    detail: error.to_string(),
                })?;
        let length = written.len();
        if length >= 1usize << (8 * width) {
            return Err(DvConvertError::Unconvertible {
                frame: report.rpus,
                offset: at,
                detail: format!(
                    "the converted RPU is {length} bytes, which a {width}-byte length prefix \
                     cannot express"
                ),
            });
        }
        for shift in (0..width).rev() {
            out.push(((length >> (8 * shift)) & 0xff) as u8);
        }
        out.extend_from_slice(&written);
        report.rpus += 1;
        at = end;
    }
    Ok(report)
}

/// How much of a read buffer is safely convertible: everything up to the last
/// start code in it.
///
/// A pipe hands out whatever bytes have arrived, and the last NAL unit in a
/// read is almost always incomplete. Converting it would refuse a perfectly
/// good RPU as unreadable; passing it through would emit a Profile 7 RPU into
/// a stream every other RPU says is 8.1. So a streaming caller converts this
/// prefix, keeps the tail, and prepends it to the next read.
///
/// Answers `0` when the buffer holds no start code at all — the caller has not
/// yet read a whole unit and must read more. The tail a caller carries is
/// therefore bounded by one NAL unit, not by the file.
pub fn whole_units_prefix(input: &[u8]) -> usize {
    let starts = start_codes(input);
    match starts.len() {
        // Nothing, or a single unit that may still be growing.
        0 | 1 => 0,
        _ => starts[starts.len() - 1].0,
    }
}

/// One Annex B unit: its start code and the NAL that follows.
///
/// The start code is carried rather than normalized so a pass-through is byte
/// for byte — ffmpeg emits four-byte codes at parameter sets and three-byte
/// ones elsewhere, and rewriting them would change bytes this stage has no
/// business changing.
struct AnnexBUnit<'a> {
    start_code: &'a [u8],
    nal: &'a [u8],
    /// Byte offset of the start code within the buffer, so a refusal names a
    /// place in the stream and not just an ordinal.
    offset: usize,
}

/// The HEVC NAL type, from the two-byte header.
fn nal_type(nal: &[u8]) -> Option<u8> {
    (nal.len() >= 2).then(|| (nal[0] >> 1) & 0x3f)
}

/// Every start code in the buffer, as `(offset, width)`.
///
/// Scanning for the byte pattern is the whole of Annex B parsing, and it is
/// safe to do without decoding anything: HEVC requires an encoder to insert an
/// emulation-prevention byte wherever `00 00 00`, `00 00 01`, `00 00 02` or
/// `00 00 03` would otherwise appear inside a NAL payload (ITU-T H.265
/// §7.4.2). So a `00 00 01` inside a conformant unit does not exist, and a
/// split at one is a unit boundary by definition. A non-conformant stream
/// would split a payload here — the fragment is then passed through byte for
/// byte unless its first two bytes happen to spell NAL type 62, which is the
/// only case that could misroute, and which cannot arise from a stream ffmpeg
/// just wrote.
fn start_codes(input: &[u8]) -> Vec<(usize, usize)> {
    let mut starts: Vec<(usize, usize)> = Vec::new();
    let mut at = 0usize;
    while at + 3 <= input.len() {
        if input[at] == 0 && input[at + 1] == 0 {
            // The two arms are mutually exclusive and their order does not
            // matter: the three-byte pattern needs `input[at + 2] == 1` and
            // the four-byte one needs it to be `0`. Said here because the
            // shape invites the assumption that longest-match-first is load
            // bearing, and a future reader reordering them would be right to.
            if input[at + 2] == 0 && at + 4 <= input.len() && input[at + 3] == 1 {
                starts.push((at, 4));
                at += 4;
                continue;
            }
            if input[at + 2] == 1 {
                starts.push((at, 3));
                at += 3;
                continue;
            }
        }
        at += 1;
    }
    starts
}

/// Split an Annex B stream into units.
///
/// Anything before the first start code is emitted as a unit with an empty
/// start code, so a caller's bytes are never dropped even when the input does
/// not begin where this expects.
fn annex_b_units(input: &[u8]) -> Vec<AnnexBUnit<'_>> {
    let starts = start_codes(input);
    if starts.is_empty() {
        return if input.is_empty() {
            Vec::new()
        } else {
            vec![AnnexBUnit {
                start_code: &input[..0],
                nal: input,
                offset: 0,
            }]
        };
    }
    let mut out = Vec::with_capacity(starts.len() + 1);
    if starts[0].0 > 0 {
        out.push(AnnexBUnit {
            start_code: &input[..0],
            nal: &input[..starts[0].0],
            offset: 0,
        });
    }
    for (index, &(offset, width)) in starts.iter().enumerate() {
        let nal_start = offset + width;
        let nal_end = starts.get(index + 1).map_or(input.len(), |&(next, _)| next);
        out.push(AnnexBUnit {
            start_code: &input[offset..nal_start],
            nal: &input[nal_start..nal_end],
            offset,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use dolby_vision::rpu::generate::{GenerateConfig, GenerateProfile};

    /// One real Profile 7 RPU, captured 2026-08-30 from
    /// `Nosferatu (2024) Remux-2160p.mkv` on nuc4 through the pipe's own first
    /// stage — `-c:v copy -bsf:v hevc_mp4toannexb,filter_units=remove_types=63`.
    ///
    /// A real one rather than a synthetic one because the whole question this
    /// module answers is whether a library's idea of a Profile 7 RPU matches
    /// what a disc remux actually carries.
    ///
    /// It lives in a file rather than a literal, and is shared with
    /// `plurxd::dvpipe`, because a hand-wrapped copy has now lost bytes twice:
    /// once on the first attempt here, and once again when it was transcribed
    /// into the second module. Both times the length assertion below caught it
    /// and the failure read as "invalid mapping_idc", which is a long way from
    /// "you dropped three bytes".
    const REAL_P7_RPU: &str = include_str!("../../../../tests/playback/dv-p7-rpu.hex");

    fn rpu_bytes() -> Vec<u8> {
        let hex = REAL_P7_RPU.trim();
        assert_eq!(
            hex.len(),
            734,
            "the fixture lost bytes on its way into the test"
        );
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
            .collect()
    }

    fn annex_b(units: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (start_code, nal) in units {
            out.extend_from_slice(start_code);
            out.extend_from_slice(nal);
        }
        out
    }

    /// A real Profile 7 RPU becomes a real Profile 8 one.
    ///
    /// The acceptance the whole milestone rests on: after this, a disc remux's
    /// RPUs describe a single-layer stream, which is what every consumer
    /// Dolby Vision decoder can actually take.
    #[test]
    fn a_profile_7_rpu_converts_to_profile_8_and_stays_parseable() {
        let rpu = rpu_bytes();
        let before = DoviRpu::parse_unspec62_nalu(&rpu).expect("the captured RPU parses");
        assert_eq!(
            before.dovi_profile, 7,
            "the fixture is what it claims to be"
        );

        let stream = annex_b(&[(&[0, 0, 1], &rpu)]);
        let mut out = Vec::new();
        let report = convert_annex_b(&stream, &mut out).expect("convert");

        assert_eq!(report.rpus, 1);
        assert_eq!(report.source_profile, Some(7));

        // The output is still one Annex B unit, and its RPU now says 8.
        let converted_nal = &out[3..];
        let after = DoviRpu::parse_unspec62_nalu(converted_nal).expect("the converted RPU parses");
        assert_eq!(
            after.dovi_profile, 8,
            "profile 8.1 is the whole point of the conversion"
        );
        assert_ne!(out, stream, "something has to have changed");
        assert_eq!(&out[..3], &[0, 0, 1], "the start code is carried through");
    }

    /// Everything that is not an RPU comes out byte for byte.
    ///
    /// This stage sits between two ffmpegs in a copy pipeline. Any byte it
    /// changes outside an RPU is a byte of picture or parameter set it had no
    /// business touching — and it would be invisible, because the stream would
    /// still decode.
    #[test]
    fn every_other_nal_unit_is_passed_through_unchanged() {
        let vps: &[u8] = &[0x40, 0x01, 0x0c, 0x01, 0xff, 0xff];
        let sps: &[u8] = &[0x42, 0x01, 0x01, 0x22, 0x20];
        let slice: &[u8] = &[0x26, 0x01, 0xaf, 0x00, 0x00, 0x01, 0x02];
        let sei: &[u8] = &[0x4e, 0x01, 0x89, 0x04];
        let rpu = rpu_bytes();

        // Four-byte start codes on the parameter sets and three-byte ones
        // elsewhere, which is what ffmpeg emits and what a byte-for-byte
        // pass-through has to preserve.
        let stream = annex_b(&[
            (&[0, 0, 0, 1], vps),
            (&[0, 0, 0, 1], sps),
            (&[0, 0, 1], sei),
            (&[0, 0, 1], &rpu),
            (&[0, 0, 1], slice),
        ]);
        let mut out = Vec::new();
        let report = convert_annex_b(&stream, &mut out).expect("convert");
        assert_eq!(report.rpus, 1);

        // The prefix up to the RPU and the suffix after it are identical.
        let prefix_len = 4 + vps.len() + 4 + sps.len() + 3 + sei.len();
        assert_eq!(&out[..prefix_len], &stream[..prefix_len]);
        let tail: &[u8] = &[0, 0, 1];
        assert!(out.ends_with(slice));
        assert!(out[..out.len() - slice.len()].ends_with(tail));
    }

    /// The length-prefixed form converts the same RPUs the Annex B form does,
    /// and re-frames them without touching the payload.
    ///
    /// This is the form that ships. The two differ only in framing — a length
    /// prefix instead of a start code — so the converted NAL bytes must come
    /// out identical, and any divergence would mean one is doing something to
    /// the payload the other is not.
    #[test]
    fn a_length_prefixed_sample_converts_to_the_same_rpu_the_annex_b_form_does() {
        let rpu = rpu_bytes();
        let slice: &[u8] = &[0x26, 0x01, 0xaf, 0x12];

        let mut annex_b_out = Vec::new();
        let stream = annex_b(&[(&[0, 0, 1], &rpu)]);
        convert_annex_b(&stream, &mut annex_b_out).expect("convert");
        let annex_b_rpu = &annex_b_out[3..];

        let mut sample = Vec::new();
        sample.extend_from_slice(&(slice.len() as u32).to_be_bytes());
        sample.extend_from_slice(slice);
        sample.extend_from_slice(&(rpu.len() as u32).to_be_bytes());
        sample.extend_from_slice(&rpu);

        let mut out = Vec::new();
        let report = convert_length_prefixed(&sample, 4, &mut out).expect("convert");
        assert_eq!(report.rpus, 1);
        assert_eq!(report.source_profile, Some(7));
        assert_eq!(report.enhancement_layer, EnhancementLayer::Full);

        // The slice is byte for byte, prefix included.
        assert_eq!(&out[..4 + slice.len()], &sample[..4 + slice.len()]);

        // …and the RPU is the same one, re-framed.
        let at = 4 + slice.len();
        let length = u32::from_be_bytes(out[at..at + 4].try_into().expect("prefix")) as usize;
        assert_eq!(&out[at + 4..at + 4 + length], annex_b_rpu);
        assert!(
            length < rpu.len(),
            "the conversion shortens the RPU, which is why the caller has size \
             fixups to do"
        );
        assert_eq!(out.len(), at + 4 + length);
    }

    /// Every legal `hvcC` prefix width, and one that is not.
    #[test]
    fn every_legal_nal_length_width_is_read_and_an_illegal_one_is_refused() {
        let rpu = rpu_bytes();
        for width in [1usize, 2, 4] {
            // A one-byte prefix cannot express this RPU's length, so that case
            // exercises the framing with a short non-RPU unit instead.
            let unit: &[u8] = if width == 1 {
                &[0x26, 0x01, 0xaf]
            } else {
                &rpu
            };
            let mut sample = Vec::new();
            for shift in (0..width).rev() {
                sample.push(((unit.len() >> (8 * shift)) & 0xff) as u8);
            }
            sample.extend_from_slice(unit);

            let mut out = Vec::new();
            let report = convert_length_prefixed(&sample, width as u8, &mut out)
                .unwrap_or_else(|error| panic!("width {width}: {error}"));
            assert_eq!(report.rpus, u64::from(width != 1));
        }

        let mut out = Vec::new();
        assert!(convert_length_prefixed(&[0, 0, 0], 3, &mut out).is_err());
    }

    /// A sample whose framing does not add up is refused, not guessed at.
    ///
    /// A declared length running past the sample means the caller handed over
    /// the wrong bytes — a mis-computed `trun` offset, most likely — and
    /// converting whatever happens to be there would write a rewritten RPU
    /// into the middle of a picture.
    #[test]
    fn a_sample_whose_lengths_do_not_add_up_is_refused() {
        let mut out = Vec::new();

        let mut overrun = 999u32.to_be_bytes().to_vec();
        overrun.extend_from_slice(&[0x26, 0x01]);
        let error = convert_length_prefixed(&overrun, 4, &mut out).expect_err("must refuse");
        assert!(error.to_string().contains("does not carry"), "{error}");
        assert!(out.is_empty(), "a refusal leaves nothing to forward");

        assert!(convert_length_prefixed(&[0, 0], 4, &mut out).is_err());
        assert!(out.is_empty());
    }

    /// A stream with no RPUs converts to itself, and says so.
    ///
    /// Not an error: whether a source that was supposed to be Profile 7 having
    /// no RPUs is a refusal is the caller's judgement, made with the file's
    /// stored facts, which this function does not have.
    #[test]
    fn a_stream_with_no_rpus_is_returned_verbatim() {
        let slice: &[u8] = &[0x26, 0x01, 0xaf, 0x00];
        let stream = annex_b(&[(&[0, 0, 0, 1], &[0x40, 0x01, 0x0c]), (&[0, 0, 1], slice)]);
        let mut out = Vec::new();
        let report = convert_annex_b(&stream, &mut out).expect("convert");
        assert_eq!(report.rpus, 0);
        assert_eq!(report.source_profile, None);
        assert_eq!(report.enhancement_layer, EnhancementLayer::None);
        assert_eq!(out, stream, "not one byte moves");
    }

    /// Bytes before the first start code are still bytes.
    ///
    /// A pipe read can start anywhere. Dropping a leading fragment would
    /// corrupt the stream silently, which is the failure mode this whole
    /// module has to avoid.
    #[test]
    fn bytes_outside_any_start_code_are_not_dropped() {
        let leading: &[u8] = &[0xde, 0xad, 0xbe, 0xef];
        let mut stream = leading.to_vec();
        stream.extend_from_slice(&annex_b(&[(&[0, 0, 1], &[0x26, 0x01, 0xaf])]));
        let mut out = Vec::new();
        convert_annex_b(&stream, &mut out).expect("convert");
        assert_eq!(out, stream);

        // …and an empty input is empty, not a panic.
        let mut empty = Vec::new();
        let report = convert_annex_b(&[], &mut empty).expect("convert");
        assert_eq!(report.rpus, 0);
        assert!(empty.is_empty());
    }

    /// An RPU the library cannot read stops the conversion rather than being
    /// passed through.
    ///
    /// Passing it through would ship a Profile 7 RPU inside a stream that
    /// every other RPU says is Profile 8.1 — a mixed stream, which is worse
    /// than either answer, and which no decoder is required to survive.
    #[test]
    fn an_unreadable_rpu_refuses_rather_than_passing_through() {
        let mut broken = rpu_bytes();
        // Keep the NAL header so it is still routed as an RPU, and corrupt the
        // payload so parsing has to fail.
        for byte in broken.iter_mut().skip(2) {
            *byte = 0xff;
        }
        let stream = annex_b(&[(&[0, 0, 1], &broken)]);
        let mut out = Vec::new();
        let error = convert_annex_b(&stream, &mut out).expect_err("must refuse");
        assert!(
            matches!(error, DvConvertError::Unreadable { frame: 0, .. }),
            "{error}"
        );
    }

    /// A refusal leaves nothing behind for a caller to forward by accident.
    ///
    /// Refusals happen partway through by construction: the bad RPU is only
    /// discovered after its predecessors have been written. A caller that
    /// forwarded `out` on its error path — a cleanup that flushes what it has,
    /// say — would publish a stream truncated mid-picture, which a segmenter
    /// indexes without complaint and a player stalls on with nothing to say
    /// why.
    #[test]
    fn a_refusal_leaves_the_output_buffer_empty() {
        let good: &[u8] = &[0x26, 0x01, 0xaf, 0x12, 0x34, 0x56, 0x78];
        let mut broken = rpu_bytes();
        for byte in broken.iter_mut().skip(2) {
            *byte = 0xff;
        }

        // Enough good bytes before the failure that a partial write would be
        // both non-empty and plausible-looking.
        let stream = annex_b(&[
            (&[0, 0, 0, 1], good),
            (&[0, 0, 1], &rpu_bytes()),
            (&[0, 0, 1], &broken),
        ]);
        let mut out = vec![0xab; 64];
        convert_annex_b(&stream, &mut out).expect_err("must refuse");
        assert!(
            out.is_empty(),
            "a refused conversion published {} bytes",
            out.len()
        );

        // The same for the profile refusal, which is the one a real
        // partially-converted remux would hit.
        let p81 = DoviRpu::profile81_config(&GenerateConfig::default())
            .expect("generate")
            .write_hevc_unspec62_nalu()
            .expect("write");
        let mixed = annex_b(&[(&[0, 0, 1], &rpu_bytes()), (&[0, 0, 1], &p81)]);
        let mut out = Vec::new();
        convert_annex_b(&mixed, &mut out).expect_err("a spliced stream is refused");
        assert!(out.is_empty());
    }

    /// A refusal names the byte it happened at, not only the RPU ordinal.
    ///
    /// An ordinal cannot be turned back into bytes on disk. The offset is what
    /// makes a refusal reproducible with `dd`, which is the difference between
    /// a diagnosable failure and a mystery.
    #[test]
    fn a_refusal_carries_the_byte_offset_of_the_offending_unit() {
        let mut broken = rpu_bytes();
        for byte in broken.iter_mut().skip(2) {
            *byte = 0xff;
        }
        let filler: &[u8] = &[0x26, 0x01, 0xaf, 0x12];
        let stream = annex_b(&[(&[0, 0, 0, 1], filler), (&[0, 0, 1], &broken)]);
        let expected = 4 + filler.len();

        let mut out = Vec::new();
        let error = convert_annex_b(&stream, &mut out).expect_err("must refuse");
        let DvConvertError::Unreadable { offset, .. } = error else {
            panic!("wrong variant: {error}");
        };
        assert_eq!(offset, expected, "the offset points at the start code");
        assert_eq!(
            &stream[offset..offset + 3],
            &[0, 0, 1],
            "…and that is where the unit really begins"
        );
    }

    /// Profile 7 is the only accepted input, and both refusals matter.
    ///
    /// `To81` converts whatever it is handed and never fails loudly, so each
    /// refusal here is the only thing standing between a mis-routed source and
    /// a stream that decodes to the wrong colours while reporting success:
    ///
    /// - **Profile 5** goes through `p5_to_p81` and comes out labelled 8.1
    ///   over an IPT-PQ-C2 base layer.
    /// - **Profile 8** comes out byte-identical — which is fine for 8.1 and
    ///   catastrophic for 8.4, and the RPU carries nothing that separates
    ///   them. That fact is demonstrated below rather than asserted.
    #[test]
    fn every_profile_but_7_is_refused_because_only_7_can_be_told_apart() {
        let p5 = DoviRpu::profile5_config(&GenerateConfig::default()).expect("generate");
        assert_eq!(p5.dovi_profile, 5);
        let p5 = p5.write_hevc_unspec62_nalu().expect("write");

        // The premise for Profile 5: without a guard this converts, and says
        // it succeeded.
        let mut unguarded = DoviRpu::parse_unspec62_nalu(&p5).expect("parses");
        unguarded
            .convert_with_mode(ConversionMode::To81)
            .expect("the library converts P5 without complaint — that is the danger");
        assert_eq!(unguarded.dovi_profile, 8);

        // …and for Profile 8: an 8.1 RPU and an 8.4 RPU both parse as profile
        // 8, and neither carries the compatibility id that tells them apart.
        // 8.4's base layer is HLG. Converting it would produce a `dvvC` that
        // says HDR10 over a stream that is not.
        let p84_config = GenerateConfig {
            profile: GenerateProfile::Profile84,
            ..GenerateConfig::default()
        };
        let p84 = DoviRpu::profile84_config(&p84_config).expect("generate");
        assert_eq!(
            p84.dovi_profile, 8,
            "8.4 is indistinguishable from 8.1 at the RPU"
        );
        let p84 = p84.write_hevc_unspec62_nalu().expect("write");

        let p81 = DoviRpu::profile81_config(&GenerateConfig::default()).expect("generate");
        let p81 = p81.write_hevc_unspec62_nalu().expect("write");

        for (nal, profile, why) in [
            (&p5, 5u8, "an IPT base layer is not the HDR10 one 8.1 means"),
            (&p84, 8u8, "8.4 is an HLG base wearing profile 8's number"),
            (
                &p81,
                8u8,
                "and 8.1 needs no conversion, so refusing costs it nothing",
            ),
        ] {
            let stream = annex_b(&[(&[0, 0, 1], nal)]);
            let mut out = Vec::new();
            let error = convert_annex_b(&stream, &mut out).expect_err(why);
            let DvConvertError::UnsupportedProfile {
                frame: 0,
                profile: got,
                ..
            } = error
            else {
                panic!("wrong variant for {why}: {error}");
            };
            assert_eq!(got, profile, "{why}");
        }
    }

    /// The report describes the *source*, so the first RPU wins.
    ///
    /// A stream whose RPUs disagree is real — a remux spliced from two sources
    /// — and the badge has to name what the title was, not what its last frame
    /// happened to be. Overwriting on every RPU would make the badge depend on
    /// where the stream ended.
    ///
    /// The known limit, recorded here rather than in a comment nobody reads:
    /// a stream that opens MEL and continues FEL is badged near-lossless while
    /// real residual detail is discarded. Refusing a mixed stream outright was
    /// the alternative; first-wins was chosen because a splice is a property of
    /// the remux, not a fault, and refusing would take a playable title away
    /// to avoid an imprecise badge on a rare one.
    #[test]
    fn the_reported_enhancement_layer_is_the_first_rpus_and_not_the_last() {
        let fel = rpu_bytes();
        let mut converted = DoviRpu::parse_unspec62_nalu(&fel).expect("fixture parses");
        converted
            .convert_with_mode(ConversionMode::ToMel)
            .expect("the library rewrites a FEL RPU's layer as minimum");
        assert_eq!(converted.dovi_profile, 7, "still profile 7, now MEL");
        assert_eq!(converted.el_type, Some(DoviELType::MEL));
        let mel = converted.write_hevc_unspec62_nalu().expect("write");

        let stream = annex_b(&[(&[0, 0, 1], &fel), (&[0, 0, 1], &mel)]);
        let mut out = Vec::new();
        let report = convert_annex_b(&stream, &mut out).expect("convert");

        assert_eq!(report.rpus, 2, "both units are rewritten");
        assert_eq!(report.source_profile, Some(7));
        assert_eq!(
            report.enhancement_layer,
            EnhancementLayer::Full,
            "the first RPU declared FEL, so the badge says detail was lost"
        );

        // …and the other way round, so this pins first-wins rather than
        // FEL-wins.
        let reversed = annex_b(&[(&[0, 0, 1], &mel), (&[0, 0, 1], &fel)]);
        let mut out = Vec::new();
        let report = convert_annex_b(&reversed, &mut out).expect("convert");
        assert_eq!(report.enhancement_layer, EnhancementLayer::Minimum);
    }

    /// A rewritten unit keeps the width of the start code it arrived with.
    ///
    /// The pass-through path copies the caller's bytes, so it cannot get this
    /// wrong; the RPU path re-emits the start code from the recorded width,
    /// and a four-byte code narrowed to three drops a zero byte out of the
    /// stream. ffmpeg emits four-byte codes at parameter sets, so a stream
    /// that ever puts an RPU behind one would decode short by a byte.
    #[test]
    fn a_rewritten_rpu_keeps_a_four_byte_start_code_four_bytes_wide() {
        let stream = annex_b(&[(&[0, 0, 0, 1], &rpu_bytes())]);
        let mut out = Vec::new();
        let report = convert_annex_b(&stream, &mut out).expect("convert");

        assert_eq!(report.rpus, 1);
        assert_eq!(
            &out[..4],
            &[0, 0, 0, 1],
            "the four-byte start code survives the rewrite"
        );
        assert_eq!(
            nal_type(&out[4..]),
            Some(RPU_NAL_TYPE),
            "and the unit behind it is still the RPU"
        );
    }

    /// The output buffer is replaced, never appended to.
    ///
    /// A streaming caller reuses one buffer across reads. If a call kept what
    /// the buffer already held, every chunk after the first would carry the
    /// previous chunk's bytes into the muxer — a stream that grows
    /// quadratically and decodes as garbage.
    #[test]
    fn a_reused_output_buffer_does_not_carry_the_previous_conversion() {
        let stream = annex_b(&[(&[0, 0, 1], &rpu_bytes())]);

        let mut fresh = Vec::new();
        convert_annex_b(&stream, &mut fresh).expect("convert");

        let mut reused = vec![0xab; 4096];
        convert_annex_b(&stream, &mut reused).expect("convert");

        assert_eq!(reused, fresh, "the second call starts from nothing");
    }

    /// The cut a streaming caller makes, so no unit is ever handed over in
    /// halves.
    ///
    /// This is what bounds the tail a session carries between reads to one NAL
    /// unit. The alternative — buffering until the stream ends — is ~72 GB for
    /// a two-hour 4K remux, which is not a thing that fits anywhere.
    #[test]
    fn a_partial_read_is_cut_at_the_last_whole_unit() {
        let first: &[u8] = &[0x26, 0x01, 0xaf];
        let second: &[u8] = &[0x40, 0x01, 0x0c];
        let stream = annex_b(&[(&[0, 0, 1], first), (&[0, 0, 0, 1], second)]);

        // The last unit may still be growing, so the cut is in front of it.
        let cut = whole_units_prefix(&stream);
        assert_eq!(cut, 3 + first.len(), "everything before the final unit");
        assert_eq!(&stream[cut..cut + 4], &[0, 0, 0, 1], "…at a start code");

        // One unit, or none, is never safe to convert yet — including when
        // that one unit is preceded by a carried fragment, which is the shape
        // every read after the first has. Cutting in front of a lone start
        // code would hand `convert_annex_b` a NAL that is still growing: a
        // good RPU refused as unreadable, or a truncated unit emitted.
        assert_eq!(whole_units_prefix(&annex_b(&[(&[0, 0, 1], first)])), 0);
        let mut carried = vec![0xde, 0xad, 0xbe, 0xef];
        carried.extend_from_slice(&annex_b(&[(&[0, 0, 1], first)]));
        assert_eq!(
            whole_units_prefix(&carried),
            0,
            "a lone start code is a unit that may still be growing, wherever it sits"
        );
        assert_eq!(whole_units_prefix(&[]), 0);
        assert_eq!(whole_units_prefix(&[0, 0]), 0);

        // And the carried tail plus the next read reconstructs the stream.
        let (head, tail) = stream.split_at(cut);
        let mut converted = Vec::new();
        convert_annex_b(head, &mut converted).expect("convert");
        let mut rest = Vec::new();
        convert_annex_b(tail, &mut rest).expect("convert");
        converted.extend_from_slice(&rest);
        assert_eq!(converted, stream, "nothing is lost at the seam");
    }

    /// The enhancement-layer type is read and reported, because it is what the
    /// conversion costs the viewer.
    #[test]
    fn the_enhancement_layer_type_is_reported_for_the_badge() {
        let stream = annex_b(&[(&[0, 0, 1], &rpu_bytes())]);
        let mut out = Vec::new();
        let report = convert_annex_b(&stream, &mut out).expect("convert");

        // Which one, not merely that one was named. The badge tells a viewer
        // whether the conversion cost them picture detail, so getting MEL and
        // FEL the wrong way round is worse than saying nothing: a FEL source
        // would be called near-lossless while its residual detail is dropped.
        // The fixture is a real disc remux and the library reads it as FEL.
        assert_eq!(
            report.enhancement_layer,
            EnhancementLayer::Full,
            "the captured Nosferatu RPU declares a full enhancement layer"
        );
        assert!(
            report.enhancement_layer.reason().contains("is lost"),
            "a FEL source must be told detail was lost: {}",
            report.enhancement_layer.reason()
        );
        assert!(
            EnhancementLayer::Minimum.reason().contains("near-lossless"),
            "…and a MEL source must not be"
        );
    }
}
