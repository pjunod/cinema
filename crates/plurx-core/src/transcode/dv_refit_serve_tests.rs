use super::*;
use dolby_vision::rpu::ConversionMode;

/// Identity curves lifted by `codes` 12-bit codes: servable, and distinct
/// per lift, so a test can tell which scene's curves an RPU carries.
pub(crate) fn lifted(codes: f64) -> RefitCurves {
    let mut curves = RefitCurves::identity();
    for piece in &mut curves.luma {
        piece[0] += codes / 4095.0;
    }
    curves
}

fn table(scenes: &[(i64, i64, Option<&RefitCurves>)]) -> ServedCurveTable {
    ServedCurveTable::new(
        "d".repeat(64),
        scenes.iter().map(|&(a, b, c)| (a, b, "1/1000", c)),
    )
    .expect("a valid table")
}

#[test]
fn a_scene_covers_its_start_up_to_the_next_start_and_the_last_runs_to_its_end() {
    let (a, b) = (lifted(2.0), lifted(5.0));
    let sa = servable(&a).expect("a");
    let sb = servable(&b).expect("b");
    // The first scene's end_pts (1040) is short of the next start: the next
    // start, not the rounded end, is the boundary.
    let t = table(&[
        (1000, 1040, Some(&a)),
        (1042, 2000, None),
        (2000, 3000, Some(&b)),
    ]);
    assert!(t.fitted());
    assert_eq!(t.curves_at(999), None, "before the first scene: identity");
    assert_eq!(t.curves_at(1000), Some(&sa));
    assert_eq!(t.curves_at(1041), Some(&sa), "up to the next scene's start");
    assert_eq!(t.curves_at(1042), None, "an identity scene");
    assert_eq!(t.curves_at(1999), None);
    assert_eq!(
        t.curves_at(2000),
        Some(&sb),
        "the boundary belongs to the later scene"
    );
    assert_eq!(t.curves_at(2999), Some(&sb));
    assert_eq!(t.curves_at(3000), None, "at the last end_pts: identity");
    assert_eq!(t.curves_at(i64::MAX), None);
    assert_eq!(t.time_base(), (1, 1000));
}

#[test]
fn a_table_that_cannot_be_served_exactly_is_refused() {
    let none: [(i64, i64, &str, Option<&RefitCurves>); 0] = [];
    assert_eq!(ServedCurveTable::new("d", none), Err(ServeError::Empty));
    assert_eq!(
        ServedCurveTable::new("d", [(0, 10, "1/1000", None), (10, 20, "1/24000", None)]),
        Err(ServeError::MixedTimeBase)
    );
    assert_eq!(
        ServedCurveTable::new("d", [(0, 10, "1/1000", None), (0, 20, "1/1000", None)]),
        Err(ServeError::Order(1))
    );
    assert_eq!(
        ServedCurveTable::new("d", [(10, 10, "1/1000", None)]),
        Err(ServeError::Order(0))
    );
    assert!(matches!(
        ServedCurveTable::new("d", [(0, 10, "0/1000", None)]),
        Err(ServeError::TimeBase(_))
    ));
    let mut wild = RefitCurves::identity();
    wild.luma[0][0] = 0.5;
    assert!(matches!(
        ServedCurveTable::new("d", [(0, 10, "1/1000", Some(&wild))]),
        Err(ServeError::Unservable(0, _))
    ));
}

#[test]
fn the_clock_maps_track_ticks_to_source_ticks_exactly_or_refuses() {
    // A 1/1000 Matroska stream copied into a 16000-tick track whose zero is
    // source pts 42 (the origin the copy subtracted).
    let clock = SourceClock {
        track_timescale: 16_000,
        track_ticks: 0,
        source_pts: 42,
        source_time_base: (1, 1000),
    };
    assert_eq!(clock.source_pts(0), Ok(42));
    assert_eq!(clock.source_pts(16 * 41), Ok(83));
    assert_eq!(
        clock.source_pts(-16),
        Ok(41),
        "a leading picture before the anchor"
    );
    assert_eq!(clock.source_pts(8), Err(ServeError::NonIntegral(8)));
    // 1001/24000 source ticks in a 24000 track: one frame is 1001 ticks.
    let ntsc = SourceClock {
        track_timescale: 24_000,
        track_ticks: 2002,
        source_pts: 0,
        source_time_base: (1001, 24_000),
    };
    assert_eq!(ntsc.source_pts(2002 + 1001 * 7), Ok(7));
    assert_eq!(ntsc.source_pts(2003), Err(ServeError::NonIntegral(2003)));
    let broken = SourceClock {
        track_timescale: 0,
        ..clock
    };
    assert_eq!(broken.source_pts(0), Err(ServeError::Clock));
}

#[test]
fn written_curves_read_back_what_apply_to_rpu_wrote() {
    let served = servable(&lifted(3.0)).expect("servable");
    let mut rpu = DoviRpu::parse_unspec62_nalu(&crate::testfixtures::p7_rpu()).expect("parses");
    rpu.convert_with_mode(ConversionMode::To81)
        .expect("converts");
    assert!(
        written_curves(&rpu).is_none_or(|curves| !curves_match(&curves, served.curves())),
        "the plain conversion does not already carry these curves"
    );
    super::super::dv_refit::apply_to_rpu(&mut rpu, &served).expect("writable");
    let bytes = rpu.write_hevc_unspec62_nalu().expect("writes");
    let back = DoviRpu::parse_unspec62_nalu(&bytes).expect("reads back");
    let written = written_curves(&back).expect("the written shape");
    assert!(curves_match(&written, served.curves()));
    assert!(!curves_match(
        &written,
        servable(&lifted(4.0)).expect("other").curves()
    ));
}

#[test]
fn the_first_keyframe_is_read_from_a_packet_probe_in_demux_order() {
    let document = serde_json::json!({
        "packets": [
            {"stream_index": 0, "pts": "40", "dts": "-1", "flags": "__"},
            {"stream_index": 0, "pts": 83, "dts": 42, "flags": "K__"},
            {"stream_index": 0, "pts": 125, "dts": 0, "flags": "K__"}
        ],
        "streams": [{"time_base": "1/1000"}]
    });
    let keyframe = SourceKeyframe::from_ffprobe(&document).expect("a keyframe");
    assert_eq!(
        keyframe,
        SourceKeyframe {
            dts: 42,
            pts: 83,
            time_base: (1, 1000)
        }
    );
    let clock = keyframe.clock(16_000, 0);
    assert_eq!(
        clock.source_pts(16 * 41),
        Ok(83),
        "the keyframe presents at its pts"
    );
    let no_base = serde_json::json!({"packets": document["packets"].clone()});
    assert!(SourceKeyframe::from_ffprobe(&no_base).is_err());
    let no_key = serde_json::json!({
        "packets": [{"pts": 1, "dts": 1, "flags": "__"}],
        "streams": [{"time_base": "1/1000"}]
    });
    assert!(SourceKeyframe::from_ffprobe(&no_key).is_err());
}
