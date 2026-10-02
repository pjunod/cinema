//! Pure LL-HLS delivery rules. These values describe published media; they
//! grant no ownership, scratch, response admission or permission to wait.
//! The daemon must fence the snapshot and bound/cancel each waiting request.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LowLatencyError {
    #[error("part target must be positive and no longer than the parent target")]
    InvalidTarget,
    #[error("part duration must be positive and no longer than its fixed target")]
    InvalidPartDuration,
    #[error("muxed track duration skew exceeds the frozen audio-frame allowance")]
    InvalidTrackSkew,
    #[error("a short dependent part must finish its parent")]
    ShortDependentPart,
    #[error("published parents must be contiguous, with only the last unfinished")]
    InvalidFrontier,
}

/// Immutable duration contract for one producer attempt. Numeric policy
/// selection belongs to source/transport qualification, not to this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartPolicy {
    target: Duration,
    parent_target: Duration,
    max_track_skew: Duration,
}

impl PartPolicy {
    pub fn new(target: Duration, parent_target: Duration) -> Result<Self, LowLatencyError> {
        if target.is_zero() || target > parent_target {
            return Err(LowLatencyError::InvalidTarget);
        }
        Ok(Self {
            target,
            parent_target,
            max_track_skew: Duration::ZERO,
        })
    }

    /// Derive a conservative target from integer video clocks and at most one
    /// audio-frame duration of muxing skew. The manifest duration is the longer
    /// track's actual span; neither span is rounded down to a nominal half second.
    pub fn from_sample_clocks(
        video_frames: u32,
        video_frame_ticks: u32,
        video_timescale: u32,
        audio_frame: Duration,
        parent_target: Duration,
    ) -> Result<Self, LowLatencyError> {
        if video_frames == 0 || video_frame_ticks == 0 || video_timescale == 0 {
            return Err(LowLatencyError::InvalidTarget);
        }
        let nanos = (u128::from(video_frames) * u128::from(video_frame_ticks) * 1_000_000_000)
            .div_ceil(u128::from(video_timescale));
        let video =
            Duration::from_nanos(u64::try_from(nanos).map_err(|_| LowLatencyError::InvalidTarget)?);
        let target = video
            .checked_add(audio_frame)
            .ok_or(LowLatencyError::InvalidTarget)?;
        let mut policy = Self::new(target, parent_target)?;
        policy.max_track_skew = audio_frame;
        Ok(policy)
    }

    pub fn validate_muxed_part(
        self,
        video: Duration,
        audio: Duration,
        independent: bool,
        final_part: bool,
    ) -> Result<Duration, LowLatencyError> {
        if video.is_zero() || audio.is_zero() {
            return Err(LowLatencyError::InvalidPartDuration);
        }
        if video.abs_diff(audio) > self.max_track_skew {
            return Err(LowLatencyError::InvalidTrackSkew);
        }
        let duration = video.max(audio);
        self.validate_part(duration, independent, final_part)?;
        Ok(duration)
    }

    pub fn target(self) -> Duration {
        self.target
    }

    pub fn parent_target(self) -> Duration {
        self.parent_target
    }

    /// Check duration only after the part's independence and finality are
    /// known. A writer must retain an undersized dependent tail until its
    /// parent closes or further samples make a valid non-final part.
    pub fn validate_part(
        self,
        duration: Duration,
        independent: bool,
        final_part: bool,
    ) -> Result<(), LowLatencyError> {
        if duration.is_zero() || duration > self.target {
            return Err(LowLatencyError::InvalidPartDuration);
        }
        // Duration fits in u128 nanoseconds; multiplying by 100 cannot
        // overflow even at Duration::MAX.
        if duration.as_nanos() * 100 < self.target.as_nanos() * 85 && !independent && !final_part {
            return Err(LowLatencyError::ShortDependentPart);
        }
        Ok(())
    }

    fn advance_part_limit(self) -> u64 {
        if self.target < Duration::from_secs(1) {
            (Duration::from_secs(3).as_nanos() / self.target.as_nanos()).min(u64::MAX as u128)
                as u64
        } else {
            3
        }
    }
}

