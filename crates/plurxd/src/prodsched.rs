//! What a producer should be doing right now.
//!
//! Companion to [`crate::titlestore`] (what exists) — this decides what to
//! make next, and it is deliberately pure: a function of the attached readers'
//! demand and the manifest, with no processes, no clock, and no disk. The loop
//! that carries the decision out lives in the transcode manager, and it does
//! nothing this module has not already decided.
//!
//! Purity is the point rather than a style preference. Today's equivalent —
//! `apply_ahead_window` — reads demand from a per-session *fetch ratchet*,
//! which means the answer depends on what a client happened to have downloaded,
//! and the awkward cases (two readers, one far behind; a seek past the
//! frontier; a reader that stopped fetching because it is paused) are only
//! reachable through a live session. Under the VOD presentation demand comes
//! from *requests* against film-time indexes, which is a small enough thing to
//! decide in one place and test exhaustively.
//!
//! Plan §2.4 is the contract: produce while `materialized_through <
//! max(attached demand frontiers) + AHEAD_HORIZON`, suspend beyond it, and
//! reposition when a demanded segment is more than `REPOSITION_GAP` ahead of
//! where the producer is — closer than that, catching up at ≥2× realtime is
//! cheaper than a pipeline restart.

use crate::titlestore::Manifest;

/// How far ahead of the furthest demand a producer may run before it is
/// suspended. Today's ahead-window value in film-time terms; ledger D9 keeps
/// the number so the M7 comparison is not confounded by a retune.
pub const AHEAD_HORIZON_SECONDS: u32 = 180;

/// Beyond this, restarting the pipeline at the demanded boundary beats letting
/// it read forward. Under it, a copy pipe running at several times realtime
/// closes the gap faster than a process can start.
pub const REPOSITION_GAP_SECONDS: u32 = 60;

/// One attached reader's demand, in plan indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Demand {
    /// The furthest segment this reader has asked for.
    pub frontier: u32,
    /// The lowest segment it may still ask for — its playhead less whatever
    /// back-window it is allowed. Used only to keep eviction honest; the
    /// producer never runs backwards for it.
    pub floor: u32,
}

/// What the producer should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing is attached and nothing is owed. The producer may be reclaimed
    /// after its grace period.
    Idle,
    /// Keep producing forward from where it is.
    Produce { next: u32 },
    /// Far enough ahead of every reader to stop until one catches up.
    Suspend { produced_through: u32, horizon: u32 },
    /// A demanded segment is too far ahead to reach by reading forward.
    Reposition { to: u32 },
}

/// Everything the decision needs that is not the manifest.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    /// The highest index this producer has materialized in its current run.
    /// `None` before it has produced anything, which is also what a freshly
    /// repositioned producer looks like.
    pub produced_through: Option<u32>,
    /// Segments per second of film, used only to turn the two horizons from
    /// seconds into indexes. Taken from the plan rather than assumed, because
    /// a copy rendition's segments are 6–15 s and a transcode rendition's are
    /// 2 s, and a horizon expressed in segments would mean two different
    /// things on the two paths.
    pub seconds_per_segment: f64,
}

impl Position {
    fn horizon_segments(&self, seconds: u32) -> u32 {
        let per = if self.seconds_per_segment > 0.0 {
            self.seconds_per_segment
        } else {
            1.0
        };
        ((f64::from(seconds) / per).ceil() as u32).max(1)
    }
}

