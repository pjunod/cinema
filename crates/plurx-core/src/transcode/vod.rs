//! Immutable encoded media: one frame grid owns the plan and every restart.

use crate::segplan::{PlanCut, PlanEntry, PlanEntryKind, SegmentPlan, SEGPLAN_VERSION};

use super::{ResolvedTranscode, TranscodeExecution, BURNED_VIDEO_LABEL};

pub const VOD_AUDIO_RATE: u32 = 48_000;
pub const VOD_AAC_FRAME_SAMPLES: u64 = 1_024;

/// Two seconds of AAC encoder preroll, starting on the film-global AAC
/// sample lattice. Video is trimmed later, at the requested plan boundary.
pub fn vod_audio_anchor(start_seconds: f64) -> u64 {
    let samples = ((start_seconds - 2.0).max(0.0) * f64::from(VOD_AUDIO_RATE)).floor() as u64;
    samples / VOD_AAC_FRAME_SAMPLES * VOD_AAC_FRAME_SAMPLES
}

/// The declared OUTPUT cadence, not a claim that the input is constant-rate.
/// FFmpeg's fps filter samples the source clock onto this grid, including VFR.
/// The nearest whole number of frames to two seconds forms a closed GOP:
/// 24000/1001 retains its cadence with 48 frames per 2.002-second entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VodFrameGrid {
    pub numerator: u32,
    pub denominator: u32,
    pub frames_per_segment: u32,
}

impl VodFrameGrid {
    pub fn new(numerator: u32, denominator: u32) -> Option<Self> {
        if numerator == 0 || denominator == 0 {
            return None;
        }
        let mut a = numerator;
        let mut b = denominator;
        while b != 0 {
            (a, b) = (b, a % b);
        }
        let numerator = numerator / a;
        let denominator = denominator / a;
        // Reject nonsensical/unbounded probe values instead of guessing a
        // cadence whose output cannot satisfy the already published plan.
        if denominator > 1_000_000
            || numerator > 1_000_000
            || numerator < denominator
            || u64::from(numerator) > 240 * u64::from(denominator)
        {
            return None;
        }
        let frames_per_segment = ((2 * u64::from(numerator) + u64::from(denominator) / 2)
            / u64::from(denominator)) as u32;
        Some(Self {
            numerator,
            denominator,
            frames_per_segment,
        })
    }

    pub fn frame_rate(self) -> String {
        format!("{}/{}", self.numerator, self.denominator)
    }

    pub fn segment_ticks(self) -> u64 {
        u64::from(self.frames_per_segment) * u64::from(self.denominator)
    }

    /// Round the final frame outward. Every advertised interval then has a
    /// whole number of output frames, including a short final entry.
    pub fn plan(self, duration_ms: i64, bits_per_second: u64) -> SegmentPlan {
        let frames = (duration_ms.max(0) as u64)
            .saturating_mul(u64::from(self.numerator))
            .div_ceil(1_000 * u64::from(self.denominator));
        let duration_ticks = frames.saturating_mul(u64::from(self.denominator));
        let mut entries = Vec::new();
        let mut start_ticks = 0;
        while start_ticks < duration_ticks {
            let span = self.segment_ticks().min(duration_ticks - start_ticks);
            entries.push(PlanEntry {
                index: entries.len() as u32,
                kind: PlanEntryKind::Video,
                start_ticks,
                duration_ticks: span,
                // Video and audio both count toward working-set admission.
                // Nominal rate is an estimate, never a publication refusal.
                est_bytes: bits_per_second.saturating_mul(span).saturating_mul(3)
                    / (u64::from(self.numerator) * 8 * 2),
                cut: if start_ticks + span == duration_ticks {
                    PlanCut::EndOfStream
                } else {
                    PlanCut::Clean
                },
                fragments: 0,
            });
            start_ticks += span;
        }
        let target_duration = entries
            .iter()
            .map(|entry| entry.duration_ticks.div_ceil(u64::from(self.numerator)) as u32)
            .max()
            .unwrap_or(0);
        SegmentPlan {
            version: SEGPLAN_VERSION,
            timescale: self.numerator,
            entries,
            target_duration,
        }
    }
}

