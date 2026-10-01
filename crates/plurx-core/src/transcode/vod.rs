//! Immutable encoded media: one frame grid owns the plan and every restart.

use crate::domain::MediaFile;
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

/// Shared AAC has its own clock: each ordinary interval contains 94 complete
/// AAC frames (2.005333 seconds), with only the final packet duration trimmed.
/// Video rung boundaries never duplicate or reset these audio intervals.
pub fn vod_shared_audio_plan(duration_ms: i64, bitrate_kbps: u32) -> SegmentPlan {
    let duration = (duration_ms.max(0) as u64).saturating_mul(u64::from(VOD_AUDIO_RATE)) / 1_000;
    let span = 94 * VOD_AAC_FRAME_SAMPLES;
    let mut entries = Vec::new();
    let mut start = 0;
    while start < duration {
        let ticks = span.min(duration - start);
        entries.push(PlanEntry {
            index: entries.len() as u32,
            kind: PlanEntryKind::AudioTail,
            start_ticks: start,
            duration_ticks: ticks,
            est_bytes: u64::from(bitrate_kbps)
                .saturating_mul(1_000)
                .saturating_mul(ticks)
                .saturating_mul(3)
                / (u64::from(VOD_AUDIO_RATE) * 16),
            cut: if start + ticks == duration {
                PlanCut::EndOfStream
            } else {
                PlanCut::Clean
            },
            fragments: 0,
        });
        start += ticks;
    }
    SegmentPlan {
        version: SEGPLAN_VERSION,
        timescale: VOD_AUDIO_RATE,
        target_duration: if entries.is_empty() { 0 } else { 3 },
        entries,
    }
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

/// A video-only rendition with actual init facts matched to its resolved plan.
/// Membership describes a compatible media shape, not a platform join receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VodVideoRung {
    rendition_id: String,
    init_id: String,
    compatibility_id: String,
    facts: crate::fmp4::AvcSampleEntryFacts,
    grid: VodFrameGrid,
    video_bitrate_kbps: u32,
}

impl VodVideoRung {
    /// The caller still owns source continuity and the exact encoder receipt.
    /// Dimensions/color come from the init; the recipe URI is supplied by the
    /// existing immutable rendition registry, never chosen by a client.
    pub fn from_verified_init(
        source: &MediaFile,
        plan: &ResolvedTranscode,
        init: &crate::fmp4::Init,
        grid: VodFrameGrid,
        rendition_id: &str,
        source_object_version: &str,
        shared_audio_recipe_id: Option<&str>,
    ) -> Result<Self, crate::fmp4::Fmp4Error> {
        use sha2::{Digest, Sha256};
        let refuse = || {
            crate::fmp4::Fmp4Error::Unsupported(
                "rendition does not match the initial continuous H.264 SDR family".into(),
            )
        };
        let digest_id = |id: &str| {
            id.len() == 64
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        let contract = plan.output_contract();
        if !digest_id(rendition_id)
            || source_object_version.is_empty()
            || source_object_version.len() > 512
            || source_object_version
                .bytes()
                .any(|byte| byte.is_ascii_control())
            || shared_audio_recipe_id.is_some_and(|id| !digest_id(id))
            || source.audio_streams.is_empty() != shared_audio_recipe_id.is_none()
            || plan.cache_identity() != &super::DecodeCacheIdentity::from_media_file(source)
            || plan.options().video_sample_envelope
                != super::VideoSampleEnvelope::ContinuousAvcHigh50
            || plan.options().input_has_audio
            || plan.options().subtitle_burn.is_some()
            || contract.normalized_geometry().is_none()
            || contract.output_grade() != super::OutputGrade::Sdr
            || contract.output_codec() != "h264"
            || contract.output_pixel_format() != "yuv420p"
            || init.tracks.len() != 1
            || init
                .video()
                .is_none_or(|video| video.timescale != grid.numerator)
            || VodFrameGrid::new(grid.numerator, grid.denominator) != Some(grid)
        {
            return Err(refuse());
        }
        let facts = crate::fmp4::avc_sample_entry_facts(init)?.ok_or_else(refuse)?;
        if !super::decode::continuous_avc_envelope_accepts(
            u32::from(facts.width),
            u32::from(facts.height),
            grid.numerator,
            grid.denominator,
            plan.options().video_bitrate_kbps,
        ) || facts.codec != "avc1.640032"
            || contract.effective_width() != Some(u32::from(facts.width))
            || contract.effective_height() != Some(u32::from(facts.height))
            || facts.color_primaries != 1
            || facts.color_transfer != 1
            || facts.color_matrix != 1
            || facts.full_range
        {
            return Err(refuse());
        }
        let mut family = Sha256::new();
        for value in [
            "continuous-h264-sdr-family-v1".to_owned(),
            source_object_version.to_owned(),
            plan.cache_identity().as_str().to_owned(),
            plan.source_facts_digest().to_owned(),
            facts.codec.clone(),
            grid.frame_rate(),
            grid.frames_per_segment.to_string(),
            "bt709-limited-yuv420p".to_owned(),
            "external-subtitles".to_owned(),
            shared_audio_recipe_id.unwrap_or("silent").to_owned(),
            plan.options()
                .audio_index
                .map_or_else(|| "default".to_owned(), |index| index.to_string()),
            plan.options().audio_offset_ms.to_string(),
            plan.options().audio_channels.to_string(),
            plan.options().audio_bitrate_kbps.to_string(),
        ] {
            family.update((value.len() as u64).to_le_bytes());
            family.update(value.as_bytes());
        }
        Ok(Self {
            rendition_id: rendition_id.to_owned(),
            init_id: hex::encode(Sha256::digest(&init.bytes)),
            compatibility_id: hex::encode(family.finalize()),
            facts,
            grid,
            video_bitrate_kbps: plan.options().video_bitrate_kbps,
        })
    }

    pub fn rendition_id(&self) -> &str {
        &self.rendition_id
    }
    pub fn init_id(&self) -> &str {
        &self.init_id
    }
    pub fn compatibility_id(&self) -> &str {
        &self.compatibility_id
    }
    pub fn facts(&self) -> &crate::fmp4::AvcSampleEntryFacts {
        &self.facts
    }
    pub fn grid(&self) -> VodFrameGrid {
        self.grid
    }
    pub fn video_bitrate_kbps(&self) -> u32 {
        self.video_bitrate_kbps
    }
}

/// A stable set of verified rungs. Controlled attachment can demand one at a
/// time; native autonomous attachment must reserve its advertised pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VodVideoFamily {
    id: String,
    rungs: Vec<VodVideoRung>,
}

