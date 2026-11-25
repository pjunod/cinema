use super::super::{
    classify, hevc_sample_cbs_annex_b_len, length_prefixed_nals, rewrite_video_samples,
    vcl_nal_type, Fragment, FragmentReader, Hdr10IndexMetadata, Init, PromotionInputs, Unit,
};
use super::TrimmedFragment;
use crate::segplan::{
    packet_clock::{associate_fragment_clock, SourcePacketObservation},
    ranges::SourceTimestamp,
    IndexRow,
};
use crate::testfixtures::{self, trimmed};

fn read(stream: &[u8]) -> (Init, Vec<Fragment>) {
    let mut reader = FragmentReader::new();
    reader.push(stream);
    let mut init = None;
    let mut fragments = Vec::new();
    while let Some(unit) = reader.next_unit().expect("the fixture parses") {
        match unit {
            Unit::Init(parsed) => init = Some(parsed),
            Unit::Fragment(fragment) => fragments.push(fragment),
            Unit::Trailer => {}
        }
    }
    (init.expect("an init"), fragments)
}

/// A prefix SEI carrying mastering-display (137) and content-light (144).
fn hdr10_sei() -> Vec<u8> {
    let mut nal = vec![39 << 1, 1, 137, 24];
    nal.extend((1..=24).map(|byte| byte as u8));
    nal.extend_from_slice(&[144, 4, 0x03, 0xe8, 0x01, 0x90, 0x80]);
    nal
}

/// The stream with `nal` put in front of every video sample.
fn with_leading_nal(stream: &[u8], nal: &[u8]) -> Vec<u8> {
    let (init, fragments) = read(stream);
    let video = init.video().expect("video");
    let width = usize::from(video.nal_length_size);
    let mut out = init.bytes.clone();
    for mut fragment in fragments {
        rewrite_video_samples(&mut fragment, &init.tracks, video.id, |sample| {
            let mut next = (nal.len() as u32).to_be_bytes()[4 - width..].to_vec();
            next.extend_from_slice(nal);
            next.extend_from_slice(sample);
            Ok(next)
        })
        .expect("a leading NAL unit");
        out.extend_from_slice(&fragment.bytes);
    }
    out
}

/// The video-only copy of each kind, plain and as a Dolby Vision HDR10 title:
/// an HDR10 prefix SEI before, and a Profile 7 RPU after, every sample's
/// slices.
fn canonical_fixtures() -> Vec<(String, Init, Vec<Fragment>)> {
    let mut fixtures = Vec::new();
    for kind in ["closed-gop", "open-gop", "clean-cra", "h264"] {
        let plain = testfixtures::video_only_pipe(kind);
        if kind != "h264" {
            let dolby =
                testfixtures::with_dolby_vision_rpus(&with_leading_nal(&plain, &hdr10_sei()));
            let (init, fragments) = read(&dolby);
            fixtures.push((format!("{kind} with HDR10 SEI and RPUs"), init, fragments));
        }
        let (init, fragments) = read(&plain);
        fixtures.push((kind.to_owned(), init, fragments));
    }
    fixtures
}

fn samples<'a>(init: &Init, fragment: &'a Fragment) -> Vec<&'a [u8]> {
    fragment
        .samples_with_bytes(init.video().expect("video").id)
        .into_iter()
        .map(|(_, bytes)| bytes.expect("a sample inside its fragment"))
        .collect()
}

/// The source clock a child would report for `whole`: each packet the size
/// `size_of` gives its sample.
fn packets(
    init: &Init,
    whole: &Fragment,
    size_of: impl Fn(usize) -> u64,
) -> (Vec<SourcePacketObservation>, SourceTimestamp) {
    let video = init.video().expect("video");
    let mut dts = 0i64;
    let packets = whole
        .track(video.id)
        .expect("video")
        .samples()
        .enumerate()
        .map(|(index, sample)| {
            let packet = SourcePacketObservation {
                sequence: index as u64,
                dts,
                pts: dts + sample.cto,
                duration: u64::from(sample.duration),
                size: size_of(index),
                checksum: 0,
                flags: 1,
                side_data_count: 0,
                time_base_num: 1,
                time_base_den: video.timescale,
            };
            dts += i64::from(sample.duration);
            packet
        })
        .collect();
    let origin = SourceTimestamp {
        dts: 0,
        time_base_num: 1,
        time_base_den: video.timescale,
    };
    (packets, origin)
}

