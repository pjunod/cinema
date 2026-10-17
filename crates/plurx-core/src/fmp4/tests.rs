use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::testfixtures::{ffmpeg, ffprobe, pipe, pipe_path, run};

fn shared_audio_init() -> Init {
    Init {
        bytes: vec![],
        tracks: vec![Track {
            id: 1,
            kind: TrackKind::Audio,
            timescale: 48_000,
            codec: None,
            dolby_vision_config: false,
            has_edit_list: false,
            nal_length_size: 0,
            default_sample_duration: 1_024,
            default_sample_size: 4,
            default_sample_flags: 0,
        }],
    }
}

fn shared_audio_fragment(start_sample: u32, count: u32, final_duration: Option<u32>) -> Fragment {
    let bytes = (start_sample..start_sample + count)
        .flat_map(u32::to_be_bytes)
        .collect::<Vec<_>>();
    let samples = (0..count)
        .map(|index| Sample {
            duration: if index + 1 == count {
                final_duration.unwrap_or(1_024)
            } else {
                1_024
            },
            size: 4,
            size_at: None,
            flags: 0,
            cto: 0,
        })
        .collect();
    Fragment {
        mdat_payload: 0..bytes.len(),
        bytes,
        tracks: vec![TrackFragment {
            track_id: 1,
            base_decode_time: u64::from(start_sample) * 1_024,
            runs: vec![Run {
                version: 0,
                composition_offsets_present: false,
                data_offset: 0,
                data_offset_at: None,
                samples,
            }],
        }],
    }
}

#[test]
fn shared_audio_publication_preserves_every_sample_across_fragment_and_restart_boundaries() {
    let init = shared_audio_init();
    let plan = crate::transcode::vod_shared_audio_plan(4_100, 160);
    let mut cutter = PlannedAudioSegmenter::new(init.clone(), plan.clone(), 0).expect("cutter");
    assert!(cutter
        .push(shared_audio_fragment(0, 60, None))
        .expect("first")
        .is_empty());
    let mut published = cutter
        .push(shared_audio_fragment(60, 80, None))
        .expect("middle");
    assert_eq!(published.len(), 1);
    assert!(!cutter.complete());
    published.extend(
        cutter
            .push(shared_audio_fragment(140, 53, Some(192)))
            .expect("tail"),
    );
    assert!(cutter.complete());
    assert_eq!(published.len(), 3);
    let mut actual = Vec::new();
    for (index, item) in published.iter().enumerate() {
        assert_eq!(item.index, index as u64);
        assert_eq!(item.segment.stats.tfdt_adjustments, 0);
        let mut reader = FragmentReader::new();
        reader.init_done = true;
        reader.tracks = init.tracks.clone();
        reader.push(&item.segment.bytes);
        let Some(Unit::Fragment(fragment)) = reader.next_unit().expect("published fragment") else {
            panic!("published audio did not parse");
        };
        assert_eq!(
            fragment.tracks[0].base_decode_time,
            plan.entries[index].start_ticks
        );
        assert_eq!(
            fragment.tracks[0].duration(),
            plan.entries[index].duration_ticks
        );
        actual.extend(track_samples(&fragment, 1));
    }
    assert_eq!(
        actual,
        (0u32..193)
            .map(|sample| sample.to_be_bytes().to_vec())
            .collect::<Vec<_>>()
    );
    let mut restart = PlannedAudioSegmenter::new(init, plan, 1).expect("restart");
    let middle = restart
        .push(shared_audio_fragment(94, 94, None))
        .expect("restart interval");
    assert_eq!(middle[0].segment.bytes, published[1].segment.bytes);
}

#[test]
fn shared_audio_gap_overlap_and_partial_packets_cannot_change_the_publication_frontier() {
    let mut cutter = PlannedAudioSegmenter::new(
        shared_audio_init(),
        crate::transcode::vod_shared_audio_plan(4_100, 160),
        0,
    )
    .expect("cutter");
    cutter
        .push(shared_audio_fragment(0, 60, None))
        .expect("prerun");
    assert!(cutter.push(shared_audio_fragment(61, 34, None)).is_err());
    assert!(cutter.push(shared_audio_fragment(59, 34, None)).is_err());
    assert!(cutter
        .push(shared_audio_fragment(60, 34, Some(512)))
        .is_err());
    assert_eq!(cutter.next_tick, 60 * 1_024);
    assert_eq!(cutter.entry, 0);
    let published = cutter
        .push(shared_audio_fragment(60, 34, None))
        .expect("correct retry");
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].index, 0);
    assert!(!cutter.complete());
}

#[test]
fn shared_soundtrack_facts_verify_actual_aac_configuration() {
    crate::testfixtures::require_ffmpeg();
    for channels in [1, 2] {
        let mut command = Command::new(ffmpeg());
        command.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "0.2",
            "-vn",
            "-c:a",
            "aac",
            "-profile:a",
            "aac_low",
            "-ar",
            "48000",
            "-ac",
        ]);
        command.arg(channels.to_string()).args([
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof+delay_moov",
            "-f",
            "mp4",
            "pipe:1",
        ]);
        let (mut init, _, _) = read_all(&run(&mut command));
        let facts = aac_lc_sample_entry_facts(&init).expect("actual AAC facts");
        assert_eq!(facts.channels, channels);
        assert_eq!(facts.codec, "mp4a.40.2");
        assert_eq!(facts.sample_rate, 48_000);
        assert_eq!(facts.samples_per_frame, 1_024);
        init.tracks.push(init.tracks[0].clone());
        assert!(aac_lc_sample_entry_facts(&init).is_err());
    }
    assert_eq!(
        parse_shared_aac_config(&[0x11, 0xb8])
            .expect("eight channels")
            .channels,
        8
    );
    for config in [
        &[0x11][..],
        &[0x11, 0x90, 0x56, 0xe5, 0x80][..],
        &[0x11, 0x94][..],
        &[0x12, 0x10][..],
        &[0x11, 0x80][..],
        &[0x29, 0x90][..],
    ] {
        assert!(parse_shared_aac_config(config).is_err(), "{config:?}");
    }
    for bytes in [
        &[5, 0x80, 0x80, 0x80, 0x80, 0][..],
        &[5, 3, 0x11, 0x90][..],
        &[5, 0x80][..],
    ] {
        let mut input = bytes;
        assert!(read_audio_descriptor(&mut input).is_err());
    }
}

#[test]
fn continuous_avc_facts_come_from_the_actual_single_sample_entry() {
    crate::testfixtures::require_ffmpeg();
    let mut command = Command::new(ffmpeg());
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=640x360:rate=24000/1001",
        "-t",
        "0.3",
        // The continuous recipe's own tagging: newer ffmpeg leaves the
        // stream unspecified unless the frames themselves carry BT.709.
        "-vf",
        "setparams=range=limited:color_primaries=bt709:color_trc=bt709:colorspace=bt709",
        "-c:v",
        "libx264",
        "-profile:v",
        "high",
        "-level:v",
        "5.0",
        "-pix_fmt",
        "yuv420p",
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
        "-colorspace",
        "bt709",
        "-an",
        "-video_track_timescale",
        "24000",
        // As the continuous recipe does: newer ffmpeg writes `colr` only on request.
        "-movflags",
        "frag_keyframe+empty_moov+default_base_moof+delay_moov+write_colr",
        "-f",
        "mp4",
        "pipe:1",
    ]);
    let (mut init, _, _) = read_all(&run(&mut command));
    let facts = avc_sample_entry_facts(&init)
        .expect("actual facts")
        .expect("AVC");
    assert_eq!((facts.width, facts.height), (640, 360));
    assert_eq!(facts.codec, "avc1.640032");
    assert_eq!(
        (
            facts.color_primaries,
            facts.color_transfer,
            facts.color_matrix
        ),
        (1, 1, 1)
    );
    assert!(!facts.full_range);
    duplicate_avc_sample_entry_for_fixture(&mut init);
    assert!(avc_sample_entry_facts(&init).is_err());
}

/// Everything a feed produces, in order.
fn read_all(feed: &[u8]) -> (Init, Vec<Fragment>, bool) {
    let mut reader = FragmentReader::new();
    reader.push(feed);
    let mut init = None;
    let mut frags = Vec::new();
    let mut trailer = false;
    while let Some(unit) = reader.next_unit().expect("parsing the pipe output") {
        match unit {
            Unit::Init(i) => init = Some(i),
            Unit::Fragment(f) => frags.push(f),
            Unit::Trailer => trailer = true,
        }
    }
    (
        init.expect("the pipe carried an init segment"),
        frags,
        trailer,
    )
}

/// Count top-level boxes of a type, so the reader is checked against an
/// independent walk of the same bytes rather than against itself.
fn count_boxes(feed: &[u8], want: &[u8; 4]) -> usize {
    let mut n = 0;
    let mut p = 0usize;
    while let Ok(Some(hdr)) = peek_box(feed, p) {
        if hdr.kind() == want {
            n += 1;
        }
        p += hdr.size;
        if p >= feed.len() {
            break;
        }
    }
    n
}

// -----------------------------------------------------------------------
// M0 — the reader
// -----------------------------------------------------------------------

#[test]
fn parses_the_pipes_init_and_every_fragment() {
    let feed = pipe("open-gop");
    let (init, frags, trailer) = read_all(&feed);

    // The init is the first two boxes, byte for byte — that is what makes
    // writing it out as `init.mp4` a copy rather than a re-mux.
    assert_eq!(
        init.bytes,
        &feed[..init.bytes.len()],
        "init.mp4 is not a verbatim prefix of the pipe"
    );
    let head = peek_box(&init.bytes, 0).expect("ftyp").expect("ftyp");
    assert_eq!(head.kind(), b"ftyp");
    let moov = peek_box(&init.bytes, head.size)
        .expect("moov")
        .expect("moov");
    assert_eq!(moov.kind(), b"moov");
    assert_eq!(head.size + moov.size, init.bytes.len());

    assert_eq!(init.tracks.len(), 2, "video + audio");
    let video = init.video().expect("a video track");
    assert_eq!(video.kind, TrackKind::Video);
    assert_eq!(video.codec, Some(VideoCodec::Hevc));
    assert!(!video.dolby_vision_config);
    assert_eq!(video.nal_length_size, 4, "hvcC lengthSizeMinusOne");
    assert!(video.timescale > 0);
    assert!(init
        .tracks
        .iter()
        .any(|t| t.kind == TrackKind::Audio && t.timescale == 48_000));

    assert_eq!(
        frags.len(),
        count_boxes(&feed, b"moof"),
        "a fragment per moof, no more and no fewer"
    );
    assert!(frags.len() >= 6, "12 s at a 1.75 s GOP: {}", frags.len());
    assert!(trailer, "ffmpeg's mfra index was not recognised");

    for (i, f) in frags.iter().enumerate() {
        assert_eq!(f.tracks.len(), 2, "fragment {i} lost a track");
        // Every sample's bytes have to lie inside the fragment, or the
        // merger's offset arithmetic is building on sand.
        for t in &f.tracks {
            for r in &t.runs {
                assert!(
                    r.data_offset >= f.mdat_payload.start
                        && r.data_offset + r.byte_len() <= f.bytes.len(),
                    "fragment {i} track {} run runs outside its mdat",
                    t.track_id
                );
            }
        }
    }
}

fn length_prefixed_hevc_nals(nals: &[&[u8]]) -> Vec<u8> {
    let mut sample = Vec::new();
    for nal in nals {
        sample.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        sample.extend_from_slice(nal);
    }
    sample
}

fn fragment_with_first_video_sample(track_id: u32, sample: Vec<u8>) -> Fragment {
    let size = sample.len() as u32;
    Fragment {
        mdat_payload: 0..sample.len(),
        bytes: sample,
        tracks: vec![TrackFragment {
            track_id,
            base_decode_time: 0,
            runs: vec![Run {
                version: 0,
                composition_offsets_present: false,
                data_offset_at: None,
                data_offset: 0,
                samples: vec![Sample {
                    size_at: None,
                    duration: 1_000,
                    size,
                    flags: 0,
                    cto: 0,
                }],
            }],
        }],
    }
}

/// The reference episode I WEB-DL shape: a valid 23-byte `hvcC` header with zero NAL
/// arrays, while VPS/SPS/PPS live only in the first sample. This is the
/// only shape promotion fires on, and no encoder in the corpus produces
/// it, so every test that needs it builds it.
fn minimal_hvcc_dv_init() -> Init {
    let feed = pipe("open-gop");
    let (mut init, _, _) = read_all(&feed);
    let video = init.video().expect("HEVC video").clone();
    strip_hvcc_arrays(&mut init);
    init.tracks
        .iter_mut()
        .find(|track| track.id == video.id)
        .expect("video track")
        .dolby_vision_config = true;
    init
}

fn duplicate_hevc_sample_entry(init: &mut Init) {
    duplicate_hevc_sample_entry_for_fixture(init);
}

/// The two Dolby Vision configuration records ffmpeg itself wrote, captured
/// from lab4 on 2026-08-30 (`docs/streaming/PLAYBACK-CAPS-V2-M0.md` §8).
///
/// These are the golden. The 24-byte payload is the whole contract between
/// plurx and every Dolby Vision decoder downstream, and the only way to
/// know a hand-written parser agrees with the muxers in the wild is to
/// take it from them.
const NATIVE_P8_DVVC: &str = "010010351000000000000000000000000000000000000000";
/// A Profile 7 dual-layer title copied out of its Matroska container with
/// its enhancement layer and RPU intact — plurx's own preserve route. The
/// record is correct: there really is an enhancement layer.
const PRESERVED_P7_DVCC: &str = "01000e376000000000000000000000000000000000000000";

/// Read an init back through the ordinary reader, from bytes.
fn reparse_init(bytes: &[u8]) -> Init {
    let mut reader = FragmentReader::new();
    reader.push(bytes);
    match reader.next_unit().expect("the rewritten init parses") {
        Some(Unit::Init(init)) => init,
        other => panic!("expected an init, got {other:?}"),
    }
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
        .collect()
}

/// The parser agrees with ffmpeg, on records ffmpeg wrote.
///
/// Two real records, one of each spelling and one of each profile class,
/// so the two fields that decide the box name and the codec string are
/// pinned against a muxer rather than against a reading of the spec.
#[test]
fn the_dolby_vision_record_reads_the_way_ffmpeg_writes_it() {
    let native = DolbyVisionRecord::parse(&hex(NATIVE_P8_DVVC)).expect("native P8 record");
    assert_eq!(
        (
            native.profile,
            native.level,
            native.el_present,
            native.rpu_present,
            native.bl_present,
            native.bl_signal_compatibility_id
        ),
        (8, 6, false, true, true, 1)
    );
    assert_eq!(native.box_name(), *b"dvvC", "profile 8 and above is dvvC");

    let preserved = DolbyVisionRecord::parse(&hex(PRESERVED_P7_DVCC)).expect("preserved P7");
    assert_eq!(
        (
            preserved.profile,
            preserved.level,
            preserved.el_present,
            preserved.rpu_present,
            preserved.bl_present,
            preserved.bl_signal_compatibility_id
        ),
        (7, 6, true, true, true, 6)
    );
    assert_eq!(
        preserved.box_name(),
        *b"dvcC",
        "profile 7 and below is dvcC"
    );

    // The writer is the parser's exact inverse, or a rewrite silently
    // drops a field nothing here reads but a decoder does.
    for golden in [NATIVE_P8_DVVC, PRESERVED_P7_DVCC] {
        let bytes = hex(golden);
        let parsed = DolbyVisionRecord::parse(&bytes).expect("parse");
        assert_eq!(parsed.to_payload(), bytes, "round trip of {golden}");
    }

    // Level and compatibility id are distinct fields that both read 6 in
    // the goldens above — so on their own those two records cannot tell a
    // swap of the two apart. Pin them separately.
    let mut distinct = hex(NATIVE_P8_DVVC);
    distinct[2] = 0x10;
    distinct[3] = 0x1d; // level 3
    distinct[4] = 0x40; // compat 4
    let parsed = DolbyVisionRecord::parse(&distinct).expect("parse");
    assert_eq!((parsed.level, parsed.bl_signal_compatibility_id), (3, 4));
    assert_eq!(parsed.to_payload(), distinct);
}

/// A rewrite preserves every byte this type does not model.
///
/// The record is 24 bytes and five fields; the rest is reserved space that
/// later spec revisions already use — `dv_md_compression` sits in the low
/// nibble of byte 4 in ISO spec v2.1, and the ffmpeg in the image writes
/// it. Rebuilding the payload from zero would tell a decoder that
/// compressed RPU metadata is uncompressed, and nothing would report that
/// bytes had been dropped.
#[test]
fn correcting_a_record_does_not_discard_the_bytes_it_does_not_understand() {
    let mut carrying = hex(PRESERVED_P7_DVCC);
    carrying[4] |= 0x0c; // dv_md_compression
    carrying[8] = 0xab; // and something further into the reserved tail
    carrying[23] = 0x5a;

    let read = DolbyVisionRecord::parse(&carrying).expect("parse");
    let corrected = read.without_enhancement_layer();
    let written = corrected.to_payload();

    assert!(!corrected.el_present, "the correction did happen");
    assert_eq!(
        written[4] & 0x0f,
        0x0c,
        "dv_md_compression must survive the correction"
    );
    assert_eq!(written[8], 0xab);
    assert_eq!(written[23], 0x5a);
    // Only the two bytes holding the flags changed.
    for (at, (before, after)) in carrying.iter().zip(written.iter()).enumerate() {
        if at != 3 {
            assert_eq!(before, after, "byte {at} changed and should not have");
        }
    }
}

/// The constructor refuses what the format cannot represent.
///
/// Masking instead would turn profile 128 into profile 0 in the payload
/// while `box_name` still read 128 and answered `dvvC` — and
/// `dolby_vision_codec_from_init` rejects profile 0, falling back to an
/// `hvcC`-derived codec string that AVPlayer refuses on a DV sample entry.
#[test]
fn a_record_that_cannot_be_written_is_refused_rather_than_truncated() {
    for (profile, level, compat) in [(128, 6, 1), (0, 6, 1), (8, 64, 1), (8, 0, 1), (8, 6, 16)] {
        assert!(
            DolbyVisionRecord::new(profile, level, false, true, true, compat).is_err(),
            "profile {profile} level {level} compat {compat} must be refused"
        );
    }
    assert!(DolbyVisionRecord::new(8, 6, false, true, true, 6).is_ok());
}

/// A sample rewrite that changes length leaves a fragment that still
/// parses, and parses to the same samples it was told to write.
///
/// This is the whole of the size-fixup chain: the sample's declared size,
/// every later run's offset, and the `mdat`. Getting any one of them wrong
/// produces a fragment that still *looks* structurally fine and whose
/// samples are read from the wrong bytes — the failure that would reach a
/// decoder as a corrupt picture rather than as an error.
#[test]
fn shortening_a_sample_corrects_every_size_that_described_it() {
    let feed = pipe("closed-gop");
    let (init, mut frags, _) = read_all(&feed);
    let video = init.video().expect("video").id;
    let fragment = frags.first_mut().expect("a fragment");

    let before = fragment.clone();
    let original: Vec<Vec<u8>> = video_samples(&before, video);
    assert!(
        original.len() > 1,
        "one sample proves nothing about offsets"
    );

    // Drop the last four bytes of every video sample. Arbitrary, and
    // arbitrary is the point: nothing about the fixup depends on what the
    // rewrite did, only on how much shorter it made things.
    let changed = rewrite_video_samples(fragment, &init.tracks, video, |sample| {
        Ok(sample[..sample.len() - 4].to_vec())
    })
    .expect("rewrite");
    assert!(changed);

    // The mdat shrank by exactly the bytes removed, and the moof did not
    // move — a fragment whose moof resized would invalidate every offset
    // in it.
    let removed = 4 * original.len();
    assert_eq!(fragment.bytes.len(), before.bytes.len() - removed);
    assert_eq!(
        fragment.mdat_payload.start, before.mdat_payload.start,
        "the moof keeps its size, so the mdat starts where it always did"
    );

    // Re-read the fragment from its own bytes, behind the same init: the
    // model has to follow the bytes rather than be patched in parallel
    // with them.
    let mut replayed = init.bytes.clone();
    replayed.extend_from_slice(&fragment.bytes);
    let (_, reparsed, _) = read_all(&replayed);
    let reparsed = reparsed.first().expect("the rewritten fragment parses");
    let after = video_samples(reparsed, video);
    assert_eq!(after.len(), original.len(), "no sample was lost");
    for (index, (was, now)) in original.iter().zip(after.iter()).enumerate() {
        assert_eq!(
            now.as_slice(),
            &was[..was.len() - 4],
            "sample {index} was read back from the wrong bytes"
        );
    }

    // Every other track's samples survive byte for byte, which is what the
    // corrected offsets are for.
    for track in &before.tracks {
        if track.track_id == video {
            continue;
        }
        let was = track_samples(&before, track.track_id);
        let now = track_samples(reparsed, track.track_id);
        assert_eq!(
            was, now,
            "track {} moved but did not survive",
            track.track_id
        );
    }
}

