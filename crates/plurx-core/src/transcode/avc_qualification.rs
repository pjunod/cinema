//! Node-local SDR encoder qualification, not a fleet or manifest-emission proof.
//!
//! A candidate is admitted only after a completed bounded encode agrees in its
//! SPS, decoder configuration, raster and every fragmented-MP4 sample duration.
//! The table names experiments, never identities: the compatibility byte comes
//! from the resulting bitstream. Missing cells retain the incumbent arguments.

use super::{EffectiveRateControl, Encoder, Rational};
use crate::fmp4::{FragmentReader, Unit};
use std::collections::BTreeMap;

const FRAMES: usize = 8;
const MAX_MEDIA: usize = 8 * 1024 * 1024;
const MAX_TRACE: usize = 1024 * 1024;

/// An immutable result of this node's actual encoder experiment. Fields are
/// private so an argv guess cannot masquerade as measured capability evidence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QualifiedSdrAvc {
    family: String,
    width: u32,
    height: u32,
    cadence: Rational,
    bitrate_kbps: u32,
    rate_control: String,
    forced_idr: bool,
    level: u8,
    codec: String,
}

impl QualifiedSdrAvc {
    pub fn codec(&self) -> &str {
        &self.codec
    }

    pub fn cadence(&self) -> Rational {
        self.cadence
    }

    pub(super) fn matches(
        &self,
        encoder: Encoder,
        raster: (u32, u32),
        cadence: Rational,
        bitrate_kbps: u32,
        rate_control: EffectiveRateControl,
        forced_idr: bool,
    ) -> bool {
        self.family == encoder.label()
            && (self.width, self.height) == raster
            && self.cadence == cadence
            && self.bitrate_kbps == bitrate_kbps
            && self.rate_control == rate_control.recipe_value()
            && self.forced_idr == forced_idr
    }

    pub(super) fn flags(&self) -> [String; 4] {
        [
            "-profile:v".into(),
            "high".into(),
            "-level:v".into(),
            format!("{}.{}", self.level / 10, self.level % 10),
        ]
    }
}

#[derive(Clone, Copy)]
struct Candidate {
    encoder: Encoder,
    width: u32,
    height: u32,
    cadence: Rational,
    bitrate_kbps: u32,
    rate_control: EffectiveRateControl,
    forced_idr: bool,
    level: u8,
}

impl Candidate {
    fn flags(self) -> [String; 4] {
        [
            "-profile:v".into(),
            "high".into(),
            "-level:v".into(),
            format!("{}.{}", self.level / 10, self.level % 10),
        ]
    }
}

/// Parse only complete SPS sections emitted by the actual trace_headers
/// bitstream filter. Duplicate fields, truncated sections, changing SPS fields
/// and absent mandatory fields refuse admission rather than choose a value.
fn sps_fields(trace: &[u8]) -> Option<BTreeMap<String, u32>> {
    let trace = std::str::from_utf8(trace).ok()?;
    if !trace.ends_with('\n') {
        return None;
    }
    let mut section = None::<BTreeMap<String, u32>>;
    let mut measured = None;
    for line in trace.lines() {
        if line.len() > 8192 {
            return None;
        }
        let Some((_, line)) = line.split_once("[trace_headers @ ") else {
            continue;
        };
        let (_, line) = line.split_once("] ")?;
        let line = line.trim();
        if line == "Sequence Parameter Set" {
            if section.is_some() {
                return None;
            }
            section = Some(BTreeMap::new());
            continue;
        }
        let Some(fields) = section.as_mut() else {
            continue;
        };
        let mut parts = line.split_whitespace();
        let position = parts.next()?;
        if !position.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let name = parts.next()?;
        let (_, value) = line.rsplit_once(" = ")?;
        let value = value.parse::<u32>().ok()?;
        if fields.len() >= 512 || fields.insert(name.to_owned(), value).is_some() {
            return None;
        }
        if name == "rbsp_stop_one_bit" {
            if value != 1 {
                return None;
            }
            let complete = section.take()?;
            if measured.as_ref().is_some_and(|old| old != &complete) {
                return None;
            }
            measured = Some(complete);
        }
    }
    if section.is_some() {
        return None;
    }
    measured
}

