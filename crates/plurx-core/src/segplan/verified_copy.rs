//! Pure paired-output copy kernel, after common init/sample normalization.
//! Observations are not source/grant authority or proof of whole-title coverage.
//! Canonical rows select video boundaries; real merged presentation spans
//! (including audio, composition offsets and absorbed tfdt gaps) bound EXTINF.

use super::{
    companion::{CompanionError, CopyCompanionVerifier},
    copy_cursor::{CopyPlanCursor, CopyPlanError},
    packet_clock::SourceFragmentClock,
    IndexRow, PlanEntry, TrackDurations,
};
use crate::fmp4::{self, CutPolicy, Fragment, Init, Segment};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifiedCopyError {
    InvalidLimit,
    InvalidFragment,
    Companion(CompanionError),
    ExpectedRowMismatch,
    PacketSequence,
    InputLimit,
    TargetExceeded,
    Overflow,
    Merge,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifiedCopyEntry {
    Video(PlanEntry),
    /// No video row or invented canonical video duration is attached to a tail.
    AudioTail {
        output_index: u32,
    },
}

/// Canonical video plan metadata is separate from the actual AV presentation.
/// `media` preserves the AV output's original first tfdt; no rebase occurs.
pub struct VerifiedCopySegment {
    pub output_index: u32,
    pub entry: VerifiedCopyEntry,
    pub rows: Vec<IndexRow>,
    pub media: Segment,
    pub presentation: fmp4::PresentationDuration,
    pub seconds: f64,
}
impl VerifiedCopySegment {
    pub fn video_entry(&self) -> Option<&PlanEntry> {
        match &self.entry {
            VerifiedCopyEntry::Video(entry) => Some(entry),
            _ => None,
        }
    }
}

/// Retains one unfinished video segment and terminal audio flushes. Pending
/// input AV bytes and cumulative tail-split byte clones have separate explicit
/// bounds. Returned output allocation remains the caller's responsibility.
pub struct VerifiedCopyKernel {
    verifier: CopyCompanionVerifier,
    cursor: Option<CopyPlanCursor>,
    av_init: Init,
    policy: CutPolicy,
    timescale: u32,
    retain_parameter_sets: bool,
    max_pending_av_bytes: usize,
    max_split_clone_bytes: usize,
    max_video_ticks: u64,
    pending_av: Vec<Fragment>,
    pending_rows: Vec<IndexRow>,
    pending_bytes: usize,
    pending_ticks: u64,
    next_packet_sequence: Option<u64>,
    next_output_index: u32,
    terminal_audio: bool,
    failed: bool,
}

impl VerifiedCopyKernel {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        canonical_init: Init,
        av_init: Init,
        policy: CutPolicy,
        tracks: TrackDurations,
        retain_parameter_sets: bool,
        max_pending_av_bytes: usize,
        max_split_clone_bytes: usize,
    ) -> Result<Self, VerifiedCopyError> {
        if max_pending_av_bytes == 0 || max_split_clone_bytes == 0 {
            return Err(VerifiedCopyError::InvalidLimit);
        }
        let timescale = canonical_init
            .video()
            .map(|track| track.timescale)
            .filter(|scale| *scale != 0)
            .ok_or(VerifiedCopyError::InvalidFragment)?;
        let max_video_ticks = u64::from(timescale)
            .checked_mul(16)
            .ok_or(VerifiedCopyError::Overflow)?;
        let verifier = CopyCompanionVerifier::new(canonical_init, av_init.clone())
            .map_err(VerifiedCopyError::Companion)?;
        Ok(Self {
            verifier,
            cursor: Some(CopyPlanCursor::new(timescale, policy, tracks)),
            av_init,
            policy,
            timescale,
            retain_parameter_sets,
            max_pending_av_bytes,
            max_split_clone_bytes,
            max_video_ticks,
            pending_av: Vec::new(),
            pending_rows: Vec::new(),
            pending_bytes: 0,
            pending_ticks: 0,
            next_packet_sequence: None,
            next_output_index: 0,
            terminal_audio: false,
            failed: false,
        })
    }

    /// A cut returns only preceding verified media, excluding this trigger pair.
    /// Failures latch without retracting an already returned prefix.
    pub fn push(
        &mut self,
        canonical: &Fragment,
        clock: SourceFragmentClock,
        av: Fragment,
        expected: Option<&IndexRow>,
    ) -> Result<Vec<VerifiedCopySegment>, VerifiedCopyError> {
        if self.failed {
            return Err(VerifiedCopyError::Failed);
        }
        let result = self.push_inner(canonical, clock, av, expected);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn push_inner(
        &mut self,
        canonical: &Fragment,
        clock: SourceFragmentClock,
        av: Fragment,
        expected: Option<&IndexRow>,
    ) -> Result<Vec<VerifiedCopySegment>, VerifiedCopyError> {
        if self.terminal_audio || av.is_empty() || clock.duration == 0 {
            return Err(VerifiedCopyError::InvalidFragment);
        }
        if av.len() > self.max_pending_av_bytes {
            return Err(VerifiedCopyError::InputLimit);
        }
        let video = self
            .av_init
            .video()
            .ok_or(VerifiedCopyError::InvalidFragment)?;
        let samples = av
            .track(video.id)
            .ok_or(VerifiedCopyError::InvalidFragment)?
            .sample_count();
        let samples = u64::try_from(samples).map_err(|_| VerifiedCopyError::Overflow)?;
        if samples == 0 || clock.next_packet_sequence.checked_sub(samples).is_none() {
            return Err(VerifiedCopyError::PacketSequence);
        }
        if let Some(previous) = self.next_packet_sequence {
            let next = previous
                .checked_add(samples)
                .ok_or(VerifiedCopyError::Overflow)?;
            if next != clock.next_packet_sequence {
                return Err(VerifiedCopyError::PacketSequence);
            }
        }
        let row = self
            .verifier
            .verify(canonical, clock, &av)
            .map_err(VerifiedCopyError::Companion)?;
        if expected.is_some_and(|expected| *expected != row) {
            return Err(VerifiedCopyError::ExpectedRowMismatch);
        }
        if row.duration > self.max_video_ticks {
            return Err(VerifiedCopyError::TargetExceeded);
        }
        let completed = self
            .cursor
            .as_mut()
            .ok_or(VerifiedCopyError::Failed)?
            .push(&row)
            .map_err(map_plan_error)?;
        let (bytes, ticks) = if completed.is_some() {
            (0, 0)
        } else {
            (self.pending_bytes, self.pending_ticks)
        };
        let next_bytes = bytes
            .checked_add(av.len())
            .ok_or(VerifiedCopyError::Overflow)?;
        let next_ticks = ticks
            .checked_add(row.duration)
            .ok_or(VerifiedCopyError::Overflow)?;
        if next_bytes > self.max_pending_av_bytes {
            return Err(VerifiedCopyError::InputLimit);
        }
        if next_ticks > self.max_video_ticks {
            return Err(VerifiedCopyError::TargetExceeded);
        }
        // Ordinary cuts keep the canonical boundary. Long audio cannot be
        // inserted before future video: reject an oversized actual merged span.
        let output = if let Some(entry) = completed {
            self.complete_video(entry)?
        } else {
            Vec::new()
        };
        self.pending_av.push(av);
        self.pending_rows.push(row);
        self.pending_bytes = next_bytes;
        self.pending_ticks = next_ticks;
        self.next_packet_sequence = Some(clock.next_packet_sequence);
        Ok(output)
    }

    fn merge_output(
        &mut self,
        fragments: &[Fragment],
        entry: Option<PlanEntry>,
        rows: Vec<IndexRow>,
    ) -> Result<VerifiedCopySegment, VerifiedCopyError> {
        let output_index = self.next_output_index;
        let sequence = output_index
            .checked_add(1)
            .ok_or(VerifiedCopyError::Overflow)?;
        let (media, presentation) = fmp4::merge_retaining_measured(
            fragments,
            &self.av_init,
            sequence,
            self.retain_parameter_sets,
        )
        .map_err(|error| match error {
            fmp4::MeasuredMergeError::Presentation(fmp4::PresentationError::Overflow) => {
                VerifiedCopyError::Overflow
            }
            _ => VerifiedCopyError::Merge,
        })?;
        // Compare rationals before serializing a floating-point EXTINF.
        if !presentation.within_seconds(16) {
            return Err(VerifiedCopyError::TargetExceeded);
        }
        let entry = if let Some(entry) = entry {
            if media.video_ticks != entry.duration_ticks {
                return Err(VerifiedCopyError::Merge);
            }
            VerifiedCopyEntry::Video(entry)
        } else {
            if media.video_ticks != 0 || !rows.is_empty() {
                return Err(VerifiedCopyError::Merge);
            }
            VerifiedCopyEntry::AudioTail { output_index }
        };
        // EXTINF advances the video clock at ordinary canonical cuts. An
        // audio lead or B-frame overlap must not add time on every reload.
        // End-of-range outputs follow the longest actual repaired track.
        let seconds = match &entry {
            VerifiedCopyEntry::Video(plan) if plan.cut != super::PlanCut::EndOfStream => plan
                .seconds(
                    self.av_init
                        .video()
                        .ok_or(VerifiedCopyError::InvalidFragment)?
                        .timescale,
                ),
            _ => presentation.longest_track_seconds(),
        };
        self.next_output_index = sequence;
        Ok(VerifiedCopySegment {
            output_index,
            entry,
            rows,
            media,
            presentation,
            seconds,
        })
    }

    fn complete_video(
        &mut self,
        entry: PlanEntry,
    ) -> Result<Vec<VerifiedCopySegment>, VerifiedCopyError> {
        let fragments = std::mem::take(&mut self.pending_av);
        let rows = std::mem::take(&mut self.pending_rows);
        let output = self.merge_output(&fragments, Some(entry), rows)?;
        self.pending_bytes = 0;
        self.pending_ticks = 0;
        Ok(vec![output])
    }

    /// Accept only terminal audio-only AV flushes after verified video. Any
    /// later video pair is refused; this is not an interleaved-audio API.
    pub fn push_audio_only(&mut self, av: Fragment) -> Result<(), VerifiedCopyError> {
        if self.failed {
            return Err(VerifiedCopyError::Failed);
        }
        let result = self.push_audio_inner(av);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn push_audio_inner(&mut self, av: Fragment) -> Result<(), VerifiedCopyError> {
        let video = self
            .av_init
            .video()
            .ok_or(VerifiedCopyError::InvalidFragment)?;
        if self.next_packet_sequence.is_none()
            || av.is_empty()
            || av.tracks.is_empty()
            || av.track(video.id).is_some()
            || av.tracks.iter().any(|track| {
                self.av_init
                    .track(track.track_id)
                    .is_none_or(|known| known.kind != fmp4::TrackKind::Audio)
                    || track.sample_count() == 0
                    || track.samples().any(|sample| sample.duration == 0)
            })
        {
            return Err(VerifiedCopyError::InvalidFragment);
        }
        let bytes = self
            .pending_bytes
            .checked_add(av.len())
            .ok_or(VerifiedCopyError::Overflow)?;
        if bytes > self.max_pending_av_bytes {
            return Err(VerifiedCopyError::InputLimit);
        }
        self.pending_av.push(av);
        self.pending_bytes = bytes;
        self.terminal_audio = true;
        Ok(())
    }

    /// Close only observed terminal media, not physical EOF or whole-title
    /// coverage. Reuse Segmenter's sample-boundary splitter, then check the
    /// actual merged PTS span of every output against the unchanged 16s target.
    pub fn finish(mut self) -> Result<Vec<VerifiedCopySegment>, VerifiedCopyError> {
        if self.failed {
            return Err(VerifiedCopyError::Failed);
        }
        let entry = self
            .cursor
            .take()
            .ok_or(VerifiedCopyError::Failed)?
            .finish()
            .map_err(map_plan_error)?;
        if self.pending_av.is_empty() {
            return Ok(Vec::new());
        }
        let policy = CutPolicy {
            max_ticks: self.policy.max_ticks.min(self.max_video_ticks),
            strict_max_ticks: Some(self.max_video_ticks),
            ..self.policy
        };
        let chunks = fmp4::split_end_of_range(
            &self.pending_av,
            &self.av_init,
            policy,
            self.timescale,
            entry.is_some(),
            Some(self.max_split_clone_bytes),
        )
        .map_err(|error| match error {
            fmp4::RangeSplitError::MemoryLimit => VerifiedCopyError::InputLimit,
            fmp4::RangeSplitError::Overflow => VerifiedCopyError::Overflow,
            fmp4::RangeSplitError::Media(fmp4::Fmp4Error::Unsupported(_)) => {
                VerifiedCopyError::TargetExceeded
            }
            fmp4::RangeSplitError::Media(_) => VerifiedCopyError::Merge,
        })?;
        let mut entry = entry;
        let mut output = Vec::new();
        for fragments in chunks {
            let video_ticks = fragments
                .iter()
                .map(|fragment| fragment.video_duration(&self.av_init))
                .sum::<u64>();
            let (video_entry, rows) = if video_ticks > 0 {
                (
                    Some(entry.take().ok_or(VerifiedCopyError::Merge)?),
                    std::mem::take(&mut self.pending_rows),
                )
            } else {
                (None, Vec::new())
            };
            output.push(self.merge_output(&fragments, video_entry, rows)?);
        }
        if entry.is_some() || !self.pending_rows.is_empty() {
            return Err(VerifiedCopyError::Merge);
        }
        Ok(output)
    }
}

fn map_plan_error(error: CopyPlanError) -> VerifiedCopyError {
    match error {
        CopyPlanError::Overflow => VerifiedCopyError::Overflow,
        CopyPlanError::Failed => VerifiedCopyError::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fmp4::{FragmentReader, Unit};

    fn fixture() -> (Init, Fragment) {
        let mut reader = FragmentReader::new();
        reader.push(include_bytes!(
            "../../tests/fixtures/dv-runtime/authored.mp4"
        ));
        let Some(Unit::Init(init)) = reader.next_unit().expect("authored init") else {
            panic!("init");
        };
        let Some(Unit::Fragment(fragment)) = reader.next_unit().expect("authored fragment") else {
            panic!("fragment");
        };
        (init, fragment)
    }

    fn kernel(init: &Init, max_bytes: usize, cut_each: bool) -> VerifiedCopyKernel {
        VerifiedCopyKernel::new(
            init.clone(),
            init.clone(),
            CutPolicy {
                floor_ticks: if cut_each { 1 } else { u64::MAX },
                first_floor_ticks: if cut_each { 1 } else { u64::MAX },
                max_bytes: if cut_each { 1 } else { usize::MAX },
                max_ticks: if cut_each {
                    u64::from(init.video().expect("video").timescale) * 16
                } else {
                    u64::MAX
                },
                strict_max_ticks: None,
            },
            TrackDurations {
                video_ms: 0,
                audio_ms: 0,
                audio_bits_per_second: 0,
            },
            true,
            max_bytes,
            max_bytes.saturating_mul(16),
        )
        .expect("matching initialized outputs")
    }

    fn pair(
        init: &Init,
        fragment: &Fragment,
        number: u64,
    ) -> (Fragment, SourceFragmentClock, Fragment) {
        let mut canonical = fragment.clone();
        let mut av = fragment.clone();
        let duration = fragment.video_duration(init);
        let video = init.video().expect("fixture video");
        canonical
            .tracks
            .iter_mut()
            .find(|track| track.track_id == video.id)
            .expect("video track")
            .base_decode_time += number * duration;
        av.tracks
            .iter_mut()
            .find(|track| track.track_id == video.id)
            .expect("video track")
            .base_decode_time = 10_000 + number * duration;
        let samples = fragment
            .track(video.id)
            .expect("video track")
            .sample_count() as u64;
        (
            canonical,
            SourceFragmentClock {
                canonical_dts: 37 + number * duration,
                duration,
                next_packet_sequence: (number + 1) * samples,
            },
            av,
        )
    }

    #[test]
    fn cold_and_cached_pairs_keep_canonical_rows_but_merge_original_av_tfdt() {
        let (init, fragment) = fixture();
        for cached in [false, true] {
            let mut engine = kernel(&init, fragment.len() * 2, true);
            let (first, first_clock, av) = pair(&init, &fragment, 0);
            let mut expected = IndexRow::from_fragment(&first, &init).expect("canonical row");
            expected.dts = 37;
            assert!(engine
                .push(&first, first_clock, av, cached.then_some(&expected))
                .expect("verified first pair")
                .is_empty());
            let (next, clock, av) = pair(&init, &fragment, 1);
            let output = engine
                .push(&next, clock, av, None)
                .expect("cut")
                .pop()
                .expect("completed prefix");
            assert_eq!(output.video_entry().expect("video entry").start_ticks, 37);
            assert_eq!(
                output.video_entry().expect("video entry").duration_ticks,
                first_clock.duration
            );
            assert_eq!(output.rows, vec![expected]);
            let mut reader = FragmentReader::new();
            reader.push(&init.bytes);
            reader.push(&output.media.bytes);
            assert!(matches!(
                reader.next_unit().expect("merged init"),
                Some(Unit::Init(_))
            ));
            let Some(Unit::Fragment(merged)) = reader.next_unit().expect("merged AV") else {
                panic!("fragment");
            };
            assert_eq!(
                merged
                    .track(init.video().expect("video").id)
                    .expect("merged video")
                    .base_decode_time,
                10_000
            );
            let suffix = engine
                .finish()
                .expect("observed suffix")
                .pop()
                .expect("one pending row");
            assert_eq!(suffix.video_entry().expect("video entry").index, 1);
            assert_eq!(suffix.rows.len(), 1);
            assert_eq!(
                suffix.video_entry().expect("video entry").start_ticks,
                37 + first_clock.duration
            );
        }
    }

    #[test]
    fn every_cached_row_field_is_checked_before_any_visibility() {
        let (init, fragment) = fixture();
        for field in 0..5 {
            let mut engine = kernel(&init, fragment.len(), true);
            let (canonical, clock, av) = pair(&init, &fragment, 0);
            let mut expected = IndexRow::from_fragment(&canonical, &init).expect("canonical row");
            expected.dts = clock.canonical_dts;
            match field {
                0 => expected.dts += 1,
                1 => expected.duration += 1,
                2 => expected.bytes += 1,
                3 => expected.video_bytes += 1,
                _ => {
                    expected.class = if expected.class == fmp4::CutClass::Dirty {
                        fmp4::CutClass::CleanIdr
                    } else {
                        fmp4::CutClass::Dirty
                    }
                }
            }
            assert!(matches!(
                engine.push(&canonical, clock, av, Some(&expected)),
                Err(VerifiedCopyError::ExpectedRowMismatch)
            ));
            assert!(matches!(engine.finish(), Err(VerifiedCopyError::Failed)));
        }
    }

    #[test]
    fn late_mismatch_latches_without_retracting_returned_prefix() {
        let (init, fragment) = fixture();
        let mut engine = kernel(&init, fragment.len() * 2, true);
        let (canonical, clock, av) = pair(&init, &fragment, 0);
        engine
            .push(&canonical, clock, av, None)
            .expect("first pair");
        let (canonical, clock, av) = pair(&init, &fragment, 1);
        let prefix = engine
            .push(&canonical, clock, av, None)
            .expect("next pair")
            .pop()
            .expect("prefix");
        let (canonical, clock, mut av) = pair(&init, &fragment, 2);
        av.tracks[0].runs[0].samples[0].size += 1;
        assert!(matches!(
            engine.push(&canonical, clock, av, None),
            Err(VerifiedCopyError::Companion(CompanionError::Samples))
        ));
        assert_eq!(prefix.rows.len(), 1);
        assert_eq!(prefix.video_entry().expect("video entry").start_ticks, 37);
        let (canonical, clock, av) = pair(&init, &fragment, 3);
        assert!(matches!(
            engine.push(&canonical, clock, av, None),
            Err(VerifiedCopyError::Failed)
        ));
        assert!(matches!(engine.finish(), Err(VerifiedCopyError::Failed)));
    }

    #[test]
    fn pending_bytes_fixed_target_and_invalid_audio_only_input_fail_explicitly() {
        let (init, fragment) = fixture();
        assert!(matches!(
            VerifiedCopyKernel::new(
                init.clone(),
                init.clone(),
                CutPolicy {
                    floor_ticks: 1,
                    first_floor_ticks: 1,
                    max_bytes: 1,
                    max_ticks: 1,
                    strict_max_ticks: None,
                },
                TrackDurations {
                    video_ms: 0,
                    audio_ms: 0,
                    audio_bits_per_second: 0
                },
                true,
                0,
                1
            ),
            Err(VerifiedCopyError::InvalidLimit)
        ));
        let mut engine = kernel(&init, fragment.len(), false);
        let (canonical, clock, av) = pair(&init, &fragment, 0);
        engine
            .push(&canonical, clock, av, None)
            .expect("within exact byte limit");
        let (canonical, clock, av) = pair(&init, &fragment, 1);
        assert!(matches!(
            engine.push(&canonical, clock, av, None),
            Err(VerifiedCopyError::InputLimit)
        ));
        assert!(matches!(engine.finish(), Err(VerifiedCopyError::Failed)));

        let mut engine = kernel(&init, fragment.len(), true);
        let (mut canonical, mut clock, mut av) = pair(&init, &fragment, 0);
        let duration = init.video().expect("video").timescale * 16 + 1;
        canonical.tracks[0].runs[0].samples[0].duration = duration;
        av.tracks[0].runs[0].samples[0].duration = duration;
        clock.duration = canonical.video_duration(&init);
        assert!(matches!(
            engine.push(&canonical, clock, av, None),
            Err(VerifiedCopyError::TargetExceeded)
        ));
        assert!(matches!(engine.finish(), Err(VerifiedCopyError::Failed)));

        let mut engine = kernel(&init, fragment.len(), true);
        let mut audio_only = fragment;
        audio_only.tracks.clear();
        assert!(matches!(
            engine.push_audio_only(audio_only),
            Err(VerifiedCopyError::InvalidFragment)
        ));
        assert!(matches!(engine.finish(), Err(VerifiedCopyError::Failed)));
    }

    #[test]
    fn packet_counter_overflow_is_typed_and_cannot_finish_as_a_partial_success() {
        let (init, fragment) = fixture();
        let mut engine = kernel(&init, fragment.len(), true);
        engine.next_packet_sequence = Some(u64::MAX);
        let clock = SourceFragmentClock {
            canonical_dts: 0,
            duration: fragment.video_duration(&init),
            next_packet_sequence: fragment
                .track(init.video().expect("video").id)
                .expect("video track")
                .sample_count() as u64,
        };
        assert!(matches!(
            engine.push(&fragment, clock, fragment.clone(), None),
            Err(VerifiedCopyError::Overflow)
        ));
        assert!(matches!(engine.finish(), Err(VerifiedCopyError::Failed)));
    }

    fn with_audio(init: &Init) -> Init {
        let mut av = init.clone();
        let mut audio = init.video().expect("fixture video").clone();
        audio.id += 1;
        audio.kind = fmp4::TrackKind::Audio;
        audio.timescale = 1_000;
        audio.codec = None;
        audio.nal_length_size = 0;
        audio.dolby_vision_config = false;
        av.tracks.push(audio);
        av
    }

    fn audio_fragment(init: &Init, dts: u64, durations: &[u32]) -> Fragment {
        let audio = init
            .tracks
            .iter()
            .find(|track| track.kind == fmp4::TrackKind::Audio)
            .expect("audio metadata");
        let bytes = (0..durations.len() as u32)
            .flat_map(u32::to_be_bytes)
            .collect::<Vec<_>>();
        Fragment {
            mdat_payload: 0..bytes.len(),
            bytes,
            tracks: vec![fmp4::TrackFragment {
                track_id: audio.id,
                base_decode_time: dts,
                runs: vec![fmp4::Run {
                    version: 0,
                    composition_offsets_present: false,
                    data_offset_at: None,
                    data_offset: 0,
                    samples: durations
                        .iter()
                        .map(|duration| fmp4::Sample {
                            duration: *duration,
                            size: 4,
                            size_at: None,
                            flags: 0,
                            cto: 0,
                        })
                        .collect(),
                }],
            }],
        }
    }

    fn append_audio(av: &mut Fragment, audio: Fragment) {
        let offset = av.bytes.len();
        av.bytes.extend_from_slice(&audio.bytes);
        av.mdat_payload.end = av.bytes.len();
        for mut track in audio.tracks {
            for run in &mut track.runs {
                run.data_offset += offset;
            }
            av.tracks.push(track);
        }
    }

    fn audio_kernel(
        init: &Init,
        av_init: &Init,
        limit: usize,
        clone_limit: usize,
    ) -> VerifiedCopyKernel {
        VerifiedCopyKernel::new(
            init.clone(),
            av_init.clone(),
            CutPolicy {
                floor_ticks: 1,
                first_floor_ticks: 1,
                max_bytes: 1,
                max_ticks: u64::from(init.video().expect("video").timescale) * 16,
                strict_max_ticks: None,
            },
            TrackDurations {
                video_ms: 0,
                audio_ms: 0,
                audio_bits_per_second: 0,
            },
            true,
            limit,
            clone_limit,
        )
        .expect("audio companion kernel")
    }

    #[test]
    fn mixed_final_audio_is_split_without_inventing_video_rows_or_raising_target() {
        let (init, fragment) = fixture();
        let av_init = with_audio(&init);
        let (canonical, clock, mut av) = pair(&init, &fragment, 0);
        av.tracks[0].base_decode_time = 0;
        append_audio(&mut av, audio_fragment(&av_init, 0, &[1_000; 45]));
        let limit = av.len() * 2;
        let mut engine = audio_kernel(&init, &av_init, limit, limit * 128);
        assert!(engine
            .push(&canonical, clock, av, None)
            .expect("mixed pair")
            .is_empty());
        let output = engine.finish().expect("bounded sample-aligned tail");
        assert!(output.len() >= 3);
        assert!(matches!(output[0].entry, VerifiedCopyEntry::Video(_)));
        assert_eq!(output[0].rows.len(), 1);
        assert_eq!(
            output[0].video_entry().expect("video").duration_ticks,
            clock.duration
        );
        let audio_id = av_init
            .tracks
            .iter()
            .find(|track| track.kind == fmp4::TrackKind::Audio)
            .expect("audio")
            .id;
        let mut audio_ticks = 0;
        let mut audio_samples = 0;
        for (index, segment) in output.iter().enumerate() {
            assert_eq!(segment.output_index as usize, index);
            assert!(segment.presentation.within_seconds(16));
            assert!(segment.seconds > 0.0 && segment.seconds <= 16.0);
            if index > 0 {
                assert!(
                    matches!(segment.entry, VerifiedCopyEntry::AudioTail { output_index } if output_index as usize == index)
                );
                assert!(segment.rows.is_empty());
                assert_eq!(segment.media.video_ticks, 0);
            }
            let mut reader = FragmentReader::new();
            reader.push(&init.bytes);
            reader.push(&segment.media.bytes);
            assert!(matches!(
                reader.next_unit().expect("init"),
                Some(Unit::Init(_))
            ));
            let Some(Unit::Fragment(merged)) = reader.next_unit().expect("merged AV") else {
                panic!("fragment");
            };
            if let Some(audio) = merged.track(audio_id) {
                audio_ticks += audio.duration();
                audio_samples += audio.sample_count();
            }
        }
        assert_eq!(audio_ticks, 45_000);
        assert_eq!(audio_samples, 45);
    }

    #[test]
    fn ordinary_extinf_uses_video_duration_when_audio_leads() {
        let (init, fragment) = fixture();
        let av_init = with_audio(&init);
        let limit = fragment.len() * 32;
        let mut engine = audio_kernel(&init, &av_init, limit, limit * 8);
        let mut ordinary = Vec::new();

        for number in 0..3 {
            let (canonical, clock, mut av) = pair(&init, &fragment, number);
            av.tracks[0].base_decode_time = 0;
            append_audio(
                &mut av,
                audio_fragment(&av_init, number * 5_000, &[1_000; 5]),
            );
            ordinary.extend(engine.push(&canonical, clock, av, None).expect("AV pair"));
        }

        assert_eq!(ordinary.len(), 2);
        let video_timescale = av_init.video().expect("video metadata").timescale;
        for segment in &ordinary {
            let video = segment.video_entry().expect("ordinary video cut");
            let video_seconds = video.seconds(video_timescale);
            assert!(segment.presentation.longest_track_seconds() > video_seconds);
            assert_eq!(segment.seconds, video_seconds);
        }
        let first = ordinary[0].video_entry().expect("first video cut");
        let second = ordinary[1].video_entry().expect("second video cut");
        assert_eq!(second.start_ticks, first.start_ticks + first.duration_ticks);
    }

    #[test]
    fn actual_sixteen_point_zero_fourteen_seconds_is_rejected_on_an_ordinary_cut() {
        let (init, fragment) = fixture();
        let av_init = with_audio(&init);
        let (canonical, clock, mut av) = pair(&init, &fragment, 0);
        av.tracks[0].base_decode_time = 0;
        append_audio(&mut av, audio_fragment(&av_init, 0, &[16_014]));
        let limit = av.len() * 2;
        let mut engine = audio_kernel(&init, &av_init, limit, limit * 4);
        engine
            .push(&canonical, clock, av, None)
            .expect("pair held before visibility");
        let (canonical, next_clock, mut av) = pair(&init, &fragment, 1);
        av.tracks[0].base_decode_time = clock.duration;
        assert!(matches!(
            engine.push(&canonical, next_clock, av, None),
            Err(VerifiedCopyError::TargetExceeded)
        ));
        assert!(matches!(engine.finish(), Err(VerifiedCopyError::Failed)));
    }

    #[test]
    fn merged_audio_tfdt_gap_is_counted_and_tail_clone_budget_is_checked() {
        let (init, fragment) = fixture();
        let av_init = with_audio(&init);
        let (mut canonical, mut clock, mut av) = pair(&init, &fragment, 0);
        let duration = init.video().expect("video").timescale * 2;
        canonical.tracks[0].runs[0].samples[0].duration = duration;
        av.tracks[0].runs[0].samples[0].duration = duration;
        av.tracks[0].base_decode_time = 0;
        clock.duration = canonical.video_duration(&init);
        append_audio(&mut av, audio_fragment(&av_init, 0, &[1_000]));
        let tail = audio_fragment(&av_init, 16_014, &[1_000]);
        let limit = av.len() + tail.len();
        let mut engine = audio_kernel(&init, &av_init, limit, limit * 8);
        engine
            .push(&canonical, clock, av.clone(), None)
            .expect("held video/audio");
        engine.push_audio_only(tail).expect("terminal audio flush");
        // Raw audio duration is only 2s. The merger extends the first sample
        // across the tfdt gap: the actual output span exceeds the fixed 16s.
        assert!(matches!(
            engine.finish(),
            Err(VerifiedCopyError::TargetExceeded)
        ));
        let mut engine = audio_kernel(&init, &av_init, limit, 1);
        engine.push(&canonical, clock, av, None).expect("held pair");
        assert!(matches!(
            engine.finish(),
            Err(VerifiedCopyError::InputLimit)
        ));
    }
}
