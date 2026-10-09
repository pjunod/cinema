//! Immutable encoded media: one frame grid owns the plan and every restart.

use crate::domain::MediaFile;
use crate::segplan::{PlanCut, PlanEntry, PlanEntryKind, SegmentPlan, SEGPLAN_VERSION};

use super::{ResolvedTranscode, TranscodeExecution, BURNED_VIDEO_LABEL};

pub const VOD_AUDIO_RATE: u32 = 48_000;
pub const VOD_AAC_FRAME_SAMPLES: u64 = 1_024;
/// The sample entry promised by encoded HEVC HLS presentations. The fMP4
/// normalizer already promotes/removes in-band parameter sets for this entry.
pub const VOD_HEVC_SAMPLE_ENTRY: &str = "hvc1";

/// Two seconds of AAC encoder preroll, starting on the film-global AAC
/// sample lattice. Video is trimmed later, at the requested plan boundary.
pub fn vod_audio_anchor(start_seconds: f64) -> u64 {
    let samples = ((start_seconds - 2.0).max(0.0) * f64::from(VOD_AUDIO_RATE)).floor() as u64;
    samples / VOD_AAC_FRAME_SAMPLES * VOD_AAC_FRAME_SAMPLES
}

/// Generation-local video origin on the same AAC preroll lattice used by
/// the ordinary publisher. Source presentation keys remain in their clock.
pub fn vod_reconstructed_video_origin(
    grid: super::VodFrameGrid,
    first_frame: u64,
) -> Result<u64, &'static str> {
    let ticks = u128::from(first_frame)
        .checked_mul(u128::from(grid.denominator))
        .ok_or("video origin overflow")?;
    if grid.numerator == 0 {
        return Err("invalid video clock");
    }
    // Preserve the incumbent publisher's rounding at AAC frame boundaries.
    // Independently rounded integer anchors can differ by one AAC frame.
    let anchor = u128::from(vod_audio_anchor(ticks as f64 / f64::from(grid.numerator)));
    let anchor_ticks = (anchor * u128::from(grid.numerator)).div_ceil(u128::from(VOD_AUDIO_RATE));
    u64::try_from(
        ticks
            .checked_sub(anchor_ticks)
            .ok_or("negative video origin")?,
    )
    .map_err(|_| "video origin overflow")
}

/// Shared AAC has its own clock: each ordinary interval contains 94 complete
/// AAC frames (2.005333 seconds), with only the final packet duration trimmed.
/// Video rung boundaries never duplicate or reset these audio intervals.
pub fn vod_shared_audio_plan(duration_ms: i64, bitrate_kbps: u32) -> SegmentPlan {
    let duration = (duration_ms.max(0) as u64).saturating_mul(u64::from(VOD_AUDIO_RATE)) / 1_000;
    shared_audio_plan_ticks(duration, bitrate_kbps)
}

