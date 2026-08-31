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
//! **Where this runs: on the far side of the muxer**, on samples in the shape
//! an fMP4 fragment carries them — each NAL unit behind an `hvcC` length
//! prefix, not an Annex B start code.
//!
//! ```text
//!  one ffmpeg  -c:v copy -bsf:v filter_units=remove_types=32-34|63
//!       │  fragmented MP4: video and audio from one open, one timeline,
//!       │  timestamps copied rather than reconstructed
//!       ▼
//!  crate::fmp4::rewrite_video_samples  ─▶  this module, one sample at a time
//!       │                                  type 62 (RPU) → 8.1, CRC32 redone
//!       │                                  anything else → byte for byte
//!       ▼
//!  the fragment reader, unchanged — the same kind of stream a plain copy
//!  produces, with only the RPUs inside it different
//! ```
//!
//! **It used to sit between two ffmpegs, and that was wrong.** The first
//! emitted a raw Annex B elementary stream for this stage and the second
//! remuxed it — but ffmpeg's raw HEVC demuxer emits every packet with no
//! timestamps at all, and the muxer then fabricates a decode-order grid:
//! `pts == dts` for every sample, presentation reordering erased, every
//! minigop played in coding order. Measured on an ordinary three-B-frame
//! encode: ~410 display-order inversions in 819 frames, on every GOP, seek or
//! no seek, closed-GOP sources included. No seek anchor could have repaired
//! it — an anchor translates a timeline, it does not reorder one. So the
//! Annex B form is gone rather than kept for a caller that might want it: a
//! function whose only correct use is one this project has withdrawn is a
//! trap with tests around it.
//!
//! The **configuration record** the output needs is written separately, by
//! [`crate::fmp4::set_dolby_vision_record`] out of the source file's stored
//! facts. ffmpeg does not derive that record from the RPUs — it copies the one
//! its input container had (measured, `docs/PLAYBACK-CAPS-V2-M0.md` §8) — so a
//! converted stream comes out of the muxer carrying the *source's* Profile 7
//! `dvcC`, describing a dual-layer stream that is no longer there. Replacing
//! it is not optional and not a nicety: a decoder reading it expects an
//! enhancement layer the conversion dropped.
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

/// Rewrite every RPU in a **length-prefixed** sample, the shape a sample has
/// inside an fMP4 fragment.
///
/// This is the only form there is. An Annex B form existed for the pipeline
/// the module header describes and withdraws; it is gone, along with the
/// buffer-cutting helper a streaming caller of it needed.
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
        // Refuse before converting, not after. `To81` has an answer for every
        // profile it is handed and none of them fail loudly; the error type's
        // own documentation says what each wrong answer looks like on screen.
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