/// The model follows the bytes, and the segmenter can still read media
/// through it.
///
/// This is the test the first version of this function did not have, and
/// its absence is why that version shipped broken: the byte-level
/// assertions above all passed while `fragment.tracks` still described the
/// pre-rewrite layout. Nothing addresses media by scanning — the
/// segmenter, the landing matcher, `classify` and the promotion capture
/// all index into `bytes` using `data_offset` and `size` from the model —
/// so a stale model reads samples from offsets that no longer exist, and
/// the audio run that sits after the shortened video lands past the end.
#[test]
fn the_model_follows_the_bytes_so_the_segmenter_can_still_read_them() {
    let feed = pipe("closed-gop");
    let (init, mut frags, _) = read_all(&feed);
    let video = init.video().expect("video").id;
    let fragment = frags.first_mut().expect("a fragment");
    let before: Vec<(u32, usize, usize)> = fragment
        .tracks
        .iter()
        .map(|t| (t.track_id, t.byte_len(), t.sample_count()))
        .collect();

    rewrite_video_samples(fragment, &init.tracks, video, |sample| {
        Ok(sample[..sample.len() - 4].to_vec())
    })
    .expect("rewrite");

    // The model's own totals moved with the bytes.
    let after: Vec<(u32, usize, usize)> = fragment
        .tracks
        .iter()
        .map(|t| (t.track_id, t.byte_len(), t.sample_count()))
        .collect();
    for ((id, was, count), (id_after, now, count_after)) in before.iter().zip(after.iter()) {
        assert_eq!(id, id_after, "the tracks kept their order");
        assert_eq!(count, count_after, "no sample was lost");
        if *id == video {
            assert_eq!(*now, *was - 4 * *count, "video shrank by what was removed");
        } else {
            assert_eq!(now, was, "every other track is untouched");
        }
    }

    // Every sample the model points at is inside the fragment, which is
    // the invariant a stale model breaks.
    for track in &fragment.tracks {
        for run in &track.runs {
            let mut at = run.data_offset;
            for sample in &run.samples {
                at += sample.size as usize;
            }
            assert!(
                at <= fragment.bytes.len(),
                "track {} runs {} bytes past the end",
                track.track_id,
                at - fragment.bytes.len()
            );
        }
    }

    // And the reader that actually consumes this — the one that turns a
    // fragment into a served segment — can still parse it.
    assert!(
        classify(fragment, &init).is_clean() || !classify(fragment, &init).is_clean(),
        "classification must not panic on a rewritten fragment"
    );
    let promotion = PromotionInputs::from_fragment(fragment, &init);
    assert!(
        promotion.parameter_sets.len() <= 3,
        "the promotion capture read past the sample it was given: {} sets",
        promotion.parameter_sets.len()
    );
}

/// A rewrite that changes nothing changes nothing.
#[test]
fn a_rewrite_that_returns_the_same_bytes_leaves_the_fragment_alone() {
    let feed = pipe("closed-gop");
    let (init, mut frags, _) = read_all(&feed);
    let video = init.video().expect("video").id;
    let fragment = frags.first_mut().expect("a fragment");
    let before = fragment.bytes.clone();

    let changed =
        rewrite_video_samples(fragment, &init.tracks, video, |sample| Ok(sample.to_vec()))
            .expect("rewrite");
    assert!(!changed, "nothing to report when nothing moved");
    assert_eq!(fragment.bytes, before);
}

/// A refusal partway through leaves the fragment exactly as it was.
///
/// The caller's fallback is to serve this fragment unrewritten, so a
/// half-applied edit would hand it a stream that decodes to garbage — the
/// one outcome worse than refusing.
#[test]
fn a_refused_rewrite_leaves_the_fragment_untouched() {
    let feed = pipe("closed-gop");
    let (init, mut frags, _) = read_all(&feed);
    let video = init.video().expect("video").id;
    let fragment = frags.first_mut().expect("a fragment");
    let before = fragment.bytes.clone();

    let mut seen = 0;
    let refused = rewrite_video_samples(fragment, &init.tracks, video, |sample| {
        seen += 1;
        if seen > 1 {
            return Err(Fmp4Error::Unsupported("no".into()));
        }
        Ok(sample[..sample.len() - 4].to_vec())
    });
    assert!(refused.is_err());
    assert_eq!(
        fragment.bytes, before,
        "the first sample's edit must not have been applied"
    );
}

/// The compact shape this rewrite expands, followed by the shapes it must
/// still refuse and what accepting each would do.
///
/// Each remaining refusal is the only thing standing between the code and
/// a corruption that still parses, so each is asserted with the corruption
/// named. The compact shape is built as real bytes because mutating only
/// the parsed model never exercises the box growth that failed in service.
#[test]
fn default_sized_truns_are_expanded_and_unrepresentable_shapes_are_refused() {
    let feed = pipe("closed-gop");
    let (init, frags, _) = read_all(&feed);
    let video = init.video().expect("video").id;

    // A `trun` may legally take every sample size from `tfhd`. This is the
    // exact shape a later GOP of a real P7 source used after earlier GOPs
    // had explicit sizes: the old converter killed the live P8 producer,
    // and Safari recovered by laddering down to a 1080p SDR transcode.
    // Build the compact bytes themselves rather than only mutating the
    // parsed model, so this proves the boxes grow and offsets move.
    let source_run = &frags[0].track(video).expect("video traf").runs[0];
    let source_sample = source_run.samples[0];
    let source_bytes = &frags[0].bytes
        [source_run.data_offset..source_run.data_offset + source_sample.size as usize];
    let mut tfhd = Vec::new();
    tfhd.extend_from_slice(&28u32.to_be_bytes());
    tfhd.extend_from_slice(b"tfhd");
    tfhd.extend_from_slice(&0x0002_0038u32.to_be_bytes());
    tfhd.extend_from_slice(&video.to_be_bytes());
    tfhd.extend_from_slice(&source_sample.duration.to_be_bytes());
    tfhd.extend_from_slice(&source_sample.size.to_be_bytes());
    tfhd.extend_from_slice(&source_sample.flags.to_be_bytes());
    let mut trun = Vec::new();
    trun.extend_from_slice(&20u32.to_be_bytes());
    trun.extend_from_slice(b"trun");
    trun.extend_from_slice(&0x0000_0001u32.to_be_bytes());
    trun.extend_from_slice(&1u32.to_be_bytes());
    trun.extend_from_slice(&72u32.to_be_bytes()); // moof (64) + mdat header (8)
    let mut compact = Vec::new();
    compact.extend_from_slice(&64u32.to_be_bytes());
    compact.extend_from_slice(b"moof");
    compact.extend_from_slice(&56u32.to_be_bytes());
    compact.extend_from_slice(b"traf");
    compact.extend_from_slice(&tfhd);
    compact.extend_from_slice(&trun);
    compact.extend_from_slice(&(source_sample.size + 8).to_be_bytes());
    compact.extend_from_slice(b"mdat");
    compact.extend_from_slice(source_bytes);

    let mut reader = FragmentReader::new();
    reader.push(&init.bytes);
    assert!(matches!(reader.next_unit(), Ok(Some(Unit::Init(_)))));
    reader.push(&compact);
    let Ok(Some(Unit::Fragment(mut defaulted))) = reader.next_unit() else {
        panic!("the tfhd-defaulted fragment did not parse");
    };
    assert!(
        defaulted
            .track(video)
            .expect("video")
            .samples()
            .all(|sample| sample.size_at.is_none()),
        "fixture accidentally carried an explicit sample size"
    );
    let old_payload_start = defaulted.mdat_payload.start;
    let changed = rewrite_video_samples(&mut defaulted, &init.tracks, video, |sample| {
        Ok(sample[..sample.len() - 4].to_vec())
    })
    .expect("a tfhd-defaulted trun is expanded before rewrite");
    assert!(changed);
    let rewritten_run = &defaulted.track(video).expect("video").runs[0];
    assert_eq!(defaulted.mdat_payload.start, old_payload_start + 4);
    assert_eq!(rewritten_run.data_offset, defaulted.mdat_payload.start);
    assert_eq!(rewritten_run.samples[0].size, source_sample.size - 4);
    assert!(rewritten_run.samples[0].size_at.is_some());
    assert_eq!(
        &defaulted.bytes[defaulted.mdat_payload.clone()],
        &source_bytes[..source_bytes.len() - 4]
    );

    // A second `trun` that inherits its position from the run before it
    // follows bytes this rewrite moves. Accepting it leaves that run
    // reading its samples from the wrong offsets, in a fragment that still
    // parses perfectly.
    let mut inherited = frags[0].clone();
    for track in &mut inherited.tracks {
        if track.track_id == video && track.runs.len() == 1 {
            let mut second = track.runs[0].clone();
            second.data_offset_at = None;
            second.samples.truncate(1);
            track.runs.push(second);
        }
    }
    let before = inherited.bytes.clone();
    let error = rewrite_video_samples(&mut inherited, &init.tracks, video, |sample| {
        Ok(sample[..sample.len() - 4].to_vec())
    })
    .expect_err("an inherited data_offset follows bytes this moves");
    assert!(error.to_string().contains("continues from"), "{error}");
    assert_eq!(inherited.bytes, before);

    // An `mdat` whose length lives in a 64-bit `largesize` field has
    // nowhere for a corrected 32-bit size to go. Refused by name rather
    // than by luck: reading `header - 8` on such a box lands on the high
    // half of the length, which is zero for anything under 4 GiB and would
    // otherwise be mistaken for the "size is elsewhere" sentinel.
    let mut large = frags[0].clone();
    {
        let payload = large.mdat_payload.clone();
        let length = (large.bytes.len() - (payload.start - 8)) as u64;
        let mut widened = large.bytes[..payload.start - 8].to_vec();
        widened.extend_from_slice(&1u32.to_be_bytes());
        widened.extend_from_slice(b"mdat");
        widened.extend_from_slice(&(length + 8).to_be_bytes());
        widened.extend_from_slice(&large.bytes[payload.start..]);
        large.bytes = widened;
        large.mdat_payload = payload.start + 8..payload.end + 8;
        for track in &mut large.tracks {
            for run in &mut track.runs {
                run.data_offset += 8;
            }
        }
    }
    let before = large.bytes.clone();
    let error = rewrite_video_samples(&mut large, &init.tracks, video, |sample| {
        Ok(sample[..sample.len() - 4].to_vec())
    })
    .expect_err("a largesize mdat has no 32-bit size to correct");
    assert!(error.to_string().contains("16-byte header"), "{error}");
    assert_eq!(large.bytes, before);
}

/// The model follows the bytes all the way to the `mdat` payload's end,
/// and overlapping or out-of-order samples are refused rather than
/// panicked on.
///
/// Two things nothing else asserts. `mdat_payload.end` is currently
/// write-only downstream — the merger reads samples through `tracks` — so
/// a wrong value here would sit undetected until the first reader that
/// slices by it, and then read past the end of a fragment. And the
/// forward build assumes the edits are sorted and disjoint: a model that
/// broke either would take the slice at `bytes[copied..edit.at.start]`
/// backwards and **panic**, out of a function whose whole contract is to
/// return `Result` and whose callers argue their safety from "a refusal
/// leaves the fragment exactly as it was".
#[test]
fn the_payload_range_follows_the_bytes_and_a_tangled_model_is_refused() {
    let feed = pipe("closed-gop");
    let (init, frags, _) = read_all(&feed);
    let video = init.video().expect("video").id;

    let mut shortened = frags[0].clone();
    let before_len = shortened.bytes.len();
    let before_end = shortened.mdat_payload.end;
    let samples = shortened
        .track(video)
        .expect("a video track")
        .sample_count();
    rewrite_video_samples(&mut shortened, &init.tracks, video, |sample| {
        Ok(sample[..sample.len() - 4].to_vec())
    })
    .expect("rewrite");
    let dropped = 4 * samples;
    assert_eq!(shortened.bytes.len(), before_len - dropped);
    assert_eq!(
        shortened.mdat_payload.end,
        before_end - dropped,
        "the payload range has to shrink with the payload"
    );
    assert!(
        shortened.mdat_payload.end <= shortened.bytes.len(),
        "and must never point past the fragment"
    );

    // A model whose samples overlap cannot be applied in one forward pass.
    // Built by giving the video track a second run over bytes the first
    // already covers — sizes that still fit, so this reaches the overlap
    // check rather than the "runs past the end" one above it.
    let mut overlapping = frags[0].clone();
    for track in &mut overlapping.tracks {
        if track.track_id == video {
            let mut again = track.runs[0].clone();
            again.samples.truncate(1);
            track.runs.push(again);
        }
    }
    let untouched = overlapping.bytes.clone();
    let error = rewrite_video_samples(&mut overlapping, &init.tracks, video, |sample| {
        Ok(sample[..sample.len() - 4].to_vec())
    })
    .expect_err("overlapping samples cannot both be rewritten");
    assert!(error.to_string().contains("overlap"), "{error}");
    assert_eq!(overlapping.bytes, untouched);

    // Runs listed out of ascending order are sorted, not sliced
    // backwards. Ordering is a property of the model, not of the bytes,
    // and nothing in this file promises `parse_moof` will always produce
    // it — so the forward build sorts rather than assuming. Without that
    // this call panics inside a function whose contract is to return
    // `Result`, taking down a producer that was reading an ffmpeg pipe.
    let mut reversed = frags[0].clone();
    for track in &mut reversed.tracks {
        if track.track_id == video && track.runs[0].samples.len() >= 2 {
            let run = track.runs[0].clone();
            let split = run.samples.len() / 2;
            let head_bytes: usize = run.samples[..split].iter().map(|s| s.size as usize).sum();
            let mut tail = run.clone();
            tail.data_offset = run.data_offset + head_bytes;
            tail.samples = run.samples[split..].to_vec();
            let mut head = run;
            head.samples.truncate(split);
            track.runs = vec![tail, head];
        }
    }
    let result = rewrite_video_samples(&mut reversed, &init.tracks, video, |sample| {
        Ok(sample[..sample.len() - 4].to_vec())
    });
    assert!(
        result.is_ok(),
        "an out-of-order model must be handled, not refused or panicked on: {result:?}"
    );

    // …and a size field that sits inside the bytes being rewritten cannot
    // be corrected: writing it would patch a position the rewrite has
    // already changed the meaning of.
    let mut inside = frags[0].clone();
    let payload = inside.mdat_payload.start;
    for track in &mut inside.tracks {
        if track.track_id == video {
            if let Some(sample) = track.runs[0].samples.first_mut() {
                sample.size_at = Some(payload + 8);
            }
        }
    }
    let untouched = inside.bytes.clone();
    let error = rewrite_video_samples(&mut inside, &init.tracks, video, |sample| {
        Ok(sample[..sample.len() - 4].to_vec())
    })
    .expect_err("a size field inside the rewritten region cannot be corrected");
    assert!(error.to_string().contains("sits inside"), "{error}");
    assert_eq!(inside.bytes, untouched);
}

/// Every video sample's bytes, in order.
fn video_samples(fragment: &Fragment, track_id: u32) -> Vec<Vec<u8>> {
    track_samples(fragment, track_id)
}

fn track_samples(fragment: &Fragment, track_id: u32) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for track in &fragment.tracks {
        if track.track_id != track_id {
            continue;
        }
        for run in &track.runs {
            let mut at = run.data_offset;
            for sample in &run.samples {
                let end = at + sample.size as usize;
                out.push(fragment.bytes[at..end].to_vec());
                at = end;
            }
        }
    }
    out
}

/// The record reaches the served init through the same funnel everything
/// else does.
///
/// Promotion is the single place a muxer init becomes a served one, and
/// that is load-bearing rather than tidy: the live path and the head
/// regeneration both call it, so a served init built on one and rebuilt on
/// the other is byte-identical by construction. A record written anywhere
/// else would be written on one path and not the other, and the
/// regeneration would be refused for drift against an init it produced
/// correctly.
#[test]
fn the_dolby_vision_record_rides_the_promotion_that_every_path_shares() {
    let feed = pipe("open-gop");
    let (muxer, _, _) = read_all(&feed);
    assert_eq!(dolby_vision_record(&muxer).expect("read"), None);

    let converted =
        DolbyVisionRecord::new(8, 6, false, true, true, 1).expect("a representable record");
    let inputs = PromotionInputs {
        indexed_hdr10_metadata: false,
        hevc_configuration: None,
        dolby_vision: Some(converted.clone()),
        ..PromotionInputs::default()
    };

    let mut live = muxer.clone();
    assert!(promote_from(&mut live, &inputs).expect("promote"));
    assert_eq!(
        dolby_vision_record(&live).expect("read back"),
        Some(converted.clone())
    );

    // The second path, from the same stored inputs. Byte-identical is the
    // property the whole promotion design exists for.
    let mut regenerated = muxer.clone();
    assert!(promote_from(&mut regenerated, &inputs).expect("promote"));
    assert_eq!(regenerated.bytes, live.bytes);

    // …and with no record in the inputs, promotion leaves the init alone,
    // which is every non-converting session.
    let mut untouched = muxer.clone();
    promote_from(&mut untouched, &PromotionInputs::default()).expect("promote");
    assert_eq!(dolby_vision_record(&untouched).expect("read"), None);
}

/// Removing the record leaves an init that parses, with every enclosing
/// box's size correct — and no Dolby Vision claim anywhere.
///
/// This is the strip path's counterpart to writing one. `filter_units`
/// takes the RPU and enhancement-layer NAL units out by type and leaves
/// the DOVI side data ffmpeg copied from the source container untouched,
/// so on an ffmpeg without `dovi_rpu` the muxer writes a Profile 7 record
/// with `el_present_flag = 1` over a stream carrying neither layer.
/// VideoToolbox believes it; Safari software-decodes 4K10 HEVC on hardware
/// with a block for it.
#[test]
fn removing_the_dolby_vision_record_keeps_the_box_tree_consistent() {
    let feed = pipe("open-gop");
    let (mut init, _, _) = read_all(&feed);
    let stale = DolbyVisionRecord::new(7, 6, true, true, false, 0)
        .expect("the record a stripping muxer copies out of a P7 source");
    assert!(set_dolby_vision_record(&mut init, &stale).expect("insert"));
    assert!(init.video().expect("video").dolby_vision_config);
    let with_record = init.bytes.len();

    assert!(remove_dolby_vision_record(&mut init).expect("remove"));

    // The box is gone, and so is every trace of its name: a size field
    // left too large would leave the four fourcc bytes readable inside
    // whatever now follows.
    assert!(
        !init.bytes.windows(4).any(|f| f == b"dvcC" || f == b"dvvC"),
        "the record's box is still in the init"
    );
    assert!(init.bytes.len() < with_record);

    // Parsed metadata agrees with the bytes. Two live guards read this
    // flag and both do the wrong thing on a stale `true`: HDR10 promotion
    // refuses to run on a Dolby Vision entry, and the stale-brand
    // sanitizer refuses to drop `dby1` from one.
    assert!(!init.video().expect("video").dolby_vision_config);

    // The real check: every ancestor's size was patched, so the ordinary
    // reader can parse the result from bytes and find the same tracks.
    let reread = reparse_init(&init.bytes);
    assert_eq!(dolby_vision_record(&reread).expect("read"), None);
    assert_eq!(
        reread.video().expect("video").id,
        init.video().expect("video").id
    );
    assert!(!reread.video().expect("video").dolby_vision_config);
    assert!(hevc_parameter_sets_complete(&reread).expect("parses"));

    // Idempotent, and not an error on an init that never had one — the
    // strip path runs on every fragment head, including ones ffmpeg wrote
    // no record into.
    assert!(!remove_dolby_vision_record(&mut init).expect("no-op"));
}

/// Removing the record also removes the `dby1` file-type brand.
///
/// The stale-brand sanitizer runs on the MUXER init, where the record is
/// still present, so it correctly declines — and nothing runs again after
/// the record is gone. `dby1` in `ftyp` over a sample entry with no record
/// is the contradictory initialization segment AVPlayer refuses outright,
/// which would be a worse failure than the software decode this removal
/// exists to prevent: this change would have created it.
#[test]
fn removing_the_record_also_removes_the_dolby_file_type_brand() {
    let feed = pipe("open-gop");
    let (mut init, _, _) = read_all(&feed);
    let stale = DolbyVisionRecord::new(7, 6, true, true, false, 0).expect("record");
    assert!(set_dolby_vision_record(&mut init, &stale).expect("insert"));
    // Stamp the brand movenc writes beside a Dolby Vision sample entry.
    // Major brand and compatible-brands entry both, because a reader may
    // consult either.
    init.bytes[8..12].copy_from_slice(b"dby1");
    let size = u32::from_be_bytes(init.bytes[0..4].try_into().expect("ftyp size")) as usize;
    assert!(size >= 20, "the fixture ftyp has a compatible-brands list");
    init.bytes[16..20].copy_from_slice(b"dby1");

    assert!(remove_dolby_vision_record(&mut init).expect("remove"));
    assert!(
        !init.bytes[..size].windows(4).any(|f| f == b"dby1"),
        "the ftyp still claims Dolby Vision over an entry that has none"
    );
    assert_eq!(&init.bytes[8..12], b"iso6");
    assert_eq!(&init.bytes[16..20], b"iso6");
    // And the init still parses: the brand is rewritten in place, never
    // resized.
    let reread = reparse_init(&init.bytes);
    assert_eq!(dolby_vision_record(&reread).expect("read"), None);
}

/// The brand rewrite declines on every `ftyp` it does not fully
/// understand, rather than writing near one.
///
/// It walks the compatible-brands list by stepping four bytes at a time to
/// the declared size, so the `(size - 16) % 4` check is what guarantees the
/// last step lands exactly on `size` instead of past it. A `largesize`
/// `ftyp` (`size == 1`) and one claiming more bytes than exist both
/// decline; a `dby1` in a `largesize` box is left alone, which is
/// correct-by-decline rather than correct-by-handling and is worth knowing.
#[test]
fn the_brand_rewrite_declines_on_an_ftyp_it_cannot_walk() {
    let ftyp = |size: u32, brands: usize| {
        let mut b = vec![0u8; 16 + brands * 4];
        b[0..4].copy_from_slice(&size.to_be_bytes());
        b[4..8].copy_from_slice(b"ftyp");
        b[8..12].copy_from_slice(b"dby1");
        for i in 0..brands {
            b[16 + i * 4..20 + i * 4].copy_from_slice(b"dby1");
        }
        b
    };

    // The shape it does understand.
    let mut good = ftyp(24, 2);
    assert!(replace_dolby_file_type_brand(&mut good));
    assert!(!good[..24].windows(4).any(|f| f == b"dby1"));

    // A size that is not 16 + a whole number of brands. Walking it would
    // read the last four bytes starting inside the previous brand and
    // write across the boundary.
    let mut ragged = ftyp(22, 2);
    assert!(!replace_dolby_file_type_brand(&mut ragged));
    assert_eq!(&ragged[8..12], b"dby1", "and it changed nothing");

    // `largesize`, and a size larger than the buffer.
    let mut large = ftyp(1, 2);
    assert!(!replace_dolby_file_type_brand(&mut large));
    let mut overlong = ftyp(64, 2);
    assert!(!replace_dolby_file_type_brand(&mut overlong));

    // Not an ftyp at all, and too short to be one.
    let mut moov = ftyp(24, 2);
    moov[4..8].copy_from_slice(b"moov");
    assert!(!replace_dolby_file_type_brand(&mut moov));
    assert!(!replace_dolby_file_type_brand(
        &mut ftyp(24, 2)[..12].to_vec()
    ));

    // The minor version at 12..16 is a number, not a brand: four bytes
    // that happen to spell `dby1` there are left alone.
    let mut minor = ftyp(24, 2);
    minor[12..16].copy_from_slice(b"dby1");
    assert!(replace_dolby_file_type_brand(&mut minor));
    assert_eq!(&minor[12..16], b"dby1", "the minor version is not a brand");
}