/// Parsed decimal delivery directives. HTTP parsing must reject malformed
/// integers rather than silently treating them as an ordinary reload.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReloadRequest {
    pub media_sequence: Option<u64>,
    pub part: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadDecision {
    /// Return the entire current playlist, including for an evicted request.
    Ready,
    /// Wait for a newer snapshot within the caller's existing request budget.
    Wait,
    /// Reject promptly. No waiter or producer work is acquired.
    BadRequest,
}

/// Counts mean parts ever published for each retained parent, including parts
/// whose tags were trimmed. They never count incomplete bytes or decrease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParentAvailability {
    pub media_sequence: u64,
    pub parts: u32,
    pub complete: bool,
}

/// Borrowed, validated view of one attempt's retained publication inventory.
/// No tasks, timers, media buffers or duplicate mutable catalog are owned here.
#[derive(Debug)]
pub struct ReloadFrontier<'a> {
    parents: &'a [ParentAvailability],
    ended: bool,
}

impl<'a> ReloadFrontier<'a> {
    pub fn new(parents: &'a [ParentAvailability], ended: bool) -> Result<Self, LowLatencyError> {
        if parents.is_empty()
            || parents.iter().enumerate().any(|(index, parent)| {
                (parent.complete && parent.parts == 0)
                    || (!parent.complete && (ended || index + 1 != parents.len()))
                    || (index > 0
                        && parents[index - 1].media_sequence.checked_add(1)
                            != Some(parent.media_sequence))
            })
        {
            return Err(LowLatencyError::InvalidFrontier);
        }
        Ok(Self { parents, ended })
    }