/// The production encoded fMP4 pipe. Source-clock filters run before the
/// final rebase, so libass and manual A/V correction retain film time after
/// seeking. The output frame grid and IDRs are identical on every restart.
pub fn vod_pipe_args(
    plan: &ResolvedTranscode,
    execution: &TranscodeExecution,
    grid: VodFrameGrid,
    duration_seconds: f64,
) -> Vec<String> {
    let media = plan.options();
    let target = execution.start_seconds.max(0.0);
    let audio_anchor = vod_audio_anchor(target) as f64 / f64::from(VOD_AUDIO_RATE);
    let audio_target = audio_anchor - media.audio_offset_ms as f64 / 1_000.0;
    let audio_target_samples = (audio_target * f64::from(VOD_AUDIO_RATE)).round() as i64;
    // A second input opened at film zero made every seek decode the whole
    // preceding soundtrack before ffmpeg could emit its init. On slower
    // hosts a resume twenty minutes into a feature therefore exhausted the
    // materialization deadline without producing one byte. Seek close to the
    // wanted AAC anchor, on the film-global AAC lattice, while retaining two
    // seconds for demux, resample, and encoder preroll.
    let audio_seek_samples = audio_target_samples
        .saturating_sub(i64::from(VOD_AUDIO_RATE) * 2)
        .max(0) as u64
        / VOD_AAC_FRAME_SAMPLES
        * VOD_AAC_FRAME_SAMPLES;
    let mut audio_seek = audio_seek_samples as f64 / f64::from(VOD_AUDIO_RATE);
    let mut input = execution.clone();
    // Decode a short preroll. Positive correction needs earlier audio;
    // negative correction is an absolute trim, not repeated per-seek silence.
    input.start_seconds = (target.min(audio_target) - 2.0).max(0.0);
    let mut args = super::encode_input_args_for_plan(plan, &input);
    // The VOD presentation owns a film-global sample-lattice correction
    // below. The shared plan builder must still own decoder/renderer/encoder
    // selection, but its ordinary one-input A/V filter would apply the same
    // semantic offset a second time.
    if let Some(index) = args.iter().position(|argument| argument == "-af") {
        args.drain(index..=index + 1);
    }
    // Explicit film-clock trim below owns the accurate landing. Letting the
    // input seek also trim audio can discard a different partial packet on
    // each restart before its sample-clock correction sees the frame.
    args.splice(0..0, ["-copyts".to_owned(), "-noaccurate_seek".to_owned()]);
    let has_audio = media.input_has_audio;
    let reopen_audio = has_audio && execution.source_input.is_file();
    if has_audio && !reopen_audio {
        // A managed physical title owns one reader. Reopening it for audio
        // would create a competing demux cursor under the same drive lease.
        // The common preroll already starts early enough for the AAC anchor,
        // so trim the audio from input zero on that exact source clock.
        audio_seek = input.start_seconds;
    }
    if reopen_audio {
        let before_map = args
            .iter()
            .position(|arg| arg == "-map")
            .expect("video map");
        let mut audio_input = vec!["-ss".to_owned(), format!("{audio_seek:.9}")];
        let mut typed_input = Vec::new();
        execution
            .source_input
            .append_input_args(&mut typed_input)
            .expect("VOD execution contains a validated input");
        audio_input.extend(
            typed_input
                .into_iter()
                .map(|argument| argument.to_string_lossy().into_owned()),
        );
        // The demux seek may land on either side of its requested timestamp.
        // Rebase the decoded PTS to the exact requested AAC lattice below,
        // then trim on a sample count; this keeps every generation on one
        // global phase without decoding the soundtrack from film zero.
        args.splice(before_map..before_map, audio_input);
        for argument in &mut args {
            if argument.starts_with("0:a:") {
                *argument = argument.replacen("0:a:", "1:a:", 1);
            }
        }
    }
    if media.subtitle_burn.as_ref().is_some_and(|burn| burn.bitmap) {
        if let Some(subtitle) = &execution.subtitle_file {
            let before_map = args
                .iter()
                .position(|arg| arg == "-map")
                .expect("video map");
            // A subtitle-only sidecar is cheap to read from zero and retains
            // display state whose cue started before the video's fast seek.
            // Both inputs retain their film timestamps under copyts, so no
            // per-input zero rebasing or synthetic sync offset is permitted.
            args.splice(
                before_map..before_map,
                ["-i".into(), subtitle.to_string_lossy().into_owned()],
            );
        }
    }
    let remaining = (duration_seconds - target).max(0.0);
    let frames =
        (remaining * f64::from(grid.numerator) / f64::from(grid.denominator)).round() as u64;
    // Discard decoded preroll before sampling the source clock. If fps runs
    // first, its start_time may stamp the first pre-target frame at `target`,
    // after which trim cannot distinguish relabelled old content from the
    // requested picture (most visible on a backward seek into VFR media).
    let first = format!(
        "trim=start={target:.9},fps={}:start_time={target:.9},",
        grid.frame_rate()
    );
    let last = format!(
        ",tpad=stop_mode=clone:stop_duration={remaining:.9},trim=end_frame={frames},setpts=PTS-{audio_anchor:.9}/TB"
    );
    // Use the same explicit even raster for CPU scale, GPU/bitmap scale,
    // identity and HLS facts; -2's independent aspect rounding can differ.
    if let (Some(width), Some(height)) = (
        plan.output_contract().effective_width(),
        plan.output_contract().effective_height(),
    ) {
        let automatic = format!("scale=-2:'min({},ih)'", media.target_height);
        for flag in ["-vf", "-filter_complex"] {
            if let Some(index) = args.iter().position(|arg| arg == flag) {
                args[index + 1] =
                    args[index + 1].replace(&automatic, &format!("scale={width}:{height}"));
            }
        }
    }
    if let Some(index) = args.iter().position(|arg| arg == "-vf") {
        args[index + 1] = format!("{first}{}{last}", args[index + 1]);
    } else if let Some(index) = args.iter().position(|arg| arg == "-filter_complex") {
        let video_input = format!("[0:{}]", plan.decode().input_video_stream());
        let graph = args[index + 1].replacen(&video_input, &format!("{video_input}{first}"), 1);
        let graph = if let Some(burn) = media
            .subtitle_burn
            .as_ref()
            .filter(|burn| burn.bitmap && execution.subtitle_file.is_some())
        {
            graph.replace(
                &format!("[0:s:{}]", burn.subtitle_index),
                &format!("[{}:s:0]", if reopen_audio { 2 } else { 1 }),
            )
        } else {
            graph
        };
        let graph = graph
            .strip_suffix(BURNED_VIDEO_LABEL)
            .expect("the shared bitmap graph ends at its mapped video label");
        args[index + 1] = format!("{graph}{last}{BURNED_VIDEO_LABEL}");
    }
    if has_audio {
        let relative_audio_target_samples =
            audio_target_samples - i64::try_from(audio_seek_samples).unwrap_or(i64::MAX);
        args.extend([
            "-af".to_owned(),
            format!(
                "asetpts=PTS-{audio_seek:.9}/TB,aresample=48000:async=1:first_pts=0,atrim=start_sample={},asetpts=PTS-({relative_audio_target_samples}),aresample=48000:async=1:first_pts=0,apad",
                relative_audio_target_samples.max(0)
            ),
        ]);
    }
    if let Some(index) = args.iter().position(|arg| arg == "-force_key_frames") {
        args[index + 1] = format!("expr:eq(mod(n,{}),0)", grid.frames_per_segment);
    }
    args.extend([
        // Chapters are library metadata, not part of an immutable media
        // rendition. ffmpeg maps them independently of the explicit video
        // and audio stream maps; when a generation starts at a later film
        // offset, the remaining chapter table changes the `moov` bytes even
        // though the codec recipe is identical. Excluding them keeps init
        // identity strict while making it depend only on the media recipe.
        "-map_chapters".to_owned(),
        "-1".to_owned(),
        "-t".to_owned(),
        format!("{:.9}", (duration_seconds - audio_anchor).max(0.0)),
        "-ar".to_owned(),
        VOD_AUDIO_RATE.to_string(),
        "-profile:a".to_owned(),
        "aac_low".to_owned(),
        // No reorder delay: the plan addresses presented frame boundaries,
        // not a decoder preroll hidden before the URI's declared start.
        "-bf".to_owned(),
        "0".to_owned(),
        "-flags".to_owned(),
        "+cgop".to_owned(),
        "-g".to_owned(),
        grid.frames_per_segment.to_string(),
        "-keyint_min".to_owned(),
        grid.frames_per_segment.to_string(),
        "-sc_threshold".to_owned(),
        "0".to_owned(),
        "-fps_mode:v".to_owned(),
        // The fps filter already made CFR. Output vsync must not duplicate
        // the first video frame across the audio-only encoder preroll.
        "passthrough".to_owned(),
        // A bare -enc_time_base also quantizes AAC to video ticks. Keep this
        // video-only; audio retains its sample clock and encoder priming.
        "-enc_time_base:v".to_owned(),
        format!("{}:{}", grid.denominator, grid.numerator),
        "-avoid_negative_ts".to_owned(),
        "disabled".to_owned(),
        "-use_editlist".to_owned(),
        "0".to_owned(),
        "-movflags".to_owned(),
        // The runner removes encoder priming and restores the film-global
        // audio lattice. Per-generation edit lists must not change the init
        // or apply a second priming shift after fragment publication.
        "+empty_moov+delay_moov+default_base_moof+frag_keyframe".to_owned(),
        "-video_track_timescale".to_owned(),
        grid.numerator.to_string(),
        "-f".to_owned(),
        "mp4".to_owned(),
        "pipe:1".to_owned(),
    ]);
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_vod_recipe_explicitly_excludes_source_chapters() {
        let source = crate::domain::MediaFile {
            id: 1,
            item_id: 1,
            path: "/media/chaptered.mkv".into(),
            size: 1,
            mtime: 1,
            duration_ms: Some(12_000),
            container: Some("mkv".into()),
            video_codec: Some("hevc".into()),
            video_codec_tag: None,
            video_profile: Some("Main".into()),
            width: Some(640),
            height: Some(360),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            bitrate: Some(1_000_000),
            audio_streams: vec![],
            subtitle_streams: vec![],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: crate::domain::DolbyVisionFacts::default(),
        };
        let options = crate::transcode::TranscodeOptions::default();
        let facts = crate::transcode::DecodeFacts::from_ffprobe_json(
            &serde_json::json!({"streams":[{
                "index":0,"codec_type":"video","codec_name":"hevc",
                "profile":"Main","width":640,"height":360,
                "pix_fmt":"yuv420p","avg_frame_rate":"24/1",
                "r_frame_rate":"24/1","disposition":{"attached_pic":0}
            }]}),
            crate::transcode::DecodeSourceIdentity::from_sha256("a".repeat(64))
                .expect("source identity"),
        )
        .expect("decode facts");
        let capabilities = crate::transcode::DecodeCapabilities::new(
            crate::transcode::DecodeCapabilitySnapshotIdentity::new(
                "f".repeat(64),
                "vod-chapter-test".to_owned(),
                Some("e".repeat(64)),
            )
            .expect("capability identity"),
            vec![],
            vec![crate::transcode::SoftwareDecoder {
                codec: "hevc".to_owned(),
                implementation: Some("hevc".to_owned()),
            }],
        )
        .expect("capabilities");
        let plan = crate::transcode::resolve_transcode(
            &crate::transcode::TranscodeRequest::new(
                crate::transcode::Encoder::Software,
                crate::transcode::TranscodeMediaOptions::from_options(&source, &options),
            ),
            &facts,
            &capabilities,
            &crate::transcode::DecodePolicySnapshot::new(
                crate::transcode::DecodePlanPolicy::Legacy,
                None,
            ),
            &crate::transcode::AttemptRestrictions::none(),
        )
        .expect("software plan");
        let execution = crate::transcode::TranscodeExecution::from_options(
            &source,
            &options,
            crate::transcode::Pacing::unpaced(),
            ".",
        )
        .expect("execution")
        .with_source_input(crate::optical::ResolvedInput::Dvd {
            path: "/media/Disc One".into(),
            title_number: 3,
            angle: 2,
        })
        .expect("managed input");
        let args = vod_pipe_args(
            &plan,
            &execution,
            VodFrameGrid::new(24, 1).expect("grid"),
            12.0,
        );
        let chapter_options = args
            .windows(2)
            .filter(|pair| pair[0] == "-map_chapters")
            .collect::<Vec<_>>();
        assert_eq!(chapter_options.len(), 1);
        assert_eq!(chapter_options[0][1], "-1");
        assert!(
            args.iter().position(|arg| arg == "-map_chapters")
                < args.iter().position(|arg| arg == "pipe:1")
        );
        assert!(args.windows(8).any(|window| window
            == [
                "-f",
                "dvdvideo",
                "-title",
                "3",
                "-angle",
                "2",
                "-i",
                "/media/Disc One"
            ]));
    }

    #[test]
    fn ntsc_grid_preserves_cadence_and_exact_restart_boundaries() {
        let grid = VodFrameGrid::new(24_000, 1_001).expect("NTSC cadence");
        let plan = grid.plan(96_000, 1_000_000);
        assert_eq!(grid.frames_per_segment, 48);
        assert_eq!(plan.entry(45).expect("seek").start_ticks, 2_162_160);
        assert!(plan
            .entries
            .iter()
            .all(|entry| { entry.start_ticks % 1_001 == 0 && entry.duration_ticks % 1_001 == 0 }));
        assert_eq!(plan.target_duration, 3);
        assert!(plan.entries[0].est_bytes >= 250_000);
    }

    #[test]
    fn frame_grid_rejects_invalid_probe_values_and_reduces_the_ratio() {
        assert!(VodFrameGrid::new(0, 1).is_none());
        assert!(VodFrameGrid::new(30, 0).is_none());
        assert!(VodFrameGrid::new(u32::MAX, 1).is_none());
        assert_eq!(
            VodFrameGrid::new(60_000, 2_002),
            VodFrameGrid::new(30_000, 1_001)
        );
    }
}