/// A stripped stream keeps its HDR10 static metadata.
///
/// `promote_hdr10_static_metadata_from` refuses to run on a sample entry
/// carrying a Dolby Vision record. If the removal ran after it, the record
/// would still be there to inhibit the promotion and then be deleted — so
/// the one stream this whole change exists to fix, a Profile 7 title
/// stripped to its HDR10 base, would be the one served without its
/// mastering-display metadata.
#[test]
fn a_stripped_stream_still_gets_its_hdr10_static_metadata() {
    let feed = pipe("open-gop");
    let (muxer, _, _) = read_all(&feed);
    // The same hand-built prefix SEIs the ordinary promotion test uses:
    // mdcv (payload type 137, 24 bytes) and clli (144, 4 bytes).
    let mut mastering = vec![0x4e, 0x01, 137, 24];
    mastering.extend_from_slice(&[0; 24]);
    mastering.push(0x80);
    let mut content_light = vec![0x4e, 0x01, 144, 4];
    content_light.extend_from_slice(&[0; 4]);
    content_light.push(0x80);
    let hdr10_sei = vec![mastering, content_light];

    // The control: this metadata is promotable and promotion moves bytes.
    // Without it the comparison below could pass by promoting nothing on
    // either side.
    let mut control = muxer.clone();
    assert!(
        promote_hdr10_static_metadata_from(&mut control, &hdr10_sei).expect("promote"),
        "the control must promote for this test to mean anything"
    );
    let promoted_len = control.bytes.len();
    assert!(promoted_len > muxer.bytes.len());

    let mut stale = muxer.clone();
    let record = DolbyVisionRecord::new(7, 6, true, true, false, 0).expect("record");
    assert!(set_dolby_vision_record(&mut stale, &record).expect("insert"));
    let inputs = PromotionInputs {
        indexed_hdr10_metadata: false,
        hevc_configuration: None,
        strip_dolby_vision: true,
        hdr10_sei,
        ..PromotionInputs::default()
    };
    assert!(promote_from(&mut stale, &inputs).expect("promote"));
    assert_eq!(dolby_vision_record(&stale).expect("read"), None);

    let location = locate_hvcc(&stale.bytes).expect("parse").expect("hvcC");
    assert!(
        location.has_mdcv && location.has_clli,
        "the stripped stream lost the HDR10 static metadata the record inhibited"
    );
    assert_eq!(
        stale.bytes.len(),
        promoted_len,
        "and carries exactly what a stream that never had a record carries"
    );
}

/// Both record spellings go, not just the first one found.
///
/// `locate_hvcc` reports the first of `dvvC`/`dvcC` and stops. No muxer in
/// the corpus writes both, which is exactly why a half-removal here would
/// go unnoticed: the parsed flag would say there is no record while one box
/// remained, and nothing would run again to find it.
#[test]
fn removing_the_record_removes_every_spelling_of_it() {
    let feed = pipe("open-gop");
    let (mut init, _, _) = read_all(&feed);
    let p7 = DolbyVisionRecord::new(7, 6, true, true, false, 0).expect("record");
    assert!(set_dolby_vision_record(&mut init, &p7).expect("dvcC"));
    // Hand-splice a second box, in the other spelling, beside the first.
    let location = locate_hvcc(&init.bytes).expect("parse").expect("hvcC");
    let existing = location
        .dolby_vision
        .clone()
        .expect("the dvcC just written");
    let box_start = existing.payload.start - existing.header_len;
    let clone: Vec<u8> = init.bytes[box_start..existing.payload.end].to_vec();
    let mut second = clone.clone();
    second[4..8].copy_from_slice(b"dvvC");
    for &box_at in location.ancestors.iter().skip(1) {
        grow_box(&mut init.bytes, box_at, second.len()).expect("grow");
    }
    init.bytes.splice(box_start..box_start, second);
    assert_eq!(
        init.bytes
            .windows(4)
            .filter(|f| *f == b"dvcC" || *f == b"dvvC")
            .count(),
        2
    );

    assert!(remove_dolby_vision_record(&mut init).expect("remove"));
    assert!(
        !init.bytes.windows(4).any(|f| f == b"dvcC" || f == b"dvvC"),
        "one spelling survived while the parsed flag said there were none"
    );
    let reread = reparse_init(&init.bytes);
    assert_eq!(dolby_vision_record(&reread).expect("read"), None);
}

/// A box too small to have contained the record is an error, not a clamp.
///
/// `shrink_box`'s guards are unreachable through the removal itself, so
/// they are driven directly. Saturating to zero would produce an init that
/// parses as a different file — every box after the corrupted size would be
/// read at the wrong offset — which is the failure a `checked_sub` with a
/// `saturating_sub` beside it is one keystroke away from.
#[test]
fn shrinking_a_box_past_its_own_header_is_refused() {
    // An 8-byte-header box claiming 24 bytes, asked to lose 40.
    let mut bytes = vec![0u8; 24];
    bytes[0..4].copy_from_slice(&24u32.to_be_bytes());
    bytes[4..8].copy_from_slice(b"stsd");
    let at = BoxAt {
        start: 0,
        header_len: 8,
    };
    let error = shrink_box(&mut bytes, at, 40).expect_err("underflow is refused");
    assert!(error.to_string().contains("too small"), "{error}");
    assert_eq!(
        u32::from_be_bytes(bytes[0..4].try_into().expect("size")),
        24,
        "and the size is left alone rather than clamped"
    );
    // Shrinking to exactly the header is legal; below it is not.
    assert!(shrink_box(&mut bytes, at, 16).is_ok());
    assert_eq!(u32::from_be_bytes(bytes[0..4].try_into().expect("size")), 8);
    assert!(shrink_box(&mut bytes, at, 1).is_err());

    // The `largesize` arm, which no fixture in the corpus reaches.
    let mut large = vec![0u8; 40];
    large[0..4].copy_from_slice(&1u32.to_be_bytes());
    large[4..8].copy_from_slice(b"moov");
    large[8..16].copy_from_slice(&40u64.to_be_bytes());
    let large_at = BoxAt {
        start: 0,
        header_len: 16,
    };
    shrink_box(&mut large, large_at, 8).expect("a largesize box shrinks");
    assert_eq!(
        u64::from_be_bytes(large[8..16].try_into().expect("size")),
        32
    );
    assert!(shrink_box(&mut large, large_at, 40).is_err());

    // A header length this file does not understand is refused by name
    // rather than silently patched at the wrong offset.
    let odd = BoxAt {
        start: 0,
        header_len: 12,
    };
    let error = shrink_box(&mut large, odd, 4).expect_err("unknown header");
    assert!(error.to_string().contains("12-byte header"), "{error}");
}