/// The HEVC NAL type, from the two-byte header.
fn nal_type(nal: &[u8]) -> Option<u8> {
    (nal.len() >= 2).then(|| (nal[0] >> 1) & 0x3f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dolby_vision::rpu::generate::{GenerateConfig, GenerateProfile};

    fn rpu_bytes() -> Vec<u8> {
        crate::testfixtures::p7_rpu()
    }

    /// One sample in the shape an fMP4 fragment carries it: each NAL unit
    /// behind a big-endian length prefix of the `hvcC` width.
    fn sample(units: &[&[u8]], width: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for unit in units {
            for shift in (0..width).rev() {
                out.push(((unit.len() >> (8 * shift)) & 0xff) as u8);
            }
            out.extend_from_slice(unit);
        }
        out
    }

    /// Walk a converted sample back into its NAL units, so a test can say
    /// what came out rather than where it sat.
    fn units_of(sample: &[u8], width: usize) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut at = 0usize;
        while at + width <= sample.len() {
            let length = sample[at..at + width]
                .iter()
                .fold(0usize, |acc, byte| (acc << 8) | usize::from(*byte));
            let body = at + width;
            assert!(body + length <= sample.len(), "a unit runs past the sample");
            out.push(sample[body..body + length].to_vec());
            at = body + length;
        }
        assert_eq!(at, sample.len(), "the sample ends on a unit boundary");
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

        let input = sample(&[&rpu], 4);
        let mut out = Vec::new();
        let report = convert_length_prefixed(&input, 4, &mut out).expect("convert");

        assert_eq!(report.rpus, 1);
        assert_eq!(report.source_profile, Some(7));

        let units = units_of(&out, 4);
        assert_eq!(units.len(), 1, "one unit in, one unit out");
        let after = DoviRpu::parse_unspec62_nalu(&units[0]).expect("the converted RPU parses");
        assert_eq!(
            after.dovi_profile, 8,
            "profile 8.1 is the whole point of the conversion"
        );
        assert_ne!(out, input, "something has to have changed");
        assert!(
            units[0].len() < rpu.len(),
            "the conversion shortens the RPU, which is why the caller has size \
             fixups to do"
        );
    }

    /// Everything that is not an RPU comes out byte for byte.
    ///
    /// Any byte this stage changes outside an RPU is a byte of picture or
    /// parameter set it had no business touching — and it would be invisible,
    /// because the stream would still decode.
    #[test]
    fn every_other_nal_unit_is_passed_through_unchanged() {
        let vps: &[u8] = &[0x40, 0x01, 0x0c, 0x01, 0xff, 0xff];
        let sps: &[u8] = &[0x42, 0x01, 0x01, 0x22, 0x20];
        let slice: &[u8] = &[0x26, 0x01, 0xaf, 0x00, 0x00, 0x01, 0x02];
        let rpu = rpu_bytes();

        let input = sample(&[vps, sps, &rpu, slice], 4);
        let mut out = Vec::new();
        let report = convert_length_prefixed(&input, 4, &mut out).expect("convert");
        assert_eq!(report.rpus, 1);

        let units = units_of(&out, 4);
        assert_eq!(units.len(), 4);
        assert_eq!(units[0], vps);
        assert_eq!(units[1], sps);
        assert_ne!(units[2], rpu, "the RPU is the one unit that changes");
        assert_eq!(units[3], slice, "including a slice full of zero bytes");

        // The prefixes of the untouched units are byte for byte too, so the
        // pass-through really is a copy and not a re-framing that happens to
        // agree.
        let head = 4 + vps.len() + 4 + sps.len();
        assert_eq!(&out[..head], &input[..head]);
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
            let input = sample(&[unit], width);
            let mut out = Vec::new();
            let report = convert_length_prefixed(&input, width as u8, &mut out)
                .unwrap_or_else(|error| panic!("width {width}: {error}"));
            assert_eq!(report.rpus, u64::from(width != 1));
        }

        // `hvcC` spells the width as `lengthSizeMinusOne`, so 3 is a value the
        // field can hold and no container ever carries. Reading a sample with
        // it would take a byte of picture as the top of a NAL length and
        // rewrite whatever it found in the middle of a frame.
        let mut out = Vec::new();
        for width in [0u8, 3, 5, 8] {
            let error = convert_length_prefixed(&[0, 0, 0], width, &mut out)
                .expect_err("not an hvcC width");
            assert!(error.to_string().contains("legal hvcC value"), "{error}");
        }
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

        // A prefix that is itself cut short, which is the other half of the
        // same mistake.
        let error = convert_length_prefixed(&[0, 0], 4, &mut out).expect_err("must refuse");
        assert!(error.to_string().contains("past the end"), "{error}");
        assert!(out.is_empty());

        // A trailing byte after a whole unit is the same thing: the sample
        // does not end on a boundary, so the caller's framing is wrong.
        let mut trailing = sample(&[&[0x26, 0x01, 0xaf]], 4);
        trailing.push(0xff);
        assert!(convert_length_prefixed(&trailing, 4, &mut out).is_err());
        assert!(out.is_empty());
    }

    /// A sample with no RPUs converts to itself, and says so.
    ///
    /// Not an error: whether a source that was supposed to be Profile 7 having
    /// no RPUs is a refusal is the caller's judgement, made with the file's
    /// stored facts, which this function does not have.
    #[test]
    fn a_sample_with_no_rpus_is_returned_verbatim() {
        let input = sample(&[&[0x40, 0x01, 0x0c], &[0x26, 0x01, 0xaf, 0x00]], 4);
        let mut out = Vec::new();
        let report = convert_length_prefixed(&input, 4, &mut out).expect("convert");
        assert_eq!(report.rpus, 0);
        assert_eq!(report.source_profile, None);
        assert_eq!(report.enhancement_layer, EnhancementLayer::None);
        assert_eq!(out, input, "not one byte moves");

        // …and an empty sample is empty, not a panic. ffmpeg does not write
        // zero-length samples, but `rewrite_video_samples` hands over whatever
        // the `trun` declared and a zero there must not be a crash.
        let mut empty = Vec::new();
        let report = convert_length_prefixed(&[], 4, &mut empty).expect("convert");
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
        let input = sample(&[&broken], 4);
        let mut out = Vec::new();
        let error = convert_length_prefixed(&input, 4, &mut out).expect_err("must refuse");
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
    /// say — would publish a sample truncated mid-picture, which the size
    /// fixups downstream would then declare as if it were whole.
    #[test]
    fn a_refusal_leaves_the_output_buffer_empty() {
        let good: &[u8] = &[0x26, 0x01, 0xaf, 0x12, 0x34, 0x56, 0x78];
        let mut broken = rpu_bytes();
        for byte in broken.iter_mut().skip(2) {
            *byte = 0xff;
        }

        // Enough good bytes before the failure that a partial write would be
        // both non-empty and plausible-looking.
        let input = sample(&[good, &rpu_bytes(), &broken], 4);
        let mut out = vec![0xab; 64];
        convert_length_prefixed(&input, 4, &mut out).expect_err("must refuse");
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
        let mixed = sample(&[&rpu_bytes(), &p81], 4);
        let mut out = Vec::new();
        convert_length_prefixed(&mixed, 4, &mut out).expect_err("a spliced stream is refused");
        assert!(out.is_empty());
    }

    /// A refusal names the byte it happened at, not only the RPU ordinal.
    ///
    /// An ordinal cannot be turned back into bytes on disk. The offset is what
    /// makes a refusal reproducible, which is the difference between a
    /// diagnosable failure and a mystery.
    #[test]
    fn a_refusal_carries_the_byte_offset_of_the_offending_unit() {
        let mut broken = rpu_bytes();
        for byte in broken.iter_mut().skip(2) {
            *byte = 0xff;
        }
        let filler: &[u8] = &[0x26, 0x01, 0xaf, 0x12];
        let input = sample(&[filler, &broken], 4);
        let expected = 4 + filler.len();

        let mut out = Vec::new();
        let error = convert_length_prefixed(&input, 4, &mut out).expect_err("must refuse");
        let DvConvertError::Unreadable { offset, .. } = error else {
            panic!("wrong variant: {error}");
        };
        assert_eq!(offset, expected, "the offset points at the length prefix");
        assert_eq!(
            &input[offset..offset + 4],
            &(broken.len() as u32).to_be_bytes(),
            "…and that is where the offending unit's prefix really is"
        );
    }

    /// Only Profile 7 is accepted, and the reason is that nothing else can be
    /// told apart from something it must not be confused with.
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
            let input = sample(&[nal], 4);
            let mut out = Vec::new();
            let error = convert_length_prefixed(&input, 4, &mut out).expect_err(why);
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

        let input = sample(&[&fel, &mel], 4);
        let mut out = Vec::new();
        let report = convert_length_prefixed(&input, 4, &mut out).expect("convert");

        assert_eq!(report.rpus, 2, "both units are rewritten");
        assert_eq!(report.source_profile, Some(7));
        assert_eq!(
            report.enhancement_layer,
            EnhancementLayer::Full,
            "the first RPU declared FEL, so the badge says detail was lost"
        );

        // …and the other way round, so this pins first-wins rather than
        // FEL-wins.
        let reversed = sample(&[&mel, &fel], 4);
        let mut out = Vec::new();
        let report = convert_length_prefixed(&reversed, 4, &mut out).expect("convert");
        assert_eq!(report.enhancement_layer, EnhancementLayer::Minimum);
    }

    /// A rewritten unit's prefix keeps the width it arrived with and states
    /// the new length.
    ///
    /// The pass-through path copies the caller's bytes, so it cannot get this
    /// wrong; the RPU path writes a fresh prefix, and the RPU shrinks. A
    /// prefix written at the wrong width, or left declaring the old length,
    /// puts every following unit in the sample at the wrong offset — and the
    /// sample still parses, because the bytes are all still there.
    #[test]
    fn a_rewritten_rpu_gets_a_prefix_of_the_declared_width_and_the_new_length() {
        let tail: &[u8] = &[0x26, 0x01, 0xaf, 0x12];
        for width in [2usize, 4] {
            let input = sample(&[&rpu_bytes(), tail], width);
            let mut out = Vec::new();
            let report = convert_length_prefixed(&input, width as u8, &mut out).expect("convert");
            assert_eq!(report.rpus, 1);

            let declared = out[..width]
                .iter()
                .fold(0usize, |acc, byte| (acc << 8) | usize::from(*byte));
            let units = units_of(&out, width);
            assert_eq!(units.len(), 2, "width {width}: the sample still frames");
            assert_eq!(declared, units[0].len(), "width {width}");
            assert!(declared < rpu_bytes().len(), "width {width}: it shrank");
            assert_eq!(units[1], tail, "width {width}: and the unit after it is intact");
        }
    }

    /// The output buffer is replaced, never appended to.
    ///
    /// A caller reuses one buffer across every sample of every fragment. If a
    /// call kept what the buffer already held, every sample after the first
    /// would carry the previous one's bytes — samples that grow without bound
    /// and decode as garbage.
    #[test]
    fn a_reused_output_buffer_does_not_carry_the_previous_conversion() {
        let input = sample(&[&rpu_bytes()], 4);

        let mut fresh = Vec::new();
        convert_length_prefixed(&input, 4, &mut fresh).expect("convert");

        let mut reused = vec![0xab; 4096];
        convert_length_prefixed(&input, 4, &mut reused).expect("convert");

        assert_eq!(reused, fresh, "the second call starts from nothing");
    }

    /// The enhancement-layer type is read and reported, because it is what the
    /// conversion costs the viewer.
    #[test]
    fn the_enhancement_layer_type_is_reported_for_the_badge() {
        let input = sample(&[&rpu_bytes()], 4);
        let mut out = Vec::new();
        let report = convert_length_prefixed(&input, 4, &mut out).expect("convert");

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