impl VodVideoFamily {
    pub fn new(mut rungs: Vec<VodVideoRung>) -> Result<Self, crate::fmp4::Fmp4Error> {
        let refuse = || {
            crate::fmp4::Fmp4Error::Unsupported(
                "continuous rungs do not form one verified family".into(),
            )
        };
        if !(2..=8).contains(&rungs.len()) {
            return Err(refuse());
        }
        let id = rungs[0].compatibility_id.clone();
        if rungs.iter().any(|rung| rung.compatibility_id != id) {
            return Err(refuse());
        }
        rungs.sort_by_key(|rung| (rung.facts.height, rung.facts.width, rung.video_bitrate_kbps));
        let mut ids = std::collections::BTreeSet::new();
        let mut shapes = std::collections::BTreeSet::new();
        for rung in &rungs {
            if !ids.insert(&rung.rendition_id)
                || !shapes.insert((rung.facts.width, rung.facts.height))
            {
                return Err(refuse());
            }
        }
        Ok(Self { id, rungs })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn rungs(&self) -> &[VodVideoRung] {
        &self.rungs
    }
}

/// AAC correction shared by muxed VOD and the single family soundtrack.
/// Seek and trim on the global sample lattice rather than restarting its phase.
struct AudioClock {
    target: f64,
    seek: f64,
    relative_target_samples: i64,
}

impl AudioClock {
    fn new(start_seconds: f64, offset_ms: i64) -> Self {
        let anchor = vod_audio_anchor(start_seconds) as f64 / f64::from(VOD_AUDIO_RATE);
        let target = anchor - offset_ms as f64 / 1_000.0;
        let target_samples = (target * f64::from(VOD_AUDIO_RATE)).round() as i64;
        let seek_samples = target_samples
            .saturating_sub(i64::from(VOD_AUDIO_RATE) * 2)
            .max(0) as u64
            / VOD_AAC_FRAME_SAMPLES
            * VOD_AAC_FRAME_SAMPLES;
        Self {
            target,
            seek: seek_samples as f64 / f64::from(VOD_AUDIO_RATE),
            relative_target_samples: target_samples
                .saturating_sub(i64::try_from(seek_samples).unwrap_or(i64::MAX)),
        }
    }