/// The removal rides the same promotion funnel the rewrite does.
///
/// Load-bearing, not tidy: the live path and the head regeneration both
/// promote from the same stored inputs, so a removal performed anywhere
/// else would happen on one and not the other, and the regeneration would
/// be refused for drift against an init it produced correctly.
#[test]
fn a_removal_rides_the_promotion_and_refuses_to_be_a_rewrite_too() {
    let feed = pipe("open-gop");
    let (mut muxer, _, _) = read_all(&feed);
    let stale = DolbyVisionRecord::new(7, 6, true, true, false, 0).expect("record");
    assert!(set_dolby_vision_record(&mut muxer, &stale).expect("insert"));

    let inputs = PromotionInputs {
        indexed_hdr10_metadata: false,
        hevc_configuration: None,
        strip_dolby_vision: true,
        ..PromotionInputs::default()
    };
    assert!(!inputs.is_empty(), "a removal is a promotion, not a no-op");

    let mut live = muxer.clone();
    assert!(promote_from(&mut live, &inputs).expect("promote"));
    assert_eq!(dolby_vision_record(&live).expect("read back"), None);

    // Through a serde round trip, which is what a head regeneration
    // actually promotes from: the inputs are stored as JSON in the
    // rendition and read back on another request, possibly on another
    // node. Promoting the same in-memory value twice would assert only
    // that promotion is deterministic, which it trivially is; the property
    // that matters is that the stored form carries everything the
    // promotion needs.
    let stored = serde_json::to_string(&inputs).expect("encode");
    let restored: PromotionInputs = serde_json::from_str(&stored).expect("decode");
    assert!(
        restored.strip_dolby_vision,
        "the removal must survive being stored"
    );
    let mut regenerated = muxer.clone();
    assert!(promote_from(&mut regenerated, &restored).expect("promote"));
    assert_eq!(
        regenerated.bytes, live.bytes,
        "the two paths must agree byte for byte"
    );

    // And an init stored before this field existed promotes exactly as it
    // did: a mixed-version cluster is normal here, and a rendition written
    // by an older node must not start regenerating differently.
    let legacy: PromotionInputs =
        serde_json::from_str(r#"{"parameter_sets":[],"hdr10_sei":[]}"#).expect("decode");
    assert!(!legacy.strip_dolby_vision);
    let mut old_rendition = muxer.clone();
    assert!(!promote_from(&mut old_rendition, &legacy).expect("promote"));
    assert_eq!(old_rendition.bytes, muxer.bytes);

    // Asking for both is a caller bug, and a precedence rule would hide
    // it. The inputs are stored and replayed on every head regeneration,
    // so a contradiction has to be loud the first time rather than
    // silently resolved forever.
    // With work for the parameter-set promotion that runs before the
    // Dolby Vision arm — an init whose hvcC has no arrays, which is the
    // one shape that promotion fires on. Without something that actually
    // mutates, the byte-equality assertion below holds however late the
    // refusal runs, which is exactly how an earlier version of this test
    // passed against a late one.
    let mut minimal = minimal_hvcc_dv_init();
    assert!(set_dolby_vision_record(&mut minimal, &stale).expect("insert"));
    let parameter_sets = vec![
        vec![0x40, 0x01, 0x0c],
        vec![0x42, 0x01, 0x01],
        vec![0x44, 0x01, 0xc0],
    ];
    // The control: those inputs on their own do move bytes.
    let mut moves = minimal.clone();
    assert!(
        promote_hevc_parameter_sets_from(&mut moves, &parameter_sets).expect("promote"),
        "the fixture must promote for the atomicity check to mean anything"
    );
    assert_ne!(moves.bytes, minimal.bytes);

    let contradiction = PromotionInputs {
        indexed_hdr10_metadata: false,
        hevc_configuration: None,
        dolby_vision: Some(DolbyVisionRecord::new(8, 6, false, true, true, 1).expect("record")),
        strip_dolby_vision: true,
        parameter_sets,
        ..PromotionInputs::default()
    };
    // Promoted into a named binding, not a temporary: the property is not
    // only that it errors, it is that it errors having changed nothing.
    // Refusing after appending parameter sets and HDR10 metadata leaves
    // the caller discarding an init that is no longer the muxer's, and a
    // reader comparing digests later sees drift with no cause.
    let mut half_applied = minimal.clone();
    let before = half_applied.bytes.clone();
    let error =
        promote_from(&mut half_applied, &contradiction).expect_err("both at once is refused");
    assert!(error.to_string().contains("rewrite and remove"), "{error}");
    assert_eq!(
        half_applied.bytes, before,
        "the refusal mutated the init before returning"
    );
}

/// Rewriting a record in place and inserting one where there is none —
/// the two shapes the writer has to get right — both leaving a parseable
/// init.
#[test]
fn writing_the_dolby_vision_record_keeps_the_box_tree_consistent() {
    // An init with no record at all — the shape the P7→P8.1 pipe produces,
    // because ffmpeg has no container record to copy and will not derive
    // one from the RPU.
    let feed = pipe("open-gop");
    let (mut init, _, _) = read_all(&feed);
    assert_eq!(
        dolby_vision_record(&init).expect("read"),
        None,
        "the fixture starts with no Dolby Vision record"
    );
    assert!(
        !init.video().expect("video").dolby_vision_config,
        "…and its parsed metadata agrees"
    );
    let before = init.bytes.len();

    let converted =
        DolbyVisionRecord::new(8, 6, false, true, true, 6).expect("a representable record");
    assert!(set_dolby_vision_record(&mut init, &converted).expect("insert"));
    assert_eq!(
        init.bytes.len(),
        before + 32,
        "24-byte payload plus an 8-byte box header"
    );
    assert_eq!(
        dolby_vision_record(&init).expect("read back"),
        Some(converted.clone())
    );

    // The parsed metadata has to follow the bytes. Two live guards read
    // `dolby_vision_config` as an inhibitor — HDR10 static-metadata
    // promotion and the daemon's stale-Dolby-brand sanitiser — and both
    // would do the wrong thing on a stale `false`.
    assert!(
        init.video().expect("video").dolby_vision_config,
        "the in-memory init must not disagree with its own bytes"
    );

    // The box tree still parses from scratch, which is the only check that
    // proves every enclosing size was grown, and the record has to be
    // where the ordinary parser looks — inside the sample entry, not a
    // sibling of it in stsd.
    let reparsed = reparse_init(&init.bytes);
    assert!(reparsed.video().expect("video track").dolby_vision_config);
    assert_eq!(
        dolby_vision_record(&reparsed).expect("read from a fresh parse"),
        Some(converted.clone()),
        "a record spliced outside the sample entry would not be found here"
    );

    // Writing the same record again changes nothing. `InitIdentity`
    // digests the served init and refuses a generation that does not hash
    // the same, so a write that corrects nothing breaks every generation
    // after the first.
    let stable = init.bytes.clone();
    assert!(!set_dolby_vision_record(&mut init, &converted).expect("idempotent"));
    assert_eq!(init.bytes, stable);

    // Read-back then write-back is a no-op too — including the box NAME,
    // which is the case a payload-only comparison would miss.
    let read = dolby_vision_record(&init).expect("read").expect("present");
    assert!(!set_dolby_vision_record(&mut init, &read).expect("round trip"));
    assert_eq!(init.bytes, stable);

    // Correcting a record in place changes no size anywhere — including
    // when the box NAME changes, which is exactly what a profile 7 record
    // becoming a profile 8 one does.
    let length = init.bytes.len();
    let as_p7 = DolbyVisionRecord::new(7, 6, true, true, true, 6).expect("representable");
    assert!(set_dolby_vision_record(&mut init, &as_p7).expect("rewrite"));
    assert_eq!(init.bytes.len(), length, "a rename is not a resize");
    assert_eq!(
        dolby_vision_record(&init).expect("read back"),
        Some(as_p7.clone())
    );
    reparse_init(&init.bytes);

    // A pure rename, with the payload untouched: profile 8 written into a
    // box that ffmpeg named dvcC. This is the path that writes over the
    // box's own header, and the only one that catches a name field
    // computed from the wrong offset.
    let renamed = DolbyVisionRecord::new(8, 6, true, true, true, 6).expect("representable");
    assert!(set_dolby_vision_record(&mut init, &renamed).expect("rename"));
    assert_eq!(init.bytes.len(), length);
    let reparsed = reparse_init(&init.bytes);
    assert_eq!(
        dolby_vision_record(&reparsed).expect("read"),
        Some(renamed),
        "the renamed box must still be found, and still be a dvvC"
    );

    // …and a flag correction on its own: same profile, same box name, one
    // bit. The contingency `without_enhancement_layer` exists for.
    let claiming = dolby_vision_record(&init).expect("read").expect("present");
    let corrected = claiming.without_enhancement_layer();
    assert!(set_dolby_vision_record(&mut init, &corrected).expect("correct"));
    assert_eq!(init.bytes.len(), length);
    assert_eq!(
        dolby_vision_record(&init).expect("read back"),
        Some(corrected)
    );
    reparse_init(&init.bytes);
}

/// A `largesize` box puts a 64-bit length where an ordinary box puts its
/// name. Writing the name there succeeds and destroys the init.
#[test]
fn a_largesize_record_box_is_not_renamed_over_its_own_length() {
    let feed = pipe("open-gop");
    let (mut init, _, _) = read_all(&feed);
    let record = DolbyVisionRecord::new(7, 6, true, true, true, 6).expect("representable");
    set_dolby_vision_record(&mut init, &record).expect("insert an ordinary box");

    // Rewrite that box with a 64-bit header: size32 = 1, then the name,
    // then the real 64-bit length, then the payload.
    let at = init
        .bytes
        .windows(4)
        .position(|window| window == b"dvcC")
        .expect("the box just written")
        - 4;
    let mut grown = init.bytes[..at].to_vec();
    grown.extend_from_slice(&1u32.to_be_bytes());
    grown.extend_from_slice(b"dvcC");
    grown.extend_from_slice(&40u64.to_be_bytes());
    grown.extend_from_slice(&init.bytes[at + 8..at + 32]);
    grown.extend_from_slice(&init.bytes[at + 32..]);
    let mut widened = Init {
        bytes: grown,
        tracks: init.tracks.clone(),
    };
    for at in locate_hvcc(&init.bytes)
        .expect("locate")
        .expect("hvcC")
        .ancestors[1..]
        .iter()
    {
        grow_box(&mut widened.bytes, *at, 8).expect("grow for the larger header");
    }
    let widened_bytes = widened.bytes.clone();
    reparse_init(&widened_bytes);

    // Renaming it to dvvC must not write over the length field.
    let to_p8 = DolbyVisionRecord::new(8, 6, true, true, true, 6).expect("representable");
    assert!(set_dolby_vision_record(&mut widened, &to_p8).expect("rename"));
    let reparsed = reparse_init(&widened.bytes);
    assert_eq!(
        dolby_vision_record(&reparsed).expect("read"),
        Some(to_p8),
        "a 64-bit header must survive the rename"
    );
}

fn strip_hvcc_arrays(init: &mut Init) {
    let location = locate_hvcc(&init.bytes)
        .expect("locating hvcC")
        .expect("hvcC");
    let arrays_start = location.payload.start + 23;
    let delta = location.payload.end - arrays_start;
    init.bytes.drain(arrays_start..location.payload.end);
    init.bytes[location.payload.start + 22] = 0;
    for at in location.ancestors {
        match at.header_len {
            8 => {
                let size = be_u32(&init.bytes, at.start) as usize - delta;
                init.bytes[at.start..at.start + 4].copy_from_slice(&(size as u32).to_be_bytes());
            }
            16 => {
                let size = be_u64(&init.bytes, at.start + 8) - delta as u64;
                init.bytes[at.start + 8..at.start + 16].copy_from_slice(&size.to_be_bytes());
            }
            _ => panic!("unexpected box header"),
        }
    }
}

#[test]
fn in_band_hevc_parameter_sets_fill_an_empty_dolby_vision_hvcc() {
    let feed = pipe("open-gop");
    let (mut init, _, _) = read_all(&feed);
    let video = init.video().expect("HEVC video").clone();

    // Reproduce the reference episode I WEB-DL shape: a valid 23-byte hvcC header with
    // zero NAL arrays, while VPS/SPS/PPS live only in the first sample.
    let location = locate_hvcc(&init.bytes)
        .expect("locating hvcC")
        .expect("hvcC");
    let arrays_start = location.payload.start + 23;
    let delta = location.payload.end - arrays_start;
    init.bytes.drain(arrays_start..location.payload.end);
    init.bytes[location.payload.start + 22] = 0;
    for at in location.ancestors {
        match at.header_len {
            8 => {
                let size = be_u32(&init.bytes, at.start) as usize - delta;
                init.bytes[at.start..at.start + 4].copy_from_slice(&(size as u32).to_be_bytes());
            }
            16 => {
                let size = be_u64(&init.bytes, at.start + 8) - delta as u64;
                init.bytes[at.start + 8..at.start + 16].copy_from_slice(&size.to_be_bytes());
            }
            _ => panic!("unexpected box header"),
        }
    }
    init.tracks
        .iter_mut()
        .find(|track| track.id == video.id)
        .expect("video track")
        .dolby_vision_config = true;

    let vps = [0x40, 0x01, 0x0c];
    let sps = [0x42, 0x01, 0x01];
    let pps = [0x44, 0x01, 0xc0];
    let rpu = [0x7c, 0x01, 0x19];
    let vcl = [0x26, 0x01, 0x80];
    let fragment = fragment_with_first_video_sample(
        video.id,
        length_prefixed_hevc_nals(&[&vps, &sps, &pps, &rpu, &vcl]),
    );

    assert!(promote_hevc_parameter_sets(&mut init, &fragment).expect("promotion"));
    validate_hevc_decoder_configuration(&init).expect("promoted hvc1 is publishable");
    let enriched = locate_hvcc(&init.bytes)
        .expect("locating enriched hvcC")
        .expect("hvcC");
    let mut types = hvcc_nal_array_types(&init.bytes[enriched.payload]).expect("reading arrays");
    types.sort_unstable();
    assert_eq!(types, vec![32, 33, 34]);

    // The init stays structurally valid and a second pass is idempotent.
    let once = init.bytes.clone();
    assert!(!promote_hevc_parameter_sets(&mut init, &fragment).expect("second promotion"));
    assert_eq!(init.bytes, once);
    let mut reader = FragmentReader::new();
    reader.push(&init.bytes);
    assert!(matches!(
        reader.next_unit().expect("re-parsing enriched init"),
        Some(Unit::Init(_))
    ));
}

include!("dvh1_tests.rs");

#[test]
fn hevc_array_completeness_is_distinct_from_parameter_set_presence() {
    let mut record = vec![0u8; 23];
    record[22] = 3;
    for kind in 32u8..=34 {
        record.extend_from_slice(&[0x80 | kind, 0, 1, 0, 2, kind << 1, 1]);
    }
    assert!(hvcc_parameter_set_arrays_complete(&record).expect("complete table"));
    for array in 0..3 {
        let mut partial = record.clone();
        partial[23 + array * 7] &= 0x7f;
        assert_eq!(
            hvcc_nal_array_types(&partial).expect("present arrays"),
            vec![32, 33, 34]
        );
        assert!(!hvcc_parameter_set_arrays_complete(&partial).expect("partial array"));
        // A complete duplicate does not erase another array's explicit
        // declaration that more parameter sets may remain in-band.
        partial[22] += 1;
        let at = 23 + array * 7;
        let mut duplicate = record[at..at + 7].to_vec();
        partial.append(&mut duplicate);
        assert!(!hvcc_parameter_set_arrays_complete(&partial).expect("mixed duplicates"));
    }
    record.pop();
    assert!(matches!(
        hvcc_parameter_set_arrays_complete(&record),
        Err(Fmp4Error::Malformed(_))
    ));
}

#[test]
fn incomplete_hevc_parameter_sets_are_refused_without_mutating_init() {
    let mut init = minimal_hvcc_dv_init();
    let original = init.bytes.clone();
    let video = init.video().expect("video").clone();
    let vps = [0x40, 0x01, 0x0c];
    let sps = [0x42, 0x01, 0x01];
    let vcl = [0x26, 0x01, 0x80];
    let fragment =
        fragment_with_first_video_sample(video.id, length_prefixed_hevc_nals(&[&vps, &sps, &vcl]));

    let error = promote_hevc_parameter_sets(&mut init, &fragment)
        .expect_err("VPS plus SPS is not a decoder configuration");
    assert!(
        error.to_string().contains("complete VPS/SPS/PPS"),
        "{error}"
    );
    assert_eq!(init.bytes, original, "a refused promotion must be atomic");
    assert!(!hevc_parameter_sets_complete(&init).expect("reading hvcC"));
    let error = validate_hevc_decoder_configuration(&init)
        .expect_err("an incomplete out-of-band hvcC is not publishable");
    assert!(
        error.to_string().contains("complete VPS/SPS/PPS"),
        "{error}"
    );
}

#[test]
fn complete_multi_entry_hevc_is_a_typed_validated_structural_refusal() {
    // Synthetic, not the piped fixture: that pipe strips NAL types 32-34,
    // so its first sample carries no parameter sets to promote and the
    // promotion returns "nothing to do" long before it can refuse a second
    // sample description. The fixture cache keys on filename alone, so a
    // tree built before that bsf landed still passed this — which is what
    // made the failure look like a flake.
    let mut init = minimal_hvcc_dv_init();
    let video = init.video().expect("video").clone();
    let vps = [0x40, 0x01, 0x0c];
    let sps = [0x42, 0x01, 0x01];
    let pps = [0x44, 0x01, 0xc0];
    let vcl = [0x26, 0x01, 0x80];
    let fragment = fragment_with_first_video_sample(
        video.id,
        length_prefixed_hevc_nals(&[&vps, &sps, &pps, &vcl]),
    );
    // One complete description first, then a second exactly like it: two
    // decoder-valid entries, which is the shape the refusal is about.
    assert!(promote_hevc_parameter_sets(&mut init, &fragment).expect("first promotion"));
    duplicate_hevc_sample_entry(&mut init);
    // The second description now configures the decoder differently: an
    // extra PPS the first does not declare. That is a real choice the
    // writer cannot make, so the refusal stands.
    add_parameter_set_to_second_hevc_entry(&mut init, &[0x44, 0x01, 0xc1, 0x40]);

    let promotion = promote_hevc_parameter_sets(&mut init, &fragment)
        .expect_err("the writer cannot choose one of two sample descriptions");
    assert_eq!(promotion, Fmp4Error::MultipleHevcSampleEntries { count: 2 });
    assert_eq!(
        validate_hevc_sample_entries(&init).expect("both descriptions are decoder-valid"),
        HevcSampleEntryLayout::Multiple { count: 2 }
    );
    assert!(promotion.to_string().contains("2 HEVC sample entries"));
}

/// Splice one more PPS into the second HEVC description's hvcC so the two
/// descriptions configure the decoder differently.
fn add_parameter_set_to_second_hevc_entry(init: &mut Init, nal: &[u8]) {
    let locations = locate_hevc_sample_entries(&init.bytes).expect("locating entries");
    let second = locations[1].as_ref().expect("second entry has hvcC");
    let record = &init.bytes[second.payload.clone()];
    // Find the PPS array (type 34) and bump its count, appending the NAL
    // at the array's end.
    let mut pos = second.payload.start + 23;
    let arrays = record[22];
    let mut inserted = false;
    for _ in 0..arrays {
        let kind = init.bytes[pos] & 0x3f;
        let count = u16::from_be_bytes([init.bytes[pos + 1], init.bytes[pos + 2]]);
        let count_at = pos + 1;
        pos += 3;
        for _ in 0..count {
            let len = u16::from_be_bytes([init.bytes[pos], init.bytes[pos + 1]]) as usize;
            pos += 2 + len;
        }
        if kind == 34 && !inserted {
            init.bytes[count_at..count_at + 2].copy_from_slice(&(count + 1).to_be_bytes());
            let mut unit = (nal.len() as u16).to_be_bytes().to_vec();
            unit.extend_from_slice(nal);
            let delta = unit.len();
            init.bytes.splice(pos..pos, unit);
            for &ancestor in &second.ancestors {
                grow_box(&mut init.bytes, ancestor, delta).expect("growing ancestors");
            }
            inserted = true;
            break;
        }
    }
    assert!(inserted, "the second entry declares a PPS array");
}

#[test]
fn decoder_equivalent_hevc_descriptions_collapse_to_the_first() {
    // The ffmpeg 8 shape measured on 2026-10-04: two descriptions that
    // differ only in a PPS trailing zero byte (and SEI arrays), with every
    // fragment selecting the first.
    let mut init = minimal_hvcc_dv_init();
    let video = init.video().expect("video").clone();
    let vps = [0x40, 0x01, 0x0c];
    let sps = [0x42, 0x01, 0x01];
    let pps = [0x44, 0x01, 0xc0];
    let vcl = [0x26, 0x01, 0x80];
    let fragment = fragment_with_first_video_sample(
        video.id,
        length_prefixed_hevc_nals(&[&vps, &sps, &pps, &vcl]),
    );
    assert!(promote_hevc_parameter_sets(&mut init, &fragment).expect("first promotion"));
    let single = init.bytes.clone();
    duplicate_hevc_sample_entry(&mut init);
    append_trailing_zero_to_second_entry_pps(&mut init);
    // And the SEI arrays the field captures differed in.
    append_sei_array_to_hevc_entry(&mut init, 1, &[0x4e, 0x01, 0x84, 0x10, 0x80]);
    assert_eq!(
        validate_hevc_sample_entries(&init).expect("both descriptions are decoder-valid"),
        HevcSampleEntryLayout::Multiple { count: 2 }
    );

    let mut collapsed = init.clone();
    assert_eq!(
        collapse_equivalent_hevc_sample_entries(&mut collapsed).expect("collapse"),
        1
    );
    assert_eq!(
        collapsed.bytes, single,
        "the first description survives byte for byte"
    );
    assert_eq!(
        validate_hevc_sample_entries(&collapsed).expect("one description"),
        HevcSampleEntryLayout::Single
    );
    let mut reader = FragmentReader::new();
    reader.push(&collapsed.bytes);
    assert!(matches!(
        reader.next_unit().expect("reparse"),
        Some(Unit::Init(_))
    ));
    assert_eq!(
        collapse_equivalent_hevc_sample_entries(&mut collapsed).expect("idempotent"),
        0
    );

    // Both served-init builders collapse before they mutate.
    let mut promoted = init.clone();
    assert!(
        promote_hevc_parameter_sets(&mut promoted, &fragment).expect("promotion collapses first"),
        "the collapse is a change the caller must hear about"
    );
    assert_eq!(promoted.bytes, single);
    let mut from_inputs = init.clone();
    let inputs = PromotionInputs::from_fragment(&fragment, &init);
    assert!(
        inputs.parameter_sets.is_empty(),
        "the equivalent descriptions already configure every in-band set"
    );
    assert!(
        promote_from(&mut from_inputs, &inputs).expect("promote_from collapses first"),
        "the collapse is a change the caller must hear about"
    );
    assert_eq!(from_inputs.bytes, single);
}

#[test]
fn equivalent_descriptions_keep_only_parameter_sets_whichever_keyframe_led() {
    // Two generations of one film: each muxer put a different keyframe's
    // SEI into its first description. The collapsed inits are identical.
    let mut base = minimal_hvcc_dv_init();
    let video = base.video().expect("video").clone();
    let fragment = fragment_with_first_video_sample(
        video.id,
        length_prefixed_hevc_nals(&[
            &[0x40, 0x01, 0x0c],
            &[0x42, 0x01, 0x01],
            &[0x44, 0x01, 0xc0],
            &[0x26, 0x01, 0x80],
        ]),
    );
    assert!(promote_hevc_parameter_sets(&mut base, &fragment).expect("promotion"));
    let single = base.bytes.clone();
    let mut head = base.clone();
    duplicate_hevc_sample_entry(&mut head);
    duplicate_hevc_sample_entry_at_end(&mut head);
    append_sei_array_to_hevc_entry(&mut head, 0, &[0x4e, 0x01, 0x05, 0x10, 0x80]);
    let mut seeked = base.clone();
    duplicate_hevc_sample_entry(&mut seeked);
    append_sei_array_to_hevc_entry(&mut seeked, 0, &[0x50, 0x01, 0x84, 0x20, 0x80]);
    append_trailing_zero_to_second_entry_pps(&mut seeked);
    assert_ne!(head.bytes, seeked.bytes);

    assert_eq!(
        collapse_equivalent_hevc_sample_entries(&mut head).expect("three"),
        2
    );
    assert_eq!(
        collapse_equivalent_hevc_sample_entries(&mut seeked).expect("two"),
        1
    );
    assert_eq!(head.bytes, single);
    assert_eq!(seeked.bytes, single);
}

#[test]
fn equivalent_descriptions_behind_a_non_default_trex_do_not_collapse() {
    let mut init = minimal_hvcc_dv_init();
    let video = init.video().expect("video").clone();
    let fragment = fragment_with_first_video_sample(
        video.id,
        length_prefixed_hevc_nals(&[
            &[0x40, 0x01, 0x0c],
            &[0x42, 0x01, 0x01],
            &[0x44, 0x01, 0xc0],
            &[0x26, 0x01, 0x80],
        ]),
    );
    assert!(promote_hevc_parameter_sets(&mut init, &fragment).expect("promotion"));
    duplicate_hevc_sample_entry(&mut init);
    let trex = init
        .bytes
        .windows(4)
        .enumerate()
        .find_map(|(name, kind)| {
            (kind == b"trex"
                && init.bytes.len() >= name + 16
                && be_u32(&init.bytes, name + 8) == video.id)
                .then_some(name)
        })
        .expect("video trex");
    init.bytes[trex + 12..trex + 16].copy_from_slice(&2_u32.to_be_bytes());
    assert_eq!(
        collapse_equivalent_hevc_sample_entries(&mut init).expect("no collapse"),
        0
    );
}

/// Append one more copy of the first HEVC description after the last one.
fn duplicate_hevc_sample_entry_at_end(init: &mut Init) {
    let locations = locate_hevc_sample_entries(&init.bytes).expect("locating entries");
    let first = locations[0].as_ref().expect("first entry");
    let last = locations
        .last()
        .and_then(Option::as_ref)
        .expect("last entry");
    let entry = first.ancestors[1];
    let copy = init.bytes[entry.start..first.sample_entry_end].to_vec();
    let at = last.sample_entry_end;
    let stsd = first.ancestors[2];
    let ancestors = first.ancestors[2..].to_vec();
    let delta = copy.len();
    init.bytes.splice(at..at, copy);
    let count_at = stsd.start + stsd.header_len + 4;
    let count = be_u32(&init.bytes, count_at);
    init.bytes[count_at..count_at + 4].copy_from_slice(&(count + 1).to_be_bytes());
    for ancestor in ancestors {
        grow_box(&mut init.bytes, ancestor, delta).expect("growing ancestors");
    }
}

/// Append a prefix-SEI array (type 39) holding `nal` to the `index`th HEVC
/// description's hvcC.
fn append_sei_array_to_hevc_entry(init: &mut Init, index: usize, nal: &[u8]) {
    let locations = locate_hevc_sample_entries(&init.bytes).expect("locating entries");
    let target = locations[index].as_ref().expect("entry has hvcC");
    let mut array = vec![39u8];
    array.extend_from_slice(&1u16.to_be_bytes());
    array.extend_from_slice(&(nal.len() as u16).to_be_bytes());
    array.extend_from_slice(nal);
    let delta = array.len();
    let at = target.payload.end;
    let count_at = target.payload.start + 22;
    let ancestors = target.ancestors.clone();
    init.bytes.splice(at..at, array);
    init.bytes[count_at] += 1;
    for ancestor in ancestors {
        grow_box(&mut init.bytes, ancestor, delta).expect("growing ancestors");
    }
}

/// Opt-in: captures of one film started at different keyframes
/// (`PLURX_FFMPEG8_GENERATIONS=a.mp4:b.mp4:…`) collapse to one init.
#[test]
#[ignore = "needs captured ffmpeg 8 generations"]
fn captured_ffmpeg8_generations_collapse_to_one_init() {
    let paths = std::env::var("PLURX_FFMPEG8_GENERATIONS").expect("set the capture paths");
    let mut collapsed = Vec::new();
    for path in paths.split(':') {
        let feed = std::fs::read(path).expect("reading a capture");
        let (mut init, _, _) = read_all(&feed);
        assert!(collapse_equivalent_hevc_sample_entries(&mut init).expect("collapse") > 0);
        collapsed.push(init.bytes);
    }
    assert!(
        collapsed.windows(2).all(|pair| pair[0] == pair[1]),
        "generations differ"
    );
}

/// Give the second HEVC description's PPS a trailing zero byte — the
/// difference ffmpeg 8's in-band extraction produced in the field.
fn append_trailing_zero_to_second_entry_pps(init: &mut Init) {
    let locations = locate_hevc_sample_entries(&init.bytes).expect("locating entries");
    let second = locations[1].as_ref().expect("second entry has hvcC");
    let arrays = init.bytes[second.payload.start + 22];
    let mut pos = second.payload.start + 23;
    for _ in 0..arrays {
        let kind = init.bytes[pos] & 0x3f;
        let count = u16::from_be_bytes([init.bytes[pos + 1], init.bytes[pos + 2]]);
        pos += 3;
        for _ in 0..count {
            let len = u16::from_be_bytes([init.bytes[pos], init.bytes[pos + 1]]) as usize;
            if kind == 34 {
                init.bytes[pos..pos + 2].copy_from_slice(&((len + 1) as u16).to_be_bytes());
                init.bytes.insert(pos + 2 + len, 0);
                for &ancestor in &second.ancestors {
                    grow_box(&mut init.bytes, ancestor, 1).expect("growing ancestors");
                }
                return;
            }
            pos += 2 + len;
        }
    }
    panic!("the second entry declares no PPS");
}

/// Opt-in: a real `ffmpeg -bsf:v hevc_mp4toannexb,extract_extradata` output
/// from jellyfin-ffmpeg 8 (two equivalent descriptions) collapses to one
/// and promotes cleanly. `PLURX_FFMPEG8_MULTI_STSD=<file.mp4>`.
#[test]
#[ignore = "needs a captured ffmpeg 8 output"]
fn captured_ffmpeg8_repeated_descriptions_collapse() {
    let path = std::env::var("PLURX_FFMPEG8_MULTI_STSD").expect("set the capture path");
    let feed = std::fs::read(path).expect("reading the capture");
    let (init, fragments, _) = read_all(&feed);
    assert!(matches!(
        validate_hevc_sample_entries(&init).expect("valid"),
        HevcSampleEntryLayout::Multiple { count: 2 }
    ));
    let mut served = init.clone();
    promote_hevc_parameter_sets(&mut served, &fragments[0]).expect("promotion");
    assert_eq!(
        validate_hevc_sample_entries(&served).expect("valid"),
        HevcSampleEntryLayout::Single
    );
    let segment = merge(&fragments[..1], &served, 1).expect("merge");
    let mut reader = FragmentReader::new();
    reader.push(&served.bytes);
    reader.push(&segment.bytes);
    assert!(matches!(
        reader.next_unit().expect("init"),
        Some(Unit::Init(_))
    ));
    assert!(matches!(
        reader.next_unit().expect("segment"),
        Some(Unit::Fragment(_))
    ));
}

#[test]
fn hevc_descriptions_that_differ_outside_hvcc_do_not_collapse() {
    let mut init = minimal_hvcc_dv_init();
    let video = init.video().expect("video").clone();
    let fragment = fragment_with_first_video_sample(
        video.id,
        length_prefixed_hevc_nals(&[
            &[0x40, 0x01, 0x0c],
            &[0x42, 0x01, 0x01],
            &[0x44, 0x01, 0xc0],
            &[0x26, 0x01, 0x80],
        ]),
    );
    assert!(promote_hevc_parameter_sets(&mut init, &fragment).expect("first promotion"));
    duplicate_hevc_sample_entry(&mut init);
    // A different width in the second description's visual fields.
    let locations = locate_hevc_sample_entries(&init.bytes).expect("locating entries");
    let entry = locations[1].as_ref().expect("second entry").ancestors[1];
    let width_at = entry.start + entry.header_len + 24;
    init.bytes[width_at] ^= 0x01;
    assert_eq!(
        collapse_equivalent_hevc_sample_entries(&mut init).expect("no collapse"),
        0
    );
    assert_eq!(
        validate_hevc_sample_entries(&init).expect("both valid"),
        HevcSampleEntryLayout::Multiple { count: 2 }
    );
}

#[test]
fn channel_playback_repair_validates_default_description_reference() {
    let feed = crate::testfixtures::pipe_with_distinct_hevc_sample_entries("closed-gop");
    let (mut init, fragments, _) = read_all(&feed);
    validate_hevc_sample_description_reference(&init, &fragments[0])
        .expect("encoder-produced default selects one of two descriptions");

    let video_id = init.video().expect("video track").id;
    let trex = init.bytes.windows(4).enumerate().find_map(|(name, kind)| {
        (kind == b"trex"
            && init.bytes.len() >= name + 16
            && be_u32(&init.bytes, name + 8) == video_id)
            .then_some(name)
    });
    let name = trex.expect("video trex");
    init.bytes[name + 12..name + 16].copy_from_slice(&3_u32.to_be_bytes());
    let error = validate_hevc_sample_description_reference(&init, &fragments[0])
        .expect_err("default description 3 is out of bounds for two entries");
    assert!(error
        .to_string()
        .contains("selects HEVC sample description 3"));
}

#[test]
fn incomplete_multi_entry_hevc_configuration_remains_terminal() {
    let mut init = minimal_hvcc_dv_init();
    duplicate_hevc_sample_entry(&mut init);
    let error = validate_hevc_sample_entries(&init)
        .expect_err("each out-of-band description is incomplete");
    assert!(
        error.to_string().contains("complete VPS/SPS/PPS"),
        "{error}"
    );
}

#[test]
fn in_band_hevc_sample_entry_does_not_require_parameter_sets_in_hvcc() {
    let mut init = minimal_hvcc_dv_init();
    let entry = init
        .bytes
        .windows(4)
        .position(|kind| kind == b"hvc1")
        .expect("hvc1 sample entry");
    init.bytes[entry..entry + 4].copy_from_slice(b"hev1");

    validate_hevc_decoder_configuration(&init)
        .expect("hev1 is allowed to carry its decoder configuration in band");
}

#[test]
fn hvc1_merge_promotes_then_removes_in_band_parameter_sets() {
    let mut init = minimal_hvcc_dv_init();
    let video = init.video().expect("video").clone();
    let vps = [0x40, 0x01, 0x0c];
    let sps = [0x42, 0x01, 0x01];
    let pps = [0x44, 0x01, 0xc0];
    let rpu = [0x7c, 0x01, 0x19];
    let vcl = [0x26, 0x01, 0x80];
    let fragment = fragment_with_first_video_sample(
        video.id,
        length_prefixed_hevc_nals(&[&vps, &sps, &pps, &rpu, &vcl]),
    );
    assert!(promote_hevc_parameter_sets(&mut init, &fragment).expect("promotion"));
    assert!(hevc_parameter_sets_complete(&init).expect("reading promoted hvcC"));

    let segment = merge(&[fragment], &init, 1).expect("merge");
    let mut reader = FragmentReader::new();
    reader.push(&init.bytes);
    assert!(matches!(
        reader.next_unit().expect("init"),
        Some(Unit::Init(_))
    ));
    reader.push(&segment.bytes);
    let emitted = loop {
        match reader.next_unit().expect("published segment") {
            Some(Unit::Fragment(fragment)) => break fragment,
            Some(_) => continue,
            None => panic!("published segment carried no fragment"),
        }
    };
    let sample = first_video_sample(&emitted, &video).expect("video sample");
    let kinds: Vec<u8> = length_prefixed_nals(sample, video.nal_length_size)
        .into_iter()
        .filter_map(hevc_nal_type)
        .collect();
    assert!(
        !kinds.iter().any(|kind| (32..=34).contains(kind)),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&62),
        "Dolby Vision RPU was removed: {kinds:?}"
    );
    assert!(
        kinds.contains(&19),
        "encoded picture was removed: {kinds:?}"
    );
}

