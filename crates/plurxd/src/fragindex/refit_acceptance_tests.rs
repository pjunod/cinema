//! Explicit offline acceptance using the private Michael5400 FEL window.
//! No server is started. The production index reader, converter and VOD
//! publisher run over real ffmpeg pipes and retain their actual output.
use super::*;
use crate::renditiondir::InitIdentity;
use dolby_vision::rpu::dovi_rpu::DoviRpu;
use plurx_core::fmp4::{CutPolicy, Fragment};
use plurx_core::segplan::{self, SegmentPlan, TrackDurations};
use plurx_core::store::dv_refit_jobs::{RefitCurveScene, RefitCurveTable, RefitVerdict};
use plurx_core::transcode::dv_refit::{ServableCurves, LUMA_PIVOT_DELTAS};
use plurx_core::transcode::dv_refit_serve::{
    curves_match, written_curves, ServedCurveTable, SourceClock,
};
use plurx_core::transcode::dv_refit_title::{self, RefitSource};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn required(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("missing {key}"))
}

async fn output(tool: &str, args: &[String]) -> Vec<u8> {
    let result = tokio::time::timeout(
        Duration::from_secs(15 * 60),
        tokio::process::Command::new(tool).args(args).output(),
    )
    .await
    .expect("media tool timed out")
    .expect("start media tool");
    assert!(
        result.status.success(),
        "{tool}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.stdout.len() <= 512 * 1024 * 1024,
        "bounded acceptance window"
    );
    result.stdout
}

fn input_args(source: &str, seek: Option<f64>) -> Vec<String> {
    let mut args = vec!["-hide_banner".into(), "-loglevel".into(), "error".into()];
    if let Some(seek) = seek {
        args.extend([
            "-noaccurate_seek".into(),
            "-ss".into(),
            format!("{seek:.9}"),
        ]);
    }
    args.extend(["-i".into(), source.into()]);
    args
}

async fn copy_pipe(ffmpeg: &str, source: &str, stream: u32, seek: Option<f64>, retain_parameter_sets: bool) -> Vec<u8> {
    let mut args = input_args(source, seek);
    args.extend([
        "-map_chapters".into(),
        "-1".into(),
        "-map".into(),
        format!("0:{stream}"),
        "-an".into(),
        "-sn".into(),
        "-c:v".into(),
        "copy".into(),
        "-tag:v".into(),
        "hvc1".into(),
        "-bsf:v".into(),
        transcode::hevc_copy_bsf_for_copy_retaining(Some("dolby_vision"), false, true, true, retain_parameter_sets),
        "-avoid_negative_ts".into(),
        "make_zero".into(),
        "-movflags".into(),
        "frag_keyframe+empty_moov+default_base_moof+delay_moov".into(),
        "-use_editlist".into(),
        "0".into(),
        "-f".into(),
        "mp4".into(),
        "pipe:1".into(),
    ]);
    output(ffmpeg, &args).await
}

// The production landing probe is private to vodserve. Use its independent
// framecrc protocol over the exact input prefix used above, measuring both
// the selected RAP and the unseeked origin rather than assuming the target.
async fn measured_landing(
    ffmpeg: &str,
    source: &str,
    stream: u32,
    seek: f64,
    timescale: u32,
    dir: &Path,
) -> crate::vodgen::LandingTime {
    let mut times = Vec::new();
    for (label, at) in [("origin", None), ("landing", Some(seek))] {
        let mut args = vec!["-copyts".into()];
        args.extend(input_args(source, at));
        args.extend([
            "-map".into(),
            format!("0:{stream}"),
            "-c:v".into(),
            "copy".into(),
            "-frames:v".into(),
            "1".into(),
            "-f".into(),
            "framecrc".into(),
            "pipe:1".into(),
        ]);
        let raw = output(ffmpeg, &args).await;
        std::fs::write(dir.join(format!("seek-{label}.framecrc")), &raw)
            .expect("measured landing receipt");
        let text = std::str::from_utf8(&raw).expect("framecrc text");
        let base = text
            .lines()
            .find_map(|line| line.strip_prefix("#tb 0:"))
            .expect("framecrc time base")
            .trim();
        let (num, den) = base.split_once('/').expect("rational framecrc time base");
        let num: f64 = num.trim().parse().expect("time base numerator");
        let den: f64 = den.trim().parse().expect("time base denominator");
        assert!(num.is_finite() && den.is_finite() && num > 0.0 && den > 0.0);
        let packet = text
            .lines()
            .find(|line| !line.starts_with('#') && !line.trim().is_empty())
            .expect("measured first packet");
        let fields: Vec<_> = packet.split(',').map(str::trim).collect();
        assert!(fields.len() >= 6);
        let pts: f64 = fields[2].parse().expect("measured packet PTS");
        let duration: f64 = fields[3].parse().expect("measured packet duration");
        times.push((pts * num / den, duration * num / den));
    }
    let (origin, _) = times[0];
    let (landed, frame) = times[1];
    assert!(origin.is_finite() && landed.is_finite() && frame.is_finite() && frame > 0.0);
    let seconds = landed - origin;
    assert!(
        seconds >= -frame,
        "measured seek landed before the source origin"
    );
    let ticks = |seconds: f64| (seconds.max(0.0) * f64::from(timescale)).round() as u64;
    crate::vodgen::LandingTime {
        ticks: ticks(seconds),
        frame_ticks: ticks(frame).max(1),
    }
}