    pub fn classify(&self, request: ReloadRequest, policy: PartPolicy) -> ReloadDecision {
        if self.ended {
            return ReloadDecision::Ready;
        }
        let Some(mut sequence) = request.media_sequence else {
            return if request.part.is_some() {
                ReloadDecision::BadRequest
            } else {
                ReloadDecision::Ready
            };
        };
        let first = &self.parents[0];
        let last = &self.parents[self.parents.len() - 1];
        if sequence < first.media_sequence {
            return ReloadDecision::Ready;
        }
        // An unfinished parent is not a Media Segment for the MSN bound.
        // With initial sequence zero and no complete parent, the virtual
        // predecessor is -1, so the furthest acceptable sequence is one.
        let max_sequence = if last.complete {
            last.media_sequence.saturating_add(2)
        } else {
            last.media_sequence.saturating_add(1)
        };
        if sequence > max_sequence {
            return ReloadDecision::BadRequest;
        }
        let mut part = request.part;
        if let Some(parent) = self
            .parents
            .iter()
            .find(|parent| parent.media_sequence == sequence)
        {
            match part {
                None if parent.complete => return ReloadDecision::Ready,
                None => return ReloadDecision::Wait,
                Some(index) if index < parent.parts => return ReloadDecision::Ready,
                Some(_) if parent.complete => {
                    // Even a very large part index on a completed parent is
                    // a request for part zero of the following parent.
                    let Some(next) = sequence.checked_add(1) else {
                        return ReloadDecision::BadRequest;
                    };
                    sequence = next;
                    part = Some(0);
                }
                Some(_) => {}
            }
        }
        let parent = self
            .parents
            .iter()
            .find(|parent| parent.media_sequence == sequence);
        if let Some(index) = part {
            // Future parent lengths are unknown. Do not park requests across
            // an unfinished boundary: only its own parts can be bounded from
            // this inventory. A completed parent can roll to its next part 0.
            if sequence > last.media_sequence
                && (!last.complete || last.media_sequence.checked_add(1) != Some(sequence))
            {
                return ReloadDecision::BadRequest;
            }
            let count = parent.map_or(0, |parent| parent.parts);
            if index < count {
                return ReloadDecision::Ready;
            }
            // Compare using a count so an empty parent's conceptual last
            // index (-1) needs neither signed casts nor unsigned subtraction.
            if u64::from(index) >= u64::from(count).saturating_add(policy.advance_part_limit()) {
                return ReloadDecision::BadRequest;
            }
        }
        ReloadDecision::Wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> PartPolicy {
        PartPolicy::new(Duration::from_millis(500), Duration::from_secs(16))
            .expect("valid protocol fixture")
    }

    fn request(sequence: u64, part: Option<u32>) -> ReloadRequest {
        ReloadRequest {
            media_sequence: Some(sequence),
            part,
        }
    }

    #[test]
    fn part_duration_preserves_fixed_target_and_dependent_tail_rules() {
        let policy = policy();
        assert_eq!(
            policy.validate_part(Duration::ZERO, true, true),
            Err(LowLatencyError::InvalidPartDuration)
        );
        assert_eq!(
            policy.validate_part(Duration::from_millis(501), true, true),
            Err(LowLatencyError::InvalidPartDuration)
        );
        assert_eq!(
            policy.validate_part(Duration::from_millis(424), false, false),
            Err(LowLatencyError::ShortDependentPart)
        );
        for (millis, independent, final_part) in [
            (425, false, false),
            (500, false, false),
            (20, true, false),
            (20, false, true),
        ] {
            assert_eq!(
                policy.validate_part(Duration::from_millis(millis), independent, final_part),
                Ok(())
            );
        }
        assert!(PartPolicy::new(Duration::ZERO, Duration::from_secs(16)).is_err());
        assert!(PartPolicy::new(Duration::from_secs(17), Duration::from_secs(16)).is_err());
    }

    #[test]
    fn blocking_reload_distinguishes_complete_parents_from_available_parts() {
        let parents = [
            ParentAvailability {
                media_sequence: 7,
                parts: 4,
                complete: true,
            },
            ParentAvailability {
                media_sequence: 8,
                parts: 2,
                complete: false,
            },
        ];
        let frontier = ReloadFrontier::new(&parents, false).expect("valid protocol fixture");
        for req in [
            ReloadRequest::default(),
            request(6, Some(u32::MAX)),
            request(7, None),
            request(8, Some(1)),
        ] {
            assert_eq!(frontier.classify(req, policy()), ReloadDecision::Ready);
        }
        for req in [request(8, None), request(8, Some(2))] {
            assert_eq!(frontier.classify(req, policy()), ReloadDecision::Wait);
        }
        assert_eq!(
            frontier.classify(
                ReloadRequest {
                    media_sequence: None,
                    part: Some(0)
                },
                policy()
            ),
            ReloadDecision::BadRequest
        );
    }

    #[test]
    fn completed_parent_rollover_waits_for_the_next_actual_part() {
        let mut parents = [
            ParentAvailability {
                media_sequence: 7,
                parts: 4,
                complete: true,
            },
            ParentAvailability {
                media_sequence: 8,
                parts: 0,
                complete: false,
            },
        ];
        for index in [4, u32::MAX] {
            assert_eq!(
                ReloadFrontier::new(&parents, false)
                    .expect("valid protocol fixture")
                    .classify(request(7, Some(index)), policy()),
                ReloadDecision::Wait
            );
        }
        parents[1].parts = 1;
        assert_eq!(
            ReloadFrontier::new(&parents, false)
                .expect("valid protocol fixture")
                .classify(request(7, Some(u32::MAX)), policy()),
            ReloadDecision::Ready
        );
    }

    #[test]
    fn future_reload_is_bounded_without_overflow_or_empty_parent_underflow() {
        let parents = [ParentAvailability {
            media_sequence: 7,
            parts: 0,
            complete: false,
        }];
        let frontier = ReloadFrontier::new(&parents, false).expect("valid protocol fixture");
        assert_eq!(
            frontier.classify(request(9, Some(5)), policy()),
            ReloadDecision::BadRequest
        );
        for req in [
            request(10, None),
            request(7, Some(6)),
            request(u64::MAX, Some(u32::MAX)),
        ] {
            assert_eq!(frontier.classify(req, policy()), ReloadDecision::BadRequest);
        }
        let parents = [ParentAvailability {
            media_sequence: u64::MAX,
            parts: 1,
            complete: true,
        }];
        assert_eq!(
            ReloadFrontier::new(&parents, false)
                .expect("valid protocol fixture")
                .classify(request(u64::MAX, Some(1)), policy()),
            ReloadDecision::BadRequest
        );
    }

    #[test]
    fn completed_presentation_does_not_wait_for_future_delivery_directives() {
        let parents = [ParentAvailability {
            media_sequence: 7,
            parts: 1,
            complete: true,
        }];
        assert_eq!(
            ReloadFrontier::new(&parents, true)
                .expect("valid protocol fixture")
                .classify(request(u64::MAX, Some(u32::MAX)), policy()),
            ReloadDecision::Ready
        );
    }

    #[test]
    fn eof_ignores_part_without_msn_and_future_bounds_use_complete_parents() {
        let parents = [
            ParentAvailability {
                media_sequence: 6,
                parts: 4,
                complete: true,
            },
            ParentAvailability {
                media_sequence: 7,
                parts: 2,
                complete: false,
            },
        ];
        let frontier = ReloadFrontier::new(&parents, false).expect("valid fixture");
        assert_eq!(
            frontier.classify(request(9, None), policy()),
            ReloadDecision::BadRequest
        );
        assert_eq!(
            frontier.classify(request(8, None), policy()),
            ReloadDecision::Wait
        );
        assert_eq!(
            frontier.classify(request(8, Some(5)), policy()),
            ReloadDecision::BadRequest
        );
        assert_eq!(
            frontier.classify(request(7, Some(7)), policy()),
            ReloadDecision::Wait
        );
        assert_eq!(
            frontier.classify(request(7, Some(8)), policy()),
            ReloadDecision::BadRequest
        );
        let completed = ReloadFrontier::new(&parents[..1], false).expect("complete fixture");
        assert_eq!(
            completed.classify(request(7, Some(5)), policy()),
            ReloadDecision::Wait
        );
        assert_eq!(
            completed.classify(request(8, Some(0)), policy()),
            ReloadDecision::BadRequest,
            "do not infer the part count of an entirely unpublished intervening parent"
        );
        let ended = ReloadFrontier::new(&parents[..1], true).expect("complete fixture");
        assert_eq!(
            ended.classify(
                ReloadRequest {
                    media_sequence: None,
                    part: Some(0)
                },
                policy()
            ),
            ReloadDecision::Ready
        );
    }

    #[test]
    fn fractional_video_cadence_and_aac_skew_fit_the_derived_target() {
        let audio_frame = Duration::from_nanos(21_333_334); // ceil(1024 / 48000 s)
        let policy =
            PartPolicy::from_sample_clocks(12, 1001, 24000, audio_frame, Duration::from_secs(16))
                .expect("valid clocks");
        let video = Duration::from_micros(500_500);
        assert_eq!(policy.target(), video + audio_frame);
        assert_eq!(
            policy.validate_muxed_part(video, video + audio_frame, false, false),
            Ok(video + audio_frame)
        );
        assert_eq!(
            policy.validate_muxed_part(
                video,
                video + audio_frame + Duration::from_nanos(1),
                true,
                true
            ),
            Err(LowLatencyError::InvalidTrackSkew)
        );
        assert!(
            PartPolicy::from_sample_clocks(12, 1001, 0, audio_frame, Duration::from_secs(16))
                .is_err()
        );
    }

    #[test]
    fn inconsistent_inventory_cannot_authorize_a_reload() {
        assert!(ReloadFrontier::new(&[], false).is_err());
        let mut parents = [
            ParentAvailability {
                media_sequence: 7,
                parts: 4,
                complete: false,
            },
            ParentAvailability {
                media_sequence: 8,
                parts: 1,
                complete: false,
            },
        ];
        assert!(ReloadFrontier::new(&parents, false).is_err());
        parents[0].complete = true;
        assert!(ReloadFrontier::new(&parents, true).is_err());
        parents[1].media_sequence = 9;
        assert!(ReloadFrontier::new(&parents, false).is_err());
        parents[1].media_sequence = 8;
        parents[0].parts = 0;
        assert!(ReloadFrontier::new(&parents, false).is_err());
    }
}
