//! The ride's own parts, with no child: placing the pass's packet clock on
//! the source's, which titles ride, and a ride that falls behind.
use super::*;
use crate::fragindex::LeadFragments;
use plurx_core::fmp4::{FragmentReader, Unit};

const PROBE: &str = r#"{
  "streams": [
    {"index": 0, "codec_type": "video", "time_base": "1/1000", "start_pts": 0},
    {"index": 1, "codec_type": "audio", "time_base": "1/1000", "start_pts": -21}
  ],
  "format": {"start_time": "-0.021000"}
}"#;

fn packet(dts: i64, pts: i64) -> SourcePacketObservation {
    SourcePacketObservation {
        sequence: 0,
        dts,
        pts,
        duration: 41,
        size: 8052,
        checksum: 0,
        flags: 1,
        side_data_count: 0,
        time_base_num: 1,
        time_base_den: 1000,
    }
}

/// FFmpeg without `-copyts` moves every packet by the format start. On a
/// file whose audio leads by 21 ms the first video packet is written at
/// -62/21 where the source, and the engine's `-copyts` clock, has -83/0. The
/// origin is recovered only when the start is a whole number of ticks and
/// the corrected first presentation time is the one the probe reported.
#[test]
fn the_source_origin_is_recovered_only_from_an_exact_and_witnessed_clock() {
    let clock = ProbedCopyClock::from_probe(PROBE).expect("a probed clock");
    assert_eq!(
        clock.source_origin(&packet(-62, 21)),
        Ok(SourceTimestamp {
            dts: -83,
            time_base_num: 1,
            time_base_den: 1000,
        })
    );
    // The witness: a first packet that is not the stream's first picture.
    assert_eq!(
        clock.source_origin(&packet(-62, 62)),
        Err("copy_clock_unknown".to_owned())
    );
    // Another time base than the probed stream's.
    let mut other = packet(-62, 21);
    other.time_base_den = 90_000;
    assert!(clock.source_origin(&other).is_err());
    // A start that is not a whole number of stream ticks is never rounded.
    let inexact = ProbedCopyClock::from_probe(&PROBE.replace("-0.021000", "-0.021500"))
        .expect("a probed clock");
    assert!(inexact.source_origin(&packet(-62, 21)).is_err());
    // A zero start is the source's own clock.
    let zero =
        ProbedCopyClock::from_probe(&PROBE.replace("-0.021000", "0.000000")).expect("a clock");
    assert_eq!(
        zero.source_origin(&packet(-83, 0)).map(|origin| origin.dts),
        Ok(-83)
    );

    // No format start, no video stream, or no first presentation time: the
    // engine refuses an unknown clock and the ride does not start.
    for unknown in [
        PROBE.replace("\"-0.021000\"", "\"N/A\""),
        PROBE.replace(
            "\"format\": {\"start_time\": \"-0.021000\"}",
            "\"format\": {}",
        ),
        PROBE.replace("\"video\"", "\"data\""),
        PROBE.replace(", \"start_pts\": 0}", "}"),
        "not json".to_owned(),
    ] {
        assert_eq!(ProbedCopyClock::from_probe(&unknown), None, "{unknown}");
    }
}

const VIDEO: CopyVideoOptions = CopyVideoOptions::new(false, false);

/// A catalogue row for `id` in `container`, over a small file that exists.
fn media_file(dir: &std::path::Path, id: i64, container: &str) -> (MediaFile, SourceStamp) {
    let path = dir.join(format!("{id}.{container}"));
    std::fs::write(&path, b"not media").expect("a source to stat");
    let mut file =
        crate::subtitle_ride_along::synthetic_media_file(&path, 12_000).expect("a catalogue row");
    file.id = id;
    file.container = Some(container.to_owned());
    file.video_codec = Some("hevc".to_owned());
    let stamp =
        crate::fragment_index_cluster::source_stamp(&std::fs::metadata(&path).expect("metadata"));
    (file, stamp)
}

fn probed() -> Option<ProbedCopyClock> {
    ProbedCopyClock::from_probe(PROBE)
}