fn split(feed: &[u8]) -> (Init, Vec<Fragment>) {
    let mut reader = FragmentReader::new();
    reader.push(feed);
    let Some(Unit::Init(init)) = reader.next_unit().expect("actual pipe init") else {
        panic!("pipe has no init");
    };
    let mut fragments = Vec::new();
    while let Some(unit) = reader.next_unit().expect("actual fragment") {
        if let Unit::Fragment(fragment) = unit {
            fragments.push(fragment);
        }
    }
    assert!(
        reader.saw_trailer(),
        "real ffmpeg must finish its complete output"
    );
    (init, fragments)
}

fn rpus(fragment: &Fragment, init: &Init) -> Vec<(i128, Vec<u8>)> {
    let video = init.video().expect("video");
    let track = fragment.track(video.id).expect("video fragment");
    let width = usize::from(video.nal_length_size);
    assert!((1..=4).contains(&width));
    let mut decode = i128::from(track.base_decode_time);
    let mut found = Vec::new();
    for run in &track.runs {
        let mut at = run.data_offset;
        for sample in &run.samples {
            let bytes = &fragment.bytes[at..at + sample.size as usize];
            let mut cursor = 0;
            let mut rpu = None;
            while cursor < bytes.len() {
                let length = bytes[cursor..cursor + width]
                    .iter()
                    .fold(0usize, |n, b| (n << 8) | usize::from(*b));
                let nal = &bytes[cursor + width..cursor + width + length];
                assert!(nal.len() >= 2);
                if (nal[0] >> 1) & 63 == 62 {
                    assert!(rpu.replace(nal.to_vec()).is_none(), "one RPU per picture");
                }
                cursor += width + length;
            }
            assert_eq!(cursor, bytes.len());
            found.push((
                decode + i128::from(sample.cto),
                rpu.expect("RPU per real picture"),
            ));
            decode += i128::from(sample.duration);
            at += sample.size as usize;
        }
    }
    found
}

struct DiskSink(PathBuf);
impl crate::vodgen::Sink for DiskSink {
    async fn materialize(&self, entry: u32, bytes: Vec<u8>) -> std::io::Result<()> {
        tokio::fs::write(self.0.join(format!("seg{entry:05}.m4s")), bytes).await
    }
}

fn readback(nal: &[u8], pts: i64) -> Value {
    let parsed = DoviRpu::parse_unspec62_nalu(nal).expect("published RPU parses");
    let curves = written_curves(&parsed).expect("actual fitted RPU shape");
    let chroma: Vec<Value> = curves
        .chroma
        .iter()
        .map(|component| {
            let coef: Vec<&[f64]> = component[1..].chunks_exact(7).collect();
            json!({"constant": component[0], "coef": coef})
        })
        .collect();
    json!({"source_pts": pts, "profile": parsed.dovi_profile, "bytes": nal.len(),
        "luma_pivot_deltas": LUMA_PIVOT_DELTAS, "luma_coef": curves.luma,
        "chroma": chroma, "bl_bit_depth": parsed.header.bl_bit_depth_minus8 + 8,
        "vdr_bit_depth": parsed.header.vdr_bit_depth_minus8 + 8,
        "coefficient_log2_denom": parsed.header.coefficient_log2_denom,
        "disable_residual_flag": parsed.header.disable_residual_flag})
}

