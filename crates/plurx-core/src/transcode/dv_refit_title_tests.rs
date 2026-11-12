use super::*;
use crate::transcode::dv_refit::RefitStats;

/// The helper's own output for the Michael5400 window (milestone 2).
const RECORDED: &str = include_str!("../../tests/fixtures/dv-refit/helper-run-michael5400.jsonl");

/// Samples of a scene whose composition is the identity mapping of the base
/// layer moved by `offset` codes, with ±`noise` alternating around it, and
/// chroma passed through. `phase` varies the chroma so fit and held-out
/// samples differ.
pub(super) fn stats(offset: i32, noise: i32, phase: u32) -> RefitStats {
    let mut stats = RefitStats::new();
    for i in 0..4000u32 {
        let y = 64 + (i * 7 + phase) % 896;
        let u = 256 + (i * 37 + phase * 3) % 512;
        let v = 256 + (i * 91 + phase * 5) % 512;
        let sign = if i % 2 == 0 { 1 } else { -1 };
        let composed = (4 * y as i32 + offset + sign * noise).clamp(0, 4095) as u16;
        stats.add(
            [y as u16, u as u16, v as u16],
            [composed, (4 * u) as u16, (4 * v) as u16],
        );
    }
    stats
}

/// A run in the helper's contract: scenes of (frames, fit, held-out), then
/// the summary. `median` is the RPU's own median scene length.
pub(super) fn output(scenes: &[(u64, RefitStats, RefitStats)], median: f64) -> String {
    let mut lines = Vec::new();
    let mut frame = 0u64;
    let pts = |frame: u64| 125 + frame as i64 * 42;
    for (frames, fit, held) in scenes {
        lines.push(
            serde_json::json!({"kind":"refit_scene","first_frame":frame,"frames":frames,
                "first_pts":pts(frame),"end_pts":pts(frame + frames) - 1,"time_base":"1/1000",
                "fit":fit,"held_out":held})
            .to_string(),
        );
        frame += frames;
    }
    let fit: f64 = scenes.iter().map(|s| s.1.samples()).sum();
    let held: f64 = scenes.iter().map(|s| s.2.samples()).sum();
    lines.push(
        serde_json::json!({"kind":"refit_summary","frames":frame,"scenes":scenes.len(),
            "rpu_scenes":scenes.len() * 2,"median_scene_frames":median,
            "degenerate":median < 24.0,"fit_samples":fit as u64,"held_out_samples":held as u64,
            "first_pts":pts(0),"end_pts":pts(frame) - 1,"time_base":"1/1000"})
        .to_string(),
    );
    lines.join("\n") + "\n"
}

/// Two scenes the fit helps (offsets 12 and −10 codes), one whose held-out
/// samples are the identity mapping (so its fit does worse there).
fn mixed() -> String {
    output(
        &[
            (48, stats(12, 0, 0), stats(12, 0, 1)),
            (96, stats(-10, 0, 2), stats(-10, 0, 3)),
            (72, stats(30, 0, 4), stats(0, 0, 5)),
        ],
        48.0,
    )
}

#[test]
fn the_recorded_helper_run_parses_and_fits() {
    let out = parse(RECORDED.as_bytes()).expect("milestone 2's recorded run");
    assert_eq!(out.scenes.len(), 5);
    assert_eq!(out.summary.frames, 311);
    let title = fit(out);
    assert_eq!(title.verdict, RefitVerdict::Fitted, "{:?}", title.reason);
    assert!(
        title.gain.is_some_and(|g| g > MIN_TITLE_GAIN),
        "{:?}",
        title.gain
    );
    let (row, scenes) = title.rows(7, 100, 5, 3, &"a".repeat(64)).expect("rows");
    assert!(row.valid());
    assert!(scenes.iter().all(StagedScene::valid));
    assert_eq!(
        (scenes[0].first_pts, scenes[0].end_pts, scenes[1].first_pts),
        (125, 2252, 2253)
    );
    let again = fit(parse(RECORDED.as_bytes()).expect("again"));
    assert_eq!(again.curve_digest, title.curve_digest, "stable across runs");
}

#[test]
fn a_mixed_title_serves_the_scenes_that_pass() {
    let title = fit(parse(mixed().as_bytes()).expect("contract"));
    assert_eq!(title.verdict, RefitVerdict::Fitted, "{:?}", title.reason);
    let solved: Vec<bool> = title.scenes.iter().map(|s| s.curves.is_some()).collect();
    assert_eq!(solved, [true, true, false]);
    assert!(title.scenes[2]
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("worse than identity")));
    assert!(title.gain.is_some_and(|g| g > MIN_TITLE_GAIN && g < 1.0));
    let (row, scenes) = title.rows(7, 100, 5, 3, &"a".repeat(64)).expect("rows");
    assert!(row.valid());
    assert_eq!(row.scene_count, 3);
    assert!(scenes.iter().all(StagedScene::valid));
}

#[test]
fn a_degenerate_title_serves_identity_everywhere() {
    let text = output(&[(48, stats(12, 0, 0), stats(12, 0, 1))], 12.0);
    let title = fit(parse(text.as_bytes()).expect("contract"));
    assert_eq!(title.verdict, RefitVerdict::Identity);
    assert!(title
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("scene_refresh_flag")));
    let first = &title.scenes[0].scene;
    assert_eq!(
        title.curve_digest,
        curve_digest([(0, first.first_pts, first.time_base.as_str(), None)])
    );
}

