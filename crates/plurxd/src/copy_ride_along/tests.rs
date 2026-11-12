//! The ride's own parts, with no child: placing the pass's packet clock on
//! the source's, the backlog bound, and the tee on the lead output's reader.
use super::*;

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

fn new_tap(limit: usize) -> (CopyRideTap, tokio::sync::mpsc::UnboundedReceiver<RideEvent>) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    (
        CopyRideTap {
            shared: Arc::new(TapShared {
                queued: AtomicUsize::new(0),
                limit,
                abandoned: AtomicBool::new(false),
                rows: AtomicUsize::new(0),
            }),
            sender,
        },
        receiver,
    )
}

/// The pass's readers never wait on the ride. What they hand over is held
/// up to the backlog bound; the first offer past it abandons the ride, and
/// from then on nothing is copied or queued.
#[test]
fn an_offer_past_the_backlog_abandons_the_ride_and_never_waits() {
    let (tap, mut receiver) = new_tap(8);
    tap.canonical(&[1, 2, 3, 4]);
    tap.canonical(&[5, 6, 7, 8]);
    assert!(!tap.shared.abandoned.load(Ordering::Acquire));
    tap.canonical(&[9]);
    assert!(tap.shared.abandoned.load(Ordering::Acquire));
    tap.canonical(&[10]);
    tap.canonical_eof();
    let mut held = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        match event {
            RideEvent::Canonical(bytes) => held.extend(bytes),
            _ => panic!("nothing else was accepted"),
        }
    }
    assert_eq!(held, [1, 2, 3, 4, 5, 6, 7, 8]);

    // An observer that is gone abandons the ride too.
    let (tap, receiver) = new_tap(8);
    drop(receiver);
    tap.canonical(&[1]);
    assert!(tap.shared.abandoned.load(Ordering::Acquire));
}

/// The lead output's reader reads exactly what it would have read; the ride
/// is handed a copy of every byte and one end of file.
#[tokio::test]
async fn the_tee_passes_every_byte_through_and_ends_once() {
    let bytes: Vec<u8> = (0..=255).cycle().take(10_000).collect();
    let (tap, mut receiver) = new_tap(usize::MAX);
    let mut tee = RideTee::new(std::io::Cursor::new(bytes.clone()), tap);
    let mut read = Vec::new();
    let mut chunk = [0_u8; 777];
    loop {
        let count = tee.read(&mut chunk).await.expect("read");
        if count == 0 {
            break;
        }
        read.extend_from_slice(&chunk[..count]);
    }
    // A reader that asks again after the end, as the index's own does.
    assert_eq!(tee.read(&mut chunk).await.expect("read"), 0);
    assert_eq!(read, bytes, "the index reads what it always read");
    drop(tee);
    let (mut copied, mut ends) = (Vec::new(), 0);
    while let Some(event) = receiver.recv().await {
        match event {
            RideEvent::Canonical(bytes) => copied.extend(bytes),
            RideEvent::CanonicalEof => ends += 1,
            _ => panic!("the tee offers only canonical bytes"),
        }
    }
    assert_eq!(copied, bytes);
    assert_eq!(ends, 1);
}