fn shared_audio_plan_ticks(duration: u64, bitrate_kbps: u32) -> SegmentPlan {
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

    fn duration_ticks(self, duration_ms: i64) -> u64 {
        let frames = (duration_ms.max(0) as u64)
            .saturating_mul(u64::from(self.numerator))
            .div_ceil(1_000 * u64::from(self.denominator));
        frames.saturating_mul(u64::from(self.denominator))
    }

    /// Shared audio covers the last whole video frame. Round across clocks
    /// once, outward to one audio sample, rather than adding an AAC interval.
    pub fn shared_audio_end_ticks(self, duration_ms: i64) -> u64 {
        let samples = (u128::from(self.duration_ticks(duration_ms)) * u128::from(VOD_AUDIO_RATE))
            .div_ceil(u128::from(self.numerator));
        u64::try_from(samples).unwrap_or(u64::MAX)
    }

    /// Round the final frame outward. Every advertised interval then has a
    /// whole number of output frames, including a short final entry.
    pub fn plan(self, duration_ms: i64, bits_per_second: u64) -> SegmentPlan {
        let duration_ticks = self.duration_ticks(duration_ms);
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
    source_object_version: String,
    shared_audio_recipe_id: Option<String>,
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
            source_object_version: source_object_version.to_owned(),
            shared_audio_recipe_id: shared_audio_recipe_id.map(str::to_owned),
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
    pub fn source_object_version(&self) -> &str {
        &self.source_object_version
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

    pub fn media_playlist(&self, plan: &SegmentPlan) -> Result<String, crate::fmp4::Fmp4Error> {
        continuous_media_playlist(
            plan,
            &self.init_id,
            self.grid.numerator,
            self.grid.segment_ticks(),
            u64::from(self.grid.denominator),
            PlanEntryKind::Video,
        )
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

/// A materialized soundtrack whose actual init matches its frozen recipe.
/// Its immutable rendition identity is independent of every video rung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VodSharedAudioRendition {
    rendition_id: String,
    init_id: String,
    recipe_id: String,
    source_object_version: String,
    facts: crate::fmp4::AacSampleEntryFacts,
    bitrate_kbps: u32,
}

impl VodSharedAudioRendition {
    pub fn from_verified_init(
        plan: &ResolvedTranscode,
        recipe: &VodSharedAudioRecipe,
        init: &crate::fmp4::Init,
        rendition_id: &str,
        source_object_version: &str,
    ) -> Result<Self, crate::fmp4::Fmp4Error> {
        use sha2::{Digest, Sha256};
        if rendition_id.len() != 64
            || !rendition_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || source_object_version.is_empty()
            || source_object_version.len() > 512
            || source_object_version
                .bytes()
                .any(|byte| byte.is_ascii_control())
            || VodSharedAudioRecipe::from_plan(plan).as_ref() != Some(recipe)
        {
            return Err(crate::fmp4::Fmp4Error::Unsupported(
                "soundtrack does not match its source-bound recipe".into(),
            ));
        }
        Ok(Self {
            rendition_id: rendition_id.to_owned(),
            init_id: hex::encode(Sha256::digest(&init.bytes)),
            recipe_id: recipe.digest.clone(),
            source_object_version: source_object_version.to_owned(),
            facts: recipe.verify_init(init)?,
            bitrate_kbps: recipe.audio_bitrate_kbps,
        })
    }
    pub fn rendition_id(&self) -> &str {
        &self.rendition_id
    }
    pub fn init_id(&self) -> &str {
        &self.init_id
    }
    pub fn recipe_id(&self) -> &str {
        &self.recipe_id
    }
    pub fn source_object_version(&self) -> &str {
        &self.source_object_version
    }
    pub fn facts(&self) -> &crate::fmp4::AacSampleEntryFacts {
        &self.facts
    }
    pub fn bitrate_kbps(&self) -> u32 {
        self.bitrate_kbps
    }

    pub fn media_playlist(&self, plan: &SegmentPlan) -> Result<String, crate::fmp4::Fmp4Error> {
        continuous_media_playlist(
            plan,
            &self.init_id,
            VOD_AUDIO_RATE,
            94 * VOD_AAC_FRAME_SAMPLES,
            VOD_AAC_FRAME_SAMPLES,
            PlanEntryKind::AudioTail,
        )
    }
}

/// Every role owns its own exact clock; segment ordinals do not pair audio
/// and video. Init maps retain the verified immutable artifact identity.
fn continuous_media_playlist(
    plan: &SegmentPlan,
    init_id: &str,
    timescale: u32,
    span: u64,
    frame_ticks: u64,
    kind: PlanEntryKind,
) -> Result<String, crate::fmp4::Fmp4Error> {
    use std::fmt::Write;
    validate_continuous_media_plan(plan, timescale, span, frame_ticks, kind)?;
    let mut out = String::with_capacity(256 + plan.entries.len() * 48);
    out.push_str("#EXTM3U\n#EXT-X-VERSION:7\n");
    let _ = writeln!(out, "#EXT-X-TARGETDURATION:{}", plan.target_duration);
    out.push_str("#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n");
    let _ = writeln!(out, "#EXT-X-MAP:URI=\"init/{init_id}.mp4\"");
    for entry in &plan.entries {
        let _ = writeln!(
            out,
            "#EXTINF:{:.6},\nsegment/{}.m4s",
            entry.seconds(timescale),
            entry.index
        );
    }
    out.push_str("#EXT-X-ENDLIST\n");
    Ok(out)
}

fn validate_continuous_media_plan(
    plan: &SegmentPlan,
    timescale: u32,
    span: u64,
    frame_ticks: u64,
    kind: PlanEntryKind,
) -> Result<(), crate::fmp4::Fmp4Error> {
    let refuse = || {
        crate::fmp4::Fmp4Error::Unsupported(
            "continuous media plan does not match its verified rendition clock".into(),
        )
    };
    if plan.version != SEGPLAN_VERSION
        || plan.timescale != timescale
        || plan.entries.is_empty()
        || plan.entries.len() > 100_000
        || !(1..=3).contains(&plan.target_duration)
    {
        return Err(refuse());
    }
    for (ordinal, entry) in plan.entries.iter().enumerate() {
        let last = ordinal + 1 == plan.entries.len();
        if entry.index as usize != ordinal
            || entry.kind != kind
            || (ordinal as u64).checked_mul(span) != Some(entry.start_ticks)
            || entry.duration_ticks == 0
            || entry.duration_ticks > span
            || (!last && entry.duration_ticks != span)
            || ((!last || kind == PlanEntryKind::Video)
                && !entry.duration_ticks.is_multiple_of(frame_ticks))
            || entry.duration_ticks.div_ceil(u64::from(timescale)) > u64::from(plan.target_duration)
        {
            return Err(refuse());
        }
    }
    Ok(())
}

/// Server-owned delivery budgets for one immutable rendition, including its
/// container overhead. An average is optional while a JIT film is incomplete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VodRenditionBandwidth {
    pub rendition_id: String,
    pub peak_bps: u64,
    pub average_bps: Option<u64>,
}
impl VodRenditionBandwidth {
    fn valid(&self) -> bool {
        self.peak_bps > 0
            && self
                .average_bps
                .is_none_or(|average| average > 0 && average <= self.peak_bps)
    }
}

/// Pair video with exactly the soundtrack included in its family identity.
/// A silent source has no soundtrack; an audio-bearing family never silently
/// drops audio or substitutes a soundtrack from another source object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VodPresentationFamily {
    id: String,
    video: VodVideoFamily,
    audio: Option<VodSharedAudioRendition>,
}
impl VodPresentationFamily {
    pub fn new(
        video: VodVideoFamily,
        audio: Option<VodSharedAudioRendition>,
    ) -> Result<Self, crate::fmp4::Fmp4Error> {
        if video.rungs.iter().any(|rung| {
            rung.shared_audio_recipe_id.as_deref()
                != audio.as_ref().map(|audio| audio.recipe_id.as_str())
                || audio.as_ref().is_some_and(|audio| {
                    audio.source_object_version != rung.source_object_version
                        || audio.rendition_id == rung.rendition_id
                        || audio.init_id == rung.init_id
                })
        }) {
            return Err(crate::fmp4::Fmp4Error::Unsupported(
                "video family and shared soundtrack do not name the same source and recipe".into(),
            ));
        }
        // Compatibility is a join class, not an attachment authority. Bind
        // the exact advertised set and actual init objects so another set of
        // compatible recipes cannot inherit this family's ledger authority.
        use sha2::{Digest, Sha256};
        let mut identity = Sha256::new();
        identity.update(b"plurx-vod-presentation-family-v1\0");
        identity.update(video.id.as_bytes());
        identity.update([video.rungs.len() as u8]);
        for rung in &video.rungs {
            identity.update(rung.rendition_id.as_bytes());
            identity.update(rung.init_id.as_bytes());
        }
        identity.update([u8::from(audio.is_some())]);
        if let Some(audio) = audio.as_ref() {
            identity.update(audio.rendition_id.as_bytes());
            identity.update(audio.init_id.as_bytes());
            identity.update(audio.recipe_id.as_bytes());
        }
        Ok(Self {
            id: hex::encode(identity.finalize()),
            video,
            audio,
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn video(&self) -> &VodVideoFamily {
        &self.video
    }
    pub fn audio(&self) -> Option<&VodSharedAudioRendition> {
        self.audio.as_ref()
    }

    /// Derive shared AAC dependencies from the parent's immutable plans.
    /// Half-open sample ranges, not matching ordinals or floating seconds,
    /// decide which audio intervals a video interval must retain.
    pub fn shared_audio_dependencies(
        &self,
        rendition_id: &str,
        video_plan: &SegmentPlan,
        video_index: u32,
        audio_plan: Option<&SegmentPlan>,
    ) -> Result<Vec<u32>, crate::fmp4::Fmp4Error> {
        let refuse = || {
            crate::fmp4::Fmp4Error::Unsupported(
                "shared soundtrack does not cover the exact planned video interval".into(),
            )
        };
        let rung = self
            .video
            .rungs
            .iter()
            .find(|rung| rung.rendition_id == rendition_id)
            .ok_or_else(refuse)?;
        validate_continuous_media_plan(
            video_plan,
            rung.grid.numerator,
            rung.grid.segment_ticks(),
            u64::from(rung.grid.denominator),
            PlanEntryKind::Video,
        )?;
        let video = video_plan.entry(video_index).ok_or_else(refuse)?;
        let Some(_) = self.audio.as_ref() else {
            return if audio_plan.is_none() {
                Ok(Vec::new())
            } else {
                Err(refuse())
            };
        };
        let audio_plan = audio_plan.ok_or_else(refuse)?;
        validate_continuous_media_plan(
            audio_plan,
            VOD_AUDIO_RATE,
            94 * VOD_AAC_FRAME_SAMPLES,
            VOD_AAC_FRAME_SAMPLES,
            PlanEntryKind::AudioTail,
        )?;
        let video_scale = u128::from(video_plan.timescale);
        let audio_scale = u128::from(audio_plan.timescale);
        let video_end =
            u128::from(video_plan.entries.last().ok_or_else(refuse)?.end_ticks()) * audio_scale;
        let audio_end =
            u128::from(audio_plan.entries.last().ok_or_else(refuse)?.end_ticks()) * video_scale;
        if audio_end < video_end || audio_end - video_end >= video_scale {
            return Err(refuse());
        }
        let from = u128::from(video.start_ticks) * audio_scale;
        let through = u128::from(video.end_ticks()) * audio_scale;
        let first = audio_plan
            .entries
            .partition_point(|entry| u128::from(entry.end_ticks()) * video_scale <= from);
        let mut dependencies = Vec::with_capacity(3);
        let mut covered = from;
        for entry in &audio_plan.entries[first..] {
            let start = u128::from(entry.start_ticks) * video_scale;
            if start >= through {
                break;
            }
            if start > covered || dependencies.len() == 3 {
                return Err(refuse());
            }
            covered = u128::from(entry.end_ticks()) * video_scale;
            dependencies.push(entry.index);
        }
        if dependencies.is_empty() || covered < through {
            return Err(refuse());
        }
        Ok(dependencies)
    }

    /// Render only verified media identities. The owner must independently
    /// retain admission and a bounded serving path for every advertised URI.
    /// Relative paths keep each rendition under the existing parent capability.
    pub fn master_playlist(
        &self,
        video_budgets: &[VodRenditionBandwidth],
        audio_budget: Option<&VodRenditionBandwidth>,
    ) -> Result<String, crate::fmp4::Fmp4Error> {
        let refuse = || {
            crate::fmp4::Fmp4Error::Unsupported(
                "continuous master has incomplete or conflicting rendition budgets".into(),
            )
        };
        let budgets: std::collections::BTreeMap<_, _> = video_budgets
            .iter()
            .map(|budget| (budget.rendition_id.as_str(), budget))
            .collect();
        if video_budgets.len() != self.video.rungs.len()
            || budgets.len() != video_budgets.len()
            || video_budgets.iter().any(|budget| !budget.valid())
            || self
                .video
                .rungs
                .iter()
                .any(|rung| !budgets.contains_key(rung.rendition_id()))
            || match (&self.audio, audio_budget) {
                (None, None) => false,
                (Some(audio), Some(budget)) => {
                    !budget.valid() || budget.rendition_id != audio.rendition_id
                }
                _ => true,
            }
        {
            return Err(refuse());
        }
        let mut out = String::from("#EXTM3U\n#EXT-X-VERSION:7\n");
        if let Some(audio) = &self.audio {
            out.push_str(&format!(
                "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"shared\",NAME=\"Audio\",DEFAULT=YES,AUTOSELECT=YES,CHANNELS=\"{}\",URI=\"audio/{}/index.m3u8\"\n",
                audio.facts.channels, audio.rendition_id,
            ));
        }
        for rung in &self.video.rungs {
            let budget = budgets[rung.rendition_id()];
            let peak = budget
                .peak_bps
                .checked_add(audio_budget.map_or(0, |audio| audio.peak_bps))
                .ok_or_else(refuse)?;
            out.push_str(&format!("#EXT-X-STREAM-INF:BANDWIDTH={peak}"));
            let average = match (budget.average_bps, audio_budget) {
                (Some(video), None) => Some(video),
                (Some(video), Some(audio)) => audio
                    .average_bps
                    .map(|audio| video.checked_add(audio).ok_or_else(refuse))
                    .transpose()?,
                _ => None,
            };
            if let Some(average) = average {
                out.push_str(&format!(",AVERAGE-BANDWIDTH={average}"));
            }
            let codecs = self.audio.as_ref().map_or_else(
                || rung.facts.codec.clone(),
                |audio| format!("{},{}", rung.facts.codec, audio.facts.codec),
            );
            let rate = f64::from(rung.grid.numerator) / f64::from(rung.grid.denominator);
            out.push_str(&format!(
                ",RESOLUTION={}x{},FRAME-RATE={rate:.3},CODECS=\"{codecs}\"",
                rung.facts.width, rung.facts.height,
            ));
            if self.audio.is_some() {
                out.push_str(",AUDIO=\"shared\"");
            }
            out.push_str(&format!("\nvideo/{}/index.m3u8\n", rung.rendition_id));
        }
        Ok(out)
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

    pub fn verify_init(
        &self,
        init: &crate::fmp4::Init,
    ) -> Result<crate::fmp4::AacSampleEntryFacts, crate::fmp4::Fmp4Error> {
        let facts = crate::fmp4::aac_lc_sample_entry_facts(init)?;
        if u32::from(facts.channels) != self.audio_channels {
            return Err(crate::fmp4::Fmp4Error::Unsupported(
                "shared AAC output channels differ from the frozen recipe".into(),
            ));
        }
        Ok(facts)
    }

    pub fn plan(&self, duration_ms: i64) -> SegmentPlan {
        vod_shared_audio_plan(duration_ms, self.audio_bitrate_kbps)
    }

    /// A presentation's soundtrack shares its film end with the frozen video
    /// cadence while keeping independent, whole-AAC-frame interval boundaries.
    pub fn plan_on_grid(&self, duration_ms: i64, grid: VodFrameGrid) -> SegmentPlan {
        shared_audio_plan_ticks(
            grid.shared_audio_end_ticks(duration_ms),
            self.audio_bitrate_kbps,
        )
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
    vod_pipe_args_with_reorder(source, plan, execution, grid, duration_seconds, false)
}

/// Optional software-H.264 reordered recipe. Hardware and other codecs keep
/// their existing recipe; readiness is advisory at the operator control.
pub fn vod_pipe_args_with_reorder(
    source: &MediaFile,
    plan: &ResolvedTranscode,
    execution: &TranscodeExecution,
    grid: VodFrameGrid,
    duration_seconds: f64,
    reorder: bool,
) -> Vec<String> {
    let media = plan.options();
    let continuous = media.video_sample_envelope == super::VideoSampleEnvelope::ContinuousAvcHigh50;
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
    // semantic offset a second time. That `-af` also carries any measured
    // downmix; the lattice chain below re-adds it ahead of its own stages.
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
    let color = if continuous {
        ",setparams=range=limited:color_primaries=bt709:color_trc=bt709:colorspace=bt709"
    } else {
        ""
    };
    let last = format!(
        ",tpad=stop_mode=clone:stop_duration={remaining:.9},trim=end_frame={frames},setpts=PTS-{audio_anchor:.9}/TB{color}"
    );
    // Use the same explicit even raster for CPU scale, GPU/bitmap scale,
    // identity and HLS facts; -2's independent aspect rounding can differ.
    let raster = if plan.output_contract().normalized_geometry().is_some()
        || plan.macos_processing_identity().is_some()
    {
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
        // The fold runs first, on the decoded preroll, so the limiter's
        // look-ahead and release state are already settled when the sample
        // trim below reaches the generation's first kept sample: adjacent
        // generations then join on identical gain.
        let downmix = media
            .audio
            .as_ref()
            .and_then(|audio| audio.downmix_filter())
            .map(|filter| format!("{filter},"))
            .unwrap_or_default();
        args.extend(["-af".to_owned(), format!("{downmix}{}", audio.filter())]);
        args.extend([
            "-ar".to_owned(),
            VOD_AUDIO_RATE.to_string(),
            "-profile:a".to_owned(),
            "aac_low".to_owned(),
        ]);
    }
    if continuous {
        // The verified continuous family promises limited-range BT.709 SDR.
        // Do not inherit absent container tags from an otherwise valid source:
        // the encoder and MP4 muxer must publish the explicit output record.
        args.extend([
            "-color_primaries".to_owned(),
            "bt709".to_owned(),
            "-color_trc".to_owned(),
            "bt709".to_owned(),
            "-colorspace".to_owned(),
            "bt709".to_owned(),
            "-color_range".to_owned(),
            "tv".to_owned(),
        ]);
    }
    if let Some(index) = args.iter().position(|arg| arg == "-force_key_frames") {
        args[index + 1] = format!("expr:eq(mod(n,{}),0)", grid.frames_per_segment);
    }
    // MP4 defaults HEVC to hev1. Name the same out-of-band-parameter-set
    // contract that HLS advertises; this argument is part of VOD identity.
    if plan.output_contract().output_codec() == "hevc" {
        args.extend(["-tag:v".to_owned(), VOD_HEVC_SAMPLE_ENTRY.to_owned()]);
    }
    let reorder = reorder
        && args
            .windows(2)
            .any(|pair| pair[0] == "-c:v" && pair[1] == "libx264");
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
        // Signed CTOs preserve the presentation grid when reordering is selected.
        "-bf".to_owned(),
        if reorder { "2" } else { "0" }.to_owned(),
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
        if continuous && reorder {
            "+empty_moov+delay_moov+default_base_moof+frag_keyframe+negative_cts_offsets+write_colr"
        } else if continuous {
            // Keep the verified continuous family's explicit color record.
            "+empty_moov+delay_moov+default_base_moof+frag_keyframe+write_colr"
        } else if reorder {
            "+empty_moov+delay_moov+default_base_moof+frag_keyframe+negative_cts_offsets"
        } else {
            "+empty_moov+delay_moov+default_base_moof+frag_keyframe"
        }
        .to_owned(),
        "-video_track_timescale".to_owned(),
        grid.numerator.to_string(),
        "-f".to_owned(),
        "mp4".to_owned(),
        "pipe:1".to_owned(),
    ]);
    args
}

/// Encode a reconstructed RGB48 NUT window once, preserving the ordinary VOD
/// soundtrack lattice and output codec contract. The caller independently
/// verifies one source picture per output-grid frame before publishing bytes.
/// No fps, clone padding or source video decoder may alter that association.
pub fn vod_reconstructed_pipe_args(
    source: &MediaFile,
    plan: &ResolvedTranscode,
    execution: &TranscodeExecution,
    grid: VodFrameGrid,
    duration_seconds: f64,
    source_fd: &str,
    audio_fd: &str,
) -> Result<Vec<String>, &'static str> {
    let output = plan
        .completed_reconstructed_output(plan.encoder())
        .ok_or("unsupported reconstructed output contract")?;
    vod_completed_reconstructed_pipe_args(
        source,
        &output,
        execution,
        grid,
        duration_seconds,
        source_fd,
        audio_fd,
    )
}

pub fn vod_completed_reconstructed_pipe_args(
    source: &MediaFile,
    output: &super::CompletedReconstructedOutputPlan<'_>,
    execution: &TranscodeExecution,
    grid: VodFrameGrid,
    duration_seconds: f64,
    source_fd: &str,
    audio_fd: &str,
) -> Result<Vec<String>, &'static str> {
    let plan = output.source_plan();
    if !matches!(
        plan.encoder(),
        super::Encoder::Software | super::Encoder::Nvenc
    ) || plan.output_contract().output_grade() != super::OutputGrade::Hdr10
        || plan.output_contract().output_codec() != "hevc"
        || plan.options().subtitle_burn.is_some()
        || plan.options().video_sample_envelope == super::VideoSampleEnvelope::ContinuousAvcHigh50
        || grid.frames_per_segment > 64
    {
        return Err("unsupported reconstructed VOD recipe");
    }
    let (width, height) = super::output_size(source, plan.options().target_height)
        .ok_or("reconstructed output raster unavailable")?;
    let mut args =
        vod_pipe_args_with_reorder(source, plan, execution, grid, duration_seconds, false);
    if args.iter().any(|arg| arg == "-filter_complex") {
        return Err("unsupported reconstructed filter graph");
    }
    let input = args
        .iter()
        .position(|arg| arg == "-i")
        .ok_or("reconstructed video input unavailable")?;
    // Replace all ordinary source-decoder options as a scope, rather than
    // leaving a hardware decoder or source seek applied to the RGB pipe.
    args.splice(
        0..input + 2,
        [
            "-hide_banner".into(),
            "-nostdin".into(),
            "-copyts".into(),
            "-filter_threads".into(),
            "1".into(),
            "-threads".into(),
            "1".into(),
            "-i".into(),
            source_fd.into(),
        ],
    );
    if plan.options().input_has_audio {
        let input = args
            .iter()
            .enumerate()
            .filter(|(_, arg)| *arg == "-i")
            .nth(1)
            .map(|(i, _)| i)
            .ok_or("reconstructed audio input unavailable")?;
        args[input + 1] = audio_fd.into();
        args.splice(input..input, ["-threads".into(), "1".into()]);
    }
    let last_input = args
        .iter()
        .rposition(|arg| arg == "-i")
        .ok_or("reconstructed input unavailable")?
        + 2;
    let mut index = last_input;
    while index + 1 < args.len() {
        if matches!(
            args[index].as_str(),
            "-threads"
                | "-threads:v"
                | "-threads:a"
                | "-filter_threads"
                | "-filter_complex_threads"
        ) {
            args.drain(index..index + 2);
        } else {
            index += 1;
        }
    }
    for index in 0..args.len().saturating_sub(1) {
        if args[index] == "-map" && args[index + 1].starts_with("0:") {
            args[index + 1] = "0:v:0".into();
        }
    }
    let target = execution.start_seconds.max(0.0);
    let first_frame =
        (target * f64::from(grid.numerator) / f64::from(grid.denominator)).round() as u64;
    let origin = vod_reconstructed_video_origin(grid, first_frame)?;
    // MOV otherwise subtracts each track's first DTS, erasing the explicit
    // video phase relative to the AAC preroll on nonzero windows.
    if let Some(index) = args.iter().position(|arg| arg == "-movflags") {
        args[index + 1].push_str("+frag_discont");
    }
    let pixel_format = if output.encoder() == super::Encoder::Nvenc {
        "p010le"
    } else {
        "yuv420p10le"
    };
    let filter = format!(
        "zscale=matrixin=gbr:rangein=full:primariesin=bt2020:transferin=smpte2084:matrix=bt2020nc:range=limited:primaries=bt2020:transfer=smpte2084:w={width}:h={height}:dither=none,format={pixel_format},settb=expr=1/{},setpts=N*{}+{origin}",
        grid.numerator, grid.denominator,
    );
    if let Some(index) = args.iter().position(|arg| arg == "-vf") {
        args[index + 1] = filter;
    } else {
        let at = args.len().saturating_sub(1);
        args.splice(at..at, ["-vf".into(), filter]);
    }
    if output.encoder() == super::Encoder::Nvenc {
        let mut index = last_input;
        while index + 1 < args.len() {
            if matches!(
                args[index].as_str(),
                "-c:v"
                    | "-preset"
                    | "-x265-params"
                    | "-profile:v"
                    | "-tier:v"
                    | "-level:v"
                    | "-b:v"
                    | "-maxrate"
                    | "-bufsize"
                    | "-color_primaries"
                    | "-color_trc"
                    | "-colorspace"
                    | "-color_range"
                    | "-forced-idr"
            ) {
                args.drain(index..index + 2);
            } else {
                index += 1;
            }
        }
        let at = args.len().saturating_sub(1);
        args.splice(
            at..at,
            output.encode_args(plan.options().video_bitrate_kbps, true),
        );
    } else if let Some(index) = args.iter().position(|arg| arg == "-x265-params") {
        let mut parameters: Vec<_> = args[index + 1]
            .split(':')
            .filter(|item| {
                !item.starts_with("pools=")
                    && !item.starts_with("frame-threads=")
                    && !item.starts_with("wpp=")
            })
            .map(str::to_owned)
            .collect();
        parameters.extend([
            "pools=none".into(),
            "frame-threads=1".into(),
            "wpp=0".into(),
        ]);
        args[index + 1] = parameters.join(":");
    } else {
        let at = args.len().saturating_sub(1);
        args.splice(
            at..at,
            [
                "-x265-params".into(),
                "pools=none:frame-threads=1:wpp=0".into(),
            ],
        );
    }
    let at = args.len().saturating_sub(1);
    args.splice(at..at, ["-threads:v".into(), "1".into()]);
    if plan.options().input_has_audio {
        let at = args.len().saturating_sub(1);
        args.splice(at..at, ["-threads:a".into(), "1".into()]);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chaptered_source() -> crate::domain::MediaFile {
        encoded_recipe_source(false)
    }

    fn encoded_recipe_source(hdr: bool) -> MediaFile {
        MediaFile {
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
            video_profile: Some(if hdr { "Main 10" } else { "Main" }.into()),
            width: Some(640),
            height: Some(360),
            bit_depth: Some(if hdr { 10 } else { 8 }),
            hdr: hdr.then(|| "hdr10".into()),
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
        }
    }

    fn recipe_plan_and_execution(
        source: &MediaFile,
        options: &crate::transcode::TranscodeOptions,
    ) -> (ResolvedTranscode, TranscodeExecution) {
        let (plan, execution, _, _) = recipe_fixture_parts(source, options);
        (plan, execution)
    }

    /// The resolved plan and execution plus the decode facts and capabilities
    /// they were resolved against, for tests that re-resolve variants.
    fn recipe_fixture_parts(
        source: &MediaFile,
        options: &crate::transcode::TranscodeOptions,
    ) -> (
        ResolvedTranscode,
        TranscodeExecution,
        crate::transcode::DecodeFacts,
        crate::transcode::DecodeCapabilities,
    ) {
        let hdr = options.pipeline.output_grade() == crate::transcode::OutputGrade::Hdr10;
        let facts = crate::transcode::DecodeFacts::from_ffprobe_json(
            &serde_json::json!({"streams":[{
                "index":0,"codec_type":"video","codec_name":"hevc",
                "profile":if hdr {"Main 10"} else {"Main"},"width":640,"height":360,
                "pix_fmt":if hdr {"yuv420p10le"} else {"yuv420p"},"avg_frame_rate":"24/1",
                "color_transfer":if hdr {"smpte2084"} else {"bt709"},
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
                crate::transcode::TranscodeMediaOptions::from_options(source, options),
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
            source,
            options,
            crate::transcode::Pacing::unpaced(),
            ".",
        )
        .expect("execution");
        (plan, execution, facts, capabilities)
    }

    fn encoded_recipe_fixture(
        hdr: bool,
    ) -> (
        MediaFile,
        ResolvedTranscode,
        TranscodeExecution,
        crate::transcode::DecodeFacts,
        crate::transcode::DecodeCapabilities,
    ) {
        let source = encoded_recipe_source(hdr);
        let options = crate::transcode::TranscodeOptions {
            pipeline: if hdr {
                crate::transcode::Pipeline::Hdr10Passthrough
            } else {
                crate::transcode::Pipeline::Cpu
            },
            ..Default::default()
        };
        let (plan, execution, facts, capabilities) = recipe_fixture_parts(&source, &options);
        (source, plan, execution, facts, capabilities)
    }

    fn pipe_args_for(
        source: &MediaFile,
        options: &crate::transcode::TranscodeOptions,
    ) -> Vec<String> {
        let (plan, execution) = recipe_plan_and_execution(source, options);
        vod_pipe_args(
            source,
            &plan,
            &execution,
            VodFrameGrid::new(24, 1).expect("grid"),
            12.0,
        )
    }

    #[test]
    fn reconstructed_window_origin_preserves_aac_preroll_at_ntsc_seek() {
        let grid = VodFrameGrid::new(24_000, 1001).expect("NTSC grid");
        assert_eq!(vod_reconstructed_video_origin(grid, 0), Ok(0));
        assert_eq!(vod_reconstructed_video_origin(grid, 48), Ok(48_048));
        // At the next window, AAC preroll starts at sample 95232. The
        // local video origin retains its sub-frame phase on that lattice.
        let origin = vod_reconstructed_video_origin(grid, 96).expect("seek origin");
        assert_eq!(origin, 48_480);
        let anchor = vod_audio_anchor(96.0 * 1001.0 / 24_000.0);
        assert_eq!(anchor, 95_232);
        assert_eq!(96 * 1001 - origin, anchor / 2);
        // This ordinary NTSC window sits on the floating sample boundary:
        // the publisher's established anchor is one AAC frame below the
        // independent integer calculation. Both actual lanes must agree.
        let anchor = vod_audio_anchor(384.0 * 1001.0 / 24_000.0);
        assert_eq!(anchor, 671_744);
        assert_eq!(vod_reconstructed_video_origin(grid, 384), Ok(48_512));
        assert_eq!(384 * 1001 - 48_512, anchor / 2);
    }

    #[test]
    fn reconstructed_window_encoder_keeps_one_to_one_video_and_audio_scope() {
        let (source, ordinary, execution, facts, capabilities) = encoded_recipe_fixture(true);
        let mut media = ordinary.options().clone();
        media.input_has_audio = true;
        media.audio_index = Some(0);
        media.audio_offset_ms = -175;
        let plan = crate::transcode::resolve_transcode(
            &crate::transcode::TranscodeRequest::new(crate::transcode::Encoder::Software, media),
            &facts,
            &capabilities,
            &crate::transcode::DecodePolicySnapshot::new(
                crate::transcode::DecodePlanPolicy::Legacy,
                None,
            ),
            &crate::transcode::AttemptRestrictions::none(),
        )
        .expect("audio HDR10 recipe");
        let args = vod_reconstructed_pipe_args(
            &source,
            &plan,
            &execution,
            VodFrameGrid::new(24, 1).expect("grid"),
            0.25,
            "/dev/fd/3",
            "/dev/fd/4",
        )
        .expect("reconstructed recipe");
        assert_eq!(
            args.windows(2)
                .filter(|p| p[0] == "-i")
                .map(|p| p[1].as_str())
                .collect::<Vec<_>>(),
            ["/dev/fd/3", "/dev/fd/4"]
        );
        assert!(args.windows(2).any(|p| p == ["-map", "0:v:0"]));
        assert!(args.windows(2).any(|p| p == ["-c:v", "libx265"]));
        assert!(args.windows(2).any(|p| p == ["-threads:a", "1"]));
        let filter = &args[args
            .iter()
            .position(|arg| arg == "-vf")
            .expect("video filter")
            + 1];
        assert!(!filter.contains("fps=") && !filter.contains("tpad=") && !filter.contains("trim="));
        assert!(filter.contains("matrixin=gbr") && filter.contains("format=yuv420p10le"));
        assert!(args
            .windows(2)
            .any(|p| p[0] == "-af" && p[1].contains("atrim=start_sample=")));
        let (source, sdr, execution, _, _) = encoded_recipe_fixture(false);
        assert!(vod_reconstructed_pipe_args(
            &source,
            &sdr,
            &execution,
            VodFrameGrid::new(24, 1).expect("grid"),
            0.25,
            "/dev/fd/3",
            "/dev/fd/4"
        )
        .is_err());
    }

    #[test]
    fn reconstructed_nvenc_keeps_verified_hardware_encoder_and_bounded_surfaces() {
        use crate::transcode::*;
        let (source, plan, execution, _, _) = encoded_recipe_fixture(true);
        let output = plan
            .completed_reconstructed_output(Encoder::Nvenc)
            .expect("bounded reconstructed NVENC output");
        let args = vod_completed_reconstructed_pipe_args(
            &source,
            &output,
            &execution,
            VodFrameGrid::new(24, 1).expect("grid"),
            0.25,
            "/dev/fd/3",
            "/dev/fd/4",
        )
        .expect("reconstructed hardware recipe");
        assert!(args.windows(2).any(|pair| pair == ["-c:v", "hevc_nvenc"]));
        assert!(args.windows(2).any(|pair| pair == ["-surfaces", "4"]));
        assert!(args.windows(2).any(|pair| pair == ["-bf", "0"]));
        assert!(!args
            .iter()
            .any(|argument| argument == "-x265-params" || argument == "-hwaccel"));
        let filter = &args[args
            .iter()
            .position(|argument| argument == "-vf")
            .expect("RGB conversion")
            + 1];
        assert!(
            filter.contains("format=p010le")
                && !filter.contains("fps=")
                && !filter.contains("tpad=")
        );
    }

    #[test]
    fn continuous_avc_signaling_uses_encoder_level_units() {
        use crate::transcode::*;
        let (source, ordinary, execution, facts, capabilities) = encoded_recipe_fixture(false);
        for encoder in [
            Encoder::Software,
            Encoder::Vaapi,
            Encoder::Qsv,
            Encoder::Nvenc,
            Encoder::VideoToolbox,
        ] {
            let plan = resolve_transcode(
                &TranscodeRequest::new(encoder, ordinary.options().clone())
                    .with_continuous_avc_video(),
                &facts,
                &capabilities,
                &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
                &AttemptRestrictions::none(),
            )
            .expect("continuous encoder plan");
            let args = vod_pipe_args(
                &source,
                &plan,
                &execution,
                VodFrameGrid::new(24, 1).expect("valid frame grid"),
                1.0,
            );
            let level = if encoder == Encoder::Software {
                "5.0"
            } else {
                "50"
            };
            assert!(
                args.windows(2).any(|pair| pair == ["-level:v", level]),
                "{encoder:?}: {args:?}"
            );
            assert!(args
                .windows(2)
                .any(|pair| pair == ["-bsf:v", "h264_metadata=zero_new_constraint_set_flags=1"]));
            assert_eq!(
                args.iter().filter(|arg| arg.as_str() == "-bsf:v").count(),
                1
            );
        }
        let ordinary_args = vod_pipe_args(
            &source,
            &ordinary,
            &execution,
            VodFrameGrid::new(24, 1).expect("valid frame grid"),
            1.0,
        );
        assert!(!ordinary_args
            .iter()
            .any(|arg| arg.starts_with("h264_metadata=")));
    }

    #[test]
    fn continuous_avc_emitted_init_matches_family() {
        use crate::fmp4::{FragmentReader, Unit};
        use crate::transcode::*;
        use std::process::Command;
        let temp = tempfile::tempdir().expect("fixture directory");
        let input = temp.path().join("source.mkv");
        let generated = Command::new(crate::testfixtures::ffmpeg())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=green:s=640x360:r=24:d=1",
                "-c:v",
                "libx265",
                "-x265-params",
                "pools=none:frame-threads=1:log-level=error",
            ])
            .arg(&input)
            .output()
            .expect("ffmpeg fixture process");
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        let (source, ordinary, mut execution, facts, capabilities) = encoded_recipe_fixture(false);
        execution.source_path = input;
        let plan = resolve_transcode(
            &TranscodeRequest::new(Encoder::Software, ordinary.options().clone())
                .with_continuous_avc_video(),
            &facts,
            &capabilities,
            &DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, None),
            &AttemptRestrictions::none(),
        )
        .expect("continuous plan");
        let grid = VodFrameGrid::new(24, 1).expect("valid frame grid");
        let output = Command::new(crate::testfixtures::ffmpeg())
            .args(vod_pipe_args(&source, &plan, &execution, grid, 1.0))
            .output()
            .expect("production encoder process");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut reader = FragmentReader::new();
        reader.push(&output.stdout);
        let Some(Unit::Init(init)) = reader.next_unit().expect("parse emitted init") else {
            panic!("production output must begin with init");
        };
        let rung = VodVideoRung::from_verified_init(
            &source,
            &plan,
            &init,
            grid,
            &"a".repeat(64),
            "source-v1",
            None,
        )
        .expect("actual encoder output satisfies the unchanged family verifier");
        assert_eq!(rung.facts.codec, "avc1.640032");
    }

    #[test]
    fn encoded_vod_hevc_sample_entry_matches_the_hls_parameter_set_contract() {
        for hdr in [false, true] {
            let (source, plan, execution, _, _) = encoded_recipe_fixture(hdr);
            let args = vod_pipe_args(
                &source,
                &plan,
                &execution,
                VodFrameGrid::new(24, 1).expect("grid"),
                12.0,
            );
            assert_eq!(args.windows(2).any(|pair| pair == ["-tag:v", "hvc1"]), hdr);
            assert!(!args.windows(2).any(|pair| pair == ["-tag:v", "hev1"]));
        }
    }

    #[test]
    fn encoded_vod_recipe_explicitly_excludes_source_chapters() {
        let (source, plan, execution, facts, capabilities) = encoded_recipe_fixture(false);
        let options = crate::transcode::TranscodeOptions {
            pipeline: crate::transcode::Pipeline::Cpu,
            ..Default::default()
        };
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
        let mut continuous_options = options.clone();
        continuous_options.video_sample_envelope =
            super::super::VideoSampleEnvelope::ContinuousAvcHigh50;
        let routed_plan = crate::transcode::resolve_transcode(
            &crate::transcode::TranscodeRequest::new(
                crate::transcode::Encoder::Software,
                crate::transcode::TranscodeMediaOptions::from_options(&source, &continuous_options),
            ),
            &facts,
            &capabilities,
            &crate::transcode::DecodePolicySnapshot::new(
                crate::transcode::DecodePlanPolicy::Legacy,
                None,
            ),
            &crate::transcode::AttemptRestrictions::none(),
        )
        .expect("normal option route resolves the same continuous recipe");
        assert_eq!(routed_plan.plan_digest(), continuous_plan.plan_digest());
        let mut audio_source = source.clone();
        audio_source.audio_streams.push(crate::domain::AudioStream {
            index: 0,
            codec: "aac".into(),
            channels: Some(2),
            channel_layout: Some("stereo".into()),
            sample_rate: Some(48_000),
            language: None,
            title: None,
            default: true,
        });
        assert!(
            crate::transcode::TranscodeMediaOptions::from_options(&audio_source, &options)
                .input_has_audio
        );
        assert!(
            !crate::transcode::TranscodeMediaOptions::from_options(
                &audio_source,
                &continuous_options
            )
            .input_has_audio
        );
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
        for (option, value) in [
            ("-color_primaries", "bt709"),
            ("-color_trc", "bt709"),
            ("-colorspace", "bt709"),
            ("-color_range", "tv"),
        ] {
            assert!(continuous_args
                .windows(2)
                .any(|pair| pair == [option, value]));
            assert!(!args.iter().any(|argument| argument == option));
        }
        assert!(continuous_args.windows(2).any(|pair| pair[0] == "-vf"
            && pair[1].ends_with(
                "setparams=range=limited:color_primaries=bt709:color_trc=bt709:colorspace=bt709"
            )));
        assert!(continuous_args
            .windows(2)
            .any(|pair| pair[0] == "-movflags" && pair[1].ends_with("+write_colr")));
        assert!(!args
            .iter()
            .any(|argument| argument.contains("setparams=") || argument.contains("write_colr")));
        assert_ne!(continuous_plan.plan_digest(), plan.plan_digest());
        assert_eq!(
            continuous_plan.output_contract().effective_width(),
            Some(640)
        );
        assert_eq!(
            continuous_plan.output_contract().effective_height(),
            Some(360)
        );
        let reordered = vod_pipe_args_with_reorder(
            &source,
            &plan,
            &execution,
            VodFrameGrid::new(24, 1).expect("grid"),
            12.0,
            true,
        );
        assert!(args.windows(2).any(|pair| pair == ["-bf", "0"]));
        assert!(reordered.windows(2).any(|pair| pair == ["-bf", "2"]));
        assert!(reordered
            .iter()
            .any(|arg| arg.contains("+negative_cts_offsets")));
        assert!(reordered
            .windows(2)
            .any(|pair| pair == ["-use_editlist", "0"]));
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

    /// The fold runs on the decoded preroll, ahead of the lattice trim, so
    /// the limiter has settled before the first kept sample and adjacent
    /// generations join on identical gain.
    #[test]
    fn encoded_vod_folds_ahead_of_the_sample_lattice() {
        use crate::playback::audio::{resolve_audio, AudioRoute, DownmixMatrix};
        let mut source = chaptered_source();
        source.audio_streams = vec![crate::domain::AudioStream {
            index: 1,
            codec: "truehd".into(),
            channels: Some(8),
            channel_layout: Some("7.1".into()),
            sample_rate: Some(48_000),
            ..Default::default()
        }];
        let mut options = crate::transcode::TranscodeOptions::default();
        let audio = resolve_audio(
            source.audio_streams.first(),
            crate::playback::default_profile(),
            AudioRoute::EncodedVod,
            0,
        );
        assert_eq!(audio.downmix, Some(DownmixMatrix::LoRo71));
        options.set_audio_delivery(audio);
        let args = pipe_args_for(&source, &options);
        let filters: Vec<_> = args
            .windows(2)
            .filter(|pair| pair[0] == "-af")
            .map(|pair| pair[1].clone())
            .collect();
        assert_eq!(filters.len(), 1, "{filters:?}");
        let fold = DownmixMatrix::LoRo71.filter().expect("measured fold");
        assert!(
            filters[0].starts_with(&format!("{fold},asetpts=PTS-")),
            "{}",
            filters[0]
        );
        assert!(filters[0].ends_with(",apad"), "{}", filters[0]);
    }

    #[test]
    fn continuous_avc_limits_use_exact_rational_macroblock_rate() {
        let accepts = super::super::decode::continuous_avc_envelope_accepts;
        assert!(accepts(1920, 1080, 60_000, 1001, 12_000));
        assert!(accepts(2560, 1440, 30_000, 1001, 12_000));
        assert!(!accepts(3840, 2160, 24_000, 1001, 40_000));
        assert!(!accepts(3840, 2160, 30, 1, 40_000));
        assert!(!accepts(4096, 2304, 24, 1, 40_000));
        assert!(!accepts(1921, 1080, 24, 1, 12_000));
        assert!(!accepts(1920, 1080, 24, 0, 12_000));
        assert!(accepts(640, 360, 24, 1, 84_375));
        assert!(!accepts(640, 360, 24, 1, 84_376));
    }

    #[test]
    fn family_membership_varies_raster_but_never_audio_or_timeline_identity() {
        let rung = |id: &str, width, height| VodVideoRung {
            rendition_id: id.repeat(64),
            init_id: "c".repeat(64),
            compatibility_id: "d".repeat(64),
            source_object_version: "source-one".into(),
            shared_audio_recipe_id: None,
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
        assert!(VodVideoFamily::new(vec![low.clone(), same_shape]).is_err());
        assert!(VodPresentationFamily::new(family.clone(), None).is_ok());
        let soundtrack = VodSharedAudioRendition {
            rendition_id: "f".repeat(64),
            init_id: "e".repeat(64),
            recipe_id: "c".repeat(64),
            source_object_version: "source-one".into(),
            facts: crate::fmp4::AacSampleEntryFacts {
                codec: "mp4a.40.2",
                channels: 2,
                sample_rate: 48_000,
                samples_per_frame: 1_024,
            },
            bitrate_kbps: 160,
        };
        assert!(VodPresentationFamily::new(family.clone(), Some(soundtrack.clone())).is_err());
        let mut voiced = family;
        for rung in &mut voiced.rungs {
            rung.shared_audio_recipe_id = Some(soundtrack.recipe_id.clone());
        }
        assert!(VodPresentationFamily::new(voiced.clone(), None).is_err());
        assert!(VodPresentationFamily::new(voiced.clone(), Some(soundtrack.clone())).is_ok());
        let paired = VodPresentationFamily::new(voiced.clone(), Some(soundtrack.clone()))
            .expect("paired soundtrack");
        let reordered = VodVideoFamily::new(voiced.rungs.iter().rev().cloned().collect())
            .expect("same sorted membership");
        assert_eq!(
            paired.id(),
            VodPresentationFamily::new(reordered, Some(soundtrack.clone()))
                .expect("same family")
                .id()
        );
        assert_ne!(
            paired.id(),
            paired.video().id(),
            "join class is not family authority"
        );
        let mut changed_membership = voiced.clone();
        changed_membership.rungs[0].rendition_id = "0".repeat(64);
        assert_ne!(
            paired.id(),
            VodPresentationFamily::new(changed_membership, Some(soundtrack.clone()))
                .expect("different member in the same join class")
                .id()
        );
        let mut changed_init = voiced.clone();
        changed_init.rungs[0].init_id = "0".repeat(64);
        assert_ne!(
            paired.id(),
            VodPresentationFamily::new(changed_init, Some(soundtrack.clone()))
                .expect("different verified init")
                .id()
        );
        let mut changed_audio = soundtrack.clone();
        changed_audio.rendition_id = "0".repeat(64);
        assert_ne!(
            paired.id(),
            VodPresentationFamily::new(voiced.clone(), Some(changed_audio))
                .expect("different actual soundtrack")
                .id()
        );
        let video_plan = low.grid.plan(4_300, 4_000_000);
        let audio_plan = shared_audio_plan_ticks(low.grid.shared_audio_end_ticks(4_300), 160);
        assert_eq!(
            paired
                .shared_audio_dependencies(low.rendition_id(), &video_plan, 0, Some(&audio_plan))
                .expect("first interval"),
            vec![0]
        );
        assert_eq!(
            paired
                .shared_audio_dependencies(low.rendition_id(), &video_plan, 1, Some(&audio_plan))
                .expect("drifting boundary"),
            vec![0, 1]
        );
        assert_eq!(
            paired
                .shared_audio_dependencies(low.rendition_id(), &video_plan, 2, Some(&audio_plan))
                .expect("final interval"),
            vec![1, 2]
        );
        assert!(paired
            .shared_audio_dependencies(low.rendition_id(), &video_plan, 3, Some(&audio_plan))
            .is_err());
        assert!(paired
            .shared_audio_dependencies(&"0".repeat(64), &video_plan, 0, Some(&audio_plan))
            .is_err());
        assert!(paired
            .shared_audio_dependencies(low.rendition_id(), &video_plan, 0, None)
            .is_err());
        assert!(
            paired
                .shared_audio_dependencies(
                    low.rendition_id(),
                    &video_plan,
                    0,
                    Some(&vod_shared_audio_plan(4_300, 160))
                )
                .is_err(),
            "short source-duration AAC cannot cover whole-frame video"
        );
        let mut broken_audio = audio_plan.clone();
        broken_audio.entries[1].start_ticks += 1;
        assert!(paired
            .shared_audio_dependencies(low.rendition_id(), &video_plan, 1, Some(&broken_audio))
            .is_err());
        broken_audio = audio_plan.clone();
        broken_audio
            .entries
            .last_mut()
            .expect("tail")
            .duration_ticks += 1;
        assert!(
            paired
                .shared_audio_dependencies(low.rendition_id(), &video_plan, 2, Some(&broken_audio))
                .is_err(),
            "an extra sample cannot silently extend the common film end"
        );
        let budgets = vec![
            VodRenditionBandwidth {
                rendition_id: low.rendition_id.clone(),
                peak_bps: 4_000_000,
                average_bps: Some(3_000_000),
            },
            VodRenditionBandwidth {
                rendition_id: high.rendition_id.clone(),
                peak_bps: 8_000_000,
                average_bps: Some(6_000_000),
            },
        ];
        let audio_budget = VodRenditionBandwidth {
            rendition_id: soundtrack.rendition_id.clone(),
            peak_bps: 192_000,
            average_bps: Some(160_000),
        };
        let master = paired
            .master_playlist(&budgets, Some(&audio_budget))
            .expect("master");
        assert_eq!(master.matches("#EXT-X-MEDIA:TYPE=AUDIO").count(), 1);
        assert_eq!(master.matches("AUDIO=\"shared\"").count(), 2);
        assert!(master.contains("BANDWIDTH=4192000,AVERAGE-BANDWIDTH=3160000"));
        assert!(master.contains("BANDWIDTH=8192000,AVERAGE-BANDWIDTH=6160000"));
        assert!(master.contains("FRAME-RATE=23.976,CODECS=\"avc1.640032,mp4a.40.2\""));
        assert!(master.contains("CHANNELS=\"2\""));
        assert!(
            master.find("1280x720").expect("low variant")
                < master.find("1920x1080").expect("high variant")
        );
        assert!(
            !master.contains("#EXT-X-INDEPENDENT-SEGMENTS"),
            "init verification alone cannot attest all fragment joins"
        );
        assert!(paired.master_playlist(&budgets, None).is_err());
        assert!(paired
            .master_playlist(&budgets[..1], Some(&audio_budget))
            .is_err());
        assert!(paired
            .master_playlist(
                &[budgets[0].clone(), budgets[0].clone()],
                Some(&audio_budget)
            )
            .is_err());
        let mut invalid = budgets.clone();
        invalid[0].average_bps = Some(invalid[0].peak_bps + 1);
        assert!(paired
            .master_playlist(&invalid, Some(&audio_budget))
            .is_err());
        invalid[0].average_bps = None;
        invalid[0].peak_bps = u64::MAX;
        assert!(paired
            .master_playlist(&invalid, Some(&audio_budget))
            .is_err());
        let mut incomplete = audio_budget.clone();
        incomplete.average_bps = None;
        assert!(!paired
            .master_playlist(&budgets, Some(&incomplete))
            .expect("incomplete film")
            .contains("AVERAGE-BANDWIDTH"));
        incomplete.rendition_id = "a".repeat(64);
        assert!(paired.master_playlist(&budgets, Some(&incomplete)).is_err());
        let silent = VodPresentationFamily::new(
            VodVideoFamily::new(vec![low, high]).expect("silent video"),
            None,
        )
        .expect("silent family");
        let silent_master = silent
            .master_playlist(&budgets, None)
            .expect("silent master");
        assert_eq!(
            silent
                .shared_audio_dependencies(
                    silent.video.rungs[0].rendition_id(),
                    &video_plan,
                    0,
                    None
                )
                .expect("silent interval"),
            Vec::<u32>::new()
        );
        assert!(silent
            .shared_audio_dependencies(
                silent.video.rungs[0].rendition_id(),
                &video_plan,
                0,
                Some(&audio_plan)
            )
            .is_err());
        assert!(!silent_master.contains("TYPE=AUDIO"));
        assert!(!silent_master.contains("mp4a"));
        assert!(silent_master.contains("BANDWIDTH=4000000,AVERAGE-BANDWIDTH=3000000"));
        assert!(silent
            .master_playlist(&budgets, Some(&audio_budget))
            .is_err());

        let video = &silent.video.rungs[0];
        let video_plan = video.grid.plan(4_300, 4_000_000);
        let audio_plan = vod_shared_audio_plan(4_300, 160);
        let video_playlist = video.media_playlist(&video_plan).expect("video clock");
        let audio_playlist = soundtrack.media_playlist(&audio_plan).expect("AAC clock");
        assert!(video_playlist.contains(&format!("init/{}.mp4", video.init_id)));
        assert!(audio_playlist.contains(&format!("init/{}.mp4", soundtrack.init_id)));
        assert!(video_playlist.contains("#EXTINF:2.002000,\nsegment/0.m4s"));
        assert!(audio_playlist.contains("#EXTINF:2.005333,\nsegment/0.m4s"));
        assert!(video_playlist.ends_with("#EXT-X-ENDLIST\n"));
        assert!(audio_playlist.ends_with("#EXT-X-ENDLIST\n"));
        assert!(video.media_playlist(&audio_plan).is_err());
        assert!(soundtrack.media_playlist(&video_plan).is_err());
        let mut broken_plan = video_plan.clone();
        broken_plan.entries[1].start_ticks += 1;
        assert!(video.media_playlist(&broken_plan).is_err());
        broken_plan = video_plan.clone();
        broken_plan.entries[0].duration_ticks -= u64::from(video.grid.denominator);
        assert!(video.media_playlist(&broken_plan).is_err());
        broken_plan = video_plan.clone();
        broken_plan.target_duration = 2;
        assert!(video.media_playlist(&broken_plan).is_err());
        broken_plan = video_plan;
        broken_plan.entries.last_mut().expect("tail").duration_ticks -= 1;
        assert!(video.media_playlist(&broken_plan).is_err());

        let mut aliased_audio = soundtrack.clone();
        aliased_audio.rendition_id = voiced.rungs[0].rendition_id.clone();
        assert!(VodPresentationFamily::new(voiced.clone(), Some(aliased_audio)).is_err());
        let mut aliased_init = soundtrack.clone();
        aliased_init.init_id = voiced.rungs[0].init_id.clone();
        assert!(VodPresentationFamily::new(voiced.clone(), Some(aliased_init)).is_err());
        let mut wrong_source = soundtrack.clone();
        wrong_source.source_object_version = "source-two".into();
        assert!(VodPresentationFamily::new(voiced.clone(), Some(wrong_source)).is_err());
        let mut wrong_recipe = soundtrack;
        wrong_recipe.recipe_id = "a".repeat(64);
        assert!(VodPresentationFamily::new(voiced, Some(wrong_recipe)).is_err());
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
    fn shared_audio_tail_covers_the_final_whole_video_frame() {
        for (numerator, denominator) in [(24_000, 1_001), (30_000, 1_001), (24, 1), (25, 1)] {
            let grid = VodFrameGrid::new(numerator, denominator).expect("cadence");
            for duration_ms in [1, 1_999, 2_000, 4_300, 96_000] {
                let video = grid.plan(duration_ms, 1_000_000);
                let last_video = video.entries.last().expect("video tail");
                let video_end = last_video.start_ticks + last_video.duration_ticks;
                let audio_end = grid.shared_audio_end_ticks(duration_ms);
                let audio = shared_audio_plan_ticks(audio_end, 160);
                let last_audio = audio.entries.last().expect("AAC tail");
                assert_eq!(
                    last_audio.start_ticks + last_audio.duration_ticks,
                    audio_end
                );
                let coverage = u128::from(audio_end) * u128::from(video.timescale);
                let required = u128::from(video_end) * u128::from(VOD_AUDIO_RATE);
                assert!(coverage >= required, "AAC must cover the final video frame");
                assert!(
                    coverage - required < u128::from(video.timescale),
                    "at most one audio sample of rounding"
                );
                for entry in audio.entries.iter().take(audio.entries.len() - 1) {
                    assert_eq!(entry.duration_ticks, 94 * VOD_AAC_FRAME_SAMPLES);
                    assert_eq!(entry.start_ticks % VOD_AAC_FRAME_SAMPLES, 0);
                }
            }
            assert_eq!(grid.shared_audio_end_ticks(0), 0);
            assert_eq!(grid.shared_audio_end_ticks(-1), 0);
        }
        let grid = VodFrameGrid::new(24_000, 1_001).expect("NTSC cadence");
        assert_eq!(grid.shared_audio_end_ticks(4_300), 208_208);
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
