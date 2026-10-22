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
//! **A refusal fails the session, and that is the intended answer.** When a
//! fragment cannot be rewritten — an unconvertible RPU, or a `trun` shape
//! whose sizes cannot be corrected — there is no in-session fallback to take:
//! the playlist already advertises `dvh1.08.06` with the converted stream's
//! cut points, and the unconverted stream has neither. Serving it would put
//! Profile 7 RPUs under a Profile 8.1 label, against an index built for
//! different segment boundaries. So [`Converter::for_init`] refuses before a
//! single fragment publishes, [`Converter::convert`] refuses before anything
//! measures, and the caller ends the generation. The client's next attempt
//! re-decides from the file's facts and reaches the delivery that needs no
//! rewrite; nothing is served in between.

#[cfg(test)]
use plurx_core::fmp4::{Fragment, Init};
pub use plurx_core::vod_generation::conversion::*;

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::fmp4::{FragmentReader, Unit};
    use plurx_core::testfixtures;
    use plurx_core::transcode::dvconvert::DvConvertError;

    /// A real production pipe — video and audio, two runs per fragment — with
    /// real Profile 7 RPUs in every video sample.
    fn dolby_vision_pipe(kind: &str) -> (Init, Vec<Fragment>) {
        testfixtures::require_ffmpeg();
        read(&testfixtures::with_dolby_vision_rpus(&testfixtures::pipe(
            kind,
        )))
    }

    /// The same pipe with no Dolby Vision in it at all.
    fn plain_pipe(kind: &str) -> (Init, Vec<Fragment>) {
        testfixtures::require_ffmpeg();
        read(&testfixtures::pipe(kind))
    }

    fn read(stream: &[u8]) -> (Init, Vec<Fragment>) {
        let mut reader = FragmentReader::new();
        reader.push(stream);
        let mut init = None;
        let mut fragments = Vec::new();
        while let Some(unit) = reader.next_unit().expect("parsing the pipe") {
            match unit {
                Unit::Init(parsed) => init = Some(parsed),
                Unit::Fragment(fragment) => fragments.push(fragment),
                Unit::Trailer => {}
            }
        }
        (init.expect("the pipe carried an init"), fragments)
    }

    /// The conversion runs, and what comes out is Profile 8.
    ///
    /// Stated as "converting twice refuses" rather than by reading bytes
    /// because that is the same guard a real second pass would hit: an RPU
    /// this converter has already rewritten is no longer Profile 7, and the
    /// profile check is the only thing standing between a double conversion
    /// and a stream that silently describes itself wrongly.
    #[test]
    fn a_profile_seven_stream_is_rewritten_and_comes_out_profile_eight() {
        let (init, fragments) = dolby_vision_pipe("closed-gop");
        let mut converter = Converter::for_init(&init).expect("a describable init");
        let mut first = fragments[0].clone();
        let before = first.bytes.clone();

        assert!(converter.convert(&mut first).expect("converts"));
        assert_ne!(first.bytes, before, "the RPUs were rewritten in place");
        assert!(
            first.bytes.len() < before.len(),
            "an 8.1 RPU is smaller than the Profile 7 one it replaces, so the \
             fragment shrinks — which is also why a converting session cannot \
             share an index with a plain copy"
        );

        let report = converter.report();
        assert!(report.rpus > 0);
        assert_eq!(report.source_profile, Some(7));

        // Converting the converted fragment refuses, because its RPUs now say 8.
        let refused = converter
            .convert(&mut first)
            .expect_err("already converted");
        assert!(
            refused.to_string().contains('8'),
            "the refusal should name the profile it found: {refused}"
        );
    }

    /// Every fragment of the stream converts, not just the first.
    ///
    /// The tally is the file's, not the fragment's: a session that converted
    /// only its opening fragment would play back as Dolby Vision for two
    /// seconds and as undecodable Profile 7 metadata thereafter, with nothing
    /// in the daemon reporting a problem.
    #[test]
    fn the_whole_stream_converts_and_the_tally_is_the_streams() {
        let (init, fragments) = dolby_vision_pipe("closed-gop");
        assert!(fragments.len() > 2, "a fixture worth walking");
        let mut converter = Converter::for_init(&init).expect("a describable init");
        let mut seen = 0;
        for fragment in &fragments {
            let mut fragment = fragment.clone();
            assert!(converter.convert(&mut fragment).expect("converts"));
            let now = converter.report().rpus;
            assert!(now > seen, "each fragment adds its own RPUs");
            seen = now;
        }
        // One per video sample, since that is what the fixture injected.
        let samples: usize = fragments
            .iter()
            .filter_map(|f| f.track(init.video().expect("video").id))
            .map(|t| t.sample_count())
            .sum();
        assert_eq!(converter.report().rpus, samples as u64);
    }

    /// A stream with no RPUs is not an error at this layer.
    ///
    /// The judgement of whether that is a problem belongs to the caller, which
    /// has the file's stored facts; here it is simply a fragment with nothing
    /// to rewrite, and the bytes must come back untouched.
    #[test]
    fn a_stream_carrying_no_rpus_changes_nothing() {
        let (init, fragments) = plain_pipe("closed-gop");
        let mut converter = Converter::for_init(&init).expect("a describable init");
        let mut fragment = fragments[0].clone();
        let before = fragment.bytes.clone();
        assert!(!converter
            .convert(&mut fragment)
            .expect("no RPUs is not an error"));
        assert_eq!(fragment.bytes, before);
        assert_eq!(converter.report().rpus, 0);
        assert_eq!(converter.report().source_profile, None);
    }

    /// An init the converter cannot describe is refused before any fragment.
    ///
    /// The width is not a formality. `hvcC` spells it as `lengthSizeMinusOne`,
    /// so 3 is a value the field can hold and no container ever carries; a
    /// converter that accepted it would read two bytes of picture as the top
    /// half of a NAL length, walk into the middle of a frame, and rewrite
    /// whatever it found there. It does not fail — it corrupts.
    #[test]
    fn an_init_whose_nal_width_is_not_an_hvcc_value_is_refused() {
        let (init, _) = plain_pipe("closed-gop");
        for width in [0u8, 3, 5, 8] {
            let mut broken = init.clone();
            for track in &mut broken.tracks {
                if track.kind == plurx_core::fmp4::TrackKind::Video {
                    track.nal_length_size = width;
                }
            }
            let refused = Converter::for_init(&broken)
                .expect_err("a {width}-byte prefix is not an hvcC value");
            assert!(
                refused.to_string().contains("NAL length prefix"),
                "{refused}"
            );
        }
        // …and the widths a real container carries are all accepted.
        for width in [1u8, 2, 4] {
            let mut ok = init.clone();
            for track in &mut ok.tracks {
                if track.kind == plurx_core::fmp4::TrackKind::Video {
                    track.nal_length_size = width;
                }
            }
            assert!(Converter::for_init(&ok).is_ok(), "{width} is an hvcC width");
        }
    }

    #[test]
    fn an_init_with_no_video_track_is_refused() {
        let (init, _) = plain_pipe("closed-gop");
        let mut audio_only = init.clone();
        audio_only
            .tracks
            .retain(|track| track.kind != plurx_core::fmp4::TrackKind::Video);
        let refused = Converter::for_init(&audio_only).expect_err("no video to convert");
        assert!(refused.to_string().contains("no video track"), "{refused}");
    }

    /// A refusal leaves the fragment exactly as it was.
    ///
    /// `rewrite_video_samples` collects every edit before applying any so that
    /// a sample failing halfway through cannot leave a fragment with some of
    /// its RPUs rewritten and the rest not — a stream that would parse, play,
    /// and be wrong. The tally must not move either: a partly-converted
    /// fragment that reported its RPUs would let the caller's `rpus == 0`
    /// check pass on a stream that was never converted.
    #[test]
    fn a_refusal_leaves_the_fragment_and_the_tally_untouched() {
        let (init, fragments) = dolby_vision_pipe("closed-gop");
        let video = init.video().expect("video").id;
        let mut converter = Converter::for_init(&init).expect("a describable init");
        let mut fragment = fragments[0].clone();

        // Break the last video sample's RPU, leaving every earlier one intact,
        // so a converter that applied as it went would have written most of
        // the fragment before it found this.
        let track = fragment.track(video).expect("a video track").clone();
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for run in &track.runs {
            let mut at = run.data_offset;
            for sample in &run.samples {
                spans.push((at, sample.size as usize));
                at += sample.size as usize;
            }
        }
        let last = *spans.last().expect("a sample");
        // The injected RPU is the sample's tail. Asserting that before
        // corrupting it is what makes this test's offset arithmetic honest:
        // a stray index would otherwise corrupt picture bytes, the conversion
        // would succeed, and the test would fail for a reason that has nothing
        // to do with what it is asking.
        let rpu = testfixtures::p7_rpu();
        let end = last.0 + last.1;
        assert_eq!(
            &fragment.bytes[end - rpu.len()..end],
            &rpu[..],
            "the fixture appends the RPU to each sample"
        );
        // Corrupt the payload and leave the two-byte NAL header alone, so the
        // unit is still routed as an RPU and has to be refused rather than
        // skipped.
        fragment.bytes[end - rpu.len() + 2..end].fill(0xff);

        let before = fragment.bytes.clone();
        let refused = converter.convert(&mut fragment).expect_err("a broken RPU");
        assert!(!refused.to_string().is_empty());
        assert_eq!(fragment.bytes, before, "no edit is applied on a refusal");
        assert_eq!(
            converter.report().rpus,
            0,
            "and nothing it saw on the way is counted"
        );
    }

    /// The error carries where it happened.
    ///
    /// Not decoration: a refusal reaches an operator as a failed session with
    /// one line of text, and "an RPU on this file will not parse" is
    /// actionable in a way that "conversion failed" is not.
    #[test]
    fn an_unreadable_rpu_names_itself() {
        let error = DvConvertError::Unreadable {
            frame: 3,
            offset: 91,
            detail: "invalid mapping_idc".into(),
        };
        let refused = Refused(error.to_string());
        assert!(
            refused.to_string().contains("invalid mapping_idc"),
            "{refused}"
        );
    }
}