fn measured(candidate: Candidate, media: &[u8], trace: &[u8]) -> Option<QualifiedSdrAvc> {
    if media.len() > MAX_MEDIA || trace.len() > MAX_TRACE {
        return None;
    }
    let fields = sps_fields(trace)?;
    let field = |name: &str| fields.get(name).copied();
    if field("profile_idc")? != 100
        || field("level_idc")? != u32::from(candidate.level)
        || field("chroma_format_idc")? != 1
        || field("bit_depth_luma_minus8")? != 0
        || field("bit_depth_chroma_minus8")? != 0
        || field("frame_mbs_only_flag")? != 1
        || field("reserved_zero_2bits")? != 0
        || field("vui_parameters_present_flag")? != 1
        || field("timing_info_present_flag")? != 1
        || field("fixed_frame_rate_flag")? > 1
        || field("frame_cropping_flag")? > 1
    {
        return None;
    }
    let mut compatibility = 0_u8;
    for index in 0..6 {
        let bit = field(&format!("constraint_set{index}_flag"))?;
        if bit > 1 {
            return None;
        }
        compatibility |= (bit as u8) << (7 - index);
    }
    let (crop_width, crop_height) = if field("frame_cropping_flag")? == 0 {
        (0, 0)
    } else {
        (
            field("frame_crop_left_offset")?
                .checked_add(field("frame_crop_right_offset")?)?
                .checked_mul(2)?,
            field("frame_crop_top_offset")?
                .checked_add(field("frame_crop_bottom_offset")?)?
                .checked_mul(2)?,
        )
    };
    let width = field("pic_width_in_mbs_minus1")?
        .checked_add(1)?
        .checked_mul(16)?
        .checked_sub(crop_width)?;
    let height = field("pic_height_in_map_units_minus1")?
        .checked_add(1)?
        .checked_mul(16)?
        .checked_sub(crop_height)?;
    let cadence = Rational::new(
        field("time_scale")?,
        field("num_units_in_tick")?.checked_mul(2)?,
    )?;
    if (width, height, cadence) != (candidate.width, candidate.height, candidate.cadence) {
        return None;
    }
    let codec = format!("avc1.64{compatibility:02X}{:02X}", candidate.level);
    let mut reader = FragmentReader::new();
    reader.push(media);
    let mut init = None;
    let mut samples = 0_usize;
    let mut next_decode_time = 0_u64;
    while let Some(unit) = reader.next_unit().ok()? {
        match unit {
            Unit::Init(value) if init.is_none() => {
                if crate::fmp4::avc_rfc6381_codec(&value).ok()?.as_deref() != Some(&codec) {
                    return None;
                }
                init = Some(value);
            }
            Unit::Fragment(fragment) => {
                let video = init.as_ref()?.video()?;
                let track = fragment.track(video.id)?;
                if video.timescale == 0 || track.base_decode_time != next_decode_time {
                    return None;
                }
                for sample in track.samples() {
                    if sample.size == 0
                        || sample.duration == 0
                        || sample.cto % i64::from(sample.duration) != 0
                        || u64::from(sample.duration) * u64::from(cadence.numerator())
                            != u64::from(video.timescale) * u64::from(cadence.denominator())
                    {
                        return None;
                    }
                    samples = samples.checked_add(1)?;
                    next_decode_time = next_decode_time.checked_add(u64::from(sample.duration))?;
                }
            }
            Unit::Trailer => {}
            Unit::Init(_) => return None,
        }
    }
    if reader.buffered() != 0 || samples != FRAMES {
        return None;
    }
    Some(QualifiedSdrAvc {
        family: candidate.encoder.label().to_owned(),
        width,
        height,
        cadence,
        bitrate_kbps: candidate.bitrate_kbps,
        rate_control: candidate.rate_control.recipe_value(),
        forced_idr: candidate.forced_idr,
        level: candidate.level,
        codec,
    })
}