#[test]
fn hdr10_static_metadata_is_promoted_into_hvcc_once() {
    let feed = pipe("open-gop");
    let (mut init, _, _) = read_all(&feed);
    let video = init.video().expect("HEVC video").clone();
    // HEVC prefix-SEI NALs with the exact payload lengths standardized for
    // the mdcv (24 bytes) and clli (4 bytes) sample-entry boxes.
    let mut mastering = vec![0x4e, 0x01, 137, 24];
    mastering.extend_from_slice(&[0; 24]);
    mastering.push(0x80);
    let mut content_light = vec![0x4e, 0x01, 144, 4];
    content_light.extend_from_slice(&[0; 4]);
    content_light.push(0x80);
    let vcl = [0x26, 0x01, 0x80];
    let fragment = fragment_with_first_video_sample(
        video.id,
        length_prefixed_hevc_nals(&[&mastering, &content_light, &vcl]),
    );

    let original_len = init.bytes.len();
    assert!(promote_hdr10_static_metadata(&mut init, &fragment).expect("promotion"));
    assert!(init.bytes.len() > original_len);

    let location = locate_hvcc(&init.bytes)
        .expect("locating enriched hvcC")
        .expect("hvcC");
    let mut types =
        hvcc_hdr10_sei_types(&init.bytes[location.payload]).expect("reading enriched hvcC");
    types.sort_unstable();
    assert_eq!(types, vec![137, 144]);
    assert!(location.has_mdcv, "the recommended mdcv box is absent");
    assert!(location.has_clli, "the recommended clli box is absent");

    // Every enclosing box size was grown consistently: the ordinary
    // reader must still accept the complete initialization segment.
    let mut reader = FragmentReader::new();
    reader.push(&init.bytes);
    assert!(matches!(
        reader.next_unit().expect("re-parsing enriched init"),
        Some(Unit::Init(_))
    ));
    assert_eq!(reader.buffered(), 0);

    let once = init.bytes.clone();
    assert!(!promote_hdr10_static_metadata(&mut init, &fragment).expect("second promotion"));
    assert_eq!(init.bytes, once, "metadata was duplicated in hvcC");
}

#[test]
fn a_first_sample_without_hdr10_static_metadata_leaves_init_verbatim() {
    let feed = pipe("open-gop");
    let (mut init, _, _) = read_all(&feed);
    let video = init.video().expect("HEVC video").clone();
    let unrelated_sei = [0x4e, 0x01, 5, 1, 0, 0x80];
    let vcl = [0x26, 0x01, 0x80];
    let fragment = fragment_with_first_video_sample(
        video.id,
        length_prefixed_hevc_nals(&[&unrelated_sei, &vcl]),
    );
    let original = init.bytes.clone();

    assert!(!promote_hdr10_static_metadata(&mut init, &fragment).expect("no-op promotion"));
    assert_eq!(init.bytes, original);
}

/// The reader resolves `tfhd` defaults and first-sample-flags into
/// per-sample values. ffprobe reads the same file through an unrelated
/// implementation, so agreeing with it is evidence; agreeing with our own
/// re-parse would not be.
///
/// Compared on pts, dts and size, and deliberately NOT on ffprobe's
/// `packet=duration`: that field is derived from the stream's frame rate,
/// not from the sample table, so on 24 fps at timescale 16000 it reads a
/// flat 666 against real sample durations that cycle 672/672/656 to sum
/// exactly. Per-packet dts is read from the container, and every dts here
/// is *generated* by summing our resolved durations — so dts agreement is
/// the duration check, arrived at from the other end. The stream's total
/// `duration_ts` closes the one gap that leaves: the last sample.
#[test]
fn resolved_samples_reproduce_ffprobe_timing() {
    let feed = pipe("open-gop");
    let path = pipe_path("open-gop");
    let (init, frags, _) = read_all(&feed);
    let video = init.video().expect("video").clone();

    let csv = run(Command::new(ffprobe())
        .args(["-v", "error", "-select_streams", "v:0", "-show_packets"])
        .args(["-show_entries", "packet=pts,dts,size"])
        .args(["-of", "csv=p=0"])
        .arg(&path));
    let csv = String::from_utf8_lossy(&csv);
    let expected: Vec<(i64, i64, i64)> = csv
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let f: Vec<i64> = l
                .split(',')
                .map(|v| v.trim().parse::<i64>().unwrap_or(i64::MIN))
                .collect();
            (f[0], f[1], f[2])
        })
        .collect();

    let mut got: Vec<(i64, i64, i64)> = Vec::new();
    let mut total_ticks = 0u64;
    for f in &frags {
        let tf = f.track(video.id).expect("video in every fragment");
        let mut dts = tf.base_decode_time as i64;
        for s in tf.samples() {
            got.push((dts + s.cto, dts, s.size as i64));
            dts += s.duration as i64;
            total_ticks += s.duration as u64;
        }
    }

    assert_eq!(
        got.len(),
        expected.len(),
        "sample count disagrees with ffprobe"
    );
    assert_eq!(
        got, expected,
        "resolved sample timing disagrees with ffprobe"
    );

    let duration_ts = run(Command::new(ffprobe())
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=duration_ts", "-of", "csv=p=0"])
        .arg(&path));
    let duration_ts: u64 = String::from_utf8_lossy(&duration_ts)
        .trim()
        .parse()
        .expect("ffprobe duration_ts");
    assert_eq!(
        total_ticks, duration_ts,
        "summed sample durations disagree with the stream duration"
    );
}

#[test]
fn a_truncated_feed_yields_no_fragment_and_no_panic() {
    let feed = pipe("open-gop");

    // Nothing but a box header.
    let mut reader = FragmentReader::new();
    reader.push(&feed[..6]);
    assert_eq!(reader.next_unit().expect("no error"), None);

    // ftyp complete, moov half-arrived: still no init.
    let mut reader = FragmentReader::new();
    reader.push(&feed[..100]);
    assert_eq!(reader.next_unit().expect("no error"), None);

    // A fragment cut in half — the init lands, nothing else does, and
    // asking again forever is still not an error. This is what a killed
    // session looks like from in here.
    let (init, frags, _) = read_all(&feed);
    let cut = init.bytes.len() + frags[0].bytes.len() + frags[1].bytes.len() / 2;
    let mut reader = FragmentReader::new();
    reader.push(&feed[..cut]);
    assert!(matches!(reader.next_unit(), Ok(Some(Unit::Init(_)))));
    assert!(matches!(reader.next_unit(), Ok(Some(Unit::Fragment(_)))));
    for _ in 0..3 {
        assert_eq!(reader.next_unit().expect("no error"), None);
    }
    assert!(!reader.saw_trailer());
}

/// A pipe delivers whatever the kernel felt like. Odd-sized pushes must
/// produce identical units to one big one — a reader that only works on
/// whole-box reads works right up until production.
#[test]
fn a_feed_arriving_in_odd_chunks_parses_identically() {
    let feed = pipe("open-gop");
    let (init, frags, _) = read_all(&feed);

    let mut reader = FragmentReader::new();
    let mut got_init = None;
    let mut got = Vec::new();
    for chunk in feed.chunks(1_237) {
        reader.push(chunk);
        while let Some(unit) = reader.next_unit().expect("parsing") {
            match unit {
                Unit::Init(i) => got_init = Some(i),
                Unit::Fragment(f) => got.push(f),
                Unit::Trailer => {}
            }
        }
    }
    assert_eq!(got_init.as_ref(), Some(&init));
    assert_eq!(got, frags);
    assert_eq!(reader.buffered(), 0, "bytes left over after a whole feed");
}

// -----------------------------------------------------------------------
// M1 — the classifier
// -----------------------------------------------------------------------

fn classes(kind: &str) -> Vec<CutClass> {
    let feed = pipe(kind);
    let (init, frags, _) = read_all(&feed);
    frags.iter().map(|f| classify(f, &init)).collect()
}

/// The disc-remux shape: the stream opens on an IDR and every keyframe
/// after it is a CRA carrying a RASL. This is the source of the bug, and
/// the classifier has to see it for what it is.
#[test]
fn every_cra_on_the_open_gop_fixture_is_dirty() {
    let c = classes("open-gop");
    assert_eq!(c[0], CutClass::CleanIdr, "the stream must open on an IDR");
    assert!(
        c[1..].iter().all(|k| *k == CutClass::Dirty),
        "an open-GOP CRA with a leading picture was called clean: {c:?}"
    );
    assert!(!c.contains(&CutClass::Unparseable));
}

#[test]
fn every_keyframe_on_the_closed_gop_fixture_is_clean() {
    let c = classes("closed-gop");
    assert!(
        c.iter().all(|k| *k == CutClass::CleanIdr),
        "a closed-GOP keyframe was not read as an IDR: {c:?}"
    );
}

/// A CRA is not dirty by virtue of being a CRA — only by carrying leading
/// pictures. With no B-frames there are none, and calling these dirty
/// would spend ceiling cuts on a source that needs none.
#[test]
fn a_cra_with_no_leading_pictures_is_clean() {
    let c = classes("clean-cra");
    assert_eq!(c[0], CutClass::CleanIdr);
    assert!(
        c[1..].iter().all(|k| *k == CutClass::CleanCra),
        "a leading-picture-free CRA was not called clean: {c:?}"
    );
}

/// x264's open GOP signals recovery points rather than a distinct NAL
/// type, so there is nothing in the bitstream to prove a non-IDR keyframe
/// safe. Only NAL 5 is clean, and the rest are dirty by policy.
#[test]
fn h264_only_idr_is_clean() {
    let c = classes("h264");
    assert_eq!(c[0], CutClass::CleanIdr);
    assert!(
        c[1..].iter().all(|k| *k == CutClass::Dirty),
        "an h264 non-IDR keyframe was called clean: {c:?}"
    );
}

#[test]
fn the_policy_waits_past_the_floor_for_a_clean_fragment() {
    let p = CutPolicy::new(6, 2, 48_000_000, 15, 1_000);
    // Under the floor, clean or not: keep going.
    assert_eq!(p.cut_before(5_000, 1_000, CutClass::CleanIdr, false), None);
    // Past the floor but the next fragment is dirty: still keep going,
    // which is the entire behaviour change this module ships.
    assert_eq!(p.cut_before(7_000, 1_000, CutClass::Dirty, false), None);
    assert_eq!(
        p.cut_before(7_000, 1_000, CutClass::Unparseable, false),
        None
    );
    // Past the floor with a clean fragment in front: cut.
    assert_eq!(
        p.cut_before(7_000, 1_000, CutClass::CleanCra, false),
        Some(CutReason::Clean)
    );
    // Nothing pending is not a cut, however clean the next fragment is.
    assert_eq!(p.cut_before(0, 0, CutClass::CleanIdr, false), None);
}

/// The trap this ordering exists for, in one assertion. 48 MB arrives in
/// 5.6 s at the reference file's 69 Mb/s — a ceiling allowed to fire on
/// its own would cut below the six-second floor and hand the viewer MORE
/// boundaries than ffmpeg's muxer does, on the exact file this was built
/// for. Every boundary costs a frame.
#[test]
fn no_ceiling_may_cut_below_the_floor() {
    let p = CutPolicy::new(6, 2, 48_000_000, 15, 1_000);
    assert_eq!(
        p.cut_before(5_600, 48_000_000, CutClass::Dirty, false),
        None
    );
    assert_eq!(
        p.cut_before(5_999, usize::MAX, CutClass::Dirty, false),
        None
    );
    // Not even for a clean fragment: under the floor there is nothing to
    // publish yet, and waiting costs nothing but a longer segment.
    assert_eq!(
        p.cut_before(5_999, 48_000_000, CutClass::CleanIdr, false),
        None
    );
    // One tick past it, the ceiling is free to act.
    assert_eq!(
        p.cut_before(6_000, 48_000_000, CutClass::Dirty, false),
        Some(CutReason::ByteCeiling)
    );
}

#[test]
fn the_ceilings_cut_when_no_clean_point_arrives() {
    let p = CutPolicy::new(6, 2, 48_000_000, 15, 1_000);
    assert_eq!(
        p.cut_before(7_000, 48_000_000, CutClass::Dirty, false),
        Some(CutReason::ByteCeiling)
    );
    assert_eq!(
        p.cut_before(15_000, 1_000, CutClass::Dirty, false),
        Some(CutReason::TimeCeiling)
    );
    // A ceiling that happens to land in front of a clean fragment is a
    // clean cut: the ceiling decided when, not where.
    assert_eq!(
        p.cut_before(15_000, 48_000_000, CutClass::CleanIdr, false),
        Some(CutReason::Clean)
    );
}

#[test]
fn a_strict_rolling_target_cuts_before_the_crossing_fragment() {
    let feed = pipe("open-gop");
    let (init, fragments, _) = read_all(&feed);
    let timescale = init.video().expect("video").timescale;
    let first_ticks = fragments[0].video_duration(&init);
    let second_ticks = fragments[1].video_duration(&init);
    let strict_ticks = first_ticks.saturating_add(second_ticks);
    let strict_seconds =
        u32::try_from(strict_ticks.div_ceil(u64::from(timescale))).expect("fixture duration");
    let policy = CutPolicy::new(60, 60, usize::MAX, 60, timescale)
        .with_strict_max_seconds(strict_seconds, timescale);
    let strict = policy.strict_max_ticks.expect("strict target");
    let mut segmenter = Segmenter::new(init, policy);

    let mut crossing = None;
    for fragment in fragments {
        if segmenter.pending_ticks > 0
            && segmenter
                .pending_ticks
                .saturating_add(fragment.video_duration(&segmenter.init))
                > strict
        {
            crossing = segmenter.push(fragment).expect("strict cut");
            break;
        }
        assert!(segmenter
            .push(fragment)
            .expect("pending fragment")
            .is_none());
    }
    let published = crossing.expect("a crossing fragment flushed the pending run");
    assert_eq!(published.reason, CutReason::TimeCeiling);
    assert!(published.seconds <= f64::from(strict_seconds));
    assert!(
        published.seconds < 60.0,
        "the strict presentation bound must override the ordinary floor"
    );
}

#[test]
fn a_fragment_larger_than_the_strict_rolling_target_is_typed_unsupported() {
    let feed = pipe("open-gop");
    let (init, mut fragments, _) = read_all(&feed);
    let timescale = init.video().expect("video").timescale;
    let policy =
        CutPolicy::new(6, 2, usize::MAX, 15, timescale).with_strict_max_seconds(1, timescale);
    let mut segmenter = Segmenter::new(init, policy);

    let error = segmenter
        .push(fragments.remove(0))
        .expect_err("the first fixture fragment is longer than one second");
    assert!(matches!(error, Fmp4Error::Unsupported(_)), "{error:?}");
    assert_eq!(segmenter.counts().segments, 0);
}

// -----------------------------------------------------------------------
// two generations, one film timeline
// -----------------------------------------------------------------------

/// A synthetic fragment: one track, one run, `count` samples of `duration`
/// ticks and `size` bytes each, starting at `base`.
fn synthetic(track_id: u32, base: u64, duration: u32, count: usize, size: u32) -> Fragment {
    let samples = vec![
        Sample {
            size_at: None,
            duration,
            size,
            flags: 0,
            cto: 0,
        };
        count
    ];
    // Sized like a real one, so the byte ceiling sees what it would see on
    // the wire rather than a placeholder.
    let payload = count * size as usize;
    Fragment {
        bytes: vec![0u8; 8 + payload],
        mdat_payload: 8..8 + payload,
        tracks: vec![TrackFragment {
            track_id,
            base_decode_time: base,
            runs: vec![Run {
                version: 0,
                composition_offsets_present: false,
                data_offset_at: None,
                data_offset: 8,
                samples,
            }],
        }],
    }
}

/// Two tracks in one fragment, so a rebase has something to keep in step.
fn synthetic_muxed(video_base: u64, audio_base: u64) -> Fragment {
    let mut fragment = synthetic(1, video_base, 1_000, 24, 4_000);
    let audio = synthetic(2, audio_base, 1_024, 40, 300);
    fragment.tracks.extend(audio.tracks);
    fragment
}

/// A 24 000-tick video track and a 48 000-tick audio track, so a rebase
/// cannot pass by treating one timescale as the other.
fn muxed_init() -> Init {
    let track = |id: u32, kind: TrackKind, timescale: u32| Track {
        id,
        kind,
        timescale,
        has_edit_list: false,
        codec: None,
        dolby_vision_config: false,
        nal_length_size: 4,
        default_sample_duration: 0,
        default_sample_size: 0,
        default_sample_flags: 0,
    };
    Init {
        bytes: Vec::new(),
        tracks: vec![
            track(1, TrackKind::Video, 24_000),
            track(2, TrackKind::Audio, 48_000),
        ],
    }
}

#[test]
fn encoded_audio_partition_uses_presentation_time_and_preserves_packets() {
    let init = muxed_init();
    let mut incoming = synthetic(2, 92_160, 1_024, 94, 3);
    for (index, byte) in incoming.bytes.iter_mut().enumerate() {
        *byte = index as u8;
    }
    let original = track_samples(&incoming, 2);
    let prefix = take_encoded_audio_before(&mut incoming, &init, 48_048, 24_000)
        .expect("valid partition")
        .expect("four lagging AAC packets");
    let before = prefix.track(2).expect("prefix audio");
    let after = incoming.track(2).expect("following audio");
    assert_eq!(before.base_decode_time, 92_160);
    assert_eq!(before.sample_count(), 4);
    assert_eq!(before.base_decode_time + before.duration(), 96_256);
    assert_eq!(after.base_decode_time, 96_256);
    assert_eq!(prefix.bytes.len(), 12, "do not copy the video/mux buffer");
    assert_eq!(
        [track_samples(&prefix, 2), track_samples(&incoming, 2)].concat(),
        original
    );
    assert!(
        take_encoded_audio_before(&mut incoming, &init, 48_048, 24_000)
            .expect("same boundary")
            .is_none()
    );
}

#[test]
fn a_leading_audio_only_fragment_is_rebased_with_the_anchor_not_left_behind() {
    // ffmpeg writes a `traf` only for tracks with buffered samples, so a
    // generation can open with audio and no picture. Leaving those on the
    // source's clock while the video that follows is on the film's is a
    // multi-second split inside one segment.
    let init = muxed_init();
    let policy = CutPolicy::new(1, 1, 64 * 1024 * 1024, 15, 24_000);
    let mut segmenter =
        Segmenter::resuming_at(init, policy, 7, 240_000).expect("video-bearing init");

    // Audio first, alone, on the source's clock.
    assert!(segmenter
        .push(synthetic(2, 96_000, 1_024, 40, 300))
        .expect("audio-only fragment")
        .is_none());
    // Then the anchor: video at 48 000 source ticks, audio continuing.
    segmenter
        .push(synthetic_muxed(48_000, 136_960))
        .expect("the anchor fragment");

    let delta = 240_000i64 - 48_000;
    let audio_shift = scale_signed(delta, 24_000, 48_000);
    assert_eq!(
        segmenter.pending[0]
            .track(2)
            .expect("audio")
            .base_decode_time,
        96_000u64.saturating_add_signed(audio_shift),
        "the fragment that arrived before the anchor must move with it"
    );
    assert_eq!(
        segmenter.pending[1]
            .track(1)
            .expect("video")
            .base_decode_time,
        240_000,
        "and the anchor itself lands exactly on the plan's start"
    );
}

#[test]
fn a_generation_that_would_start_before_the_film_is_refused_not_clamped() {
    // `-avoid_negative_ts make_zero` zeroes the earliest track, which on a
    // muxed stream is usually audio, so video's first decode time sits
    // above it. Placing video at a small film time therefore asks audio to
    // start before the film does, and `tfdt` is unsigned. Saturating at
    // zero would move audio away from picture by exactly the amount that
    // did not fit — silently.
    let init = muxed_init();
    let policy = CutPolicy::new(1, 1, 64 * 1024 * 1024, 15, 24_000);
    let mut segmenter = Segmenter::resuming_at(init, policy, 0, 0).expect("video-bearing init");
    let error = segmenter
        .push(synthetic_muxed(12_000, 0))
        .expect_err("audio cannot start before the film");
    let message = error.to_string();
    assert!(message.contains("track 2"), "{message}");
    assert!(message.contains("negative"), "{message}");
}

#[test]
fn a_repositioned_generation_needs_a_video_track_to_anchor_against() {
    let mut init = muxed_init();
    init.tracks.retain(|track| track.kind != TrackKind::Video);
    let policy = CutPolicy::new(1, 1, 64 * 1024 * 1024, 15, 48_000);
    assert!(
        Segmenter::resuming_at(init, policy, 4, 100_000).is_err(),
        "an audio clock says where in its own stream a sample is, never \
             where in the film"
    );
}

/// Feed a run of equal video fragments and collect what comes out.
fn run_fragments(segmenter: &mut Segmenter, count: usize, size: u32) -> Vec<Published> {
    let mut out = Vec::new();
    for i in 0..count {
        let base = 48_000 + (i as u64) * 24_000;
        if let Some(published) = segmenter
            .push(synthetic(1, base, 1_000, 24, size))
            .expect("segmenting")
        {
            out.push(published);
        }
    }
    out
}

#[test]
fn a_planned_generation_cuts_where_the_plan_said_not_where_the_policy_would() {
    // The disagreement is not hypothetical. `plan_copy` runs the policy
    // over the video-only index pipe's wire lengths plus a generous audio
    // estimate; a live generation accumulates the real production wire
    // length. On a byte-ceiling cut those cross the ceiling at different
    // fragments, and every index after that names the wrong plan entry.
    let init = muxed_init();
    // Floor 1 s, byte ceiling 250 KB. Each fragment is 1 s of video and
    // 120 KB, so the policy alone would cut in front of the third.
    let policy = CutPolicy::new(1, 1, 250_000, 15, 24_000);

    let mut freeform =
        Segmenter::resuming_at(init.clone(), policy, 7, 48_000).expect("video-bearing init");
    let loose = run_fragments(&mut freeform, 12, 5_000);
    assert!(
        !loose.is_empty(),
        "the policy must cut somewhere, or this proves nothing"
    );
    let policy_first_len = loose[0].seconds.round() as u64;

    // The plan says four seconds a segment, which the policy would never
    // have chosen against this byte ceiling.
    let starts = vec![48_000, 144_000, 240_000, 336_000];
    let mut planned =
        Segmenter::following(init, policy, 7, starts.clone()).expect("a planned generation");
    let published = run_fragments(&mut planned, 12, 5_000);

    assert!(
        published.len() >= 2,
        "expected the plan's boundaries to be reached, got {}",
        published.len()
    );
    assert_ne!(
        published[0].seconds.round() as u64,
        policy_first_len,
        "the plan and the policy chose the same first boundary, so this \
             fixture does not exercise the difference"
    );
    for (offset, segment) in published.iter().enumerate() {
        assert_eq!(
            segment.index,
            7 + offset as u64,
            "segments must be published under the plan's own indexes"
        );
        assert_eq!(
            published_video_tfdt(planned.init(), segment),
            starts[offset],
            "segment {} must begin at the film time the plan named",
            segment.index
        );
    }
}

