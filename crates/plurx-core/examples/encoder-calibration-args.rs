//! Export production encoder options for offline calibration tools.
//! This exports arguments, not a capability verdict or a qualified playback plan.
//!
//! A screening candidate is the production argv of one base mode (`vbr` or
//! `qvbr`) with `--extra` tokens appended, one token per `--extra`. Extras may
//! add options; they may not restate an option production already sets, nor
//! touch the GOP, keyframe, reordering or codec contract the screen measures.

use plurx_core::transcode::{
    hls_keyframe_args, EffectiveRateControl, Encoder, OutputGrade, Pipeline, SEGMENT_SECONDS,
    VOD_HEVC_SAMPLE_ENTRY,
};
use serde_json::json;

/// Options a candidate may never carry: they would replace the keyframe,
/// reordering or codec contract the screen holds fixed (B-frames on hardware
/// encoders are a separate per-family proof).
const RESERVED_EXTRA: &[&str] = &[
    "-g",
    "-bf",
    "-force_key_frames",
    "-forced_idr",
    "-c:v",
    "-codec:v",
    "-vcodec",
    "-b_strategy",
    "-idr_interval",
    "-adaptive_b",
];

fn validate_extra(base: &[String], extra: &[String]) -> Result<(), String> {
    for token in extra {
        if token.is_empty() || token.len() > 64 || token.chars().any(char::is_control) {
            return Err(format!("invalid --extra token: {token:?}"));
        }
        if token.starts_with('-') && token.parse::<f64>().is_err() {
            if RESERVED_EXTRA.contains(&token.as_str()) {
                return Err(format!("--extra may not set {token}"));
            }
            if base.iter().any(|arg| arg == token) {
                return Err(format!("--extra may not restate production option {token}"));
            }
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut family = None;
    let mut grade = None;
    let mut bitrate = None;
    let mut quality = None;
    let mut threads = None;
    let mut candidate_base = None;
    let mut extra = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or("each option requires a value")?;
        match arg.as_str() {
            "--family" if family.is_none() => family = Some(value),
            "--grade" if grade.is_none() => grade = Some(value),
            "--bitrate-kbps" if bitrate.is_none() => bitrate = Some(value.parse::<u32>()?),
            "--quality" if quality.is_none() => quality = Some(value.parse::<u8>()?),
            "--threads" if threads.is_none() => threads = Some(value.parse::<u32>()?),
            "--candidate-base" if candidate_base.is_none() => candidate_base = Some(value),
            "--extra" => extra.push(value),
            _ => return Err(format!("unknown or repeated option: {arg}").into()),
        }
    }
    let encoder = match family.as_deref() {
        Some("software") => Encoder::Software,
        Some("qsv") => Encoder::Qsv,
        Some("vaapi") => Encoder::Vaapi,
        Some("nvenc") => Encoder::Nvenc,
        Some("videotoolbox") => Encoder::VideoToolbox,
        _ => return Err("--family must name software, qsv, vaapi, nvenc or videotoolbox".into()),
    };
    let grade = match grade.as_deref() {
        None | Some("sdr") => OutputGrade::Sdr,
        Some("hdr10") => OutputGrade::Hdr10,
        _ => return Err("--grade must be sdr or hdr10".into()),
    };
    if encoder.video_codec_for(grade).is_none() {
        return Err("the encoder has no recipe for this grade".into());
    }
    let bitrate = bitrate
        .filter(|v| (1..=100_000).contains(v))
        .ok_or("--bitrate-kbps must be 1..100000")?;
    let threads = threads
        .filter(|v| (1..=16).contains(v))
        .ok_or("--threads must be 1..16")?;
    let quality = quality.ok_or("--quality is required")?;
    if (encoder == Encoder::VideoToolbox && !(1..=100).contains(&quality))
        || (encoder != Encoder::VideoToolbox && quality > 51)
    {
        return Err("--quality must be 0..51 (VideoToolbox: 1..100)".into());
    }
    let production =
        |rc: EffectiveRateControl| encoder.encode_args_for(grade, bitrate, rc, true, Some(threads));
    let mode = |rc: EffectiveRateControl| {
        json!({
            "recipe_value": rc.recipe_value(),
            "encoder_args": production(rc),
        })
    };
    let base = match candidate_base.as_deref() {
        None | Some("vbr") => EffectiveRateControl::Vbr,
        Some("qvbr") => EffectiveRateControl::Qvbr { quality },
        Some(_) => return Err("--candidate-base must be vbr or qvbr".into()),
    };
    let base_args = production(base);
    validate_extra(&base_args, &extra)?;
    let candidate_args: Vec<String> = base_args.iter().chain(&extra).cloned().collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema_version": 2,
            "scope": "encoder_arguments_only",
            "family": encoder.family_name(),
            "bitrate_kbps": bitrate,
            "quality": quality,
            "software_threads": threads,
            "output_grade": grade.name(),
            "vod_hevc_sample_entry": if grade == OutputGrade::Hdr10 { Some(VOD_HEVC_SAMPLE_ENTRY) } else { None },
            "hdr10_1080_filter": if grade == OutputGrade::Hdr10 {
                Pipeline::Hdr10Passthrough.filters(Some(1920), 1080, Some("hdr10"))
            } else { None },
            "input_args": encoder.init_args(),
            "upload_filter": encoder.filter_suffix_for(grade),
            "modes": {
                "vbr": mode(EffectiveRateControl::Vbr),
                "qvbr": mode(EffectiveRateControl::Qvbr { quality }),
            },
            // The rolling producer's keyframe policy and segment length, so a
            // capture forces IDRs exactly as production does (no `-g`).
            "keyframe_args": hls_keyframe_args(),
            "segment_seconds": SEGMENT_SECONDS,
            "candidate": {
                "base": if matches!(base, EffectiveRateControl::Vbr) { "vbr" } else { "qvbr" },
                "extra_args": extra,
                "encoder_args": candidate_args,
            },
        }))?
    );
    Ok(())
}