async fn probe_output(ffmpeg: &str, candidate: Candidate) -> Option<(Vec<u8>, Vec<u8>)> {
    use tokio::io::AsyncReadExt;
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "info", "-nostdin"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    args.extend(candidate.encoder.init_args());
    args.extend([
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!(
            "testsrc2=size={}x{}:rate={}/{}",
            candidate.width,
            candidate.height,
            candidate.cadence.numerator(),
            candidate.cadence.denominator()
        ),
        "-vf".into(),
        candidate.encoder.filter_suffix().map_or_else(
            || "format=yuv420p".to_owned(),
            |upload| format!("format=yuv420p,{upload}"),
        ),
        "-frames:v".into(),
        FRAMES.to_string(),
        "-an".into(),
    ]);
    args.extend(candidate.encoder.encode_args_for(
        super::OutputGrade::Sdr,
        candidate.bitrate_kbps,
        candidate.rate_control,
        candidate.forced_idr,
        Some(2),
    ));
    args.extend(candidate.flags());
    args.extend(
        [
            "-bsf:v",
            "trace_headers",
            "-movflags",
            "+frag_keyframe+empty_moov+default_base_moof",
            "-f",
            "mp4",
            "pipe:1",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    let mut command = tokio::process::Command::new(ffmpeg);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let (mut child, _job) = crate::process::spawn_job_owned(
        &mut command,
        crate::process::ChildWork::background("SDR AVC qualification"),
    )
    .ok()?;
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;
    let completed = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let media = async {
            let mut bytes = Vec::new();
            stdout
                .take((MAX_MEDIA + 1) as u64)
                .read_to_end(&mut bytes)
                .await?;
            Ok::<_, std::io::Error>(bytes)
        };
        let trace = async {
            let mut bytes = Vec::new();
            stderr
                .take((MAX_TRACE + 1) as u64)
                .read_to_end(&mut bytes)
                .await?;
            Ok::<_, std::io::Error>(bytes)
        };
        tokio::join!(child.wait(), media, trace)
    })
    .await;
    match completed {
        Ok((Ok(status), Ok(media), Ok(trace))) if status.success() => Some((media, trace)),
        _ => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1), child.wait()).await;
            None
        }
    }
}