fn check_curve(table: &ServedCurveTable, pts: i64, nal: &[u8]) -> bool {
    let parsed = DoviRpu::parse_unspec62_nalu(nal).expect("published RPU parses");
    assert_eq!(parsed.dovi_profile, 8, "actual conversion is Profile 8");
    assert!(parsed.header.disable_residual_flag);
    if let Some(expected) = table.curves_at(pts) {
        let actual = written_curves(&parsed).expect("fitted scene has fitted mapping");
        assert!(
            curves_match(&actual, expected.curves()),
            "stored curve differs at source PTS {pts}"
        );
        true
    } else {
        false
    }
}

async fn publish(
    feed: &[u8],
    index: &FragmentIndex,
    plan: &SegmentPlan,
    policy: CutPolicy,
    table: Arc<ServedCurveTable>,
    start: (u32, Option<crate::vodgen::LandingTime>),
    dir: &Path,
) -> Init {
    tokio::fs::create_dir(dir)
        .await
        .expect("fresh publication directory");
    let (mut muxer, _) = split(feed);
    crate::copyseg::sanitize_stale_dolby_brand(&mut muxer);
    let identity =
        InitIdentity::establish(&muxer, index.promotion.clone()).expect("real rendition identity");
    let served = identity.served_init_for(&muxer).expect("served init");
    tokio::fs::write(dir.join("init.mp4"), &served.bytes)
        .await
        .expect("retain actual init");
    let landing_clock = start.1.map(|time| -> crate::vodgen::LandingClock {
        Box::new(move || Box::pin(async move { Ok(time) }))
    });
    let generation = crate::vodgen::Generation {
        plan: plan.clone(),
        index: Some(index.clone()),
        landing_clock,
        encoded_audio_anchor: None,
        encoded_frame_ticks: None,
        encoded_video_origin: None,
        identity,
        convert_dolby_vision: true,
        dv_refit: Some(table),
        retain_hevc_parameter_sets: false,
        start_entry: start.0,
        policy,
    };
    let outcome = crate::vodgen::run(
        feed,
        generation,
        &DiskSink(dir.to_owned()),
        "real-refit-acceptance",
    )
    .await;
    assert!(
        matches!(outcome, crate::vodgen::Outcome::Ran { produced_through: Some(last) }
        if last == plan.entries.last().expect("plan").index),
        "{outcome:?}"
    );
    served
}