/// Only Matroska and ISO base media ride; every other container, and a
/// title with no probed clock, is left to the standalone form.
#[test]
fn only_matroska_and_iso_media_with_a_probed_clock_ride() {
    let dir = crate::test_tempdir().expect("directory");
    for (at, container) in ["mkv", "webm", "mp4", "m4v", "mov"].into_iter().enumerate() {
        let (file, stamp) = media_file(dir.path(), 74_100 + at as i64, container);
        assert!(
            CopyRidePlan::for_pass(&file, VIDEO, probed(), stamp).is_some(),
            "{container} rides"
        );
        assert!(
            CopyRidePlan::for_pass(&file, VIDEO, None, stamp).is_none(),
            "{container} without a probed clock does not"
        );
    }
    for (at, container) in ["ts", "m2ts", "avi", "wmv", "mpg", "ogv", "flv"]
        .into_iter()
        .enumerate()
    {
        let (file, stamp) = media_file(dir.path(), 74_200 + at as i64, container);
        assert!(
            CopyRidePlan::for_pass(&file, VIDEO, probed(), stamp).is_none(),
            "{container} does not ride"
        );
    }
    let (mut unnamed, stamp) = media_file(dir.path(), 74_300, "mkv");
    unnamed.container = None;
    assert!(CopyRidePlan::for_pass(&unnamed, VIDEO, probed(), stamp).is_none());
}

/// The fixture pipe's init and first fragment, as the lead reader parses
/// them.
fn parsed_head() -> (Init, Fragment) {
    plurx_core::testfixtures::require_ffmpeg();
    let mut reader = FragmentReader::new();
    reader.push(&plurx_core::testfixtures::pipe("closed-gop"));
    let Some(Unit::Init(init)) = reader.next_unit().expect("fixture pipe") else {
        panic!("fixture init");
    };
    let Some(Unit::Fragment(fragment)) = reader.next_unit().expect("fixture pipe") else {
        panic!("fixture fragment");
    };
    (init, fragment)
}

/// A ride whose packets never arrive holds what the lead reader hands it up
/// to its bound and is then abandoned: once, counted, visible on the meter,
/// and taking nothing more. Finishing it does not wait for a clock that
/// never ends, and the source's next pass does not ride until the source
/// changes.
#[tokio::test]
async fn a_ride_that_falls_behind_is_abandoned_counted_and_not_ridden_again() {
    let dir = crate::test_tempdir().expect("directory");
    let (file, stamp) = media_file(dir.path(), 74_400, "mkv");
    let (init, fragment) = parsed_head();
    let plan = CopyRidePlan::for_pass(&file, VIDEO, probed(), stamp)
        .expect("a ride")
        .without_clock_for_test(fragment.len() + 1);
    // A clock that stays open and silent for as long as the test holds it.
    let (_open, clock) = tokio::io::duplex(64);
    let ride = CopyRide::start(plan, &file, clock);
    let (tap, meter) = (ride.tap(), ride.meter());
    let abandoned_before = crate::copy_analysis::rides(RideOutcome::Abandoned);

    tap.init(&init);
    tap.fragment(None, fragment.clone());
    assert!(
        !meter.abandoned() && tap.wants_fragments(),
        "within the bound"
    );
    tap.fragment(None, fragment.clone());
    assert!(meter.abandoned() && !tap.wants_fragments(), "past it");
    tap.fragment(None, fragment);
    tap.end();
    assert!(
        crate::copy_analysis::rides(RideOutcome::Abandoned) > abandoned_before,
        "an abandonment is counted"
    );

    assert!(
        ride.finish(true).await.is_none(),
        "an abandoned ride keeps no rows, and its open clock is not waited for"
    );
    assert!(
        CopyRidePlan::for_pass(&file, VIDEO, probed(), stamp).is_none(),
        "the same source does not ride again"
    );
    let mut replaced = stamp;
    replaced.size += 1;
    assert!(
        CopyRidePlan::for_pass(&file, VIDEO, probed(), replaced).is_some(),
        "another source identity is another question"
    );
}

/// A pass that did not build keeps nothing and is not waited for either:
/// the clock reader of a child that may still be alive is stopped.
#[tokio::test]
async fn a_pass_that_did_not_build_stops_its_ride_without_waiting() {
    let dir = crate::test_tempdir().expect("directory");
    let (file, stamp) = media_file(dir.path(), 74_500, "mkv");
    let plan = CopyRidePlan::for_pass(&file, VIDEO, probed(), stamp).expect("a ride");
    let (_open, clock) = tokio::io::duplex(64);
    let ride = CopyRide::start(plan, &file, clock);
    let not_built_before = crate::copy_analysis::rides(RideOutcome::NotBuilt);
    assert!(ride.finish(false).await.is_none());
    assert!(crate::copy_analysis::rides(RideOutcome::NotBuilt) > not_built_before);
    assert!(CopyRidePlan::for_pass(&file, VIDEO, probed(), stamp).is_none());
}