/// A bounded partial matrix: a missing cell is explicitly unqualified. The
/// deadline covers readers and child completion, not merely process startup.
pub(super) async fn qualify(ffmpeg: &str, caps: &super::EncoderCaps) -> Vec<QualifiedSdrAvc> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut qualified = Vec::new();
    // 852 is the incumbent even-rounded 16:9 480p raster; 854 is the plan's
    // proposed raster. Neither observation qualifies the other.
    for (width, height, bitrate_kbps, level) in [
        (640, 360, 1200, 31),
        (852, 480, 2000, 31),
        (854, 480, 2000, 31),
        (1280, 720, 4000, 32),
        (1920, 1080, 8000, 42),
        (3840, 2160, 20000, 52),
    ] {
        for (numerator, denominator) in [(24000, 1001), (30000, 1001), (60000, 1001), (60, 1)] {
            let cadence = Rational::new(numerator, denominator).expect("nonzero fixed probe grid");
            for encoder in [
                Encoder::Software,
                Encoder::Nvenc,
                Encoder::Qsv,
                Encoder::Vaapi,
                Encoder::VideoToolbox,
            ] {
                if !caps.available(encoder) {
                    continue;
                }
                // Probe VBR independently. Quality mode is a different cell,
                // including its actual family-specific quality value.
                for rate_control in [
                    EffectiveRateControl::Vbr,
                    EffectiveRateControl::Qvbr {
                        quality: encoder.default_quality(),
                    },
                ] {
                    if tokio::time::Instant::now() + std::time::Duration::from_secs(4) > deadline {
                        return qualified;
                    }
                    let candidate = Candidate {
                        encoder,
                        width,
                        height,
                        cadence,
                        bitrate_kbps,
                        rate_control,
                        forced_idr: caps.forced_idr.wanted_by(encoder),
                        level,
                    };
                    if let Some((media, trace)) = probe_output(ffmpeg, candidate).await {
                        if let Some(proof) = measured(candidate, &media, &trace) {
                            qualified.push(proof);
                        }
                    }
                }
            }
        }
    }
    qualified
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn wedged_sdr_qualification_is_bounded_and_never_issues_a_proof() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/transcode/fixtures/ffmpeg-wedged-probe"
        );
        let candidate = Candidate {
            encoder: Encoder::Software,
            width: 640,
            height: 360,
            cadence: Rational::new(60, 1).expect("fixed grid"),
            bitrate_kbps: 1200,
            rate_control: EffectiveRateControl::Vbr,
            forced_idr: false,
            level: 31,
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            probe_output(fixture, candidate),
        )
        .await
        .expect("inner deadline and owned-child cleanup complete");
        assert!(
            result.is_none(),
            "timeout is unqualified, never successful output"
        );
    }

    #[test]
    fn only_exact_qualified_plans_change_encoder_flags_and_recipe_identity() {
        use crate::transcode::*;
        let mut file = crate::transcode::tests::file(None);
        file.width = Some(640);
        file.height = Some(360);
        file.video_codec = Some("h264".to_owned());
        file.bit_depth = Some(8);
        let options = TranscodeOptions {
            target_height: 360,
            video_bitrate_kbps: 1200,
            ..TranscodeOptions::default()
        };
        let facts = DecodeFacts::from_ffprobe_json(
            &serde_json::json!({"streams":[{
                "index":0,"codec_type":"video","codec_name":"h264","profile":"High",
                "pix_fmt":"yuv420p","width":640,"height":360,"field_order":"progressive",
                "avg_frame_rate":"24000/1001","r_frame_rate":"24000/1001",
                "color_transfer":"bt709","color_primaries":"bt709","color_space":"bt709"
            }]}),
            DecodeSourceIdentity::from_sha256("a".repeat(64)).expect("test source identity"),
        )
        .expect("bound facts");
        let capabilities = DecodeCapabilities::new(
            DecodeCapabilitySnapshotIdentity::new("b".repeat(64), "test-node".into(), None)
                .expect("test inventory identity"),
            vec![],
            vec![SoftwareDecoder {
                codec: "h264".into(),
                implementation: None,
            }],
        )
        .expect("software inventory");
        let plan = resolve_transcode(
            &TranscodeRequest::new(
                Encoder::Software,
                TranscodeMediaOptions::from_options_with_facts(&file, &options, &facts),
            ),
            &facts,
            &capabilities,
            &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
            &AttemptRestrictions::none(),
        )
        .expect("legacy software plan");
        let cadence = Rational::new(24000, 1001).expect("fixed grid");
        // Pure identity fixture; only probe_output + measured constructs
        // production proofs. The separate real-encode regression proves that
        // boundary against actual SPS/avcC/sample durations.
        let proof = QualifiedSdrAvc {
            family: Encoder::Software.label().into(),
            width: 640,
            height: 360,
            cadence,
            bitrate_kbps: 1200,
            rate_control: EffectiveRateControl::Vbr.recipe_value(),
            forced_idr: false,
            level: 31,
            codec: "avc1.64001F".into(),
        };
        let caps = EncoderCaps {
            sdr_avc: vec![proof],
            ..EncoderCaps::default()
        };
        let execution = TranscodeExecution::from_options(&file, &options, Pacing::unpaced(), ".")
            .expect("execution");
        let old_args = hls_args_for_plan(&plan, &execution);
        let qualified = plan
            .clone()
            .with_sdr_avc_qualification(&caps, Some(cadence), false);
        assert_eq!(
            qualified
                .output_contract()
                .sdr_avc()
                .expect("exact cell")
                .codec(),
            "avc1.64001F"
        );
        let qualified_args = hls_args_for_plan(&qualified, &execution);
        assert!(qualified_args
            .windows(2)
            .any(|pair| pair == ["-level:v", "3.1"]));
        assert_ne!(qualified_args, old_args);
        let digest = PipelineDigest {
            ffmpeg_build: "test-build".into(),
        };
        let old_hash = Recipe::new(&digest, &plan, false).hash();
        assert_ne!(Recipe::new(&digest, &qualified, false).hash(), old_hash);
        for cadence in [None, Rational::new(60, 1), Rational::new(120, 1)] {
            let fallback = qualified
                .clone()
                .with_sdr_avc_qualification(&caps, cadence, false);
            assert!(fallback.output_contract().sdr_avc().is_none());
            assert_eq!(hls_args_for_plan(&fallback, &execution), old_args);
            assert_eq!(fallback.plan_digest(), plan.plan_digest());
            assert_eq!(Recipe::new(&digest, &fallback, false).hash(), old_hash);
        }
        let refused =
            plan.clone()
                .with_sdr_avc_qualification(&EncoderCaps::default(), Some(cadence), false);
        assert_eq!(hls_args_for_plan(&refused, &execution), old_args);
        assert_eq!(Recipe::new(&digest, &refused, false).hash(), old_hash);
    }

    #[test]
    fn sps_trace_refuses_truncation_duplicate_fields_and_configuration_changes() {
        let prefix = "[trace_headers @ 0x1] ";
        let first = format!("{prefix}Sequence Parameter Set\n{prefix}8 profile_idc 01100100 = 100\n{prefix}24 level_idc 00011111 = 31\n{prefix}32 rbsp_stop_one_bit 1 = 1\n");
        assert_eq!(
            sps_fields(first.as_bytes())
                .expect("complete SPS")
                .get("level_idc"),
            Some(&31)
        );
        assert!(sps_fields(first.trim_end().as_bytes()).is_none());
        assert!(sps_fields(
            first
                .replace("32 rbsp_stop_one_bit 1 = 1", "24 level_idc 00011111 = 31")
                .as_bytes()
        )
        .is_none());
        assert!(
            sps_fields(format!("{first}{}", first.replace("= 31", "= 32")).as_bytes()).is_none()
        );
        assert_eq!(
            sps_fields(format!("{first}{first}").as_bytes()),
            sps_fields(first.as_bytes())
        );
        assert!(sps_fields(format!("{prefix}{}\n", "x".repeat(8193)).as_bytes()).is_none());
    }

    #[tokio::test]
    async fn actual_sdr_encode_requires_matching_sps_raster_grid_and_complete_media() {
        // Explicit local tool selection is development evidence for that build,
        // never a substitute for the shipping FFmpeg/family matrix.
        let ffmpeg = std::env::var("PLURX_TEST_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_owned());
        let candidate = Candidate {
            encoder: Encoder::Software,
            width: 640,
            height: 360,
            cadence: Rational::new(24000, 1001).expect("fixed NTSC grid"),
            bitrate_kbps: 1200,
            rate_control: EffectiveRateControl::Vbr,
            forced_idr: false,
            level: 31,
        };
        let (media, trace) = probe_output(&ffmpeg, candidate)
            .await
            .expect("completed bounded real encode");
        let proof =
            measured(candidate, &media, &trace).expect("SPS, avcC and sample durations agree");
        assert!(proof.codec().starts_with("avc1.64"));
        assert!(proof.codec().ends_with("1F"));
        assert_eq!(proof.flags(), candidate.flags());
        assert!(proof.matches(
            candidate.encoder,
            (640, 360),
            candidate.cadence,
            1200,
            EffectiveRateControl::Vbr,
            false
        ));
        for wrong in [
            Candidate {
                width: 642,
                ..candidate
            },
            Candidate {
                height: 362,
                ..candidate
            },
            Candidate {
                level: 32,
                ..candidate
            },
            Candidate {
                cadence: Rational::new(60, 1).expect("60 fps"),
                ..candidate
            },
        ] {
            assert!(measured(wrong, &media, &trace).is_none());
        }
        assert!(measured(candidate, &media[..media.len() - 1], &trace).is_none());
        assert!(measured(candidate, &media, &trace[..trace.len() - 1]).is_none());
        let changed = String::from_utf8(trace.clone())
            .expect("trace UTF-8")
            .replace("= 100\n", "= 77\n");
        assert!(measured(candidate, &media, changed.as_bytes()).is_none());
        assert!(!proof.matches(
            Encoder::Nvenc,
            (640, 360),
            candidate.cadence,
            1200,
            EffectiveRateControl::Vbr,
            false
        ));
        assert!(!proof.matches(
            candidate.encoder,
            (640, 360),
            candidate.cadence,
            1201,
            EffectiveRateControl::Vbr,
            false
        ));
        assert!(!proof.matches(
            candidate.encoder,
            (640, 360),
            candidate.cadence,
            1200,
            EffectiveRateControl::Qvbr { quality: 23 },
            false
        ));
        assert!(!proof.matches(
            candidate.encoder,
            (640, 360),
            candidate.cadence,
            1200,
            EffectiveRateControl::Vbr,
            true
        ));
        eprintln!("actual SDR qualification: {}", proof.codec());
    }
}