/// Required: PLURX_FEL_REAL_SOURCE, PLURX_FEL_REAL_HELPER,
/// PLURX_FEL_REAL_VIDEO_INDEX, PLURX_FFMPEG, PLURX_FFPROBE,
/// PLURX_FEL_CANDIDATE_SHA (full compiled source SHA), and
/// PLURX_FEL_COPY_ACCEPTANCE_DIR (new durable output directory).
/// Run explicitly with --ignored --exact. This is a video-only, offline copy
/// pipeline acceptance; it does not qualify a deployed HTTP session or client.
#[tokio::test]
#[ignore = "requires private Michael5400 media and the current helper/media tools"]
async fn real_fitted_index_and_seek_publish_stored_curves() {
    let candidate = required("PLURX_FEL_CANDIDATE_SHA");
    assert!(
        candidate.len() == 40 && candidate.bytes().all(|b| b.is_ascii_hexdigit()),
        "full candidate SHA required"
    );
    let source = required("PLURX_FEL_REAL_SOURCE");
    let helper = required("PLURX_FEL_REAL_HELPER");
    let stream: u32 = required("PLURX_FEL_REAL_VIDEO_INDEX")
        .parse()
        .expect("video index");
    let ffmpeg = required("PLURX_FFMPEG");
    let ffprobe = required("PLURX_FFPROBE");
    let dir = PathBuf::from(required("PLURX_FEL_COPY_ACCEPTANCE_DIR"));
    assert!(
        dir.is_absolute() && !dir.exists(),
        "new absolute owned output directory required"
    );
    let metadata = std::fs::metadata(&source).expect("real FEL fixture");
    assert!(
        metadata.len() <= 256 * 1024 * 1024,
        "bounded private window, not a whole title"
    );
    tokio::fs::create_dir(&dir)
        .await
        .expect("owned durable receipt directory");
    let stats = output(
        &helper,
        &[
            "--refit-stats".into(),
            source.clone(),
            stream.to_string(),
            "--threads".into(),
            "2".into(),
        ],
    )
    .await;
    std::fs::write(dir.join("helper.jsonl"), &stats).expect("helper receipt");
    let fit =
        dv_refit_title::fit(dv_refit_title::parse(&stats).expect("complete current helper output"));
    assert_eq!(
        fit.verdict,
        RefitVerdict::Fitted,
        "fixture must actually fit"
    );
    let key = RefitSource {
        file_id: 1,
        source_size: i64::try_from(metadata.len()).expect("bounded size"),
        source_mtime: 1,
        video_index: i64::from(stream),
        probe_revision: 1,
        helper_identity: hex::encode(Sha256::digest(
            std::fs::read(&helper).expect("helper identity"),
        )),
    };
    let (title, rows) = fit.rows(&key).expect("actual quantized publication rows");
    std::fs::write(
        dir.join("stored-rows.json"),
        serde_json::to_vec_pretty(&json!({"title": title, "scenes": rows})).expect("rows JSON"),
    )
    .expect("rows receipt");
    let scenes = rows
        .into_iter()
        .map(|row| RefitCurveScene {
            first_frame: row.first_frame,
            frames: row.frames,
            first_pts: row.first_pts,
            end_pts: row.end_pts,
            time_base: row.time_base,
            curves: row
                .coefficients
                .as_ref()
                .map(ServableCurves::from_quantised)
                .transpose()
                .expect("stored integers load"),
        })
        .collect();
    let stored = RefitCurveTable {
        title,
        superseded_at_ms: None,
        scenes,
    };
    let table = Arc::new(
        crate::dv_refit_serving::served_table(&stored)
            .expect("serving table")
            .expect("fitted table"),
    );
    let probe_bytes = output(
        &ffprobe,
        &[
            "-v".into(),
            "error".into(),
            "-select_streams".into(),
            stream.to_string(),
            "-show_packets".into(),
            "-show_streams".into(),
            "-show_entries".into(),
            "packet=dts,pts:stream=time_base".into(),
            "-of".into(),
            "json".into(),
            source.clone(),
        ],
    )
    .await;
    std::fs::write(dir.join("source-packets.json"), &probe_bytes).expect("packet receipt");
    let probe: Value = serde_json::from_slice(&probe_bytes).expect("ffprobe JSON");
    let packet = &probe["packets"][0];
    let (num, den) = parse_time_base(
        probe["streams"][0]["time_base"]
            .as_str()
            .expect("time base"),
    )
    .expect("rational base");
    let first_pts = packet["pts"].as_i64().expect("first PTS");
    let packet_hash_output = output(&ffmpeg, &filtered_packet_hash_args(&source, stream, false)).await;
    std::fs::write(dir.join("source-first-filtered.framehash"), &packet_hash_output).expect("first source packet identity receipt");
    let sample_hash = filtered_packet_hash(&packet_hash_output, first_pts,
        (u32::try_from(num).expect("num"), u32::try_from(den).expect("den"))).expect("independent first packet identity");
    let origin = RefitPacketOrigin {
        first_dts: packet["dts"].as_i64(),
        first_pts: packet["pts"].as_i64().expect("first PTS"),
        time_base_num: u32::try_from(num).expect("num"),
        time_base_den: u32::try_from(den).expect("den"),
        first_sample_sha256: Some(sample_hash),
    };
    assert_eq!(
        (origin.time_base_num, origin.time_base_den),
        table.time_base()
    );
    let feed = copy_pipe(&ffmpeg, &source, stream, None, false).await;
    let (init, fragments) = split(&feed);
    let first_rpu = rpus(fragments.first().expect("real fragments"), &init)[0]
        .1
        .clone();
    assert_eq!(
        DoviRpu::parse_unspec62_nalu(&first_rpu)
            .expect("source RPU")
            .dovi_profile,
        7
    );
    std::fs::write(dir.join("source-first-p7.nal"), first_rpu).expect("current source P7 RPU");
    let record = fmp4::DolbyVisionRecord::new(8, 6, false, true, true, 1).expect("8.1 record");
    let index = match index_stream_with_progress(
        &feed[..],
        SourceIdentity::new(
            metadata.len(),
            1,
            format!("dv-p7-to-p81-rpu-v2:{}", table.digest()),
        ),
        None,
        DolbyVisionPass::Rewrite(Box::new(record)),
        None,
        Some(Arc::clone(&table)),
        Some(origin),
    )
    .await
    {
        IndexOutcome::Built(index) => *index,
        other => panic!("real fitted index refused: {other:?}"),
    };
    std::fs::write(
        dir.join("index.json"),
        serde_json::to_vec_pretty(&index).expect("index JSON"),
    )
    .expect("index receipt");
    let clock = SourceClock::for_index_landing(
        &index,
        0,
        fragments[0]
            .track(init.video().expect("video").id)
            .expect("track")
            .base_decode_time,
    )
    .expect("index clock");
    let mut fitted = crate::dvpipe::Converter::for_init(&init)
        .expect("converter")
        .with_refit(Arc::clone(&table), clock)
        .expect("clock");
    let empty = Arc::new(
        ServedCurveTable::from_servable(
            "0".repeat(64),
            stored.scenes.iter().map(|scene| {
                (
                    scene.first_pts,
                    scene.end_pts,
                    scene.time_base.as_str(),
                    None,
                )
            }),
        )
        .expect("unfitted table"),
    );
    let mut v1 = crate::dvpipe::Converter::for_init(&init).expect("v1 converter");
    let mut unfitted = crate::dvpipe::Converter::for_init(&init)
        .expect("unfitted converter")
        .with_refit(empty, clock)
        .expect("clock");
    let mut fitted_count = 0;
    assert_eq!(fragments.len(), index.rows.len());
    for (fragment, row) in fragments.iter().zip(&index.rows) {
        let (mut converted, mut baseline, mut unchanged) =
            (fragment.clone(), fragment.clone(), fragment.clone());
        fitted.convert(&mut converted).expect("fitted index output");
        v1.convert(&mut baseline).expect("v1 output");
        unfitted.convert(&mut unchanged).expect("unfitted output");
        assert_eq!(
            unchanged.bytes, baseline.bytes,
            "unfitted output must remain byte-for-byte v1"
        );
        assert_eq!(
            IndexRow::from_fragment(&converted, &init)
                .expect("measured row")
                .video_bytes,
            row.video_bytes
        );
        let baseline_rpus = rpus(&baseline, &init);
        for ((pts, nal), (_, identity)) in rpus(&converted, &init).iter().zip(baseline_rpus) {
            let pts = clock.source_pts(*pts).expect("exact source PTS");
            if check_curve(&table, pts, nal) {
                fitted_count += 1;
            } else {
                assert_eq!(*nal, identity, "identity scene is exactly v1");
            }
        }
    }
    assert!(fitted_count > 0);
    let policy = CutPolicy::new(2, 1, 48_000_000, 15, index.timescale);
    let ticks: u64 = index.rows.iter().map(|row| row.duration).sum();
    let duration_ms =
        i64::try_from(u128::from(ticks) * 1000 / u128::from(index.timescale)).expect("duration");
    let plan = segplan::plan_copy(
        &index,
        &policy,
        &TrackDurations {
            video_ms: duration_ms,
            audio_ms: 0,
            audio_bits_per_second: 0,
        },
    );
    assert!(plan.len() >= 2, "fixture must offer a nonzero seek entry");
    let served = publish(
        &feed,
        &index,
        &plan,
        policy,
        Arc::clone(&table),
        (0, None),
        &dir.join("whole"),
    )
    .await;
    let published_clock = SourceClock {
        track_ticks: 0,
        ..clock
    };
    let mut full = BTreeMap::new();
    let mut readbacks = Vec::new();
    for entry in &plan.entries {
        let bytes = std::fs::read(dir.join("whole").join(format!("seg{:05}.m4s", entry.index)))
            .expect("actual materialization");
        let mut reader = FragmentReader::new();
        reader.push(&served.bytes);
        reader.push(&bytes);
        while let Some(unit) = reader.next_unit().expect("actual served boxes") {
            if let Unit::Fragment(fragment) = unit {
                for (pts, nal) in rpus(&fragment, &served) {
                    let pts = published_clock.source_pts(pts).expect("served source PTS");
                    if check_curve(&table, pts, &nal) {
                        readbacks.push(readback(&nal, pts));
                    }
                    assert!(
                        full.insert(pts, nal).is_none(),
                        "every published PTS is unique"
                    );
                }
            }
        }
    }
    assert_eq!(
        full.len(),
        usize::try_from(fit.frames).expect("fit frames"),
        "publisher preserves every source picture"
    );
    assert!(!readbacks.is_empty());
    std::fs::write(
        dir.join("patch-readback.json"),
        serde_json::to_vec_pretty(&json!({"frames": [&readbacks[0]]})).expect("readback JSON"),
    )
    .expect("actual patch readback");
    std::fs::write(
        dir.join("fitted-readback.json"),
        serde_json::to_vec_pretty(&json!({"frames": readbacks})).expect("readback JSON"),
    )
    .expect("all fitted readback");
    let target = &plan.entries[plan.entries.len() / 2];
    assert!(target.index > 0);
    let seek_seconds = target.start_ticks as f64 / f64::from(plan.timescale);
    let measured = measured_landing(
        &ffmpeg,
        &source,
        stream,
        seek_seconds,
        index.timescale,
        &dir,
    )
    .await;
    let seek_feed = copy_pipe(&ffmpeg, &source, stream, Some(seek_seconds), false).await;
    let seek_init = publish(
        &seek_feed,
        &index,
        &plan,
        policy,
        Arc::clone(&table),
        (target.index, Some(measured)),
        &dir.join("seek"),
    )
    .await;
    assert_eq!(
        served.bytes, seek_init.bytes,
        "seek keeps the attached init identity"
    );
    let mut seek_frames = 0;
    for entry in plan
        .entries
        .iter()
        .filter(|entry| entry.index >= target.index)
    {
        let bytes = std::fs::read(dir.join("seek").join(format!("seg{:05}.m4s", entry.index)))
            .expect("seek materialization");
        let mut reader = FragmentReader::new();
        reader.push(&seek_init.bytes);
        reader.push(&bytes);
        while let Some(unit) = reader.next_unit().expect("seek served boxes") {
            if let Unit::Fragment(fragment) = unit {
                for (pts, nal) in rpus(&fragment, &seek_init) {
                    let pts = published_clock.source_pts(pts).expect("seek source PTS");
                    check_curve(&table, pts, &nal);
                    assert_eq!(
                        full.get(&pts),
                        Some(&nal),
                        "seek's first and every later RPU equal the whole publication at {pts}"
                    );
                    seek_frames += 1;
                }
            }
        }
    }
    assert!(seek_frames > 0);
    let receipt = json!({"scope": "offline video-only production index/converter/VOD publisher; no deployed HTTP session",
        "source_sha256": hex::encode(Sha256::digest(std::fs::read(&source).expect("source hash"))),
        "candidate_sha": candidate, "helper_path": helper, "helper_sha256": key.helper_identity,
        "ffmpeg_path": ffmpeg, "ffprobe_path": ffprobe, "curve_digest": table.digest(), "source_origin": index.dv_refit_origin,
        "seek_seconds": seek_seconds, "seek_entry": target.index,
        "measured_landing_ticks": measured.ticks, "measured_frame_ticks": measured.frame_ticks, "frames": full.len(),
        "fitted_frames": fitted_count, "seek_frames": seek_frames,
        "served_init_sha256": hex::encode(Sha256::digest(&served.bytes)),
        "patch_readback_origin": "first fitted RPU parsed from whole/seg*.m4s actual publisher output"});
    std::fs::write(
        dir.join("source-receipt.json"),
        serde_json::to_vec_pretty(&receipt).expect("receipt JSON"),
    )
    .expect("acceptance receipt");
}