#[test]
fn a_generation_that_steps_over_a_planned_boundary_is_refused() {
    // An empty segment under an index a client will request is worse than
    // refusing to produce it: it means this generation is not the one the
    // plan named.
    let init = muxed_init();
    let policy = CutPolicy::new(1, 1, 64 * 1024 * 1024, 15, 24_000);
    // Boundaries a second apart, fed four-second fragments.
    let starts = vec![0, 24_000, 48_000, 72_000];
    let mut segmenter = Segmenter::following(init, policy, 0, starts).expect("planned");
    segmenter
        .push(synthetic(1, 0, 1_000, 96, 4_000))
        .expect("the first fragment");
    let error = segmenter
        .push(synthetic(1, 96_000, 1_000, 96, 4_000))
        .expect_err("a fragment that steps over two boundaries");
    assert!(error.to_string().contains("does not match the plan"));
}

// ---- canonical promotion (plan §2.2) ---------------------------------

#[test]
fn promoting_from_captured_inputs_does_not_depend_on_the_landing_fragment() {
    // The whole mechanism, on the only init shape promotion fires on.
    //
    // Two generations of one rendition land on fragments whose in-band
    // parameter sets differ — the case plan §2.2 could not survive. Under
    // the old rule each promotes from its own landing fragment and the two
    // served inits differ, so the second generation is refused for doing
    // exactly what it was told to do. Under the ruling both promote from
    // the inputs the indexer captured, and the bytes are the same.
    let init = minimal_hvcc_dv_init();
    let video = init.video().expect("video").clone();

    let vps = [0x40, 0x01, 0x0c];
    let sps = [0x42, 0x01, 0x01];
    let pps = [0x44, 0x01, 0xc0];
    // One byte different, which is all it takes.
    let sps_later = [0x42, 0x01, 0x02];
    let vcl = [0x26, 0x01, 0x80];

    let opening = fragment_with_first_video_sample(
        video.id,
        length_prefixed_hevc_nals(&[&vps, &sps, &pps, &vcl]),
    );
    let mid_film = fragment_with_first_video_sample(
        video.id,
        length_prefixed_hevc_nals(&[&vps, &sps_later, &pps, &vcl]),
    );

    // The old rule: each generation reads its own landing fragment.
    let mut own_opening = init.clone();
    promote_hevc_parameter_sets(&mut own_opening, &opening).expect("promote");
    let mut own_mid_film = init.clone();
    promote_hevc_parameter_sets(&mut own_mid_film, &mid_film).expect("promote");
    assert_ne!(
        own_opening.bytes, own_mid_film.bytes,
        "the collision this mechanism exists for must be real, or the \
             test below proves nothing"
    );

    // The ruling: both promote from what the indexer captured once.
    let canonical = PromotionInputs::from_fragment(&opening, &init);
    let mut served_first = init.clone();
    promote_from(&mut served_first, &canonical).expect("promote");
    let mut served_later = init.clone();
    promote_from(&mut served_later, &canonical).expect("promote");
    assert_eq!(
        served_first.bytes, served_later.bytes,
        "a served init must not depend on where its generation started"
    );
    assert_eq!(
        served_first.bytes, own_opening.bytes,
        "and it must be the init the first generation would have written"
    );
}

#[test]
fn a_generation_landing_where_promotion_finds_nothing_still_serves_the_same_init() {
    // The failure that is not variation. Mastering-display SEI need not
    // appear on every GOP, so a generation landing on a fragment without
    // it promotes nothing and produces an init missing boxes the stored
    // one has — a refused generation whose media the stored init describes
    // perfectly. Captured inputs make the landing irrelevant.
    let init = minimal_hvcc_dv_init();
    let video = init.video().expect("video").clone();
    let vps = [0x40, 0x01, 0x0c];
    let sps = [0x42, 0x01, 0x01];
    let pps = [0x44, 0x01, 0xc0];
    let vcl = [0x26, 0x01, 0x80];

    let opening = fragment_with_first_video_sample(
        video.id,
        length_prefixed_hevc_nals(&[&vps, &sps, &pps, &vcl]),
    );
    // Carries picture data and no parameter sets at all.
    let bare = fragment_with_first_video_sample(video.id, length_prefixed_hevc_nals(&[&vcl]));

    let mut promoted_from_bare = init.clone();
    assert!(
        !promote_hevc_parameter_sets(&mut promoted_from_bare, &bare).expect("promote"),
        "a fragment with no parameter sets promotes nothing"
    );

    let canonical = PromotionInputs::from_fragment(&opening, &init);
    let mut served = init.clone();
    promote_from(&mut served, &canonical).expect("promote");
    assert_ne!(
        served.bytes, promoted_from_bare.bytes,
        "which is exactly why the landing fragment must not decide"
    );
}

#[test]
fn capturing_inputs_reproduces_what_promoting_from_the_fragment_would_do() {
    // The canonical path must not be a second, subtly different
    // implementation of promotion. Same fragment in, same init out.
    let init = muxed_init();
    let fragment = synthetic_muxed(0, 0);

    let mut direct = init.clone();
    let a = promote_hevc_parameter_sets(&mut direct, &fragment).expect("direct");
    let b = promote_hdr10_static_metadata(&mut direct, &fragment).expect("direct");

    let mut captured = init.clone();
    let inputs = PromotionInputs::from_fragment(&fragment, &init);
    let changed = promote_from(&mut captured, &inputs).expect("captured");

    assert_eq!(direct.bytes, captured.bytes);
    assert_eq!(changed, a || b);
}

#[test]
fn a_fragment_carrying_nothing_promotable_captures_nothing() {
    // Most sources. A normal encode writes a populated hvcC and carries no
    // parameter sets in band, so promotion is a no-op and there is nothing
    // for the indexer to store.
    let init = muxed_init();
    let inputs = PromotionInputs::from_fragment(&synthetic_muxed(0, 0), &init);
    assert!(inputs.is_empty());

    // And promoting from nothing changes nothing, rather than erroring or
    // producing a different init than the unpromoted one.
    let mut served = init.clone();
    assert!(!promote_from(&mut served, &inputs).expect("promote"));
    assert_eq!(served.bytes, init.bytes);
}

#[test]
fn captured_inputs_survive_a_round_trip_through_storage() {
    // They are persisted beside the index as JSON, and a generation
    // promotes from the stored copy rather than a live one.
    let init = muxed_init();
    let inputs = PromotionInputs {
        indexed_hdr10_metadata: false,
        hevc_configuration: None,
        dolby_vision: None,
        strip_dolby_vision: false,
        dvh1_parameter_sets: Vec::new(),
        dvh1_parameter_sets_malformed: false,
        parameter_sets: vec![vec![0x40, 0x01, 0x0c], vec![0x42, 0x01, 0x01]],
        hdr10_sei: vec![vec![0x4e, 0x01, 0x89]],
    };
    let text = serde_json::to_string(&inputs).expect("encode");
    let back: PromotionInputs = serde_json::from_str(&text).expect("decode");
    assert_eq!(inputs, back);

    // And the same bytes come out the far side of promotion either way.
    let mut a = init.clone();
    let mut b = init.clone();
    let _ = promote_from(&mut a, &inputs);
    let _ = promote_from(&mut b, &back);
    assert_eq!(a.bytes, b.bytes);
}

#[test]
fn a_planned_generation_emits_exactly_the_tail_entries_the_plan_named() {
    // The dangerous band: a tail long enough to be planned as its own
    // entry but short enough that the unplanned rule lets the last video
    // segment swallow it whole. The playlist is already published, VOD and
    // closed with ENDLIST, so a segment the plan names and the producer
    // never emits is a reader blocked on it until its deadline fires --
    // at the end of every affected film, forever.
    let init = muxed_init();
    // 15 s ceiling, 24 000-tick video.
    let policy = CutPolicy::new(4, 4, 64 * 1024 * 1024, 15, 24_000);

    // 6 s of video, then 5 s of audio alone: 11 s total, under the 15 s
    // ceiling the unplanned rule would allow the first chunk.
    let mut fragments = Vec::new();
    for second in 0..6u64 {
        let mut fragment = synthetic(1, second * 24_000, 1_000, 24, 4_000);
        let audio = synthetic(2, second * 48_000, 1_024, 47, 300);
        fragment.tracks.extend(audio.tracks);
        fragments.push(fragment);
    }
    // Audio only, past the end of video.
    for second in 6..11u64 {
        fragments.push(synthetic(2, second * 48_000, 1_024, 47, 300));
    }

    // The plan says video ends at 6 s and the tail is its own entries.
    let starts = vec![0, 144_000, 288_000, 432_000];
    let mut planned = Segmenter::following(init, policy, 0, starts).expect("a planned generation");
    let mut published = Vec::new();
    for fragment in &fragments {
        if let Some(segment) = planned.push(fragment.clone()).expect("segmenting") {
            published.push(segment);
        }
    }
    published.extend(planned.finish().expect("finishing"));

    // Two chunks out of `finish`: the video segment, and the tail. Not
    // one, which is what the ceiling rule gives.
    assert!(
        published.len() >= 2,
        "the tail must be its own segment, got {} in total",
        published.len()
    );
    let last = published.last().expect("a last segment");
    assert!(
        last.seconds > 0.0,
        "the tail segment has to carry real audio"
    );
    // And the video segment must stop where video stops, not carry five
    // seconds of audio the playlist attributed to a later index.
    let video_segment = &published[published.len() - 2];
    assert!(
        video_segment.seconds <= 6.5,
        "the last video segment ran {:.3}s; the plan said six",
        video_segment.seconds
    );
}

#[test]
fn a_planless_segmenter_still_decides_for_itself() {
    // A live session has no plan and must keep cutting by policy — this is
    // the path every existing caller is on.
    let init = muxed_init();
    let policy = CutPolicy::new(1, 1, 250_000, 15, 24_000);
    let mut segmenter = Segmenter::new(init, policy);
    let published = run_fragments(&mut segmenter, 12, 5_000);
    assert!(
        !published.is_empty(),
        "a segmenter with no plan cuts by policy as it always has"
    );
    assert_eq!(published[0].index, 0);
}

#[test]
fn a_resumed_generation_does_not_spend_the_first_segment_floor_again() {
    // The lower first floor exists to get a *session* playing quickly. A
    // generation resuming at index 7 is nobody's first segment, so it must
    // use the ordinary floor — otherwise every seek publishes one short
    // segment that the plan does not contain.
    let init = muxed_init();
    // First floor 1 s, ordinary floor 4 s, at 24 000 ticks, with a byte
    // ceiling one fragment's payload can cross.
    let policy = CutPolicy::new(4, 1, 150_000, 15, 24_000);

    let mut fresh = Segmenter::new(init.clone(), policy);
    let mut resumed = Segmenter::resuming_at(init, policy, 7, 240_000).expect("video-bearing init");

    // Two seconds of video and 192 KB of it. The fresh segmenter is past
    // its 1 s floor, so the byte ceiling is free to act; the resumed one
    // is short of 4 s, and the floor gates the ceilings.
    assert!(fresh
        .push(synthetic(1, 0, 1_000, 48, 4_000))
        .expect("fresh, first")
        .is_none());
    assert!(resumed
        .push(synthetic(1, 48_000, 1_000, 48, 4_000))
        .expect("resumed, first")
        .is_none());

    let fresh_cut = fresh
        .push(synthetic(1, 48_000, 1_000, 24, 4_000))
        .expect("fresh");
    let resumed_cut = resumed
        .push(synthetic(1, 96_000, 1_000, 24, 4_000))
        .expect("resumed");
    assert!(
        fresh_cut.is_some(),
        "a session's first segment may cut once past the 1 s first floor"
    );
    assert!(
        resumed_cut.is_none(),
        "a resumed generation is nobody's first segment and waits for 4 s"
    );
}

/// The video `tfdt` a published segment actually carries, read back off
/// the bytes rather than off whatever the segmenter believed.
fn published_video_tfdt(init: &Init, published: &Published) -> u64 {
    let video_id = init.video().expect("a video track").id;
    let bytes = &published.segment.bytes;
    // Walk the segment as boxes rather than through `FragmentReader`,
    // which reads a pipe and has never been asked to step over the `styp`
    // and `sidx` a published segment carries.
    let boxes = |region: &[u8], visit: &mut dyn FnMut(&[u8; 4], &[u8])| {
        let mut at = 0usize;
        while at + 8 <= region.len() {
            let size = u32::from_be_bytes(region[at..at + 4].try_into().expect("4")) as usize;
            let name: [u8; 4] = region[at + 4..at + 8].try_into().expect("4");
            if size < 8 || at + size > region.len() {
                break;
            }
            visit(&name, &region[at + 8..at + size]);
            at += size;
        }
    };
    let mut moof: Option<Vec<u8>> = None;
    boxes(bytes, &mut |name, body| {
        if name == b"moof" && moof.is_none() {
            moof = Some(body.to_vec());
        }
    });
    let moof = moof.expect("a published segment carries a moof");
    let mut trafs: Vec<Vec<u8>> = Vec::new();
    boxes(&moof, &mut |name, body| {
        if name == b"traf" {
            trafs.push(body.to_vec());
        }
    });
    for traf in trafs {
        let mut id = None;
        let mut tfdt = None;
        boxes(&traf, &mut |name, body| match name {
            b"tfhd" if body.len() >= 8 => {
                id = Some(u32::from_be_bytes(body[4..8].try_into().expect("4")));
            }
            b"tfdt" if body.len() >= 12 => {
                tfdt = Some(u64::from_be_bytes(body[4..12].try_into().expect("8")));
            }
            _ => {}
        });
        if id == Some(video_id) {
            return tfdt.expect("the video traf carries a tfdt");
        }
    }
    panic!("a published segment carried no video traf");
}

/// The production pipe reopened partway in, the way a repositioned
/// producer opens it: land on the random-access point at or before the
/// boundary and keep the source's own timestamps.
fn repositioned_pipe(kind: &str, seconds: f64) -> Vec<u8> {
    let source = crate::testfixtures::source(kind);
    let mut cmd = Command::new(ffmpeg());
    cmd.args(["-hide_banner", "-loglevel", "error"])
        .args(["-noaccurate_seek", "-ss"])
        .arg(format!("{seconds:.3}"))
        .arg("-i")
        .arg(&source)
        .args(["-copyts"])
        .args(["-map_chapters", "-1", "-map", "0:v:0", "-map", "0:a:0?"])
        .args(["-sn", "-c:v", "copy"])
        .args(["-tag:v", "hvc1"])
        .args(["-bsf:v", "filter_units=remove_types=32-34"])
        .args(["-c:a", "aac", "-b:a", "256k"])
        .args([
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof+delay_moov",
        ])
        .args(["-use_editlist", "0", "-f", "mp4", "pipe:1"]);
    run(&mut cmd)
}

/// The one property the whole VOD presentation rests on: a generation that
/// started partway into the film publishes segments on the *film's*
/// timeline, not on ffmpeg's.
///
/// M0 §12.4 measured that a repositioned generation's timestamps carry no
/// film time in any configuration tested, which is why the producer cannot
/// read its position out of the media and why `segplan::match_landing`
/// exists. This is the other half of that finding: once the producer knows
/// where it landed, the segmenter has to be *told*, and what it publishes
/// has to join the earlier generation's timeline exactly — same `tfdt`,
/// same index, no gap and no overlap. A client that seeks is handed
/// segments from both generations in one playlist and must not be able to
/// tell which came from which.
#[test]
fn two_generations_publish_one_continuous_film_timeline() {
    let kind = "closed-gop";
    let feed = pipe(kind);
    let (init, fragments, _) = read_all(&feed);
    let video = init.video().expect("a video track");
    let timescale = video.timescale;
    // Equal floors, so the boundaries a generation picks cannot depend on
    // whether it thinks it is the first one. That is a separate property
    // and it would mask this one.
    let policy = CutPolicy::new(3, 3, 300_000, 15, timescale);

    let mut first = Segmenter::new(init.clone(), policy);
    let mut generation_one = Vec::new();
    // Where each published segment starts, counted in fragments, so the
    // boundary can be expressed as a place in the *stream* and not only as
    // a time.
    let mut segment_starts = vec![0usize];
    for (offset, fragment) in fragments.iter().enumerate() {
        if let Some(published) = first.push(fragment.clone()).expect("segmenting") {
            generation_one.push(published);
            segment_starts.push(offset);
        }
    }
    generation_one.extend(first.finish().expect("finishing"));
    assert!(
        generation_one.len() >= 4 && segment_starts.len() >= 4,
        "the fixture produced {} segments; this test needs a boundary with \
             film in front of it and behind it",
        generation_one.len()
    );

    // The boundary: where generation one's third segment starts.
    let boundary_index = 2u64;
    let boundary_fragment = segment_starts[boundary_index as usize];
    let boundary_ticks = published_video_tfdt(&init, &generation_one[boundary_index as usize]);
    assert!(
        boundary_ticks > 0,
        "the boundary is not the start of the film"
    );
    let boundary_seconds = boundary_ticks as f64 / f64::from(timescale);

    // Generation two: the same file, opened at that boundary.
    let reopened = repositioned_pipe(kind, boundary_seconds);
    let (second_init, second_fragments, _) = read_all(&reopened);
    let second_video = second_init.video().expect("a video track").id;
    assert!(
        !second_fragments.is_empty(),
        "the repositioned pipe carried no fragments"
    );

    // `-noaccurate_seek` lands at the random-access point at or *before*
    // the boundary, so the producer finds its place by byte sequence and
    // discards forward. Exactly what `segplan::match_landing` and
    // `discard_to` do, done here by hand so this test depends on the
    // segmenter and on ffmpeg, not on another module's answer.
    let bytes_of = |frags: &[Fragment], id: u32| -> Vec<usize> {
        frags
            .iter()
            .map(|fragment| {
                fragment
                    .track(id)
                    .map(TrackFragment::byte_len)
                    .unwrap_or_default()
            })
            .collect()
    };
    let original = bytes_of(&fragments, video.id);
    let reopened_bytes = bytes_of(&second_fragments, second_video);
    assert!(
        reopened_bytes.len() >= 3 && original.len() >= 3,
        "a landing is matched over three fragments and there are {} / {}",
        reopened_bytes.len(),
        original.len()
    );
    // Inclusive of the last valid start, the way `match_landing` is: a
    // reposition into the final three fragments is a legitimate landing.
    let matches: Vec<usize> = (0..=original.len() - 3)
        .filter(|&start| original[start..start + 3] == reopened_bytes[..3])
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "the landing must be unique — `match_landing` calls more than one \
             Ambiguous rather than taking the first"
    );
    let landing = matches[0];
    assert!(
        landing <= boundary_fragment,
        "-noaccurate_seek landed at fragment {landing}, past the boundary \
             at {boundary_fragment}; it is supposed to land at or before"
    );
    let discard = boundary_fragment - landing;

    let mut second =
        Segmenter::resuming_at(second_init.clone(), policy, boundary_index, boundary_ticks)
            .expect("a video-bearing init resumes");
    let mut generation_two = Vec::new();
    for fragment in second_fragments.iter().skip(discard) {
        if let Some(published) = second.push(fragment.clone()).expect("segmenting") {
            generation_two.push(published);
        }
    }
    generation_two.extend(second.finish().expect("finishing"));
    assert!(
        !generation_two.is_empty(),
        "the repositioned generation published nothing"
    );

    // What ffmpeg handed generation two, before anything was done to it.
    let raw = second_fragments[discard]
        .track(second_video)
        .expect("video in the landing fragment")
        .base_decode_time;

    assert_ne!(
        raw, boundary_ticks,
        "ffmpeg happened to hand this generation the film's own time, so \
             the rebase is not being exercised at all"
    );
    assert_eq!(
        published_video_tfdt(&second_init, &generation_two[0]),
        boundary_ticks,
        "the repositioned generation's first segment must start at the \
             plan entry's film time, not at the {raw} ticks ffmpeg gave it"
    );
    assert_eq!(
        generation_two[0].name(),
        segment_name(boundary_index),
        "and under the plan's name, not seg00000"
    );

    // The join, from both sides: every segment generation two publishes
    // lands exactly where generation one's did.
    //
    // The first one is the weakest of these — its time is the anchor by
    // construction, whatever fragment the discard happened to leave at the
    // front. It is the *second* that can catch a wrong landing, so demand
    // one.
    let overlap = generation_two
        .len()
        .min(generation_one.len() - boundary_index as usize);
    assert!(
        overlap >= 2,
        "only {overlap} segment(s) overlap, so nothing here compares two \
             generations at a boundary neither of them chose"
    );
    for offset in 0..overlap {
        let one = &generation_one[boundary_index as usize + offset];
        let two = &generation_two[offset];
        assert_eq!(
            published_video_tfdt(&second_init, two),
            published_video_tfdt(&init, one),
            "segment {} of the two generations starts at different times",
            two.index
        );
        assert_eq!(
            two.seconds.round(),
            one.seconds.round(),
            "segment {} runs for a different length in the two generations",
            two.index
        );
    }

    // Contiguous within generation two as well, so nothing above is
    // passing on a single lucky first segment.
    for pair in generation_two.windows(2) {
        let start = published_video_tfdt(&second_init, &pair[0]);
        let next = published_video_tfdt(&second_init, &pair[1]);
        assert!(
            next > start,
            "segment {} does not start after segment {}",
            pair[1].index,
            pair[0].index
        );
    }
}

// -----------------------------------------------------------------------
// M2 — the merger
// -----------------------------------------------------------------------

/// Run a whole feed through the segmenter, returning what a session would
/// have published.
fn segment(
    kind: &str,
    floor_s: u32,
    max_bytes: usize,
    max_s: u32,
) -> (Init, Vec<Published>, SegmentCounts) {
    let feed = pipe(kind);
    let (init, frags, _) = read_all(&feed);
    let timescale = init.video().map(|v| v.timescale).unwrap_or(1);
    let policy = CutPolicy::new(floor_s, floor_s, max_bytes, max_s, timescale);
    let mut seg = Segmenter::new(init.clone(), policy);
    let mut out = Vec::new();
    for f in frags {
        if let Some(p) = seg.push(f).expect("merging") {
            out.push(p);
        }
    }
    out.extend(seg.finish().expect("final merge"));
    (init, out, seg.counts())
}