#[test]
fn a_title_under_five_percent_keeps_identity() {
    // Offset 2 under ±20 noise: the fit removes 4 of 404 codes² per sample.
    let text = output(
        &[
            (48, stats(2, 20, 0), stats(2, 20, 1)),
            (48, stats(2, 20, 2), stats(2, 20, 3)),
        ],
        48.0,
    );
    let title = fit(parse(text.as_bytes()).expect("contract"));
    assert!(
        title.scenes.iter().all(|s| s.curves.is_some()),
        "scenes pass alone"
    );
    assert_eq!(title.verdict, RefitVerdict::Identity);
    let gain = title.gain.expect("measured");
    assert!(gain > 0.0 && gain < MIN_TITLE_GAIN, "{gain}");
    assert!(title.reason.as_deref().is_some_and(|r| r.contains("floor")));
    let identity = curve_digest(title.scenes.iter().map(|s| {
        (
            s.scene.first_frame as i64,
            s.scene.first_pts,
            s.scene.time_base.as_str(),
            None,
        )
    }));
    assert_eq!(
        title.curve_digest, identity,
        "an identity title digests identity curves"
    );
}

#[test]
fn the_digest_is_stable_and_names_the_curves() {
    let first = fit(parse(mixed().as_bytes()).expect("first"));
    let second = fit(parse(mixed().as_bytes()).expect("second"));
    assert_eq!(first, second);
    let other = output(
        &[
            (48, stats(13, 0, 0), stats(13, 0, 1)),
            (96, stats(-10, 0, 2), stats(-10, 0, 3)),
            (72, stats(30, 0, 4), stats(0, 0, 5)),
        ],
        48.0,
    );
    assert_ne!(
        fit(parse(other.as_bytes()).expect("other")).curve_digest,
        first.curve_digest
    );
    assert_ne!(first.curve_digest, empty_digest());
}

#[test]
fn malformed_or_truncated_helper_output_is_refused() {
    let good = mixed();
    let lines: Vec<&str> = good.trim_end().split('\n').collect();
    let joined = |lines: &[&str]| lines.join("\n");
    assert!(parse(joined(&lines[..3]).as_bytes())
        .expect_err("refused")
        .contains("truncated"));
    assert!(parse(&good.as_bytes()[..good.len() / 2]).is_err());
    let recorded: Vec<&str> = RECORDED.trim_end().split('\n').collect();
    assert!(
        parse(joined(&recorded[..5]).as_bytes()).is_err(),
        "no summary"
    );
    // A scene after the summary, a missing scene, counts that disagree.
    let mut late = lines.clone();
    late.push(lines[0]);
    assert!(parse(joined(&late).as_bytes()).is_err());
    assert!(parse(joined(&[lines[0], lines[2], lines[3]]).as_bytes()).is_err());
    let mut value: serde_json::Value = serde_json::from_str(lines[3]).expect("summary");
    value["fit_samples"] = (value["fit_samples"].as_u64().expect("count") + 1).into();
    let wrong = value.to_string();
    assert!(parse(joined(&[lines[0], lines[1], lines[2], &wrong]).as_bytes()).is_err());
    // Statistics of the wrong shape, an unknown kind or field, a bad time base.
    let mut scene: serde_json::Value = serde_json::from_str(lines[0]).expect("scene");
    scene["fit"]["ysum"] = serde_json::json!([1.0, 2.0]);
    let shape = scene.to_string();
    assert!(parse(joined(&[&shape, lines[1], lines[2], lines[3]]).as_bytes()).is_err());
    let kind = lines[0].replace("refit_scene", "refit_frame");
    assert!(parse(joined(&[&kind, lines[1], lines[2], lines[3]]).as_bytes()).is_err());
    let extra = lines[3].replace("\"degenerate\":false", "\"degenerate\":false,\"extra\":1");
    assert!(parse(joined(&[lines[0], lines[1], lines[2], &extra]).as_bytes()).is_err());
    assert!(parse(b"").is_err());
}

#[test]
fn stored_statistics_and_curves_read_back_exactly() {
    // Serving and re-solving read the stored JSON back: the parse must give
    // the very f64 the fit wrote (serde_json's `float_roundtrip`), or the
    // stored digest would name curves nobody can reproduce.
    let title = fit(parse(RECORDED.as_bytes()).expect("contract"));
    let (_, scenes) = title.rows(7, 100, 5, 3, &"a".repeat(64)).expect("rows");
    for (stored, solved) in scenes.iter().zip(&title.scenes) {
        let fit: RefitStats = serde_json::from_str(&stored.fit_json).expect("fit");
        let held: RefitStats = serde_json::from_str(&stored.held_out_json).expect("held out");
        assert_eq!((&fit, &held), (&solved.scene.fit, &solved.scene.held_out));
        let curves = stored
            .curves_json
            .as_deref()
            .map(|text| serde_json::from_str::<RefitCurves>(text).expect("curves"));
        assert_eq!(curves.as_ref(), solved.curves.as_ref());
    }
    let mut x = 0.1_f64;
    for _ in 0..10_000 {
        x = (x * 1.618_033_988_749_895 + 0.000_123_456_789).fract() * 3.7 - 1.2;
        let back: f64 =
            serde_json::from_str(&serde_json::to_string(&x).expect("write")).expect("read");
        assert_eq!(back.to_bits(), x.to_bits(), "{x:e}");
    }
}

#[test]
fn a_refusal_row_is_valid_and_bounded() {
    let row = refusal(
        7,
        100,
        5,
        3,
        &"b".repeat(64),
        &format!("full-resolution enhancement layer\n{}", "x".repeat(4000)),
    );
    assert!(row.valid());
    assert_eq!(row.verdict, RefitVerdict::Refused);
    assert_eq!(row.curve_digest, empty_digest());
    assert!(row.reason.as_deref().is_some_and(|r| !r.contains('\n')));
}
