//! Export production encoder options for offline calibration tools.
//! This exports arguments, not a capability verdict or a qualified playback plan.

use plurx_core::transcode::{EffectiveRateControl, Encoder, OutputGrade, Pipeline};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut family = None;
    let mut grade = None;
    let mut bitrate = None;
    let mut quality = None;
    let mut threads = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or("each option requires a value")?;
        match arg.as_str() {
            "--family" if family.is_none() => family = Some(value),
            "--grade" if grade.is_none() => grade = Some(value),
            "--bitrate-kbps" if bitrate.is_none() => bitrate = Some(value.parse::<u32>()?),
            "--quality" if quality.is_none() => quality = Some(value.parse::<u8>()?),
            "--threads" if threads.is_none() => threads = Some(value.parse::<u32>()?),
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
    let mode = |rc: EffectiveRateControl| {
        json!({
            "recipe_value": rc.recipe_value(),
            "encoder_args": encoder.encode_args_for(grade, bitrate, rc, true, Some(threads)),
        })
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "scope": "encoder_arguments_only",
            "family": encoder.family_name(),
            "bitrate_kbps": bitrate,
            "quality": quality,
            "software_threads": threads,
            "output_grade": grade.name(),
            "hdr10_1080_filter": if grade == OutputGrade::Hdr10 {
                Pipeline::Hdr10Passthrough.filters(Some(1920), 1080, Some("hdr10"))
            } else { None },
            "input_args": encoder.init_args(),
            "upload_filter": encoder.filter_suffix_for(grade),
            "modes": {
                "vbr": mode(EffectiveRateControl::Vbr),
                "qvbr": mode(EffectiveRateControl::Qvbr { quality }),
            },
        }))?
    );
    Ok(())
}