fn clock_fixture() -> (Init, Fragment, [u8; 32]) {
    let feed = include_bytes!("../../../plurx-core/tests/fixtures/dv-runtime/authored.mp4");
    let (mut init, mut fragments) = split(feed);
    let video = init.tracks.iter_mut().find(|track| track.kind == TrackKind::Video).expect("video");
    video.timescale = 1000;
    let id = video.id;
    let mut fragment = fragments.remove(0);
    let track = fragment.tracks.iter_mut().find(|track| track.track_id == id).expect("video track");
    track.base_decode_time = 1000;
    track.runs[0].samples[0].cto = 83;
    let run = &track.runs[0];
    let sample = &run.samples[0];
    let hash = Sha256::digest(&fragment.bytes[run.data_offset..run.data_offset + sample.size as usize]).into();
    (init, fragment, hash)
}

fn packet_origin(first_dts: Option<i64>, hash: Option<[u8; 32]>) -> RefitPacketOrigin {
    RefitPacketOrigin { first_dts, first_pts: 125, time_base_num: 1, time_base_den: 1000,
        first_sample_sha256: hash }
}

#[test]
fn known_dts_is_checked_against_the_first_sample_composition_offset() {
    let (init, fragment, _) = clock_fixture();
    assert_eq!(packet_origin(Some(42), None).resolve(&init, &fragment).expect("matching DTS").first_dts, 42);
    assert!(packet_origin(Some(43), None).resolve(&init, &fragment).is_err());
}