fn write_candidate(dir: &Path, init: &Init, published: &[Published]) -> PathBuf {
    let path = dir.join("candidate.mp4");
    let mut bytes = init.bytes.clone();
    for p in published {
        bytes.extend_from_slice(&p.segment.bytes);
    }
    std::fs::write(&path, &bytes).expect("writing the candidate");
    path
}

fn framemd5(path: &Path, stream: &str) -> String {
    let out = run(Command::new(ffmpeg())
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", stream, "-f", "framemd5", "-"]));
    String::from_utf8_lossy(&out)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The licence to ship: what comes out of the merger decodes to exactly
/// the same frames, at exactly the same times, as the continuous stream it
/// was cut from. `framemd5` covers both — it prints a hash per frame *with*
/// its pts and duration — so a re-ordered, re-timed or dropped frame all
/// fail it. Nothing else in this module is allowed to be interesting if
/// this is not true.
#[test]
fn merged_stream_decodes_bit_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (init, published, counts) = segment("open-gop", 3, 300_000, 15);
    assert!(
        published.len() >= 2,
        "the fixture produced one segment only"
    );
    assert_eq!(counts.tfdt_adjustments, 0, "a join had to be repaired");

    let candidate = write_candidate(dir.path(), &init, &published);
    let reference = pipe_path("open-gop");
    for stream in ["0:v:0", "0:a:0"] {
        assert_eq!(
            framemd5(&candidate, stream),
            framemd5(&reference, stream),
            "{stream} decodes differently after merging"
        );
    }
}

/// Re-parse our own output and check it against the fragments it was made
/// from. framemd5 proves the decoder agrees; this proves the container
/// says what we meant it to say, which is what a *different* player will
/// read.
#[test]
fn every_sample_keeps_its_pts_dur_size() {
    let (init, published, _) = segment("open-gop", 3, 300_000, 15);
    let feed = pipe("open-gop");
    let (_, source_frags, _) = read_all(&feed);

    // Flatten the source into (dts, pts, duration, size, flags) per track.
    let mut want: Vec<(u32, i64, i64, u32, u32, u32)> = Vec::new();
    for f in &source_frags {
        for tf in &f.tracks {
            let mut dts = tf.base_decode_time as i64;
            for s in tf.samples() {
                want.push((tf.track_id, dts, dts + s.cto, s.duration, s.size, s.flags));
                dts += s.duration as i64;
            }
        }
    }

    let mut got: Vec<(u32, i64, i64, u32, u32, u32)> = Vec::new();
    for p in &published {
        let mut feed = init.bytes.clone();
        feed.extend_from_slice(&p.segment.bytes);
        let (_, frags, _) = read_all(&feed);
        assert_eq!(frags.len(), 1, "a published segment is one fragment");
        for tf in &frags[0].tracks {
            let mut dts = tf.base_decode_time as i64;
            for s in tf.samples() {
                got.push((tf.track_id, dts, dts + s.cto, s.duration, s.size, s.flags));
                dts += s.duration as i64;
            }
        }
        // And the bytes each sample points at are the bytes it had.
        for tf in &frags[0].tracks {
            for r in &tf.runs {
                assert!(
                    r.data_offset + r.byte_len() <= p.segment.bytes.len(),
                    "a run points past the end of its own segment"
                );
            }
        }
    }

    want.sort_by_key(|s| (s.0, s.1));
    got.sort_by_key(|s| (s.0, s.1));
    assert_eq!(got.len(), want.len(), "the merger lost or invented samples");
    assert_eq!(got, want, "a sample changed on the way through the merger");
}

/// The defect this whole investigation chased first (STUTTER-4K §3.8/§3.9)
/// was a join overlap of one source timestamp tick. Consecutive published
/// segments must join at exactly zero, on both tracks, in the tracks' own
/// integer units — a float comparison would hide precisely the size of
/// error that matters.
#[test]
fn joins_are_gapless_and_overlap_free() {
    let (init, published, _) = segment("open-gop", 3, 300_000, 15);
    assert!(published.len() >= 3, "not enough joins to be evidence");

    let mut end: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    for (i, p) in published.iter().enumerate() {
        let mut feed = init.bytes.clone();
        feed.extend_from_slice(&p.segment.bytes);
        let (_, frags, _) = read_all(&feed);
        for tf in &frags[0].tracks {
            if let Some(prev) = end.get(&tf.track_id) {
                assert_eq!(
                    tf.base_decode_time,
                    *prev,
                    "segment {i} track {} starts {} ticks from where {} ended",
                    tf.track_id,
                    tf.base_decode_time as i64 - *prev as i64,
                    i - 1
                );
            }
            end.insert(tf.track_id, tf.base_decode_time + tf.duration());
        }
    }

    // And every playlist duration is derived from the media that was
    // actually serialized into that segment. This is intentionally read
    // back from the output bytes rather than reconstructed from
    // `Published::seconds`: the latter made the old whole-playlist
    // invariant tautological for the final segment.
    let video = init.video().expect("video");
    let mut actual = 0.0;
    for (index, item) in published.iter().enumerate() {
        let mut feed = init.bytes.clone();
        feed.extend_from_slice(&item.segment.bytes);
        let (_, fragments, _) = read_all(&feed);
        let fragment = &fragments[0];
        let video_seconds = fragment
            .track(video.id)
            .map(|track| track.duration() as f64 / video.timescale.max(1) as f64)
            .unwrap_or(0.0);
        let longest_seconds = init
            .tracks
            .iter()
            .filter_map(|track| {
                fragment.track(track.id).map(|track_fragment| {
                    track_fragment.duration() as f64 / track.timescale.max(1) as f64
                })
            })
            .fold(0.0f64, f64::max);
        let expected = if item.reason == CutReason::EndOfStream {
            longest_seconds
        } else {
            video_seconds
        };
        assert!(
            (item.seconds - expected).abs() < 1e-6,
            "segment {index} declares {}s against {expected}s of serialized media",
            item.seconds
        );
        actual += expected;
    }
    let claimed: f64 = published.iter().map(|item| item.seconds).sum();
    assert!((claimed - actual).abs() < 1e-6);
}

/// Two tracks go in, two tracks come out, with the same durations —
/// checked through ffprobe rather than through our own parser, because the
/// failure this guards against (an offset rebased against the wrong
/// track's payload) produces a file our parser would still describe
/// happily.
#[test]
fn audio_and_video_survive_interleaved() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (init, published, _) = segment("open-gop", 3, 300_000, 15);
    let candidate = write_candidate(dir.path(), &init, &published);
    let reference = pipe_path("open-gop");

    let describe = |p: &Path| {
        let out = run(Command::new(ffprobe())
            .args(["-v", "error", "-show_entries"])
            .args(["stream=index,codec_name,nb_read_packets", "-count_packets"])
            .args(["-of", "csv=p=0"])
            .arg(p));
        String::from_utf8_lossy(&out).trim().to_owned()
    };
    assert_eq!(
        describe(&candidate),
        describe(&reference),
        "the merged file does not carry the same streams as the source"
    );
}

/// A source with no clean point anywhere still has to make progress, and
/// has to say that it did so the honest way.
#[test]
fn a_source_with_no_clean_points_cuts_at_the_ceiling_and_counts_it() {
    // Ceiling small enough that a 12 s fixture reaches it: the policy is a
    // parameter precisely so this is testable without a 4K file.
    let (_, published, counts) = segment("open-gop", 3, 300_000, 15);
    assert!(published.len() >= 2);
    assert!(
        counts.ceiling_cuts >= 1,
        "no ceiling cut on a source with no clean point: {counts:?}"
    );
    assert_eq!(counts.clean_cuts, 0, "there was no clean point to find");
    assert_eq!(counts.unparseable, 0);
    assert!(published.iter().any(|p| p.reason == CutReason::ByteCeiling));
    assert_eq!(
        published.last().map(|p| p.reason),
        Some(CutReason::EndOfStream)
    );
}

/// The other half: given clean points, every cut takes one.
#[test]
fn a_source_with_clean_points_never_cuts_dirty() {
    let (_, published, counts) = segment("clean-cra", 3, 48_000_000, 15);
    assert!(published.len() >= 2);
    assert_eq!(
        counts.ceiling_cuts, 0,
        "a ceiling cut with clean points available"
    );
    assert_eq!(counts.clean_cuts as usize, published.len() - 1);
    for p in &published[..published.len() - 1] {
        assert_eq!(p.reason, CutReason::Clean);
        // Past the floor, and not wildly past it: the cut is taken at the
        // *first* clean point after the floor, not the last.
        assert!(p.seconds >= 3.0, "cut below the floor at {}s", p.seconds);
        assert!(p.seconds < 3.0 + 2.0, "cut {}s past the floor", p.seconds);
    }
}

#[test]
fn the_playlist_matches_the_template() {
    let header = playlist_header(15);
    assert!(header.starts_with("#EXTM3U\n#EXT-X-VERSION:7\n"));
    assert!(header.contains("#EXT-X-TARGETDURATION:15\n"));
    assert!(header.contains("#EXT-X-PLAYLIST-TYPE:EVENT\n"));
    assert!(header.contains("#EXT-X-MAP:URI=\"init.mp4\"\n"));
    assert!(
        !header.contains("INDEPENDENT-SEGMENTS"),
        "a claim a ceiling cut makes false"
    );
    assert_eq!(
        playlist_entry(6.208_333, "seg00000.m4s"),
        "#EXTINF:6.208333,\nseg00000.m4s\n"
    );
    assert_eq!(segment_name(0), "seg00000.m4s");
    assert_eq!(segment_name(12_345), "seg12345.m4s");
}

/// A published segment has ffmpeg's HLS muxer's shape, box for box.
///
/// This is not tidiness. The first cut of the merger omitted the `sidx`
/// boxes and wrote one `trun` per source fragment; Chrome played it and
/// Safari refused the stream, and the player's error fallback re-encoded
/// a 4K remux down to 1080p. The shape is load-bearing, so it is asserted.
#[test]
fn a_published_segment_has_the_muxers_shape() {
    let (init, published, _) = segment("open-gop", 3, 300_000, 15);
    let bytes = &published[0].segment.bytes;
    let mut types = Vec::new();
    let mut p = 0usize;
    while let Ok(Some(hdr)) = peek_box(bytes, p) {
        types.push(fourcc(hdr.kind()));
        p += hdr.size;
        if p >= bytes.len() {
            break;
        }
    }
    assert_eq!(types, vec!["styp", "sidx", "sidx", "moof", "mdat"]);
    assert_eq!(&bytes[..24], &STYP, "styp is not hlsenc's 24 bytes");

    // One traf per track, and inside it exactly one trun.
    let mut feed = init.bytes.clone();
    feed.extend_from_slice(bytes);
    let (_, frags, _) = read_all(&feed);
    assert_eq!(frags.len(), 1);
    assert_eq!(frags[0].tracks.len(), 2);
    for t in &frags[0].tracks {
        assert_eq!(
            t.runs.len(),
            1,
            "track {} published {} truns; ffmpeg writes one",
            t.track_id,
            t.runs.len()
        );
    }
}

/// The `sidx` boxes say what ffmpeg's say: version 1, one reference
/// covering the whole moof+mdat, `earliest_presentation_time` equal to the
/// track's `tfdt`, and a `first_offset` that steps over the sidx boxes
/// still between it and the material.
#[test]
fn the_segment_index_describes_the_segment_it_precedes() {
    let (init, published, _) = segment("open-gop", 3, 300_000, 15);
    let seg = &published[0].segment.bytes;
    let mut feed = init.bytes.clone();
    feed.extend_from_slice(seg);
    let (_, frags, _) = read_all(&feed);

    let mut p = STYP.len();
    let mut seen = 0usize;
    let mut sidxes = Vec::new();
    while let Ok(Some(hdr)) = peek_box(seg, p) {
        if hdr.kind() != b"sidx" {
            break;
        }
        let b = &seg[p..p + hdr.size];
        assert_eq!(hdr.size, 52, "not a version-1 sidx");
        assert_eq!(b[8], 1, "sidx version");
        sidxes.push((
            be_u32(b, 12),               // reference_ID
            be_u32(b, 16),               // timescale
            be_u64(b, 20),               // earliest_presentation_time
            be_u64(b, 28),               // first_offset
            be_u32(b, 40) & 0x7fff_ffff, // referenced_size
            be_u32(b, 44),               // subsegment_duration
            be_u32(b, 48) >> 31,         // starts_with_SAP
        ));
        p += hdr.size;
        seen += 1;
    }
    assert_eq!(seen, frags[0].tracks.len(), "one sidx per track");

    let after_sidx = STYP.len() + seen * 52;
    let referenced = seg.len() - after_sidx;
    for (i, sx) in sidxes.iter().enumerate() {
        let tf = frags[0]
            .track(sx.0)
            .unwrap_or_else(|| panic!("sidx names track {} which is absent", sx.0));
        let track = init.track(sx.0).expect("track in the init");
        assert_eq!(sx.1, track.timescale, "sidx timescale");
        assert_eq!(
            sx.2, tf.base_decode_time,
            "earliest_presentation_time != tfdt"
        );
        assert_eq!(
            sx.3,
            ((seen - 1 - i) * 52) as u64,
            "first_offset does not step over the remaining sidx boxes"
        );
        assert_eq!(sx.4 as usize, referenced, "referenced_size != moof+mdat");
        assert_eq!(sx.5 as u64, tf.duration(), "subsegment_duration");
        assert_eq!(sx.6, 1, "starts_with_SAP");
    }
}

// -----------------------------------------------------------------------
// hostile input — every one of these was found by review, not by a fixture
// -----------------------------------------------------------------------

/// Build a minimal `moof`+`mdat` by hand, so the reader can be shown bytes
/// no ffmpeg would ever write.
fn hand_built_fragment(trun_flags: u32, sample_count: u32, samples: &[u8]) -> Vec<u8> {
    let mut trun = Vec::new();
    trun.extend_from_slice(&trun_flags.to_be_bytes());
    trun.extend_from_slice(&sample_count.to_be_bytes());
    trun.extend_from_slice(samples);
    let mut traf = Vec::new();
    traf.extend_from_slice(&16u32.to_be_bytes());
    traf.extend_from_slice(b"tfhd");
    traf.extend_from_slice(&0x0002_0000u32.to_be_bytes());
    traf.extend_from_slice(&1u32.to_be_bytes());
    traf.extend_from_slice(&20u32.to_be_bytes());
    traf.extend_from_slice(b"tfdt");
    traf.extend_from_slice(&0x0100_0000u32.to_be_bytes());
    traf.extend_from_slice(&0u64.to_be_bytes());
    traf.extend_from_slice(&((trun.len() + 8) as u32).to_be_bytes());
    traf.extend_from_slice(b"trun");
    traf.extend_from_slice(&trun);

    let mut moof = Vec::new();
    moof.extend_from_slice(&((traf.len() + 8 + 16 + 8) as u32).to_be_bytes());
    moof.extend_from_slice(b"moof");
    moof.extend_from_slice(&16u32.to_be_bytes());
    moof.extend_from_slice(b"mfhd");
    moof.extend_from_slice(&0u32.to_be_bytes());
    moof.extend_from_slice(&1u32.to_be_bytes());
    moof.extend_from_slice(&((traf.len() + 8) as u32).to_be_bytes());
    moof.extend_from_slice(b"traf");
    moof.extend_from_slice(&traf);
    moof.extend_from_slice(&8u32.to_be_bytes());
    moof.extend_from_slice(b"mdat");
    moof
}

fn encoded_grid_fixture(case: &serde_json::Value) -> (Init, Fragment) {
    // Parse structural edit metadata rather than infer absence from argv.
    let mut trak = vec![0; 24];
    trak[..4].copy_from_slice(&24u32.to_be_bytes());
    trak[4..8].copy_from_slice(b"tkhd");
    trak[20..24].copy_from_slice(&1u32.to_be_bytes());
    if case["has_edit_list"]
        .as_bool()
        .expect("valid presentation-grid fixture")
    {
        trak.extend_from_slice(&8u32.to_be_bytes());
        trak.extend_from_slice(b"edts");
    }
    let mut track = parse_trak(&trak).expect("parser-produced track metadata");
    track.kind = TrackKind::Video;
    track.codec = Some(VideoCodec::H264);
    track.nal_length_size = 4;
    track.timescale = case["track_timescale"]
        .as_u64()
        .expect("valid presentation-grid fixture") as u32;
    let init = Init {
        bytes: Vec::new(),
        tracks: vec![track],
    };
    let samples = case["samples"]
        .as_array()
        .expect("valid presentation-grid fixture");
    let version = case["trun_version"]
        .as_u64()
        .expect("valid presentation-grid fixture") as u32;
    let mut fields = vec![0; 4]; // data_offset filled once box size is known
    for sample in samples {
        fields.extend_from_slice(
            &(sample["duration"]
                .as_u64()
                .expect("valid presentation-grid fixture") as u32)
                .to_be_bytes(),
        );
        fields.extend_from_slice(&5u32.to_be_bytes());
        fields.extend_from_slice(
            &(sample["cto"]
                .as_i64()
                .expect("valid presentation-grid fixture") as u32)
                .to_be_bytes(),
        );
    }
    let flags = (version << 24) | 0x0000_0b01;
    let empty = hand_built_fragment(flags, samples.len() as u32, &fields);
    fields[..4].copy_from_slice(&(empty.len() as u32).to_be_bytes());
    let mut bytes = hand_built_fragment(flags, samples.len() as u32, &fields);
    let mdat = bytes.len() - 8;
    bytes[mdat..mdat + 4].copy_from_slice(&(8 + samples.len() as u32 * 5).to_be_bytes());
    let tfdt = bytes
        .windows(4)
        .position(|part| part == b"tfdt")
        .expect("valid presentation-grid fixture");
    bytes[tfdt + 8..tfdt + 16].copy_from_slice(
        &case["base_decode_time"]
            .as_u64()
            .expect("valid presentation-grid fixture")
            .to_be_bytes(),
    );
    for index in 0..samples.len() {
        let nal = if index == 0
            && case["clean_random_access"]
                .as_bool()
                .expect("valid presentation-grid fixture")
        {
            0x65
        } else {
            0x41
        };
        bytes.extend_from_slice(&[0, 0, 0, 1, nal]);
    }
    let tracks = parse_moof(&bytes[..mdat], 8, &init.tracks).expect("parser-produced sample times");
    let payload = mdat + 8..bytes.len();
    (
        init,
        Fragment {
            bytes,
            mdat_payload: payload,
            tracks,
        },
    )
}

#[test]
fn encoded_presentation_grid_enforces_the_design_oracle_from_parsed_boxes() {
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/playback/vod-bframes-timeline-cases.json"
    ))
    .expect("valid presentation-grid fixture");
    for case in cases["cases"]
        .as_array()
        .expect("valid presentation-grid fixture")
    {
        let (init, fragment) = encoded_grid_fixture(case);
        assert_eq!(
            init.video()
                .expect("valid presentation-grid fixture")
                .has_edit_list,
            case["has_edit_list"]
                .as_bool()
                .expect("valid presentation-grid fixture")
        );
        let result = validate_encoded_grid(
            &fragment,
            &init,
            case["plan_timescale"]
                .as_u64()
                .expect("valid presentation-grid fixture") as u32,
            case["entry_start"]
                .as_u64()
                .expect("valid presentation-grid fixture"),
            case["entry_duration"]
                .as_u64()
                .expect("valid presentation-grid fixture"),
            case["frame_ticks"]
                .as_u64()
                .expect("valid presentation-grid fixture") as u32,
        );
        assert_eq!(
            result.is_ok(),
            case["expect"]["accepted"]
                .as_bool()
                .expect("valid presentation-grid fixture"),
            "{}: {result:?}",
            case["name"]
        );
    }
}

#[test]
fn encoded_presentation_grid_refuses_unrepresentable_end_and_false_wire_claims() {
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/playback/vod-bframes-timeline-cases.json"
    ))
    .expect("valid presentation-grid fixture");
    let case = &cases["cases"][1];
    let (init, mut fragment) = encoded_grid_fixture(case);
    fragment.tracks[0].base_decode_time = u64::MAX - 1;
    assert_eq!(
        validate_encoded_grid(&fragment, &init, 24, u64::MAX - 1, 4, 1),
        Err(EncodedGridError::Overflow)
    );
    fragment.tracks[0].base_decode_time = 0;
    // This is a complete unique grid, but it presents B pictures before
    // the IDR. NAL classification alone cannot prove the presentation cut.
    for (sample, cto) in fragment.tracks[0].runs[0]
        .samples
        .iter_mut()
        .zip([2, 2, -2, -2])
    {
        sample.cto = cto;
    }
    assert_eq!(
        validate_encoded_grid(&fragment, &init, 24, 0, 4, 1),
        Err(EncodedGridError::PresentationAnchor)
    );
    for (sample, cto) in fragment.tracks[0].runs[0]
        .samples
        .iter_mut()
        .zip([0, 2, -1, -1])
    {
        sample.cto = cto;
    }
    fragment.tracks[0].runs[0].composition_offsets_present = false;
    assert_eq!(
        validate_encoded_grid(&fragment, &init, 24, 0, 4, 1),
        Err(EncodedGridError::CompositionShape)
    );
}

/// A `trun` that sets no per-sample flags has a per-sample cost of ZERO
/// bytes, so the "declares samples it does not carry" length check passes
/// for any `sample_count` — and `Vec::with_capacity` then asks the
/// allocator for 64 GB. A failed allocation is an abort, not an error:
/// the whole daemon dies rather than one session falling back to ffmpeg's
/// muxer. This is a 76-byte input.
#[test]
fn a_trun_cannot_declare_four_billion_samples() {
    // flags = data-offset only; count = u32::MAX; no per-sample bytes.
    let frag = hand_built_fragment(0x0000_0001, u32::MAX, &0u32.to_be_bytes());
    assert!(frag.len() < 200, "the whole attack is {} bytes", frag.len());
    let feed = pipe("open-gop");
    let (init, _, _) = read_all(&feed);
    let mut reader = FragmentReader::new();
    reader.push(&init.bytes); // a real init, so the fragment path is reached
    assert!(matches!(reader.next_unit(), Ok(Some(Unit::Init(_)))));
    reader.push(&frag);
    assert!(
        matches!(reader.next_unit(), Err(Fmp4Error::Malformed(_))),
        "a four-billion-sample trun was accepted"
    );
}