/// A fragment's mastering-display payload and content-light maxima.
type Hdr10 = (Option<Vec<u8>>, Option<[u16; 2]>);

/// What every reader of a canonical fragment reads from it, besides its row.
#[derive(Debug, PartialEq)]
struct Readings {
    class: super::super::CutClass,
    promotion: PromotionInputs,
    hdr10: Result<Hdr10, String>,
}

fn readings(init: &Init, fragment: &Fragment) -> Readings {
    let mut hdr10 = Hdr10IndexMetadata::default();
    let observed = hdr10.observe_fragment(fragment, init);
    Readings {
        class: classify(fragment, init),
        promotion: PromotionInputs::from_fragment(fragment, init),
        hdr10: observed
            .map(|()| (hdr10.mastering().map(<[u8]>::to_vec), hdr10.light()))
            .map_err(|error| error.to_string()),
    }
}

/// The trimmer keeps every NAL unit that is not slice data whole, wherever
/// it sits in its sample (a Dolby Vision RPU follows the slices), and of each
/// slice its length prefix and two-byte header. What it reports as dropped
/// is exactly the rest.
#[test]
fn a_trimmed_fragment_keeps_every_unit_that_is_not_slice_data() {
    let mut rpus = 0;
    for (name, init, fragments) in canonical_fixtures() {
        let video = init.video().expect("video");
        let codec = video.codec.expect("a codec");
        for whole in &fragments {
            let kept = trimmed(&init, whole, 7);
            let view = super::VideoOnlyFragment::from(&kept);
            let pairs = samples(&init, whole)
                .into_iter()
                .zip(samples(&init, kept.kept()));
            for (index, (whole_sample, kept_sample)) in pairs.enumerate() {
                let whole_nals = length_prefixed_nals(whole_sample, video.nal_length_size);
                let kept_nals = length_prefixed_nals(kept_sample, video.nal_length_size);
                assert_eq!(whole_nals.len(), kept_nals.len(), "{name}");
                let mut dropped = 0;
                for (whole_nal, kept_nal) in whole_nals.iter().zip(&kept_nals) {
                    if vcl_nal_type(whole_nal[0], codec).1 {
                        assert_eq!(*kept_nal, &whole_nal[..2], "{name}: a slice's header");
                        dropped += whole_nal.len() - 2;
                    } else {
                        assert_eq!(kept_nal, whole_nal, "{name}: a unit that is not a slice");
                        rpus += usize::from((whole_nal[0] >> 1) & 0x3f == 62);
                    }
                }
                assert!(dropped > 0, "{name}: every sample carries slice data");
                assert_eq!(view.dropped_from(index) as usize, dropped, "{name}");
            }
            assert_eq!(kept.wire_len(), whole.len() as u64, "{name}");
            assert!(kept.held_len() < whole.len(), "{name}");
        }
    }
    assert!(
        rpus > 0,
        "the Dolby Vision fixtures carry RPUs after slices"
    );
}

/// The cut class, the promotion inputs (parameter sets and HDR10 SEI of the
/// first sample), the HDR10 readings of every sample, and the index row are
/// the whole fragment's, however the `mdat` was chunked on arrival.
#[test]
fn a_trimmed_fragment_reads_as_the_whole_one() {
    let mut hdr10 = 0;
    for (name, init, fragments) in canonical_fixtures() {
        for whole in &fragments {
            let expected = readings(&init, whole);
            hdr10 += usize::from(matches!(&expected.hdr10, Ok((Some(_), Some(_)))));
            let row = IndexRow::from_fragment(whole, &init).expect("a row");
            for chunk in [1, 7, 4096, usize::MAX] {
                let kept = trimmed(&init, whole, chunk);
                assert_eq!(readings(&init, kept.kept()), expected, "{name}");
                assert_eq!(
                    IndexRow::from_fragment(&kept, &init),
                    Ok(row.clone()),
                    "{name}"
                );
            }
        }
    }
    assert!(hdr10 > 0, "the HDR10 fixtures carry both SEI messages");
}

