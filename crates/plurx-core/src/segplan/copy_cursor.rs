//! Incremental copy cuts over canonical video rows. Arrival batching and the
//! audiovisual muxer's wire size never participate in boundary selection.
//! The caller verifies row identity, continuity and source authority first.

use super::{IndexRow, PlanEntry, PlanEntryKind, TrackDurations};
use crate::fmp4::{CutPolicy, CutReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyPlanError {
    Overflow,
    Failed,
}

/// Keeps only the unfinished segment. Completed entries can be committed with
/// their media and rows before another fragment arrives. Finishing a cursor
/// closes only that observed range; it does not prove whole-title EOF.
pub struct CopyPlanCursor {
    timescale: u32,
    policy: CutPolicy,
    tracks: TrackDurations,
    next_index: u32,
    pending_start: Option<u64>,
    pending_ticks: u64,
    pending_bytes: u64,
    pending_fragments: u32,
    failed: bool,
}

impl CopyPlanCursor {
    pub fn new(timescale: u32, policy: CutPolicy, tracks: TrackDurations) -> Self {
        Self {
            timescale: timescale.max(1),
            policy,
            tracks,
            next_index: 0,
            pending_start: None,
            pending_ticks: 0,
            pending_bytes: 0,
            pending_fragments: 0,
            failed: false,
        }
    }

    /// An arriving row starts the next segment when it triggers a cut. The
    /// returned entry covers preceding rows, excluding this arriving row.
    pub fn push(&mut self, row: &IndexRow) -> Result<Option<PlanEntry>, CopyPlanError> {
        if self.failed {
            return Err(CopyPlanError::Failed);
        }
        let result = self.push_checked(row);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn push_checked(&mut self, row: &IndexRow) -> Result<Option<PlanEntry>, CopyPlanError> {
        let reason = self.policy.cut_before(
            self.pending_ticks,
            usize::try_from(self.with_headroom()).unwrap_or(usize::MAX),
            row.class,
            self.next_index == 0,
        );
        let (ticks, bytes, fragments) = if reason.is_some() {
            (0u64, 0u64, 0u32)
        } else {
            (
                self.pending_ticks,
                self.pending_bytes,
                self.pending_fragments,
            )
        };
        // Validate before mutating: a rejected row cannot expose a completed
        // entry or partially advance the cursor.
        let next_ticks = ticks
            .checked_add(row.duration)
            .ok_or(CopyPlanError::Overflow)?;
        let next_bytes = bytes
            .checked_add(u64::from(row.bytes))
            .ok_or(CopyPlanError::Overflow)?;
        let next_fragments = fragments.checked_add(1).ok_or(CopyPlanError::Overflow)?;
        row.dts
            .checked_add(row.duration)
            .ok_or(CopyPlanError::Overflow)?;
        let next_index = if reason.is_some() {
            self.next_index
                .checked_add(1)
                .ok_or(CopyPlanError::Overflow)?
        } else {
            self.next_index
        };
        let completed = reason.map(|reason| self.entry(reason));
        if reason.is_some() || self.pending_start.is_none() {
            self.pending_start = Some(row.dts);
        }
        self.next_index = next_index;
        self.pending_ticks = next_ticks;
        self.pending_bytes = next_bytes;
        self.pending_fragments = next_fragments;
        Ok(completed)
    }

    fn with_headroom(&self) -> u64 {
        self.pending_bytes
            .saturating_add(self.tracks.headroom(self.pending_ticks, self.timescale))
    }

    fn entry(&self, reason: CutReason) -> PlanEntry {
        PlanEntry {
            index: self.next_index,
            kind: PlanEntryKind::Video,
            start_ticks: self.pending_start.unwrap_or(0),
            duration_ticks: self.pending_ticks,
            est_bytes: self.with_headroom(),
            cut: reason.into(),
            fragments: self.pending_fragments,
        }
    }

    /// Close only the pending suffix, preserving any earlier rejected-row failure.
    pub fn finish(self) -> Result<Option<PlanEntry>, CopyPlanError> {
        if self.failed {
            return Err(CopyPlanError::Failed);
        }
        Ok((self.pending_fragments != 0).then(|| self.entry(CutReason::EndOfStream)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fmp4::CutClass;
    use crate::segplan::PlanCut;

    fn policy() -> CutPolicy {
        CutPolicy {
            floor_ticks: 3_000,
            first_floor_ticks: 1_000,
            max_bytes: 3_400,
            max_ticks: 4_000,
            strict_max_ticks: None,
        }
    }

    fn tracks(audio_bits_per_second: u32) -> TrackDurations {
        TrackDurations {
            video_ms: 30_000,
            audio_ms: 30_000,
            audio_bits_per_second,
        }
    }

    fn row(dts: u64, bytes: u32, class: CutClass) -> IndexRow {
        IndexRow {
            dts,
            duration: 1_000,
            bytes,
            // Payload-only bytes are deliberately unlike canonical wire bytes.
            // Neither this number nor a production audiovisual mux size cuts.
            video_bytes: 1,
            class,
        }
    }

    fn entry(
        index: u32,
        start: u64,
        ticks: u64,
        bytes: u64,
        cut: PlanCut,
        fragments: u32,
    ) -> PlanEntry {
        PlanEntry {
            index,
            kind: PlanEntryKind::Video,
            start_ticks: start,
            duration_ticks: ticks,
            est_bytes: bytes,
            cut,
            fragments,
        }
    }

    #[test]
    fn mixed_boundaries_use_first_floor_canonical_bytes_and_audio_headroom() {
        // 800 audio bits/s => 100 bytes/s, reserved at 150% => 150 bytes/s.
        // The 3-row byte cut is 3,000 video bytes + 450 audio headroom;
        // without the audio allowance it would not reach the 3,400 ceiling.
        let mut cursor = CopyPlanCursor::new(1_000, policy(), tracks(800));
        let classes = [
            CutClass::CleanIdr,
            CutClass::Dirty,
            CutClass::CleanCra,
            CutClass::Dirty,
            CutClass::Unparseable,
            CutClass::Dirty,
            CutClass::Dirty,
            CutClass::Dirty,
            CutClass::Dirty,
            CutClass::Dirty,
            CutClass::CleanIdr,
            CutClass::CleanCra,
            CutClass::CleanIdr,
        ];
        let mut completed = Vec::new();
        for (i, class) in classes.into_iter().enumerate() {
            let bytes = if i < 5 { 1_000 } else { 100 };
            if let Some(segment) = cursor
                .push(&row(9_000 + i as u64 * 1_000, bytes, class))
                .expect("valid canonical row")
            {
                completed.push(segment);
            }
        }
        assert_eq!(
            completed,
            vec![
                entry(0, 9_000, 2_000, 2_300, PlanCut::Clean, 2),
                entry(1, 11_000, 3_000, 3_450, PlanCut::ByteCeiling, 3),
                entry(2, 14_000, 4_000, 1_000, PlanCut::TimeCeiling, 4),
                entry(3, 18_000, 3_000, 750, PlanCut::Clean, 3),
            ]
        );
        // Finishing must not repeat a committed prefix or fabricate audio tail.
        assert_eq!(
            cursor.finish(),
            Ok(Some(entry(4, 21_000, 1_000, 250, PlanCut::EndOfStream, 1)))
        );
    }

    #[test]
    fn finish_emits_only_observed_suffix_and_empty_cursor_has_none() {
        assert_eq!(
            CopyPlanCursor::new(1_000, policy(), tracks(0)).finish(),
            Ok(None)
        );
        let mut cursor = CopyPlanCursor::new(1_000, policy(), tracks(0));
        assert_eq!(cursor.push(&row(42_000, 700, CutClass::CleanIdr)), Ok(None));
        assert_eq!(
            cursor.finish(),
            Ok(Some(entry(0, 42_000, 1_000, 700, PlanCut::EndOfStream, 1)))
        );
    }

    #[test]
    fn rejected_cut_trigger_latches_failure_without_exposing_pending_segment() {
        let mut cursor = CopyPlanCursor::new(1_000, policy(), tracks(0));
        assert_eq!(cursor.push(&row(0, 700, CutClass::CleanIdr)), Ok(None));
        // This clean row would cut the first segment, but its end overflows.
        let bad = IndexRow {
            dts: u64::MAX,
            duration: 1,
            ..row(0, 700, CutClass::CleanIdr)
        };
        assert_eq!(cursor.push(&bad), Err(CopyPlanError::Overflow));
        assert_eq!(cursor.next_index, 0);
        assert_eq!(
            (
                cursor.pending_start,
                cursor.pending_ticks,
                cursor.pending_bytes,
                cursor.pending_fragments
            ),
            (Some(0), 1_000, 700, 1)
        );
        assert_eq!(
            cursor.push(&row(1_000, 700, CutClass::CleanIdr)),
            Err(CopyPlanError::Failed)
        );
        assert_eq!(cursor.finish(), Err(CopyPlanError::Failed));
    }

    #[test]
    fn representation_overflows_fail_before_advancing_or_emitting() {
        // Exercise representation bounds directly: reaching these counters by
        // streaming billions of otherwise valid rows is neither necessary nor
        // bounded test work. Every failure must poison finish as well as push.
        for counter in 0..4 {
            let mut cursor = CopyPlanCursor::new(
                1_000,
                CutPolicy {
                    floor_ticks: u64::MAX,
                    first_floor_ticks: u64::MAX,
                    max_bytes: usize::MAX,
                    max_ticks: u64::MAX,
                    strict_max_ticks: None,
                },
                tracks(0),
            );
            cursor
                .push(&row(0, 1, CutClass::CleanIdr))
                .expect("valid initial row");
            let mut next = row(1_000, 1, CutClass::Dirty);
            match counter {
                0 => cursor.pending_ticks = u64::MAX - 1,
                1 => cursor.pending_bytes = u64::MAX,
                2 => cursor.pending_fragments = u32::MAX,
                _ => {
                    cursor.policy.floor_ticks = 1;
                    cursor.next_index = u32::MAX;
                    next.class = CutClass::CleanIdr;
                }
            }
            let before = (
                cursor.next_index,
                cursor.pending_start,
                cursor.pending_ticks,
                cursor.pending_bytes,
                cursor.pending_fragments,
            );
            assert_eq!(
                cursor.push(&next),
                Err(CopyPlanError::Overflow),
                "counter {counter}"
            );
            assert_eq!(
                (
                    cursor.next_index,
                    cursor.pending_start,
                    cursor.pending_ticks,
                    cursor.pending_bytes,
                    cursor.pending_fragments
                ),
                before
            );
            assert_eq!(cursor.finish(), Err(CopyPlanError::Failed));
        }
    }
}