#[test]
fn missing_dts_is_resolved_only_from_the_same_independently_hashed_packet() {
    let (init, fragment, hash) = clock_fixture();
    assert_eq!(packet_origin(None, Some(hash)).resolve(&init, &fragment).expect("same first packet").first_dts, 42);
    assert!(packet_origin(None, None).resolve(&init, &fragment).is_err());
    let mut wrong = hash; wrong[0] ^= 1;
    assert!(packet_origin(None, Some(wrong)).resolve(&init, &fragment).is_err());
}

#[test]
fn missing_dts_refuses_a_nonintegral_composition_offset() {
    let (mut init, mut fragment, hash) = clock_fixture();
    init.tracks.iter_mut().find(|track| track.kind == TrackKind::Video).expect("video").timescale = 24000;
    fragment.tracks[0].runs[0].samples[0].cto = 1;
    assert!(packet_origin(None, Some(hash)).resolve(&init, &fragment).is_err());
}

#[test]
fn filtered_packet_hash_binds_its_first_pts_and_time_base() {
    let hash = "ab".repeat(32);
    let raw = format!("#tb 0: 1/1000\n0, NOPTS, 125, 41, 123, {hash}\n");
    assert_eq!(hex::encode(filtered_packet_hash(raw.as_bytes(), 125, (1, 1000)).expect("matching packet")), hash);
    assert!(filtered_packet_hash(raw.as_bytes(), 126, (1, 1000)).is_err());
    assert!(filtered_packet_hash(raw.as_bytes(), 125, (1, 24000)).is_err());
}