/// The packet clock associates with a trimmed fragment exactly as with the
/// whole one, in both framings a clock packet's size can have: the muxed
/// sample's own, and the Annex B one a bitstream filter leaves. A packet
/// whose size is neither is refused for both.
#[test]
fn a_trimmed_fragment_associates_with_its_packet_clock_as_the_whole_one() {
    for (name, init, fragments) in canonical_fixtures() {
        let video = init.video().expect("video");
        for whole in &fragments {
            let kept = trimmed(&init, whole, 4096);
            let whole_samples = samples(&init, whole);
            let sizes: Vec<u64> = whole_samples.iter().map(|s| s.len() as u64).collect();
            let mut framings: Vec<Box<dyn Fn(usize) -> u64>> = vec![Box::new(|index| sizes[index])];
            if video.codec == Some(super::VideoCodec::Hevc) {
                framings.push(Box::new(|index| {
                    hevc_sample_cbs_annex_b_len(whole_samples[index], video.nal_length_size)
                        .expect("Annex B length") as u64
                }));
            }
            for size_of in &framings {
                let (clock, origin) = packets(&init, whole, size_of);
                let expected = associate_fragment_clock(whole, &init, &clock, 0, None, origin, 0)
                    .expect("the whole fragment associates");
                assert_eq!(
                    associate_fragment_clock(&kept, &init, &clock, 0, None, origin, 0),
                    Ok(expected),
                    "{name}"
                );
                // Neither framing of any sample is a megabyte off the other.
                let (wrong, origin) = packets(&init, whole, |index| size_of(index) + (1 << 20));
                assert!(
                    associate_fragment_clock(whole, &init, &wrong, 0, None, origin, 0).is_err(),
                    "{name}"
                );
                assert!(
                    associate_fragment_clock(&kept, &init, &wrong, 0, None, origin, 0).is_err(),
                    "{name}: a packet that is not its sample's size"
                );
            }
        }
    }
}

/// Offsets within `sample` of each NAL unit's length prefix.
fn nal_starts(sample: &[u8], width: usize) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut at = 0;
    while at + width <= sample.len() {
        starts.push(at);
        let length = sample[at..at + width]
            .iter()
            .fold(0usize, |length, byte| (length << 8) | usize::from(*byte));
        at += width + length;
    }
    starts
}