    fn filter(&self) -> String {
        format!(
            "asetpts=PTS-{:.9}/TB,aresample=48000:async=1:first_pts=0,atrim=start_sample={},asetpts=PTS-({}),aresample=48000:async=1:first_pts=0,apad",
            self.seek, self.relative_target_samples.max(0), self.relative_target_samples
        )
    }
}

/// Frozen soundtrack semantics. Video shape, bitrate and encoder selection
/// never enter this digest, so all rungs reuse one audio recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VodSharedAudioRecipe {
    audio_index: i64,
    audio_offset_ms: i64,
    audio_channels: u32,
    audio_bitrate_kbps: u32,
    digest: String,
}

/// One decoder, one filter worker and one AAC encoder worker are reserved.
pub const VOD_SHARED_AUDIO_CPU_THREADS: usize = 3;

impl VodSharedAudioRecipe {
    pub fn from_plan(plan: &ResolvedTranscode) -> Option<Self> {
        use sha2::{Digest, Sha256};
        let media = plan.options();
        if !media.input_has_audio
            || !(1..=8).contains(&media.audio_channels)
            || !(16..=1024).contains(&media.audio_bitrate_kbps)
        {
            return None;
        }
        let audio_index = media.audio_index.unwrap_or(0);
        let mut hash = Sha256::new();
        for value in [
            "continuous-shared-aac-lc-48k-v1".to_owned(),
            plan.cache_identity().as_str().to_owned(),
            plan.source_facts_digest().to_owned(),
            audio_index.to_string(),
            media.audio_offset_ms.to_string(),
            media.audio_channels.to_string(),
            media.audio_bitrate_kbps.to_string(),
        ] {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
        Some(Self {
            audio_index,
            audio_offset_ms: media.audio_offset_ms,
            audio_channels: media.audio_channels,
            audio_bitrate_kbps: media.audio_bitrate_kbps,
            digest: hex::encode(hash.finalize()),
        })
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn plan(&self, duration_ms: i64) -> SegmentPlan {
        vod_shared_audio_plan(duration_ms, self.audio_bitrate_kbps)
    }

    /// The publisher removes priming and restores the film-global clock.
    /// Each stage's explicit thread cap matches the admission reservation.
    pub fn args(
        &self,
        execution: &TranscodeExecution,
        duration_seconds: f64,
    ) -> Option<Vec<String>> {
        if !duration_seconds.is_finite()
            || duration_seconds <= execution.start_seconds
            || !execution.start_seconds.is_finite()
            || execution.start_seconds < 0.0
        {
            return None;
        }
        let clock = AudioClock::new(execution.start_seconds, self.audio_offset_ms);
        let anchor = vod_audio_anchor(execution.start_seconds) as f64 / f64::from(VOD_AUDIO_RATE);
        Some(vec![
            "-copyts".into(),
            "-filter_threads".into(),
            "1".into(),
            "-noaccurate_seek".into(),
            "-ss".into(),
            format!("{:.9}", clock.seek),
            "-threads".into(),
            "1".into(),
            "-i".into(),
            execution.source_path.to_string_lossy().into_owned(),
            "-map".into(),
            format!("0:a:{}", self.audio_index),
            "-vn".into(),
            "-sn".into(),
            "-dn".into(),
            "-map_chapters".into(),
            "-1".into(),
            "-af".into(),
            clock.filter(),
            "-c:a".into(),
            "aac".into(),
            "-threads:a".into(),
            "1".into(),
            "-profile:a".into(),
            "aac_low".into(),
            "-ac".into(),
            self.audio_channels.to_string(),
            "-b:a".into(),
            format!("{}k", self.audio_bitrate_kbps),
            "-ar".into(),
            VOD_AUDIO_RATE.to_string(),
            "-t".into(),
            format!("{:.9}", duration_seconds - anchor),
            "-avoid_negative_ts".into(),
            "disabled".into(),
            "-use_editlist".into(),
            "0".into(),
            "-movflags".into(),
            "+empty_moov+delay_moov+default_base_moof".into(),
            "-frag_duration".into(),
            "2000000".into(),
            "-f".into(),
            "mp4".into(),
            "pipe:1".into(),
        ])
    }
}

/// Compatibility entry point for existing resolved VOD callers.
pub fn vod_shared_audio_args(
    plan: &ResolvedTranscode,
    execution: &TranscodeExecution,
    duration_seconds: f64,
) -> Option<Vec<String>> {
    VodSharedAudioRecipe::from_plan(plan)?.args(execution, duration_seconds)
}

/// The production encoded fMP4 pipe. Source-clock filters run before the
/// final rebase, so libass and manual A/V correction retain film time after
/// seeking. The output frame grid and IDRs are identical on every restart.
pub fn vod_pipe_args(
    source: &MediaFile,
    plan: &ResolvedTranscode,
    execution: &TranscodeExecution,
    grid: VodFrameGrid,
    duration_seconds: f64,
) -> Vec<String> {
    let media = plan.options();
    let target = execution.start_seconds.max(0.0);
    let audio_anchor = vod_audio_anchor(target) as f64 / f64::from(VOD_AUDIO_RATE);
    let audio = AudioClock::new(target, media.audio_offset_ms);
    let audio_target = audio.target;
    let audio_seek = audio.seek;
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
    if has_audio {
        let before_map = args
            .iter()
            .position(|arg| arg == "-map")
            .expect("video map");
        // The demux seek may land on either side of its requested timestamp.
        // Rebase the decoded PTS to the exact requested AAC lattice below,
        // then trim on a sample count; this keeps every generation on one
        // global phase without decoding the soundtrack from film zero.
        args.splice(
            before_map..before_map,
            [
                "-ss".into(),
                format!("{audio_seek:.9}"),
                "-i".into(),
                execution.source_path.to_string_lossy().into_owned(),
            ],
        );
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
    let raster = if plan.output_contract().normalized_geometry().is_some() {
        plan.output_contract()
            .effective_width()
            .zip(plan.output_contract().effective_height())
            .map(|(width, height)| (i64::from(width), i64::from(height)))
    } else {
        super::output_size(source, media.target_height)
    };
    if let Some((width, height)) = raster {
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
                &format!("[{}:s:0]", if has_audio { 2 } else { 1 }),
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
        args.extend(["-af".to_owned(), audio.filter()]);
        args.extend([
            "-ar".to_owned(),
            VOD_AUDIO_RATE.to_string(),
            "-profile:a".to_owned(),
            "aac_low".to_owned(),
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
            downloaded_subtitles: Vec::new(),
            id: 1,
            item_id: 1,
            path: "/media/chaptered.mkv".into(),
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
            dolby_vision: crate::domain::DolbyVisionFacts::default(),
        };
        let options = crate::transcode::TranscodeOptions::default();
        let facts = crate::transcode::DecodeFacts::from_ffprobe_json(
            &serde_json::json!({"streams":[{
                "index":0,"codec_type":"video","codec_name":"hevc",
                "profile":"Main","width":640,"height":360,
                "pix_fmt":"yuv420p","avg_frame_rate":"24/1",
                "r_frame_rate":"24/1","sample_aspect_ratio":"1:1","disposition":{"attached_pic":0}
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
        .expect("execution");
        let args = vod_pipe_args(
            &source,
            &plan,
            &execution,
            VodFrameGrid::new(24, 1).expect("grid"),
            12.0,
        );
        assert!(args.iter().any(|arg| arg == "-an"));
        assert!(!args.iter().any(|arg| arg.starts_with("0:a:")));
        assert!(!args.iter().any(|arg| arg == "-c:a" || arg == "-profile:a"));
        assert!(vod_shared_audio_args(&plan, &execution, 12.0).is_none());

        let mut audio_options = plan.options().clone();
        audio_options.input_has_audio = true;
        audio_options.audio_index = Some(2);
        audio_options.audio_offset_ms = -175;
        let audio_plan = crate::transcode::resolve_transcode(
            &crate::transcode::TranscodeRequest::new(
                crate::transcode::Encoder::Software,
                audio_options,
            ),
            &facts,
            &capabilities,
            &crate::transcode::DecodePolicySnapshot::new(
                crate::transcode::DecodePlanPolicy::Legacy,
                None,
            ),
            &crate::transcode::AttemptRestrictions::none(),
        )
        .expect("audio plan");
        assert_ne!(audio_plan.plan_digest(), plan.plan_digest());
        let soundtrack = VodSharedAudioRecipe::from_plan(&audio_plan).expect("soundtrack");
        let mut changed_video = audio_plan.options().clone();
        changed_video.video_bitrate_kbps += 1000;
        let changed_video_plan = crate::transcode::resolve_transcode(
            &crate::transcode::TranscodeRequest::new(
                crate::transcode::Encoder::Software,
                changed_video,
            ),
            &facts,
            &capabilities,
            &crate::transcode::DecodePolicySnapshot::new(
                crate::transcode::DecodePlanPolicy::Legacy,
                None,
            ),
            &crate::transcode::AttemptRestrictions::none(),
        )
        .expect("different video rung");
        assert_ne!(changed_video_plan.plan_digest(), audio_plan.plan_digest());
        assert_eq!(
            VodSharedAudioRecipe::from_plan(&changed_video_plan)
                .expect("same soundtrack")
                .digest(),
            soundtrack.digest()
        );
        assert_eq!(soundtrack.plan(12_000).timescale, VOD_AUDIO_RATE);
        assert_eq!(VOD_SHARED_AUDIO_CPU_THREADS, 3);
        let mut restart = execution.clone();
        restart.start_seconds = 10.01;
        let audio_args =
            vod_shared_audio_args(&audio_plan, &restart, 12.0).expect("selected audio producer");
        assert!(audio_args.windows(2).any(|pair| pair == ["-map", "0:a:2"]));
        assert!(!audio_args.iter().any(|arg| arg == "-vf" || arg == "-c:v"));
        assert!(audio_args.iter().any(|arg| arg == "-vn"));
        for option in ["-threads", "-threads:a", "-filter_threads"] {
            assert!(audio_args.windows(2).any(|pair| pair == [option, "1"]));
        }
        let clock = AudioClock::new(restart.start_seconds, -175);
        assert!(audio_args
            .windows(2)
            .any(|pair| pair[0] == "-af" && pair[1] == clock.filter()));
        restart.start_seconds = 12.0;
        assert!(vod_shared_audio_args(&audio_plan, &restart, 12.0).is_none());
        let continuous_plan = crate::transcode::resolve_transcode(
            &crate::transcode::TranscodeRequest::new(
                crate::transcode::Encoder::Software,
                plan.options().clone(),
            )
            .with_continuous_avc_video(),
            &facts,
            &capabilities,
            &crate::transcode::DecodePolicySnapshot::new(
                crate::transcode::DecodePlanPolicy::Legacy,
                None,
            ),
            &crate::transcode::AttemptRestrictions::none(),
        )
        .expect("continuous video plan");
        let continuous_args = vod_pipe_args(
            &source,
            &continuous_plan,
            &execution,
            VodFrameGrid::new(24, 1).expect("grid"),
            12.0,
        );
        assert!(continuous_args
            .windows(2)
            .any(|pair| pair == ["-profile:v", "high"]));
        assert!(continuous_args
            .windows(2)
            .any(|pair| pair == ["-level:v", "5.0"]));
        assert!(continuous_args.iter().any(|arg| arg == "-an"));
        assert_ne!(continuous_plan.plan_digest(), plan.plan_digest());
        assert_eq!(
            continuous_plan.output_contract().effective_width(),
            Some(640)
        );
        assert_eq!(
            continuous_plan.output_contract().effective_height(),
            Some(360)
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
    }

    #[test]
    fn continuous_avc_limits_use_exact_rational_macroblock_rate() {
        let accepts = super::super::decode::continuous_avc_envelope_accepts;
        assert!(accepts(1920, 1080, 60_000, 1001, 12_000));
        assert!(accepts(3840, 2160, 24_000, 1001, 40_000));
        assert!(!accepts(3840, 2160, 30, 1, 40_000));
        assert!(!accepts(4096, 2304, 24, 1, 40_000));
        assert!(!accepts(1921, 1080, 24, 1, 12_000));
        assert!(!accepts(1920, 1080, 24, 0, 12_000));
        assert!(accepts(640, 360, 24, 1, 112_500));
        assert!(!accepts(640, 360, 24, 1, 112_501));
    }

    #[test]
    fn family_membership_varies_raster_but_never_audio_or_timeline_identity() {
        let rung = |id: &str, width, height| VodVideoRung {
            rendition_id: id.repeat(64),
            init_id: "c".repeat(64),
            compatibility_id: "d".repeat(64),
            facts: crate::fmp4::AvcSampleEntryFacts {
                codec: "avc1.640032".into(),
                width,
                height,
                color_primaries: 1,
                color_transfer: 1,
                color_matrix: 1,
                full_range: false,
            },
            grid: VodFrameGrid::new(24_000, 1_001).expect("grid"),
            video_bitrate_kbps: 4_000,
        };
        let low = rung("a", 1280, 720);
        let high = rung("b", 1920, 1080);
        let family = VodVideoFamily::new(vec![high.clone(), low.clone()]).expect("family");
        assert_eq!(family.rungs()[0].facts().height, 720);
        assert_eq!(family.rungs()[1].facts().height, 1080);
        let mut changed = high.clone();
        changed.compatibility_id = "e".repeat(64);
        assert!(VodVideoFamily::new(vec![low.clone(), changed]).is_err());
        assert!(VodVideoFamily::new(vec![low.clone(), low.clone()]).is_err());
        let mut same_shape = low.clone();
        same_shape.rendition_id = "f".repeat(64);
        assert!(VodVideoFamily::new(vec![low, same_shape]).is_err());
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
