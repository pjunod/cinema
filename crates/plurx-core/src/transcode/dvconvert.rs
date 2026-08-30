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
//! ffmpeg#2 writes no Dolby Vision configuration record — it copies the one
//! its input container had, and a raw Annex B stream has no container
//! (measured: `docs/PLAYBACK-CAPS-V2-M0.md` §8). The record is written
//! separately by [`crate::fmp4::set_dolby_vision_record`].
//!
//! Three names for the output get confused with each other, so all three at
//! once. The **sample entry** is `hvc1`, not `dvh1`: Profile 8.1 is a
//! backward-compatible enhancement of HDR10, and Apple's ISOBMFF contract
//! requires a compatible stream to keep the base entry — which is also what
//! [`super::hevc_copy_tag_for_format`] independently answers for any source
//! with a compatible base, so the conversion needs no special case there. The
//! **configuration record** is `dvvC`, the profile ≥ 8 spelling, and not the
//! `dvcC` the Profile 7 source arrived with (plan §4.8). The **playlist**
//! still advertises `dvh1.08.06`, because `SUPPLEMENTAL-CODECS` is where the
//! Dolby Vision profile is declared and it is a codec string, not a box name.
//! Earlier drafts of the plan say `dvh1`/`dvcC` throughout; §4.8 is the
//! correction, and this is what the code does.

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
/// Every variant carries the byte offset of the offending NAL unit inside the
/// buffer it was handed as well as the RPU ordinal, because the ordinal alone
/// cannot be turned back into bytes on disk and a refusal that cannot be
/// reproduced cannot be diagnosed.
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
    /// [`ConversionMode::To81`] converts whatever it is given. Handed a
    /// Profile 5 RPU it calls `p5_to_p81`, which produces an RPU *labelled*
    /// 8.1 over a base layer that is IPT-PQ-C2, not HDR10 — every frame the
    /// wrong colour, and nothing downstream able to tell, because the label
    /// says the conversion succeeded. Profiles 7 and 8 are the only inputs
    /// whose base layer is already the BT.2020/PQ one 8.1 promises.
    #[error("the Dolby Vision RPU at frame {frame} (byte {offset}) declares profile {profile}; only profiles 7 and 8 have the HDR10 base layer profile 8.1 describes")]
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
/// [`whole_units_prefix`] is that cut, and it is what bounds a streaming
/// caller's memory to one NAL unit rather than to the file (a two-hour 4K
/// remux is ~72 GB, so "buffer the stream" is not an option that exists).
///
/// **A stream with no RPUs converts to itself**, and answers `rpus: 0`. That
/// is not an error: the caller decides whether a source it believed was
/// Profile 7 having no RPUs is a reason to refuse, and it has the file's
/// stored facts to decide with, which this function does not.
pub fn convert_annex_b(input: &[u8], out: &mut Vec<u8>) -> Result<Converted, DvConvertError> {
    let mut report = Converted::default();
    out.clear();
    out.reserve(input.len());

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
        // profile it is handed and none of them fail loudly: a Profile 5 RPU
        // becomes a well-formed 8.1-labelled RPU over an IPT base layer, which
        // is a wrong-colour stream nothing downstream can detect.
        if !matches!(rpu.dovi_profile, 7 | 8) {
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
    use dolby_vision::rpu::generate::GenerateConfig;

    /// One real Profile 7 RPU, captured 2026-08-30 from
    /// `Nosferatu (2024) Remux-2160p.mkv` on nuc4 through the pipe's own first
    /// stage — `-c:v copy -bsf:v hevc_mp4toannexb,filter_units=remove_types=63`.
    ///
    /// A real one rather than a synthetic one because the whole question this
    /// module answers is whether a library's idea of a Profile 7 RPU matches
    /// what a disc remux actually carries.
    /// 367 bytes, split only for the line width — joined back before use, so
    /// a wrapped literal cannot quietly lose a byte the way a hand-wrapped one
    /// did on the first attempt.
    const REAL_P7_RPU: [&str; 8] = [
        "7c011908090840613650ae20002008020080200802007f801ffc00ffc001fffa0000030100000300d0000008000006",
        "800000400000340000030200000301a000001000000d0000030080000068000004000003034000002000000a800000",
        "800000e18618800000800000800000800000800000800000b0c30c8000008000008000008000008000008000009861",
        "86800000800000800000800000800000540000040000040000070c30c4000004000004000004000004000004000005",
        "861864000004000004000004000004000004000004c30c340000040000040000040000040120000100100100000301",
        "00480000400400400000401200001001001000001a2566000035ea2566f9fceb1c256644ca00000301000003000800",
        "00030008000003001c36224301860a5e308e051400000301a63e5affff000003000003000003000060207dce015140",
        "300800410999810100000300040281e00f000003000009060fa000320000030000cface62380",
    ];

    fn rpu_bytes() -> Vec<u8> {
        let hex = REAL_P7_RPU.concat();
        assert_eq!(
            hex.len(),
            734,
            "the fixture lost bytes on its way into the literal"
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

    /// A profile this conversion has no correct answer for is refused, not
    /// converted.
    ///
    /// `To81` converts whatever it is handed. Given a Profile 5 RPU it calls
    /// `p5_to_p81` and produces a well-formed RPU *labelled* 8.1 sitting over
    /// an IPT-PQ-C2 base layer — every frame the wrong colour, with no error
    /// anywhere and nothing downstream able to notice. The guard is the only
    /// thing between a mis-routed source and that stream.
    #[test]
    fn an_rpu_from_a_profile_without_an_hdr10_base_is_refused() {
        // A genuine Profile 5 RPU from the library's own generator, so the
        // guard is tested against what a P5 source really carries rather than
        // against a hand-edited header.
        let rpu = DoviRpu::profile5_config(&GenerateConfig::default()).expect("generate");
        assert_eq!(rpu.dovi_profile, 5, "the fixture for this test is a P5 RPU");
        let p5 = rpu.write_hevc_unspec62_nalu().expect("write");

        // The premise: without the guard this converts, and reports success.
        let mut unguarded = DoviRpu::parse_unspec62_nalu(&p5).expect("parses");
        unguarded
            .convert_with_mode(ConversionMode::To81)
            .expect("the library converts P5 without complaint — that is the danger");
        assert_eq!(unguarded.dovi_profile, 8);

        let stream = annex_b(&[(&[0, 0, 1], &p5)]);
        let mut out = Vec::new();
        let error = convert_annex_b(&stream, &mut out).expect_err("must refuse");
        assert!(
            matches!(
                error,
                DvConvertError::UnsupportedProfile {
                    frame: 0,
                    profile: 5,
                    ..
                }
            ),
            "{error}"
        );
    }

    /// The report describes the *source*, so the first RPU wins.
    ///
    /// A stream whose RPUs disagree is real — a remux spliced from two sources
    /// — and the badge has to name what the title was, not what its last frame
    /// happened to be. Overwriting on every RPU would make the badge depend on
    /// where the stream ended.
    #[test]
    fn the_reported_source_profile_is_the_first_rpus_and_not_the_last() {
        let p7 = rpu_bytes();
        let mut already_8 = DoviRpu::parse_unspec62_nalu(&p7).expect("fixture parses");
        already_8
            .convert_with_mode(ConversionMode::To81)
            .expect("convert");
        assert_eq!(already_8.dovi_profile, 8);
        let p8 = already_8.write_hevc_unspec62_nalu().expect("write");

        let stream = annex_b(&[(&[0, 0, 1], &p7), (&[0, 0, 1], &p8)]);
        let mut out = Vec::new();
        let report = convert_annex_b(&stream, &mut out).expect("convert");

        assert_eq!(report.rpus, 2, "both units are rewritten");
        assert_eq!(
            report.source_profile,
            Some(7),
            "the source is what the first RPU declared"
        );
        assert_ne!(
            report.enhancement_layer,
            EnhancementLayer::None,
            "and so is the enhancement layer — the P8 RPU declares none"
        );
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
    /// This is what bounds a converting session's memory to one NAL unit. The
    /// alternative — buffering the stream — is ~72 GB for a two-hour 4K remux,
    /// which is not a thing that fits anywhere.
    #[test]
    fn a_partial_read_is_cut_at_the_last_whole_unit() {
        let first: &[u8] = &[0x26, 0x01, 0xaf];
        let second: &[u8] = &[0x40, 0x01, 0x0c];
        let stream = annex_b(&[(&[0, 0, 1], first), (&[0, 0, 0, 1], second)]);

        // The last unit may still be growing, so the cut is in front of it.
        let cut = whole_units_prefix(&stream);
        assert_eq!(cut, 3 + first.len(), "everything before the final unit");
        assert_eq!(&stream[cut..cut + 4], &[0, 0, 0, 1], "…at a start code");

        // One unit, or none, is never safe to convert yet.
        assert_eq!(whole_units_prefix(&annex_b(&[(&[0, 0, 1], first)])), 0);
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

        // Whatever this disc carries, it must be named — and the reason string
        // has to say what was lost rather than only that something was.
        assert!(
            matches!(
                report.enhancement_layer,
                EnhancementLayer::Minimum | EnhancementLayer::Full
            ),
            "a Profile 7 source declares one or the other: {:?}",
            report.enhancement_layer
        );
        assert!(report
            .enhancement_layer
            .reason()
            .contains("enhancement layer"));
    }
}