/// A sample whose NAL units do not fit it is kept whole from the first one
/// that does not, so each reader fails or stops on it exactly as it would
/// have on the whole fragment: a zero-length unit, a unit running past its
/// sample, and a truncated length prefix, each after a slice already trimmed.
#[test]
fn a_malformed_sample_is_kept_whole_from_its_fault() {
    let plain = testfixtures::video_only_pipe("closed-gop");
    let stream = testfixtures::with_dolby_vision_rpus(&with_leading_nal(&plain, &hdr10_sei()));
    let (init, fragments) = read(&stream);
    let video = init.video().expect("video");
    let width = usize::from(video.nal_length_size);
    let sound = &fragments[0];
    let first = sound.track(video.id).expect("video").runs[0].data_offset;
    let sample = samples(&init, sound)[0];
    let starts = nal_starts(sample, width);
    let last = *starts.last().expect("a last NAL unit");
    assert!(starts.len() >= 3, "an SEI, a slice and an RPU");
    let last_length = sample.len() - last - width;

    let faults: [(&str, usize, usize); 3] = [
        ("a zero-length unit", last, 0),
        ("a unit running past its sample", last, last_length + 1),
        ("a truncated length prefix", last, last_length - 2),
    ];
    for (fault, at, length) in faults {
        let mut whole = sound.clone();
        let field = first + at;
        whole.bytes[field..field + width]
            .copy_from_slice(&(length as u32).to_be_bytes()[4 - width..]);
        let kept = trimmed(&init, &whole, 5);
        let kept_sample = samples(&init, kept.kept())[0];
        let whole_sample = samples(&init, &whole)[0];
        assert!(
            kept.held_len() < whole.len(),
            "{fault}: earlier slices trimmed"
        );
        assert_eq!(
            &kept_sample[kept_sample.len() - (whole_sample.len() - at)..],
            &whole_sample[at..],
            "{fault}: kept whole from the fault"
        );
        assert_eq!(
            readings(&init, kept.kept()),
            readings(&init, &whole),
            "{fault}"
        );
        assert_eq!(
            hevc_sample_cbs_annex_b_len(kept_sample, video.nal_length_size).is_err(),
            hevc_sample_cbs_annex_b_len(whole_sample, video.nal_length_size).is_err(),
            "{fault}"
        );
        assert_eq!(
            IndexRow::from_fragment(&kept, &init),
            IndexRow::from_fragment(&whole, &init),
            "{fault}"
        );
    }
}

/// A `trun` may take its sample size from the `tfhd`. With a single sample,
/// what is kept is declared there and the `moof` keeps its shape, so a
/// rewrite that changes the sample's size grows the kept `moof` by the same
/// four bytes it grows the whole one: the row after it is the whole
/// fragment's. With several samples sharing that one size there is nowhere
/// to declare what is kept of each, and the fragment is kept whole.
#[test]
fn a_tfhd_sized_sample_is_trimmed_only_where_its_size_can_be_declared() {
    let stream = testfixtures::with_dolby_vision_rpus(&testfixtures::video_only_pipe("closed-gop"));
    let (init, fragments) = read(&stream);
    let video = init.video().expect("video").id;
    let grow = |sample: &[u8]| {
        let mut next = sample.to_vec();
        next.extend_from_slice(&[0, 0, 0, 3, 39 << 1, 1, 0x80]);
        Ok(next)
    };

    let mut whole = testfixtures::tfhd_sized_fragment(&init, &fragments[0], 1);
    let mut kept = trimmed(&init, &whole, 3);
    assert!(
        kept.held_len() < whole.len(),
        "the single sample is trimmed"
    );
    assert_eq!(
        IndexRow::from_fragment(&kept, &init),
        IndexRow::from_fragment(&whole, &init)
    );
    let moof = whole.mdat_payload.start;
    assert!(rewrite_video_samples(&mut whole, &init.tracks, video, grow).expect("whole"));
    assert!(rewrite_video_samples(kept.kept_mut(), &init.tracks, video, grow).expect("kept"));
    assert_eq!(whole.mdat_payload.start, moof + 4, "the whole moof grew");
    assert_eq!(
        kept.kept().mdat_payload.start,
        moof + 4,
        "and so did the kept one"
    );
    assert_eq!(
        IndexRow::from_fragment(&kept, &init),
        IndexRow::from_fragment(&whole, &init),
        "the row after a rewrite counts the moof's growth"
    );

    let several = testfixtures::tfhd_sized_fragment(&init, &fragments[0], 2);
    assert_eq!(
        trimmed(&init, &several, 3),
        TrimmedFragment::whole(several),
        "two samples sharing a tfhd size are kept whole"
    );
}

/// A fragment that is not the init's single video track is kept whole.
#[test]
fn a_fragment_that_is_not_video_alone_is_kept_whole() {
    let (init, fragments) = read(&testfixtures::pipe("closed-gop"));
    assert!(init.tracks.len() > 1, "the production pipe carries audio");
    for whole in fragments {
        assert_eq!(trimmed(&init, &whole, 9), TrimmedFragment::whole(whole));
    }
}