/// Decide what one producer should do.
///
/// `demands` is every attached reader. An empty slice means nothing is
/// attached, which is [`Action::Idle`] however much of the rendition exists —
/// a producer with no reader is spending the machine on nobody.
pub fn decide(manifest: &Manifest, demands: &[Demand], position: Position) -> Action {
    if demands.is_empty() {
        return Action::Idle;
    }
    let Some(furthest) = demands.iter().map(|demand| demand.frontier).max() else {
        return Action::Idle;
    };

    // The nearest thing anybody has asked for that does not exist. Producing
    // is always about a gap, never about an index: a reader that seeked
    // backwards into an evicted stretch is owed those bytes just as much as
    // one at the frontier.
    let lowest_floor = demands.iter().map(|demand| demand.floor).min().unwrap_or(0);
    let Some(gap) = manifest.next_gap(lowest_floor) else {
        // Everything anybody can ask for exists. Whether to stop depends on
        // how far past the furthest demand the producer has already run.
        return match position.produced_through {
            Some(through) => {
                let horizon = position.horizon_segments(AHEAD_HORIZON_SECONDS);
                if through >= furthest.saturating_add(horizon) {
                    Action::Suspend {
                        produced_through: through,
                        horizon,
                    }
                } else {
                    Action::Idle
                }
            }
            None => Action::Idle,
        };
    };

    // A gap beyond the ahead horizon is not owed yet — that is what the
    // horizon means. Suspend rather than run to the end of a two-hour film for
    // a reader who has watched four minutes.
    let horizon = position.horizon_segments(AHEAD_HORIZON_SECONDS);
    let ceiling = furthest.saturating_add(horizon);
    if gap > ceiling {
        return match position.produced_through {
            Some(through) => Action::Suspend {
                produced_through: through,
                horizon,
            },
            None => Action::Idle,
        };
    }

    match position.produced_through {
        // Never produced anything: it starts where it is needed, and starting
        // is a reposition whenever that is not the beginning.
        None => {
            if gap == 0 {
                Action::Produce { next: 0 }
            } else {
                Action::Reposition { to: gap }
            }
        }
        Some(through) => {
            // Behind the gap by more than a restart costs.
            let reposition = position.horizon_segments(REPOSITION_GAP_SECONDS);
            if gap > through.saturating_add(reposition) {
                Action::Reposition { to: gap }
            } else if gap <= through {
                // The gap is behind the producer — an evicted stretch a reader
                // rewound into. Reading forward can never reach it.
                Action::Reposition { to: gap }
            } else {
                Action::Produce { next: gap }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::fmp4::{CutClass, CutPolicy};
    use plurx_core::segplan::{plan_copy, FragmentIndex, IndexRow, SourceIdentity, TrackDurations};

    /// 7 s segments, so the 180 s horizon is 26 of them and the 60 s
    /// reposition gap is 9 — numbers small enough to reason about and unequal
    /// enough that a test cannot pass by confusing the two.
    fn manifest(entries: usize) -> Manifest {
        let mut rows = Vec::new();
        let mut dts = 0u64;
        for i in 0..entries * 4 {
            let duration = if i % 2 == 0 { 28_016 } else { 28_032 };
            rows.push(IndexRow {
                dts,
                duration,
                bytes: 100_000,
                class: CutClass::CleanIdr,
            });
            dts += duration;
        }
        let index = FragmentIndex::new(
            16_000,
            rows,
            "sha",
            SourceIdentity::new(1, 1, "fingerprint"),
        );
        let policy = CutPolicy::new(6, 2, 64 * 1024 * 1024, 15, 16_000);
        let ms = (dts * 1000 / 16_000) as i64;
        Manifest::new(plan_copy(
            &index,
            &policy,
            &TrackDurations {
                video_ms: ms,
                audio_ms: ms,
                audio_bits_per_second: 256_000,
            },
        ))
    }

    fn position(through: Option<u32>) -> Position {
        Position {
            produced_through: through,
            seconds_per_segment: 7.0,
        }
    }

    fn demand(frontier: u32) -> Demand {
        Demand { frontier, floor: 0 }
    }

    #[test]
    fn no_readers_means_no_production_however_empty_the_rendition_is() {
        let manifest = manifest(40);
        assert_eq!(decide(&manifest, &[], position(None)), Action::Idle);
        assert_eq!(decide(&manifest, &[], position(Some(3))), Action::Idle);
    }

    #[test]
    fn a_fresh_rendition_starts_at_the_beginning() {
        let manifest = manifest(40);
        assert_eq!(
            decide(&manifest, &[demand(0)], position(None)),
            Action::Produce { next: 0 }
        );
    }

    #[test]
    fn a_producer_fills_the_next_gap_forward() {
        let mut manifest = manifest(40);
        for index in 0..5 {
            manifest.materialize(index, 1_000, 0);
        }
        assert_eq!(
            decide(&manifest, &[demand(6)], position(Some(4))),
            Action::Produce { next: 5 }
        );
    }

    #[test]
    fn demand_is_the_furthest_of_every_attached_reader() {
        let mut manifest = manifest(40);
        for index in 0..5 {
            manifest.materialize(index, 1_000, 0);
        }
        // One reader at the start, one well ahead: the near one does not hold
        // the producer back, and the far one does not strand the near one.
        let action = decide(&manifest, &[demand(0), demand(20)], position(Some(4)));
        assert_eq!(action, Action::Produce { next: 5 });
    }

    #[test]
    fn a_producer_far_past_every_reader_suspends() {
        let mut manifest = manifest(60);
        for index in 0..40 {
            manifest.materialize(index, 1_000, 0);
        }
        // 180 s at 7 s a segment is 26 segments; a reader at 2 with the
        // producer through 40 is past the horizon.
        match decide(&manifest, &[demand(2)], position(Some(40))) {
            Action::Suspend {
                produced_through,
                horizon,
            } => {
                assert_eq!(produced_through, 40);
                assert_eq!(horizon, 26);
            }
            other => panic!("expected suspension, got {other:?}"),
        }
    }

    #[test]
    fn a_far_seek_repositions_rather_than_reading_forward() {
        let mut manifest = manifest(80);
        // Everything up to 34 exists — a second reader watched it, or an
        // earlier pass made it — so the nearest gap really is the far one.
        for index in 0..35 {
            manifest.materialize(index, 1_000, 0);
        }
        // The producer sits at 4. The gap at 35 is 31 segments ahead, over the
        // 9-segment reposition gap, so a restart at the boundary beats reading
        // there.
        match decide(&manifest, &[demand(35)], position(Some(4))) {
            Action::Reposition { to } => assert_eq!(to, 35),
            other => panic!("expected a reposition, got {other:?}"),
        }
    }

    #[test]
    fn a_near_seek_catches_up_rather_than_restarting() {
        let mut manifest = manifest(80);
        for index in 0..5 {
            manifest.materialize(index, 1_000, 0);
        }
        // Six segments ahead is inside the 9-segment gap: a copy pipe running
        // at several times realtime gets there before a new process would
        // finish starting.
        assert_eq!(
            decide(&manifest, &[demand(10)], position(Some(4))),
            Action::Produce { next: 5 }
        );
    }

    #[test]
    fn a_rewind_into_an_evicted_stretch_repositions_backwards() {
        let mut manifest = manifest(60);
        for index in 0..30 {
            manifest.materialize(index, 1_000, 0);
        }
        manifest.evict(3);
        // Reading forward from 29 can never reach 3. This is the case a
        // fetch-ratchet demand model cannot express at all, which is why the
        // decision moved to requests.
        let readers = [Demand {
            frontier: 30,
            floor: 0,
        }];
        match decide(&manifest, &readers, position(Some(29))) {
            Action::Reposition { to } => assert_eq!(to, 3),
            other => panic!("expected a backwards reposition, got {other:?}"),
        }
    }

    #[test]
    fn a_gap_past_the_horizon_is_not_owed_yet() {
        let mut manifest = manifest(80);
        for index in 0..5 {
            manifest.materialize(index, 1_000, 0);
        }
        manifest.evict(4);
        // Reader at 2, horizon 26, so nothing past 28 is owed. The gap at 4 is
        // owed; make it exist and the next gap is past the horizon.
        manifest.materialize(4, 1_000, 0);
        for index in 5..40 {
            manifest.materialize(index, 1_000, 0);
        }
        match decide(&manifest, &[demand(2)], position(Some(39))) {
            Action::Suspend { .. } => {}
            other => panic!("expected suspension past the horizon, got {other:?}"),
        }
    }

    #[test]
    fn a_complete_rendition_with_a_reader_inside_the_horizon_is_idle() {
        let mut manifest = manifest(20);
        for index in 0..manifest.len() as u32 {
            manifest.materialize(index, 1_000, 0);
        }
        assert_eq!(
            decide(&manifest, &[demand(2)], position(Some(3))),
            Action::Idle
        );
    }

    #[test]
    fn the_horizons_are_segments_derived_from_seconds_not_a_segment_count() {
        // A transcode rendition's 2 s segments must give a horizon three and a
        // half times the size of a 7 s copy rendition's, or 180 seconds means
        // two different things on the two paths.
        let short = Position {
            produced_through: Some(0),
            seconds_per_segment: 2.0,
        };
        let long = Position {
            produced_through: Some(0),
            seconds_per_segment: 7.0,
        };
        assert_eq!(short.horizon_segments(AHEAD_HORIZON_SECONDS), 90);
        assert_eq!(long.horizon_segments(AHEAD_HORIZON_SECONDS), 26);
        assert_eq!(short.horizon_segments(REPOSITION_GAP_SECONDS), 30);
        assert_eq!(long.horizon_segments(REPOSITION_GAP_SECONDS), 9);
    }

    #[test]
    fn a_zero_length_segment_never_divides_by_zero() {
        let broken = Position {
            produced_through: Some(0),
            seconds_per_segment: 0.0,
        };
        assert!(broken.horizon_segments(AHEAD_HORIZON_SECONDS) >= 1);
    }
}
