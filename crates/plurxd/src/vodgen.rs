//! VOD generation engine is shared from plurx-core.

pub use plurx_core::vod_generation::*;

#[cfg(test)]
use crate::copyseg::sanitize_stale_dolby_brand;
#[cfg(test)]
use crate::renditiondir::InitIdentity;
#[cfg(test)]
use plurx_core::fmp4::{CutPolicy, FragmentReader, Init, Unit};
#[cfg(test)]
use plurx_core::segplan::{
    entry_row, FragmentIndex, PlanEntryKind, SegmentPlan, LANDING_WINDOW,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use plurx_core::domain::MediaFile;
    use plurx_core::segplan::{SourceIdentity, TrackDurations};
    use plurx_core::testfixtures::{self, pipe};
    use plurx_core::transcode::{copy_pipe_args, Pacing};

    use crate::fragindex::{index_stream, DolbyVisionPass, IndexOutcome};

    #[tokio::test]
    async fn reconstructed_nonzero_window_publishes_aligned_video_and_aac_timestamps() {
        let mut command = std::process::Command::new(testfixtures::ffmpeg());
        command.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-copyts",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:r=24:d=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=4",
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-vf",
            "format=yuv420p10le,settb=1/24,setpts=PTS+48",
            "-c:v",
            "libx265",
            "-x265-params",
            "pools=none:frame-threads=1:wpp=0",
            "-threads:v",
            "1",
            "-bf",
            "0",
            "-g",
            "48",
            "-c:a",
            "aac",
            "-af",
            "asettb=expr=1/48000,asetpts=PTS+1024",
            "-threads:a",
            "1",
            "-ar",
            "48000",
            "-enc_time_base:v",
            "1:24",
            "-video_track_timescale",
            "24",
            "-fps_mode:v",
            "passthrough",
            "-avoid_negative_ts",
            "disabled",
            "-use_editlist",
            "0",
            "-movflags",
            "+empty_moov+delay_moov+default_base_moof+frag_keyframe+frag_discont",
            "-t",
            "4.021333333",
            "-f",
            "mp4",
            "pipe:1",
        ]);
        let feed = testfixtures::run(&mut command);
        let init = muxer_init(&feed);
        let video_id = init.video().expect("encoded video").id;
        let audio_id = init
            .tracks
            .iter()
            .find(|track| track.kind == plurx_core::fmp4::TrackKind::Audio)
            .expect("encoded AAC")
            .id;
        let identity =
            InitIdentity::establish(&init, Default::default()).expect("encoded init identity");
        let grid = plurx_core::transcode::VodFrameGrid::new(24, 1).expect("video clock");
        let generation = Generation {
            plan: grid.plan(4_000, 0),
            index: None,
            landing_clock: None,
            encoded_frame_ticks: Some(1),
            encoded_audio_anchor: Some(0),
            encoded_video_origin: Some(48),
            identity,
            start_entry: 1,
            policy: CutPolicy::new(2, 2, 48_000_000, 15, 24),
            convert_dolby_vision: false,
            retain_hevc_parameter_sets: true,
        };
        let sink = MemSink::default();
        let outcome = run(&feed[..], generation, &sink, "reconstructed-audio").await;
        assert!(
            matches!(
                outcome,
                Outcome::Ran {
                    produced_through: Some(1)
                }
            ),
            "{outcome:?}"
        );
        let writes = sink.taken();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, 1);
        let mut reader = FragmentReader::new();
        reader.push(&init.bytes);
        reader.push(&writes[0].1);
        let mut first_video = None;
        let mut first_audio = None;
        let mut video_frames = 0;
        let mut video_end = 0;
        let mut audio_end = 0;
        while let Some(unit) = reader.next_unit().expect("actual published boxes") {
            if let Unit::Fragment(fragment) = unit {
                for track in fragment.tracks {
                    if track.track_id == video_id {
                        first_video.get_or_insert(track.base_decode_time);
                        video_frames += track.sample_count();
                        video_end = video_end.max(
                            track.base_decode_time
                                + track
                                    .samples()
                                    .map(|sample| u64::from(sample.duration))
                                    .sum::<u64>(),
                        );
                    }
                    if track.track_id == audio_id {
                        first_audio.get_or_insert(track.base_decode_time);
                        audio_end = audio_end.max(
                            track.base_decode_time
                                + track
                                    .samples()
                                    .map(|sample| u64::from(sample.duration))
                                    .sum::<u64>(),
                        );
                    }
                }
            }
        }
        assert_eq!(first_video, Some(48));
        assert_eq!(first_audio, Some(96_256));
        assert_eq!(video_frames, 48);
        assert_eq!(video_end, 96);
        assert!(
            (192_000..=193_024).contains(&audio_end),
            "AAC reaches the same window end: {audio_end}"
        );
        assert!(
            first_audio.expect("actual audio") - first_video.expect("actual video") * 2_000 < 1_024,
            "AAC remains within one packet of the same film cut"
        );
    }

    #[tokio::test]
    async fn completed_profile81_authoring_reaches_the_real_publisher() {
        use plurx_core::transcode::dv_processing::dv_author_profile81_window;
        let encoded = include_bytes!("../../plurx-core/tests/fixtures/dv-runtime/authored.mp4");
        let rpus = vec![
            include_bytes!("../../plurx-core/tests/fixtures/dv-runtime/frame-000.nal").to_vec(),
            include_bytes!("../../plurx-core/tests/fixtures/dv-runtime/frame-001.nal").to_vec(),
            include_bytes!("../../plurx-core/tests/fixtures/dv-runtime/frame-002.nal").to_vec(),
        ];
        let mut reader = FragmentReader::new();
        reader.push(encoded);
        let Some(Unit::Init(mut base_init)) = reader.next_unit().expect("fixture init") else {
            panic!("missing init")
        };
        plurx_core::fmp4::remove_dolby_vision_record(&mut base_init).expect("base config");
        let video = base_init.video().expect("video").id;
        let mut base = base_init.bytes.clone();
        while let Some(unit) = reader.next_unit().expect("fixture fragment") {
            if let Unit::Fragment(mut fragment) = unit {
                plurx_core::fmp4::rewrite_video_samples(
                    &mut fragment,
                    &base_init.tracks,
                    video,
                    |sample| {
                        let mut output = Vec::new();
                        let mut at = 0;
                        while at < sample.len() {
                            let length = u32::from_be_bytes(
                                sample[at..at + 4].try_into().expect("NAL length"),
                            ) as usize;
                            let end = at + 4 + length;
                            if (sample[at + 4] >> 1) & 63 != 62 {
                                output.extend_from_slice(&sample[at..end]);
                            }
                            at = end;
                        }
                        Ok(output)
                    },
                )
                .expect("strip control RPU");
                base.extend_from_slice(&fragment.bytes);
            }
        }
        assert!(
            reader.saw_trailer(),
            "the retained encoder actually completed"
        );
        // Preserve its authentic completion input. The author rebuilds the
        // random-access trailer after changing sample sizes.
        let mut offset = 0;
        while offset < encoded.len() {
            let length =
                u32::from_be_bytes(encoded[offset..offset + 4].try_into().expect("box length"))
                    as usize;
            if &encoded[offset + 4..offset + 8] == b"mfra" {
                base.extend_from_slice(&encoded[offset..offset + length]);
            }
            offset += length;
        }
        let feed = dv_author_profile81_window(&base, (64, 64), 24000, 48048, 1001, &rpus, 1)
            .expect("completed author");
        let init = muxer_init(&feed);
        let grid = plurx_core::transcode::VodFrameGrid::new(24000, 1001).expect("grid");
        let generation = Generation {
            plan: grid.plan(125, 0),
            index: None,
            landing_clock: None,
            encoded_frame_ticks: Some(1001),
            encoded_audio_anchor: None,
            encoded_video_origin: Some(48048),
            identity: InitIdentity::establish(&init, Default::default())
                .expect("authored identity"),
            start_entry: 0,
            policy: CutPolicy::new(2, 2, 48_000_000, 15, 24000),
            convert_dolby_vision: false,
            retain_hevc_parameter_sets: false,
        };
        let sink = MemSink::default();
        let outcome = run(&feed[..], generation, &sink, "completed-p81").await;
        assert!(
            matches!(
                outcome,
                Outcome::Ran {
                    produced_through: Some(0)
                }
            ),
            "P81 publisher outcome: {outcome:?}"
        );
        assert_eq!(sink.taken().len(), 1);
    }

    /// A sink that remembers what it was handed and can start refusing.
    #[derive(Default)]
    struct MemSink {
        writes: Mutex<Vec<(u32, Vec<u8>)>>,
        /// Refuse with this error kind once this many writes have landed.
        refuse_after: Option<(usize, std::io::ErrorKind)>,
    }

    impl MemSink {
        fn refusing(after: usize, kind: std::io::ErrorKind) -> MemSink {
            MemSink {
                writes: Mutex::new(Vec::new()),
                refuse_after: Some((after, kind)),
            }
        }

        fn taken(&self) -> Vec<(u32, Vec<u8>)> {
            self.writes.lock().expect("sink lock").clone()
        }
    }

    impl Sink for MemSink {
        async fn materialize(&self, entry: u32, bytes: Vec<u8>) -> std::io::Result<()> {
            let mut writes = self.writes.lock().expect("sink lock");
            if let Some((after, kind)) = self.refuse_after {
                if writes.len() >= after {
                    return Err(std::io::Error::from(kind));
                }
            }
            writes.push((entry, bytes));
            Ok(())
        }
    }

    /// Everything one fixture film's generations share.
    struct Film {
        index: FragmentIndex,
        plan: SegmentPlan,
        identity: InitIdentity,
        policy: CutPolicy,
        video_id: u32,
        /// The production pipe, cached — what a full-film generation reads.
        feed: Vec<u8>,
    }

    /// The video-only index pipe over a fixture — the exact command
    /// `fragindex`'s own tests run (`copy_index_pipe_args`' shape).
    fn index_pipe_bytes(kind: &str) -> Vec<u8> {
        let source = testfixtures::source(kind);
        let mut command = std::process::Command::new(testfixtures::ffmpeg());
        command
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&source)
            .args(["-map_chapters", "-1", "-map", "0:v:0?", "-an", "-sn"])
            .args(["-c:v", "copy", "-tag:v", "hvc1"])
            .args(["-bsf:v", "filter_units=remove_types=32-34"])
            .args(["-avoid_negative_ts", "make_zero"])
            .args([
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof+delay_moov",
            ])
            .args(["-use_editlist", "0", "-f", "mp4", "pipe:1"]);
        testfixtures::run(&mut command)
    }

    /// The production pipe's muxer init — `Unit::Init`, the byte string the
    /// identity check compares.
    fn muxer_init(feed: &[u8]) -> Init {
        let mut reader = FragmentReader::new();
        reader.push(feed);
        loop {
            match reader.next_unit().expect("parsing the pipe") {
                Some(Unit::Init(init)) => return init,
                Some(_) => continue,
                None => panic!("the pipe carried no init"),
            }
        }
    }

    /// A real film: index from the video-only pipe, plan from the index,
    /// identity established from the production pipe's own first generation —
    /// the whole M3 attach sequence, in miniature.
    ///
    /// Brisk policy for `copyseg`'s reason: the 12 s fixture cannot reach a
    /// mid-film entry under the shipped 6 s floor, and the property under
    /// test is the plan being followed, not the constant.
    async fn film() -> Film {
        let feed = pipe("clean-cra");
        let index = match index_stream(
            std::io::Cursor::new(index_pipe_bytes("clean-cra")),
            SourceIdentity::new(1, 1, "fingerprint"),
            None,
            DolbyVisionPass::Untouched,
        )
        .await
        {
            IndexOutcome::Built(index) => *index,
            other => panic!("the fixture must index: {other:?}"),
        };
        let policy = CutPolicy::new(3, 1, 48_000_000, 15, index.timescale);
        let plan = plurx_core::segplan::plan_copy(
            &index,
            &policy,
            &TrackDurations {
                video_ms: 12_000,
                audio_ms: 12_000,
                audio_bits_per_second: 256_000,
            },
        );
        let mut init = muxer_init(&feed);
        sanitize_stale_dolby_brand(&mut init);
        let video_id = init.video().expect("a video track").id;
        let identity =
            InitIdentity::establish(&init, index.promotion.clone()).expect("establishing identity");
        Film {
            index,
            plan,
            identity,
            policy,
            video_id,
            feed,
        }
    }

    /// The same fixture with real Profile 7 RPUs in every video sample,
    /// indexed and identified the way a converting session's rendition is.
    ///
    /// Every step is the production one: the index is built by the converting
    /// index pass, the identity is established from the converted pass's
    /// promotion inputs, and the feed is a production pipe — video and audio
    /// from one open of one file, which is the whole design.
    async fn converting_film() -> Film {
        testfixtures::require_ffmpeg();
        let feed = testfixtures::with_dolby_vision_rpus(&pipe("clean-cra"));
        let record = plurx_core::fmp4::DolbyVisionRecord::new(8, 6, false, true, true, 1)
            .expect("a describable record");
        let indexed = testfixtures::with_dolby_vision_rpus(&index_pipe_bytes("clean-cra"));
        let index = match index_stream(
            std::io::Cursor::new(indexed),
            SourceIdentity::new(1, 1, "fingerprint+p81"),
            None,
            DolbyVisionPass::Rewrite(Box::new(record)),
        )
        .await
        {
            IndexOutcome::Built(index) => *index,
            other => panic!("the converting pass must index: {other:?}"),
        };
        let policy = CutPolicy::new(3, 1, 48_000_000, 15, index.timescale);
        let plan = plurx_core::segplan::plan_copy(
            &index,
            &policy,
            &TrackDurations {
                video_ms: 12_000,
                audio_ms: 12_000,
                audio_bits_per_second: 256_000,
            },
        );
        let mut init = muxer_init(&feed);
        sanitize_stale_dolby_brand(&mut init);
        let video_id = init.video().expect("a video track").id;
        let identity =
            InitIdentity::establish(&init, index.promotion.clone()).expect("establishing identity");
        Film {
            index,
            plan,
            identity,
            policy,
            video_id,
            feed,
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|at| at == needle)
    }

    /// A converting generation converts, and what it materializes is the
    /// converted stream.
    ///
    /// Two halves, and both are load-bearing. The first is that no Profile 7
    /// RPU reaches the client: the media is what the playlist says it is,
    /// checked against the fixture's own bytes rather than against a tally the
    /// conversion keeps about itself. The second is that a generation which
    /// did *not* convert cannot land at all — the index it lands against was
    /// built from converted fragments, and an 8.1 RPU is smaller than the
    /// Profile 7 one it replaces, so every `video_bytes` disagrees. That is
    /// the property that makes the two streams uncombinable by construction,
    /// which is what stops a converting session ever being served unconverted
    /// bytes under a Dolby Vision label.
    #[tokio::test]
    async fn a_converting_generation_serves_the_converted_stream() {
        let film = converting_film().await;
        let rpu = testfixtures::p7_rpu();
        assert!(
            contains(&film.feed, &rpu),
            "the feed carries the Profile 7 RPUs the conversion is for"
        );

        let sink = MemSink::default();
        let mut converting = generation(&film, 0);
        converting.convert_dolby_vision = true;
        let outcome = run(&film.feed[..], converting, &sink, "test").await;
        let Outcome::Ran { produced_through } = outcome else {
            panic!("the converting generation did not run: {outcome:?}");
        };
        assert_eq!(produced_through, Some(film.plan.len() as u32 - 1));

        let writes = sink.taken();
        assert_eq!(writes.len(), film.plan.len());
        for (entry, bytes) in &writes {
            assert!(
                !contains(mdat_payload(bytes), &rpu),
                "entry {entry} carried an unrewritten Profile 7 RPU"
            );
        }

        // And the same generation with the conversion off cannot land.
        let off = MemSink::default();
        let refused = run(&film.feed[..], generation(&film, 0), &off, "off").await;
        assert!(
            matches!(refused, Outcome::Failed(_)),
            "an unconverted generation must not land on a converted index, \
             got {refused:?}"
        );
        assert!(off.taken().is_empty());
    }

    /// A refusal after the landing fails the generation rather than
    /// publishing what it could not rewrite.
    ///
    /// This is the last guard, and it is the only one. Once `engage()` has
    /// run, `push_to_segmenter` compares nothing against the index — the
    /// landing matcher reads only what was buffered before it resolved — so a
    /// conversion that refuses on fragment 200 (an unreadable RPU, a Profile 8
    /// one spliced in from another source, a `trun` shape the rewrite cannot
    /// correct) would otherwise pass that fragment through with its Profile 7
    /// RPUs intact, into a segment served under a playlist advertising
    /// `dvh1.08.06`. A decoder that said it takes 8 and not 7 would be handed
    /// 7 partway through the film, with nothing anywhere reporting a problem.
    #[tokio::test]
    async fn a_refusal_after_the_landing_fails_the_generation() {
        let film = converting_film().await;
        // Past the landing window, so the generation is producing by the time
        // the bad fragment arrives — which is the whole point of the test.
        let broken_at = LANDING_WINDOW + 1;
        assert!(
            film.index.rows.len() > broken_at,
            "the fixture must have a fragment past the landing window"
        );
        let feed = testfixtures::with_one_unconvertible_rpu(&film.feed, broken_at);

        let sink = MemSink::default();
        let mut converting = generation(&film, 0);
        converting.convert_dolby_vision = true;
        let outcome = run(&feed[..], converting, &sink, "test").await;
        let Outcome::Failed(Failure::Stream(reason)) = outcome else {
            panic!("an unconvertible RPU must fail the generation, got {outcome:?}");
        };
        assert!(reason.contains("cannot be converted"), "{reason}");

        // Whatever it published before the refusal is entries the plan
        // promised, produced from fragments that did convert. What must not
        // exist is a segment built from the fragment that did not — so the
        // run has to have stopped, not skipped.
        let writes = sink.taken();
        assert!(
            writes.len() < film.plan.len(),
            "a refused generation must not have finished the plan"
        );

        // The same feed with the conversion off is the control: it fails for a
        // different reason (it cannot land on a converted index at all), which
        // is what makes the assertion above about the refusal and not about
        // the corruption.
        let off = MemSink::default();
        let refused = run(&feed[..], generation(&film, 0), &off, "off").await;
        assert!(matches!(refused, Outcome::Failed(_)), "{refused:?}");
    }

    /// An init the conversion cannot describe fails before a single segment.
    ///
    /// Refusing here rather than at the first fragment is the difference
    /// between a failed generation and a published segment: the rendition's
    /// playlist already names every entry, so anything materialized under one
    /// of those indexes is a permanent cache hit that admission can make
    /// durable.
    #[tokio::test]
    async fn an_init_the_conversion_cannot_describe_fails_before_any_segment() {
        let film = converting_film().await;
        // `hvcC` spells the NAL length width as `lengthSizeMinusOne`, so 2
        // means a three-byte prefix — a value the field can hold and no
        // container carries. A converter that accepted it would read a byte of
        // picture as the top of a NAL length.
        let feed = with_hvcc_length_size(&film.feed, 3);
        let mut init = muxer_init(&feed);
        sanitize_stale_dolby_brand(&mut init);
        let identity = InitIdentity::establish(&init, film.index.promotion.clone())
            .expect("establishing identity");

        let sink = MemSink::default();
        let outcome = run(
            &feed[..],
            Generation {
                convert_dolby_vision: true,
                identity,
                ..generation(&film, 0)
            },
            &sink,
            "test",
        )
        .await;
        let Outcome::Failed(Failure::Stream(reason)) = outcome else {
            panic!("an undescribable init must fail the generation, got {outcome:?}");
        };
        assert!(reason.contains("cannot be converted"), "{reason}");
        assert!(
            sink.taken().is_empty(),
            "the refusal must land before any segment is materialized"
        );
    }

    /// Rewrite the `hvcC`'s `lengthSizeMinusOne` in a stream's init.
    ///
    /// A one-byte edit in a fixed-width field, so nothing resizes and every
    /// offset in the stream stays where it was.
    fn with_hvcc_length_size(stream: &[u8], width: u8) -> Vec<u8> {
        let mut out = stream.to_vec();
        let at = out
            .windows(4)
            .position(|window| window == b"hvcC")
            .expect("an hvcC box");
        // The field is the 22nd byte of the record, low two bits.
        let field = at + 4 + 21;
        out[field] = (out[field] & 0xfc) | (width - 1);
        out
    }

    fn generation(film: &Film, start_entry: u32) -> Generation {
        Generation {
            convert_dolby_vision: false,
            retain_hevc_parameter_sets: false,
            plan: film.plan.clone(),
            index: Some(film.index.clone()),
            landing_clock: None,
            encoded_audio_anchor: None,
            encoded_frame_ticks: None,
            encoded_video_origin: None,
            identity: film.identity.clone(),
            start_entry,
            policy: film.policy,
        }
    }

    /// Walk one level of MP4 boxes.
    fn box_walk<'a>(region: &'a [u8], mut visit: impl FnMut(&[u8; 4], &'a [u8])) {
        let mut at = 0usize;
        while at + 8 <= region.len() {
            let size =
                u32::from_be_bytes(region[at..at + 4].try_into().expect("a box size")) as usize;
            if size < 8 || at + size > region.len() {
                break;
            }
            let name: [u8; 4] = region[at + 4..at + 8].try_into().expect("a box name");
            visit(&name, &region[at + 8..at + size]);
            at += size;
        }
    }

    /// A materialized segment's `mdat` payload. The merger copies each
    /// track's samples contiguously, video first, so the film's video bytes
    /// are the payload's prefix.
    fn mdat_payload(segment: &[u8]) -> &[u8] {
        let mut found = None;
        box_walk(segment, |name, body| {
            if name == b"mdat" && found.is_none() {
                found = Some(body);
            }
        });
        found.expect("a materialized segment carries an mdat")
    }

    /// The video `tfdt` a materialized segment actually carries, read off the
    /// bytes rather than off what the segmenter believed.
    fn video_tfdt(segment: &[u8], video_id: u32) -> u64 {
        let mut moofs = Vec::new();
        box_walk(segment, |name, body| {
            if name == b"moof" {
                moofs.push(body);
            }
        });
        let moof = moofs.first().expect("a segment carries a moof");
        let mut trafs = Vec::new();
        box_walk(moof, |name, body| {
            if name == b"traf" {
                trafs.push(body);
            }
        });
        for traf in trafs {
            let mut id = None;
            let mut tfdt = None;
            box_walk(traf, |name, body| match name {
                b"tfhd" if body.len() >= 8 => {
                    id = Some(u32::from_be_bytes(
                        body[4..8].try_into().expect("a track id"),
                    ));
                }
                b"tfdt" if body.len() >= 12 => {
                    tfdt = Some(if body[0] == 1 {
                        u64::from_be_bytes(body[4..12].try_into().expect("a v1 tfdt"))
                    } else {
                        u64::from(u32::from_be_bytes(
                            body[4..8].try_into().expect("a v0 tfdt"),
                        ))
                    });
                }
                _ => {}
            });
            if id == Some(video_id) {
                return tfdt.expect("the video traf carries a tfdt");
            }
        }
        panic!("no video traf in the segment");
    }

    /// The byte offset of the end of the pipe's `fragments`-th fragment —
    /// a cut there is exactly what a SIGKILL between two flushes leaves:
    /// a drained pipe, on a boundary, with no trailer. Verbatim contiguity
    /// (init, then moof+mdat pairs, nothing between) is asserted as we walk,
    /// so the cut cannot silently land inside some box this walk skipped.
    fn fragment_boundary_cut(feed: &[u8], fragments: usize) -> usize {
        let mut reader = FragmentReader::new();
        reader.push(feed);
        let mut cut = 0usize;
        let mut seen = 0usize;
        while let Some(unit) = reader.next_unit().expect("parsing the pipe") {
            match unit {
                Unit::Init(init) => {
                    assert_eq!(&feed[..init.bytes.len()], &init.bytes[..]);
                    cut += init.bytes.len();
                }
                Unit::Fragment(fragment) => {
                    assert_eq!(&feed[cut..cut + fragment.len()], &fragment.bytes[..]);
                    cut += fragment.len();
                    seen += 1;
                    if seen == fragments {
                        return cut;
                    }
                }
                Unit::Trailer => {}
            }
        }
        panic!("the pipe carried only {seen} fragments");
    }

    /// The film's video payload of one plan entry, in bytes — summed
    /// `video_bytes` of the index rows the entry covers. This is the quantity
    /// that must come out identical from every generation, because video
    /// samples are copied.
    fn video_payload_len(film: &Film, entry: u32) -> usize {
        let mut row = 0usize;
        for earlier in &film.plan.entries[..entry as usize] {
            row += earlier.fragments as usize;
        }
        let count = film.plan.entries[entry as usize].fragments as usize;
        film.index.rows[row..row + count]
            .iter()
            .map(|row| row.video_bytes as usize)
            .sum()
    }

    /// The whole point, end to end: a generation started at entry 0
    /// materializes every entry the plan promised, in order, each exactly
    /// once, and says how far it got.
    #[tokio::test]
    async fn a_full_film_generation_materializes_every_plan_entry_in_order() {
        let film = film().await;
        assert!(
            film.plan.len() >= 4,
            "the fixture must plan several entries under the brisk policy, \
             got {}",
            film.plan.len()
        );
        let sink = MemSink::default();
        let outcome = run(&film.feed[..], generation(&film, 0), &sink, "test").await;
        let Outcome::Ran { produced_through } = outcome else {
            panic!("the generation did not run: {outcome:?}");
        };
        let writes = sink.taken();
        assert_eq!(writes.len(), film.plan.len(), "an entry per plan entry");
        for (offset, (entry, bytes)) in writes.iter().enumerate() {
            assert_eq!(*entry as usize, offset, "entries must arrive in order");
            assert!(!bytes.is_empty());
        }
        assert_eq!(produced_through, Some(film.plan.len() as u32 - 1));
    }

    /// The rendition serves whichever generation's bytes happen to be on
    /// disk, so a re-run of the same generation must produce the same bytes.
    #[tokio::test]
    async fn rerunning_a_generation_materializes_identical_bytes() {
        let film = film().await;
        let first = MemSink::default();
        let second = MemSink::default();
        let one = run(&film.feed[..], generation(&film, 0), &first, "test").await;
        let two = run(&film.feed[..], generation(&film, 0), &second, "test").await;
        assert!(matches!(one, Outcome::Ran { .. }), "{one:?}");
        assert!(matches!(two, Outcome::Ran { .. }), "{two:?}");
        let first = first.taken();
        let second = second.taken();
        assert_eq!(first.len(), second.len());
        assert_eq!(
            first[0], second[0],
            "entry 0's bytes must not depend on which run produced them"
        );
        assert_eq!(first, second, "every entry, not only the first");
    }

    /// Plan §2.2's identity check, at the runner: a pipe whose muxer init is
    /// not the rendition's is refused before a single sink write.
    #[tokio::test]
    async fn a_drifted_muxer_init_is_refused_before_a_single_sink_write() {
        let film = film().await;
        // A different fixture's pipe: a stand-in for a changed video
        // pipeline under a rendition whose playlist a client already holds.
        let drifted = pipe("closed-gop");
        let sink = MemSink::default();
        let outcome = run(&drifted[..], generation(&film, 0), &sink, "test").await;
        assert!(
            matches!(outcome, Outcome::Failed(Failure::InitDrift(_))),
            "a drifted init must fail typed, got {outcome:?}"
        );
        assert!(
            sink.taken().is_empty(),
            "a refused generation must not have written anything"
        );
    }

    /// The core noncontiguous-correctness assertion: a real repositioned
    /// producer — `copy_pipe_args` with a start offset, a live ffmpeg —
    /// lands, discards forward to its entry's boundary, and its first
    /// materialized index is exactly `start_entry`, carrying the same film
    /// video bytes the full-film run put under that index and starting at
    /// the film time the plan named.
    ///
    /// Whole-segment byte identity across generations is not a property this
    /// path has: the audio branch is re-encoded from the seek point, so its
    /// bytes differ by construction, and each generation's timeline is
    /// anchored at its own first boundary. What is invariant — and what a
    /// player needs — is the entry's video payload and its planned index and
    /// start.
    #[tokio::test]
    async fn a_mid_film_generation_lands_on_its_entry_and_matches_the_full_run() {
        let film = film().await;
        // The reference: the same entries from a full-film generation.
        let full = MemSink::default();
        let outcome = run(&film.feed[..], generation(&film, 0), &full, "full").await;
        assert!(matches!(outcome, Outcome::Ran { .. }), "{outcome:?}");
        let full = full.taken();

        let start_entry = 2u32;
        let entry = film.plan.entry(start_entry).expect("a mid-film entry");
        assert_eq!(entry.kind, PlanEntryKind::Video);
        let target_row = film
            .index
            .rows
            .iter()
            .position(|row| row.dts == entry.start_ticks)
            .expect("the entry begins at an index row");
        assert!(
            target_row >= 1,
            "the entry must have a fragment in front of it for the seek to \
             land on"
        );
        // Seek strictly between the previous fragment boundary and the
        // entry's: `-noaccurate_seek` then lands on the RAP *before* the
        // entry, so the landing has a real discard to count.
        let mid_ticks = (film.index.rows[target_row - 1].dts + entry.start_ticks) / 2;
        let start_seconds = mid_ticks as f64 / f64::from(film.index.timescale);

        let src = testfixtures::source("clean-cra");
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
            dolby_vision: Default::default(),
        };
        let args = copy_pipe_args(&file, start_seconds, None, true, Pacing::unpaced(), false);
        let mut child = tokio::process::Command::new(testfixtures::ffmpeg())
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawning ffmpeg");
        let stdout = child.stdout.take().expect("stdout pipe");

        let sink = MemSink::default();
        let outcome = run(stdout, generation(&film, start_entry), &sink, "mid").await;
        let _ = child.wait().await;

        let Outcome::Ran { produced_through } = outcome else {
            panic!("the repositioned generation did not run: {outcome:?}");
        };
        let writes = sink.taken();
        assert!(!writes.is_empty(), "it must have materialized something");
        assert_eq!(
            writes[0].0, start_entry,
            "the discards must land the generation exactly on its entry"
        );
        for (offset, (entry, _)) in writes.iter().enumerate() {
            assert_eq!(*entry, start_entry + offset as u32);
        }
        assert_eq!(
            produced_through,
            Some(film.plan.len() as u32 - 1),
            "a mid-film generation runs to the end of the plan"
        );

        // The media-time contract: the segment begins at the film time the
        // plan named for it, whatever timestamps ffmpeg handed us.
        assert_eq!(
            video_tfdt(&writes[0].1, film.video_id),
            entry.start_ticks,
            "the entry must begin at the plan's own film time"
        );

        // And it carries the same film: the entry's video payload —
        // copied samples, the invariant quantity — is byte-identical to
        // what the full-film run put under the same index.
        let payload = video_payload_len(&film, start_entry);
        assert!(payload > 0);
        let of_full = mdat_payload(&full[start_entry as usize].1);
        let of_mid = mdat_payload(&writes[0].1);
        assert!(of_full.len() >= payload && of_mid.len() >= payload);
        assert_eq!(
            of_mid[..payload],
            of_full[..payload],
            "entry {start_entry}'s video bytes differ between generations"
        );
    }

    /// A killed producer: the pipe stops mid-fragment. What reached the sink
    /// is exactly the entries that completed — no tail, no claim of the rest
    /// of the film — and the generation still Ran.
    #[tokio::test]
    async fn a_truncated_pipe_materializes_what_completed_and_no_more() {
        let film = film().await;
        let cut = film.feed.len() * 2 / 3;
        let sink = MemSink::default();
        let outcome = run(&film.feed[..cut], generation(&film, 0), &sink, "test").await;
        let Outcome::Ran { produced_through } = outcome else {
            panic!("a killed pipe still Ran: {outcome:?}");
        };
        let writes = sink.taken();
        assert!(!writes.is_empty(), "two thirds of the film cut something");
        assert!(
            writes.len() < film.plan.len(),
            "a killed pipe must not claim the whole plan"
        );
        for (offset, (entry, _)) in writes.iter().enumerate() {
            assert_eq!(*entry as usize, offset);
        }
        assert_eq!(produced_through, Some(writes.len() as u32 - 1));
    }

    /// The kill that copyseg's rule would mistake for a clean end: ffmpeg
    /// flushes the pipe per fragment, so a SIGKILL routinely leaves the pipe
    /// drained exactly on a fragment boundary, mid-entry, with no trailer.
    /// The pending rump of the half-read entry must NOT reach the sink —
    /// published under its real index it would be a permanent cache hit
    /// carrying a fraction of its media — and the generation still Ran,
    /// ending at the last complete entry.
    #[tokio::test]
    async fn a_kill_on_a_fragment_boundary_publishes_no_partial_entry() {
        let film = film().await;
        // Four fragments in: entry 0 (fragment 0) and entry 1 (fragments
        // 1-2) are complete, and fragment 3 — half of entry 2 — is pending.
        let cut = fragment_boundary_cut(&film.feed, 4);
        assert!(cut < film.feed.len());
        let sink = MemSink::default();
        let outcome = run(&film.feed[..cut], generation(&film, 0), &sink, "test").await;
        let Outcome::Ran { produced_through } = outcome else {
            panic!("a killed pipe still Ran: {outcome:?}");
        };
        let indexes: Vec<u32> = sink.taken().iter().map(|(entry, _)| *entry).collect();
        assert_eq!(
            indexes,
            vec![0, 1],
            "only the entries whose media fully arrived may materialize; a \
             drained pipe with no trailer is a killed pipe, not a finished \
             film"
        );
        assert_eq!(produced_through, Some(1));
    }

    #[tokio::test]
    async fn completed_output_notification_requires_real_trailer_and_successful_tail_writes() {
        use std::sync::atomic::{AtomicU32, Ordering};
        #[derive(Default)]
        struct ObservedSink {
            inner: MemSink,
            completed: AtomicU32,
        }
        impl Sink for ObservedSink {
            async fn materialize(&self, entry: u32, bytes: Vec<u8>) -> std::io::Result<()> {
                self.inner.materialize(entry, bytes).await
            }
            async fn completed_output(&self) {
                self.completed.fetch_add(1, Ordering::Relaxed);
            }
        }
        let film = film().await;
        let full = ObservedSink::default();
        assert!(matches!(
            run(
                &film.feed[..],
                generation(&film, 0),
                &full,
                "measurement-full"
            )
            .await,
            Outcome::Ran { .. }
        ));
        assert_eq!(full.completed.load(Ordering::Relaxed), 1);
        let truncated = ObservedSink::default();
        let cut = fragment_boundary_cut(&film.feed, 4);
        assert!(matches!(
            run(
                &film.feed[..cut],
                generation(&film, 0),
                &truncated,
                "measurement-cut"
            )
            .await,
            Outcome::Ran { .. }
        ));
        assert_eq!(truncated.completed.load(Ordering::Relaxed), 0);
        let refused = ObservedSink {
            inner: MemSink::refusing(1, std::io::ErrorKind::Other),
            completed: AtomicU32::new(0),
        };
        assert!(matches!(
            run(
                &film.feed[..],
                generation(&film, 0),
                &refused,
                "measurement-refused"
            )
            .await,
            Outcome::Failed(_)
        ));
        assert_eq!(refused.completed.load(Ordering::Relaxed), 0);
    }

    /// The production pipe with its audio outrunning the plan: the fixture's
    /// 12 s of film against 12.3 s of tone, under a plan whose probe said the
    /// tracks are equal — the shape of a tail the plan skipped. The plan
    /// names no tail entry, while `Segmenter::finish` splits at the exact
    /// video boundary and hands the trailing audio back as its own chunk —
    /// one index past the plan. (The tone runs 0.3 s over rather than the
    /// threshold's 50 ms because `-avoid_negative_ts make_zero` stretches
    /// the video timeline by the audio priming delay, and a sliver under one
    /// AAC frame past the stretched video end never leaves the last chunk.)
    fn audiotail_pipe() -> Vec<u8> {
        let source = testfixtures::source("clean-cra");
        let mut command = std::process::Command::new(testfixtures::ffmpeg());
        command
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&source)
            .args(["-f", "lavfi", "-t", "12.3"])
            .args(["-i", "sine=frequency=440:sample_rate=48000"])
            .args(["-map", "0:v:0", "-map", "1:a:0", "-sn"])
            .args(["-c:v", "copy", "-tag:v", "hvc1"])
            .args(["-bsf:v", "filter_units=remove_types=32-34"])
            .args(["-c:a", "aac", "-b:a", "256k"])
            .args(["-avoid_negative_ts", "make_zero"])
            .args([
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof+delay_moov",
            ])
            .args(["-use_editlist", "0", "-f", "mp4", "pipe:1"]);
        testfixtures::run(&mut command)
    }

    /// The end-of-film twin of the kill test: a source whose audio outruns
    /// the plan's last entry makes `finish` emit one chunk at an index the
    /// playlist never names. Nothing can ever fetch it, so it is dropped
    /// with a warning — not handed to a sink that would refuse it as
    /// out-of-plan and poison the rendition at the end of every complete
    /// watch of an affected title.
    #[tokio::test]
    async fn an_unplanned_rump_one_past_the_plan_is_dropped_not_a_failure() {
        let film = film().await;
        let feed = audiotail_pipe();
        // The sliver-bearing pipe is its own first generation: same video
        // branch (so the film's index and plan still describe it), its own
        // muxer init to establish against.
        let mut init = muxer_init(&feed);
        sanitize_stale_dolby_brand(&mut init);
        let identity = InitIdentity::establish(&init, film.index.promotion.clone())
            .expect("establishing identity");
        let generation = Generation {
            convert_dolby_vision: false,
            retain_hevc_parameter_sets: false,
            plan: film.plan.clone(),
            index: Some(film.index.clone()),
            landing_clock: None,
            encoded_audio_anchor: None,
            encoded_frame_ticks: None,
            encoded_video_origin: None,
            identity,
            start_entry: 0,
            policy: film.policy,
        };
        let planned = film.plan.len() as u32;
        let sink = MemSink::default();
        let outcome = run(&feed[..], generation, &sink, "test").await;
        let Outcome::Ran { produced_through } = outcome else {
            panic!("an unplanned rump must not fail the generation: {outcome:?}");
        };
        let writes = sink.taken();
        assert_eq!(
            writes.len(),
            planned as usize,
            "every planned entry and nothing else"
        );
        assert!(
            writes.iter().all(|(entry, _)| *entry < planned),
            "the sink must never see an index the plan does not name"
        );
        assert_eq!(produced_through, Some(planned - 1));
    }

    /// A sink whose directory went away is a session ending, not a fault:
    /// stop quietly, report what was produced.
    #[tokio::test]
    async fn a_sink_that_vanished_ends_the_generation_quietly() {
        let film = film().await;
        let sink = MemSink::refusing(1, std::io::ErrorKind::NotFound);
        let outcome = run(&film.feed[..], generation(&film, 0), &sink, "test").await;
        assert!(
            matches!(
                outcome,
                Outcome::Ran {
                    produced_through: Some(0)
                }
            ),
            "a vanished sink ends the session with what landed: {outcome:?}"
        );
        assert_eq!(sink.taken().len(), 1);
    }

    /// Any other sink refusal is a typed failure the serving layer reports.
    #[tokio::test]
    async fn a_sink_refusal_other_than_absence_fails_typed() {
        let film = film().await;
        let sink = MemSink::refusing(1, std::io::ErrorKind::PermissionDenied);
        let outcome = run(&film.feed[..], generation(&film, 0), &sink, "test").await;
        let Outcome::Failed(Failure::Sink(error)) = outcome else {
            panic!("a refused write must fail typed: {outcome:?}");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    /// The ffmpeg-upgrade case, at the runner: an index that no longer
    /// describes the pipe's fragments misses rather than misaligns, and the
    /// generation fails typed before anything is written.
    #[tokio::test]
    async fn an_index_that_no_longer_describes_the_pipe_fails_the_landing() {
        let mut film = film().await;
        for row in &mut film.index.rows {
            row.video_bytes = row.video_bytes.wrapping_add(1);
        }
        let sink = MemSink::default();
        let outcome = run(&film.feed[..], generation(&film, 0), &sink, "test").await;
        assert!(
            matches!(outcome, Outcome::Failed(Failure::Landing(_))),
            "a stale index must be a typed landing failure, got {outcome:?}"
        );
        assert!(sink.taken().is_empty());
    }

    /// The fixture film behind four extra index rows whose first three
    /// weigh what the film's opening three fragments weigh, and the first
    /// video entry past them.
    ///
    /// A generation fed the film from its top lands at index row 4, but its
    /// first three fragments match rows 0..3 just as well: three fragments
    /// cannot tell the two apart, and the fourth — which the extra rows make
    /// different — can. Both sit at or before the target row.
    async fn film_with_an_early_repeat() -> (Film, u32) {
        let mut film = film().await;
        let real = film.index.rows.clone();
        assert!(real.len() >= 4, "the fixture needs four fragments");
        let step = real[0].duration;
        let base = real[0].dts;
        let mut rows: Vec<plurx_core::segplan::IndexRow> = (0..4u64)
            .map(|offset| plurx_core::segplan::IndexRow {
                dts: base + offset * step,
                duration: step,
                ..real[(offset % 3) as usize].clone()
            })
            .collect();
        rows[3].video_bytes = real[3].video_bytes.wrapping_add(1);
        rows.extend(real.iter().map(|row| plurx_core::segplan::IndexRow {
            dts: row.dts + 4 * step,
            ..row.clone()
        }));
        film.index.rows = rows;
        film.plan = plurx_core::segplan::plan_copy(
            &film.index,
            &film.policy,
            &TrackDurations {
                video_ms: 12_000,
                audio_ms: 12_000,
                audio_bits_per_second: 256_000,
            },
        );
        let start_entry = film
            .plan
            .entries
            .iter()
            .filter(|entry| entry.kind == PlanEntryKind::Video)
            .find(|entry| entry_row(&film.index, entry).is_some_and(|row| row >= 4))
            .map(|entry| entry.index)
            .expect("the shifted plan has a video entry inside the real film");
        (film, start_entry)
    }

    /// Lab F1 (2026-10-11): a frontier generation failed "landing
    /// undetermined" because three byte counts elsewhere in the index matched
    /// its first three fragments. The producer now reads on while the landing
    /// is ambiguous, and the true landing is the one that keeps matching.
    #[tokio::test]
    async fn an_ambiguous_landing_reads_further_and_lands_on_its_entry() {
        let (film, start_entry) = film_with_an_early_repeat().await;
        let sink = MemSink::default();
        let outcome = run(&film.feed[..], generation(&film, start_entry), &sink, "f1").await;
        assert!(
            matches!(outcome, Outcome::Ran { .. }),
            "an early byte-count repeat must not fail the landing: {outcome:?}"
        );
        let writes = sink.taken();
        assert_eq!(
            writes.first().map(|(entry, _)| *entry),
            Some(start_entry),
            "the landing at row 4 must discard forward to entry {start_entry}"
        );
    }

    /// The first video entry whose row is further from row 0 than a landing
    /// window, so a generation fed from the top of the film lands more
    /// fragments before its target than the buffer that decided the landing.
    fn an_entry_past_the_landing_window(film: &Film) -> u32 {
        film.plan
            .entries
            .iter()
            .filter(|entry| entry.kind == PlanEntryKind::Video)
            .find(|entry| entry_row(&film.index, entry).is_some_and(|row| row > LANDING_WINDOW))
            .map(|entry| entry.index)
            .expect("the fixture needs a video entry past the landing window")
    }

    /// Review of F1 (2026-10-11): `engage` dropped only the discards its
    /// three-fragment buffer held. A landing further back than that threw the
    /// rest of the count away and published the fragments in front of the
    /// target under the target's index — the wrong film, silently.
    #[tokio::test]
    async fn a_landing_further_back_than_its_buffer_discards_all_the_way_to_its_entry() {
        let film = film().await;
        let full = MemSink::default();
        let outcome = run(&film.feed[..], generation(&film, 0), &full, "full").await;
        assert!(matches!(outcome, Outcome::Ran { .. }), "{outcome:?}");
        let full = full.taken();

        let start_entry = an_entry_past_the_landing_window(&film);
        let sink = MemSink::default();
        let outcome = run(&film.feed[..], generation(&film, start_entry), &sink, "far").await;
        assert!(matches!(outcome, Outcome::Ran { .. }), "{outcome:?}");
        let writes = sink.taken();
        assert_eq!(writes.first().map(|(entry, _)| *entry), Some(start_entry));
        let payload = video_payload_len(&film, start_entry);
        assert!(payload > 0);
        let of_full = mdat_payload(&full[start_entry as usize].1);
        let of_far = mdat_payload(&writes[0].1);
        assert!(of_full.len() >= payload && of_far.len() >= payload);
        assert_eq!(
            of_far[..payload],
            of_full[..payload],
            "entry {start_entry} must carry its own video, not the fragments in front of it"
        );
    }

    /// A landing that the next fragments contradict is a coincidence, not a
    /// landing: rows 0..3 still weigh what the generation emits, row 3 no
    /// longer does. This is also the shape of a true landing past the target
    /// with a three-fragment coincidence before it.
    #[tokio::test]
    async fn a_landing_its_later_fragments_contradict_fails_typed() {
        let mut film = film().await;
        let start_entry = an_entry_past_the_landing_window(&film);
        let row = &mut film.index.rows[LANDING_WINDOW];
        row.video_bytes = row.video_bytes.wrapping_add(1);
        let sink = MemSink::default();
        let outcome = run(
            &film.feed[..],
            generation(&film, start_entry),
            &sink,
            "liar",
        )
        .await;
        let Outcome::Failed(Failure::Landing(why)) = &outcome else {
            panic!("a contradicted landing must fail typed: {outcome:?}");
        };
        assert!(why.contains("did not hold"), "{why}");
        assert!(
            sink.taken().is_empty(),
            "nothing of the wrong film is published"
        );
    }

    /// Reading further is bounded by one segment's byte ceiling; past it a
    /// landing that is still ambiguous stays the typed refusal it always was.
    #[tokio::test]
    async fn an_ambiguous_landing_past_the_segment_ceiling_is_refused() {
        let (film, start_entry) = film_with_an_early_repeat().await;
        let mut generation = generation(&film, start_entry);
        generation.policy.max_bytes = 1;
        let sink = MemSink::default();
        let outcome = run(&film.feed[..], generation, &sink, "f1").await;
        let Outcome::Failed(Failure::Unplaced { why, transient }) = &outcome else {
            panic!("an unresolved landing must fail typed: {outcome:?}");
        };
        assert!(why.contains("undetermined"), "{why}");
        assert!(!transient, "nothing could change the same bytes' answer");
        assert!(sink.taken().is_empty());
    }

    /// A clock that answers `answer` once.
    fn clock(answer: Result<LandingTime, String>) -> Option<LandingClock> {
        Some(Box::new(move || Box::pin(async move { answer })))
    }

    /// A time between two fragment starts: it names no row.
    fn off_index(film: &Film) -> LandingTime {
        let row = &film.index.rows[1];
        LandingTime {
            ticks: row.dts + row.duration / 2,
            frame_ticks: 1,
        }
    }

    /// Review of F1: the first ambiguity went straight to the landing time,
    /// and a time the index refused failed the whole rendition — where before
    /// a repeat the next fragment settles landed by its bytes. The time is
    /// now one more piece of evidence: refused or unmeasured, the producer
    /// reads further.
    #[tokio::test]
    async fn a_landing_time_that_does_not_place_it_leaves_the_bytes_to_settle_it() {
        let (film, start_entry) = film_with_an_early_repeat().await;
        for answer in [Ok(off_index(&film)), Err("probe timed out".to_owned())] {
            let mut generation = generation(&film, start_entry);
            generation.landing_clock = clock(answer.clone());
            let sink = MemSink::default();
            let outcome = run(&film.feed[..], generation, &sink, "f1").await;
            assert!(
                matches!(outcome, Outcome::Ran { .. }),
                "{answer:?} must not fail the landing: {outcome:?}"
            );
            assert_eq!(
                sink.taken().first().map(|(entry, _)| *entry),
                Some(start_entry)
            );
        }
    }

    /// Past the reading ceiling an unsettled landing is entry-scoped, and only
    /// a time that was never measured leaves it open to another try.
    #[tokio::test]
    async fn an_unsettled_landing_names_what_the_time_said() {
        let (film, start_entry) = film_with_an_early_repeat().await;
        for (answer, expect_transient) in [
            (Ok(off_index(&film)), false),
            (Err("probe timed out".to_owned()), true),
        ] {
            let mut generation = generation(&film, start_entry);
            generation.policy.max_bytes = 1;
            generation.landing_clock = clock(answer);
            let sink = MemSink::default();
            let outcome = run(&film.feed[..], generation, &sink, "f1").await;
            let Outcome::Failed(Failure::Unplaced { why, transient }) = &outcome else {
                panic!("an unsettled landing is entry-scoped: {outcome:?}");
            };
            assert_eq!(*transient, expect_transient, "{why}");
            assert!(why.contains("landing time"), "{why}");
            assert!(sink.taken().is_empty());
        }
    }
}