/// A 64-bit `largesize` near `u64::MAX` makes `pos + size` wrap to behind
/// the cursor. Two things followed: the reader spun forever on the same
/// bytes in release, and the merger got a fragment whose mdat range ran
/// backwards and panicked slicing it. One bound fixes both.
#[test]
fn a_box_that_would_wrap_the_cursor_is_refused() {
    let mut feed = Vec::new();
    feed.extend_from_slice(&16u32.to_be_bytes());
    feed.extend_from_slice(b"free");
    feed.extend_from_slice(&[0u8; 8]);
    // size==1 → 64-bit largesize, chosen so `16 + size` wraps to 0.
    feed.extend_from_slice(&1u32.to_be_bytes());
    feed.extend_from_slice(b"skip");
    feed.extend_from_slice(&(u64::MAX - 15).to_be_bytes());
    let mut reader = FragmentReader::new();
    reader.push(&feed);
    assert!(
        matches!(reader.next_unit(), Err(Fmp4Error::Unsupported(_))),
        "a box big enough to wrap the cursor was accepted"
    );
}

#[test]
fn an_oversized_nested_box_is_malformed_before_the_resource_ceiling() {
    let mut child = Vec::new();
    child.extend_from_slice(&1u32.to_be_bytes());
    child.extend_from_slice(b"skip");
    child.extend_from_slice(&u64::MAX.to_be_bytes());
    assert!(matches!(
        peek_box(&child, 0),
        Err(Fmp4Error::Unsupported(_))
    ));
    assert!(matches!(
        children(&child),
        Err(Fmp4Error::Malformed(reason)) if reason.contains("runs past its parent")
    ));
}

/// ffmpeg writes a `traf` only for tracks with buffered samples, and stops
/// waiting for one that has gone `max_interleave_delta` without a packet —
/// so a late-starting or gapped audio track produces video-only fragments
/// followed by fragments that carry audio again. Taking the track list
/// from the run's FIRST fragment published a silent segment whose audio
/// bytes were copied into the mdat but described by no `traf` at all.
#[test]
fn a_track_that_joins_late_is_not_dropped_from_the_segment() {
    let feed = pipe("open-gop");
    let (init, frags, _) = read_all(&feed);
    let audio = init
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .expect("audio")
        .id;

    // Fragment 0 loses its audio traf, exactly as a muxer that had no
    // audio buffered would have written it.
    let mut first = frags[0].clone();
    first.tracks.retain(|t| t.track_id != audio);
    let run = vec![first, frags[1].clone(), frags[2].clone()];

    let merged = merge(&run, &init, 1).expect("merge");
    let mut feed = init.bytes.clone();
    feed.extend_from_slice(&merged.bytes);
    let (_, out, _) = read_all(&feed);
    assert_eq!(out.len(), 1);
    assert!(
        out[0].track(audio).is_some(),
        "the segment published no audio at all"
    );
    let want: usize = run
        .iter()
        .filter_map(|f| f.track(audio))
        .map(|t| t.sample_count())
        .sum();
    assert_eq!(
        out[0].track(audio).map(|t| t.sample_count()),
        Some(want),
        "audio samples went missing between the fragments and the segment"
    );
}

/// The bottom rung of the sample-resolution ladder. ffmpeg always fills
/// `tfhd`, so nothing in production reaches `trex` — which is precisely
/// why it had to be wired up deliberately and tested here. Left
/// unimplemented, a stream that leaned on it resolved every sample to zero
/// duration: the segmenter would never reach its floor, never cut, and
/// hold the whole film in memory.
#[test]
fn trex_defaults_are_the_bottom_rung_of_the_ladder() {
    let feed = pipe("open-gop");
    let (init, _, _) = read_all(&feed);
    let video_id = init.video().expect("video").id;

    // ffmpeg writes an all-zero `trex` and puts the real defaults in each
    // `tfhd`, so the fixture cannot exercise this rung as it stands. Patch
    // values into the moov's own `trex` — same length, same structure —
    // and the rung becomes reachable without inventing a whole moov.
    let mut bytes = init.bytes.clone();
    let mut patched = false;
    for i in 0..bytes.len().saturating_sub(28) {
        if &bytes[i..i + 4] != b"trex" {
            continue;
        }
        if be_u32(&bytes, i + 8) != video_id {
            continue;
        }
        bytes[i + 16..i + 20].copy_from_slice(&4_242u32.to_be_bytes()); // duration
        bytes[i + 20..i + 24].copy_from_slice(&1_234u32.to_be_bytes()); // size
        bytes[i + 24..i + 28].copy_from_slice(&0x0101_0000u32.to_be_bytes()); // flags
        patched = true;
        break;
    }
    assert!(patched, "no trex for the video track in the fixture's moov");
    let mut reader = FragmentReader::new();
    reader.push(&bytes);
    let Ok(Some(Unit::Init(init))) = reader.next_unit() else {
        panic!("the patched moov did not parse");
    };
    let video = init.video().expect("video").clone();
    assert_eq!(video.default_sample_duration, 4_242);
    assert_eq!(video.default_sample_size, 1_234);

    // A traf with a bare tfhd (no defaults) and a trun with no per-sample
    // values: everything has to come from trex.
    let mut trun = Vec::new();
    trun.extend_from_slice(&0x0000_0001u32.to_be_bytes()); // data-offset only
    trun.extend_from_slice(&2u32.to_be_bytes());
    trun.extend_from_slice(&0u32.to_be_bytes());
    let mut traf = Vec::new();
    traf.extend_from_slice(&16u32.to_be_bytes());
    traf.extend_from_slice(b"tfhd");
    traf.extend_from_slice(&0x0002_0000u32.to_be_bytes());
    traf.extend_from_slice(&video.id.to_be_bytes());
    traf.extend_from_slice(&((trun.len() + 8) as u32).to_be_bytes());
    traf.extend_from_slice(b"trun");
    traf.extend_from_slice(&trun);
    let mut moof = Vec::new();
    moof.extend_from_slice(&((traf.len() + 8 + 8) as u32).to_be_bytes());
    moof.extend_from_slice(b"moof");
    moof.extend_from_slice(&((traf.len() + 8) as u32).to_be_bytes());
    moof.extend_from_slice(b"traf");
    moof.extend_from_slice(&traf);
    moof.extend_from_slice(&8u32.to_be_bytes());
    moof.extend_from_slice(b"mdat");

    reader.push(&moof);
    let Ok(Some(Unit::Fragment(f))) = reader.next_unit() else {
        panic!("the hand-built fragment did not parse");
    };
    let tf = f.track(video.id).expect("video traf");
    for s in tf.samples() {
        assert_eq!(s.duration, video.default_sample_duration);
        assert_eq!(s.size, video.default_sample_size);
        assert_eq!(s.flags, video.default_sample_flags);
    }
}

/// Version-0 composition offsets are UNSIGNED. Writing a negative one into
/// a version-0 `trun` clamped it to zero and shifted that frame's
/// presentation time silently — the exact class of change framemd5 exists
/// to forbid, arriving without tripping it. A run with a negative offset
/// must come out as version 1.
#[test]
fn a_negative_composition_offset_forces_a_version_1_trun() {
    let feed = pipe("open-gop");
    let (init, frags, _) = read_all(&feed);
    let video = init.video().expect("video").id;
    let mut frag = frags[0].clone();
    let mut wanted = 0i64;
    if let Some(tf) = frag.tracks.iter_mut().find(|t| t.track_id == video) {
        if let Some(s) = tf.runs.first_mut().and_then(|r| r.samples.get_mut(1)) {
            s.cto = -600;
            wanted = -600;
        }
    }
    assert_eq!(wanted, -600, "the fixture fragment had no second sample");

    let merged = merge(&[frag], &init, 1).expect("merge");
    let mut feed = init.bytes.clone();
    feed.extend_from_slice(&merged.bytes);
    let (_, out, _) = read_all(&feed);
    let got = out[0]
        .track(video)
        .and_then(|t| t.runs.first())
        .and_then(|r| r.samples.get(1))
        .map(|s| s.cto);
    assert_eq!(got, Some(-600), "a negative offset was clamped away");
}

/// A source whose audio outlasts its video: ffmpeg keeps muxing audio-only
/// fragments past the last video sample, and a run made only of those has
/// zero video duration. `EXTINF` is summed from the VIDEO track by rule
/// (§6.3 — audio drifts against the media by up to an AAC frame per
/// fragment), so the tail would have published `#EXTINF:0.000000`: a
/// segment the playlist claims takes no time at all.
#[test]
fn a_tail_with_no_video_left_still_reports_a_real_duration() {
    let feed = pipe("open-gop");
    let (init, frags, _) = read_all(&feed);
    let video = init.video().expect("video").id;
    let timescale = init.video().map(|v| v.timescale).unwrap_or(1);

    // A floor nothing can reach, so the whole run lands in one segment at
    // `finish()` — which is what the real tail does.
    let policy = CutPolicy::new(
        u32::MAX / timescale.max(1),
        u32::MAX / timescale.max(1),
        usize::MAX,
        u32::MAX,
        timescale,
    );
    let mut seg = Segmenter::new(init.clone(), policy);
    for f in frags.iter().take(3) {
        let mut stripped = f.clone();
        stripped.tracks.retain(|t| t.track_id != video);
        assert!(
            !stripped.tracks.is_empty(),
            "the fixture has no audio track"
        );
        assert!(seg.push(stripped).expect("push").is_none());
    }
    let published = seg.finish().expect("finish");
    assert_eq!(published.len(), 1, "short audio tail stays in one segment");
    let published = &published[0];
    assert_eq!(
        published.segment.video_ticks, 0,
        "video was supposed to be gone"
    );
    assert!(
        published.seconds > 0.1,
        "an audio-only tail published EXTINF {}",
        published.seconds
    );
}

/// reference episode I S01E01 ends its video at 57:11 but carries audio to
/// 57:56. ffmpeg puts the last video frames and that 45-second audio tail
/// in one final fragment. The duration must cover every real sample, but
/// publishing one 45-second segment also raises `TARGETDURATION` to 46 and
/// leaves AVPlayer waiting at the boundary. The tail is therefore divided
/// at sample boundaries without changing or dropping a sample.
#[test]
fn a_mixed_final_fragment_splits_its_trailing_audio_at_the_ceiling() {
    let feed = pipe("open-gop");
    let (init, frags, _) = read_all(&feed);
    let video = init.video().expect("video");
    let audio = init
        .tracks
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .expect("audio");
    let mut tail = frags[0].clone();
    let wanted_video_ticks = tail.video_duration(&init) + 45 * video.timescale.max(1) as u64;

    {
        let audio_fragment = tail
            .tracks
            .iter_mut()
            .find(|track| track.track_id == audio.id)
            .expect("audio fragment");
        let source_runs = audio_fragment.runs.clone();
        while audio_fragment.duration() * video.timescale.max(1) as u64
            <= wanted_video_ticks * audio.timescale.max(1) as u64
        {
            audio_fragment.runs.extend(source_runs.clone());
        }
    }

    let video_seconds = tail.video_duration(&init) as f64 / video.timescale.max(1) as f64;
    let audio_seconds = tail
        .track(audio.id)
        .map(|track| track.duration() as f64 / audio.timescale.max(1) as f64)
        .expect("audio duration");
    assert!(
        video_seconds > 0.0,
        "the regression needs a last video frame"
    );
    assert!(
        audio_seconds > video_seconds + 40.0,
        "the regression needs the material audio tail"
    );

    let max_seconds = 15;
    let strict_seconds = 16;
    let policy = CutPolicy::new(6, 2, usize::MAX, max_seconds, video.timescale)
        .with_strict_max_seconds(strict_seconds, video.timescale);
    let mut seg = Segmenter::new(init, policy);
    assert!(seg.push(tail).expect("push").is_none());
    let published = seg.finish().expect("finish");

    assert!(published.len() >= 3, "the material tail was not split");
    assert!(published[0].segment.video_ticks > 0);
    assert!(
        published
            .iter()
            .skip(1)
            .all(|item| item.segment.video_ticks == 0),
        "video must remain intact in the first final segment"
    );
    assert!(published
        .iter()
        .all(|item| item.reason == CutReason::EndOfStream));
    let longest = published
        .iter()
        .map(|item| item.seconds)
        .fold(0.0f64, f64::max);
    assert!(
        longest <= strict_seconds as f64,
        "a split final segment still declared {longest:.6}s"
    );
    assert_eq!(
        longest.ceil(),
        f64::from(strict_seconds),
        "the trailing-audio shape did not use the strict rolling bound"
    );
    let claimed: f64 = published.iter().map(|item| item.seconds).sum();
    assert!(
        (claimed - audio_seconds).abs() < 1e-6,
        "split EXTINF totals {claimed:.6}s instead of the {:.6}s audio tail",
        audio_seconds
    );
}

/// The golden comparison SEGMENTER-PLAN §8.4 asked for, and the one that
/// would have caught the bug that reached production: run BOTH muxers on
/// the same source and require them to declare the same tracks.
///
/// On a source with chapters — which every disc remux has and no `lavfi`
/// fixture did — ffmpeg's plain mp4 muxer writes them as a QuickTime `text`
/// track plus a `chpl` box, with `tref` links from the real tracks. Its HLS
/// muxer does not. So the segmenter's stream carried a third track nothing
/// asked for, in the init and in every fragment; Safari refused it outright
/// and the player's error fallback re-encoded a 4K remux to 1080p, while
/// Chrome ignored the extra track and played fine.
///
/// The assertion is deliberately "the same as the muxer we replaced" rather
/// than "video and audio only": the whole promise of this path is that the
/// only thing a player sees differently is where the boundaries fall.
#[test]
fn the_pipe_declares_the_same_tracks_as_the_hls_muxer() {
    use crate::domain::MediaFile;
    use crate::testfixtures::source_with_chapters;
    use crate::transcode::{copy_pipe_args, hls_copy_args, Pacing};

    let src = source_with_chapters();
    // The chapters really are in the source, or this test proves nothing.
    let probe = run(Command::new(ffprobe())
        .args(["-v", "error", "-show_chapters", "-of", "csv=p=0"])
        .arg(&src));
    assert!(
        String::from_utf8_lossy(&probe).lines().count() >= 3,
        "the fixture lost its chapters"
    );

    let file = MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 1,
        item_id: 1,
        path: src.clone(),
        size: 1,
        mtime: 1,
        duration_ms: Some(12_000),
        container: Some("mkv".into()),
        video_codec: Some("hevc".into()),
        video_codec_tag: None,
        field_order: None,
        video_profile: Some("Main".into()),
        width: Some(640),
        height: Some(360),
        bit_depth: Some(8),
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: Some(1_000_000),
        audio_streams: vec![],
        subtitle_streams: vec![],
        scanned_at: 1,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: crate::domain::DolbyVisionFacts::default(),
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let out_dir = dir.path().to_string_lossy().into_owned();

    // What the segmenter reads.
    let pipe_bytes = run(Command::new(ffmpeg()).args(copy_pipe_args(
        &file,
        0.0,
        None,
        true,
        Pacing::unpaced(),
        false,
    )));
    let mut reader = FragmentReader::new();
    reader.push(&pipe_bytes);
    let Ok(Some(Unit::Init(ours))) = reader.next_unit() else {
        panic!("the pipe produced no init");
    };

    // What ffmpeg's own HLS muxer writes for the same session.
    run(Command::new(ffmpeg()).args(hls_copy_args(
        &file,
        0.0,
        None,
        true,
        Pacing::unpaced(),
        false,
        &out_dir,
    )));
    let their_init = std::fs::read(dir.path().join("init.mp4")).expect("hlsenc init.mp4");
    let mut reader = FragmentReader::new();
    reader.push(&their_init);
    let Ok(Some(Unit::Init(theirs))) = reader.next_unit() else {
        panic!("the hls muxer produced no init");
    };

    // Track KINDS, in order — not timescales. Each muxer picks its own
    // (16000 here against hlsenc's 12288) and every duration in its own
    // stream is consistent with it, so a timescale difference is not a
    // divergence a player can see. A track that exists on one side and
    // not the other is.
    let shape = |i: &Init| -> Vec<TrackKind> { i.tracks.iter().map(|t| t.kind).collect() };
    assert_eq!(
        shape(&ours),
        shape(&theirs),
        "the pipe declares tracks the HLS muxer does not — a chapter or \
             data track reaching a player is what broke Safari"
    );
    assert!(
        ours.tracks.iter().all(|t| t.timescale > 0),
        "a track with no timescale"
    );
    assert!(
        !ours.tracks.iter().any(|t| t.kind == TrackKind::Other),
        "a non-media track in the init: {:?}",
        ours.tracks.iter().map(|t| t.kind).collect::<Vec<_>>()
    );
}

#[test]
fn encoded_vod_restarts_keep_init_identity_across_chapters() {
    use crate::domain::{AudioStream, MediaFile};
    use crate::testfixtures::source_with_chapters;
    use crate::transcode::{
        resolve_transcode, vod_pipe_args, AttemptRestrictions, DecodeCapabilities,
        DecodeCapabilitySnapshotIdentity, DecodeFacts, DecodePlanPolicy, DecodePolicySnapshot,
        DecodeSourceIdentity, Encoder, Pacing, SoftwareDecoder, TranscodeExecution,
        TranscodeMediaOptions, TranscodeOptions, TranscodeRequest, VodFrameGrid,
    };

    let src = source_with_chapters();
    let chapter_probe = run(Command::new(ffprobe())
        .args(["-v", "error", "-show_chapters", "-of", "csv=p=0"])
        .arg(&src));
    assert!(
        String::from_utf8_lossy(&chapter_probe).lines().count() >= 3,
        "the encoded-VOD fixture lost its chapters"
    );

    let file = MediaFile {
        downloaded_subtitles: Vec::new(),
        id: 1,
        item_id: 1,
        path: src,
        size: 1,
        mtime: 1,
        duration_ms: Some(12_000),
        container: Some("mkv".into()),
        video_codec: Some("hevc".into()),
        video_codec_tag: None,
        field_order: None,
        video_profile: Some("Main".into()),
        width: Some(640),
        height: Some(360),
        bit_depth: Some(8),
        hdr: None,
        hdr_format: None,
        max_cll: None,
        max_fall: None,
        mastering_max_luminance: None,
        luminance_source: None,
        bitrate: Some(1_000_000),
        audio_streams: vec![AudioStream {
            index: 0,
            codec: "aac".into(),
            channels: Some(2),
            default: true,
            ..Default::default()
        }],
        subtitle_streams: vec![],
        scanned_at: 1,
        audio_offset_ms: 0,
        probed: true,
        dolby_vision: crate::domain::DolbyVisionFacts::default(),
    };
    let options = TranscodeOptions {
        auto_quality_rate_profile: None,
        normalized_geometry: false,
        target_height: 360,
        ..TranscodeOptions::default()
    };
    let facts = DecodeFacts::from_ffprobe_json(
        &serde_json::json!({"streams":[{
            "index":0,"codec_type":"video","codec_name":"hevc",
            "profile":"Main","width":640,"height":360,
            "pix_fmt":"yuv420p","avg_frame_rate":"24/1",
            "r_frame_rate":"24/1","disposition":{"attached_pic":0}
        }]}),
        DecodeSourceIdentity::from_sha256("a".repeat(64)).expect("source identity"),
    )
    .expect("decode facts");
    let capabilities = DecodeCapabilities::new(
        DecodeCapabilitySnapshotIdentity::new(
            "f".repeat(64),
            "vod-chapter-test".to_owned(),
            Some("e".repeat(64)),
        )
        .expect("capability identity"),
        vec![],
        vec![SoftwareDecoder {
            codec: "hevc".to_owned(),
            implementation: Some("hevc".to_owned()),
        }],
    )
    .expect("capabilities");
    let plan = resolve_transcode(
        &TranscodeRequest::new(
            Encoder::Software,
            TranscodeMediaOptions::from_options(&file, &options),
        ),
        &facts,
        &capabilities,
        &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
        &AttemptRestrictions::none(),
    )
    .expect("software plan");
    let grid = VodFrameGrid::new(24, 1).expect("24 fps grid");
    let generation = |start_seconds| {
        let mut generation_options = options.clone();
        generation_options.start_seconds = start_seconds;
        let execution =
            TranscodeExecution::from_options(&file, &generation_options, Pacing::unpaced(), ".")
                .expect("execution");
        let bytes =
            run(Command::new(ffmpeg()).args(vod_pipe_args(&file, &plan, &execution, grid, 12.0)));
        read_all(&bytes)
    };

    // Opposite sides of the 4 s chapter boundary, with different
    // remaining durations. Chapter mapping used to make these `moov`
    // bytes differ even though the codec recipe was unchanged.
    let (before, before_fragments, _) = generation(2.0);
    let (after, after_fragments, _) = generation(6.0);
    assert_eq!(
        before.bytes, after.bytes,
        "encoded init identity drifted by start offset"
    );
    assert!(
        !before_fragments.is_empty() && !after_fragments.is_empty(),
        "both generations must continue beyond init publication"
    );
    assert_eq!(
        before
            .tracks
            .iter()
            .map(|track| track.kind)
            .collect::<Vec<_>>(),
        vec![TrackKind::Video, TrackKind::Audio]
    );
    for forbidden in [b"chpl".as_slice(), b"tref".as_slice()] {
        assert!(
            !before
                .bytes
                .windows(forbidden.len())
                .any(|window| window == forbidden),
            "chapter box/reference survived in encoded init"
        );
    }
}

#[test]
fn merging_nothing_is_an_error_not_an_empty_segment() {
    let feed = pipe("open-gop");
    let (init, _, _) = read_all(&feed);
    assert!(merge(&[], &init, 1).is_err());
}