/// Both header policies must identify their own first packet. A hash from
/// the stripping output must never qualify the retained-header sample.
#[tokio::test]
async fn missing_dts_probe_matches_each_outputs_parameter_set_retention() {
    use plurx_core::testfixtures;
    testfixtures::require_ffmpeg();
    let ffmpeg = testfixtures::ffmpeg();
    let source = testfixtures::source("closed-gop");
    let source = source.to_str().expect("fixture path");
    let raw = output(&testfixtures::ffprobe(), &[
        "-v".into(), "error".into(), "-select_streams".into(), "v:0".into(),
        "-show_packets".into(), "-show_streams".into(), "-show_entries".into(),
        "packet=pts:stream=time_base".into(), "-of".into(), "json".into(), source.into()]).await;
    let probe: Value = serde_json::from_slice(&raw).expect("source packet clock");
    let pts = probe["packets"][0]["pts"].as_i64().expect("first packet PTS");
    let (num, den) = parse_time_base(probe["streams"][0]["time_base"].as_str().expect("source base")).expect("rational source base");
    let base = (u32::try_from(num).expect("num"), u32::try_from(den).expect("den"));
    let mut cases = Vec::new();
    for retain in [false, true] {
        let video = transcode::CopyVideoOptions::new(false, true)
            .with_dolby_vision_conversion(true).with_parameter_set_retention(retain);
        let hash_output = output(&ffmpeg, &filtered_packet_hash_args(source, 0,
            video.retains_hevc_parameter_sets())).await;
        let hash = filtered_packet_hash(&hash_output, pts, base).expect("actual filtered first packet");
        let feed = copy_pipe(&ffmpeg, source, 0, None, video.retains_hevc_parameter_sets()).await;
        let (init, mut fragments) = split(&feed);
        let origin = RefitPacketOrigin { first_dts: None, first_pts: pts, time_base_num: base.0,
            time_base_den: base.1, first_sample_sha256: Some(hash) };
        origin.resolve(&init, &fragments[0]).expect("same selected header policy matches");
        cases.push((hash, init, fragments.remove(0), origin));
    }
    assert_ne!(cases[0].0, cases[1].0, "fixture must exercise actual in-band parameter sets");
    let wrong = RefitPacketOrigin { first_sample_sha256: Some(cases[0].0), ..cases[1].3 };
    assert!(wrong.resolve(&cases[1].1, &cases[1].2).is_err(), "stripped packet cannot qualify retained headers");
}
