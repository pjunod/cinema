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
//!
//! One rule sits above all of that, and it is the one review caught this
//! module getting wrong: **a blocked request outranks the ahead window.**
//! Under §2.3 a segment GET blocks until the bytes exist or the client's own
//! first-byte deadline fires, so a reader waiting on a segment is the only
//! demand here with a clock running against it. The first cut of this module
//! served the lowest hole at or above any reader's back-window floor, which is
//! a segment nobody had asked for — so a reader blocked at index 200 could
//! watch the producer walk backwards to refill an evicted index 5 it might
//! rewind into some day, and time out while it did.

// M2 builds the store and the scheduler before M3 wires either to the
// transcode manager, so nothing outside the tests calls these yet. The allow
// is scoped to this module and comes out when the manager starts asking.
#![allow(dead_code)]

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
    /// The segment this reader's in-flight GET is waiting on, if it has one.
    ///
    /// This is a *request*, not a guess about what it might want: the
    /// connection is open, the response has not started, and the client's
    /// first-byte deadline is counting down. It is the only field here that
    /// can make a producer move against the ahead window, and the only one
    /// that can make it move backwards.
    pub blocked_on: Option<u32>,
    /// The furthest segment this reader has asked for. Where the ahead window
    /// is measured from once nothing is blocked.
    pub frontier: u32,
}

impl Demand {
    /// A reader that is ahead-filling: it has everything it has asked for and
    /// is not waiting on anything.
    pub fn idle_at(frontier: u32) -> Demand {
        Demand {
            blocked_on: None,
            frontier,
        }
    }

    /// A reader whose open GET is waiting on `index`.
    pub fn waiting_on(index: u32) -> Demand {
        Demand {
            blocked_on: Some(index),
            frontier: index,
        }
    }
}

/// What the producer should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing is attached and nothing is owed. The producer may be reclaimed
    /// after its grace period.
    Idle,
    /// Keep producing forward from where it is.
    Produce { next: u32 },
    /// Stop until something changes. `reason` says which bound is holding it
    /// and what would release it, because the two are cleared by entirely
    /// different events and a log line saying only "suspended" cannot be acted
    /// on.
    Suspend { produced_through: u32, reason: Hold },
    /// A demanded segment is too far ahead to reach by reading forward.
    Reposition { to: u32 },
    /// Over the working-set budget with a reader waiting. Free `wanted` bytes
    /// before producing anything else.
    ///
    /// Distinct from [`Action::Suspend`] on purpose. Suspending here would
    /// park the producer against a bound that only *it* can clear, with a
    /// reader blocked on a segment behind it: eviction is what makes room, and
    /// nothing else is going to run it.
    ///
    /// `wanted` clears the hold in one sweep. It is measured to the *release*
    /// line, not to the line that was crossed — freeing back to the budget
    /// leaves the producer exactly on it, and the next decision asks for the
    /// other half anyway. One round trip, not two, and no chance of a
    /// free-one-produce-one cycle that runs for the length of a film.
    MakeRoom { wanted: u64 },
}

/// Why a producer is stopped, and what would start it again.
///
/// The reason is not decoration. The two holds are cleared by entirely
/// different events — a reader advancing, or somebody else on the node
/// releasing bytes — so a caller that waits on the wrong one waits forever,
/// and a log line saying only "suspended" cannot be acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// Far enough ahead of every reader. Cleared by a reader advancing.
    Ahead { horizon: u32 },
    /// The node's working set is full and nobody here is waiting on a
    /// segment, so there is nothing to make room *for*. Cleared by another
    /// rendition releasing bytes, or by a reader arriving and turning this
    /// into [`Action::MakeRoom`].
    ///
    /// `free_bytes` is an amount to free, in the same units and measured the
    /// same way as [`Action::MakeRoom`]'s `wanted` — not a level to get under.
    /// Two adjacent answers from one function handing a caller two different
    /// meanings under names that both read as byte counts is how a caller
    /// frees five gigabytes of a hundred and wonders why the hold stands.
    WorkingSetFull { free_bytes: u64 },
    /// Over the budget with a reader waiting and **nothing evictable** — every
    /// remaining segment is either inside a reader's window or belongs to an
    /// admitted rendition, which by rule never gives one up.
    ///
    /// A real stall, and the reason [`Action::MakeRoom`] is not simply
    /// repeated: asking again with identical inputs gets the identical answer
    /// while the blocked reader's deadline runs out. Only something outside
    /// this rendition clears it, and somebody has to be told.
    NoRoom { wanted: u64 },
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
    /// The node's working set, across every rendition — not this one's.
    ///
    /// A producer inside its ahead horizon can still be the one filling the
    /// disk, and a bound only this rendition can see would let it. The same
    /// reasoning `transcode`'s global scratch cap is built on.
    pub working_set: WorkingSet,
}

/// The node-wide working-set budget, and where it currently stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkingSet {
    pub used_bytes: u64,
    pub budget_bytes: u64,
    /// Whether the producer is already stopped for this bound.
    ///
    /// Load-bearing, and the reason this is not simply `used > budget`.
    /// Entering at the budget and leaving at half of it is what stops a
    /// producer sitting exactly on the line from suspending and resuming once
    /// per segment for the length of a film — the same hysteresis
    /// `transcode::ahead_hold` applies to the scratch caps, for the same
    /// reason.
    pub held: bool,
}

impl WorkingSet {
    /// Bytes that must be freed to clear the hold, or `None` when there is
    /// room.
    ///
    /// Two lines, not one. Whether the producer is *over* is judged against
    /// the budget when it is running and against half the budget once it is
    /// held — that hysteresis is what stops a producer sitting on the line
    /// from suspending and resuming once per segment for the length of a film.
    /// But the amount to free is always measured to the *release* line, so one
    /// sweep clears the hold: freeing back to the budget lands exactly on it,
    /// and the very next decision asks for the other half.
    fn over_by(&self) -> Option<u64> {
        if self.budget_bytes == 0 {
            return None;
        }
        let release = self.budget_bytes / 2;
        let enter = if self.held {
            release
        } else {
            self.budget_bytes
        };
        (self.used_bytes > enter).then(|| self.used_bytes.saturating_sub(release))
    }
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
    let horizon = position.horizon_segments(AHEAD_HORIZON_SECONDS);
    let reposition = position.horizon_segments(REPOSITION_GAP_SECONDS);

    // Owed right now: the segments open requests are waiting on and the store
    // does not have. An index off the end of the plan is dropped rather than
    // chased — that request is a 404 the reader is owed instead of bytes, and
    // it is the delivery path's answer to give, not the producer's.
    let mut owed: Vec<u32> = demands
        .iter()
        .filter_map(|demand| demand.blocked_on)
        .filter(|index| {
            manifest
                .state(*index)
                .is_some_and(|state| !state.is_materialized())
        })
        .collect();
    owed.sort_unstable();
    owed.dedup();

    // The working set is a disk, and a full one cannot be produced into for
    // whoever is waiting. It outranks the blocked reader for that reason and
    // no other — so it gates *work*, and only work. A producer with nothing to
    // do is not the one holding the bytes, and holding its process open
    // against a bound it cannot clear is the same mistake as suspending a
    // producer with no readers.
    let pressure = position.working_set.over_by();

    if !owed.is_empty() {
        if let Some(wanted) = pressure {
            // Somebody is blocked, so suspending parks the producer against a
            // bound only it can clear while a reader waits on a segment behind
            // it. Eviction is what makes room and nothing else will run it —
            // unless there is nothing to evict, which is a stall and has to be
            // said rather than spun on.
            return if manifest.has_evictable(&owed) {
                Action::MakeRoom { wanted }
            } else {
                Action::Suspend {
                    produced_through: position.produced_through.unwrap_or(owed[0]),
                    reason: Hold::NoRoom { wanted },
                }
            };
        }
        return serve_blocked(manifest, &owed, position.produced_through, reposition);
    }

    // Nothing is blocked, so this is ahead-fill, and it runs *forward from the
    // producer* rather than from the lowest hole in the rendition. A hole
    // behind the producer that nobody is waiting on is not its business:
    // blocking GETs turn that hole into a request the instant anybody wants
    // it, and running backwards for it on spec is exactly what starved the
    // reader above.
    let from = match position.produced_through {
        Some(through) => through.saturating_add(1),
        None => furthest,
    };
    let Some(gap) = manifest.next_gap(from) else {
        // Everything in front of every reader exists. Whether to stop depends
        // on how far past the furthest demand the producer has already run —
        // and not at all on the working set, which this producer is not adding
        // to.
        return stop(position.produced_through, furthest, horizon);
    };

    // A gap beyond the ahead horizon is not owed yet — that is what the
    // horizon means. Suspend rather than run to the end of a two-hour film for
    // a reader who has watched four minutes.
    if gap > furthest.saturating_add(horizon) {
        return stop(position.produced_through, furthest, horizon);
    }

    // There is real ahead-fill to do and no room to do it in. Nobody is
    // waiting, so the honest answer is to stop and let the bytes go to the
    // renditions that have readers blocked on them.
    if let Some(free_bytes) = pressure {
        return match position.produced_through {
            Some(through) => Action::Suspend {
                produced_through: through,
                reason: Hold::WorkingSetFull { free_bytes },
            },
            // Never produced anything, so there is no progress to report and
            // nothing of this rendition's to give up. `stop` answers `Idle`
            // for the same state and the same reason.
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
            // `from` put the gap strictly ahead of the producer, so the only
            // question left is whether reading to it beats restarting there.
            if gap > through.saturating_add(reposition) {
                Action::Reposition { to: gap }
            } else {
                Action::Produce { next: gap }
            }
        }
    }
}

/// Which owed segment this producer goes after, given where it is.
///
/// `owed` is sorted, deduplicated, and non-empty.
///
/// When the producer cannot reach any owed segment by reading forward, it
/// repositions to the *lowest* one rather than the nearest. That is the only
/// single move that can serve every waiting reader without a second restart:
/// producing forward from the lowest sweeps through the rest, while starting
/// anywhere else strands everyone below it indefinitely. A spread too wide for
/// one producer to close is a signal to admission (ledger D12) that the
/// rendition needs a second one — it is not a reason for this one to thrash
/// between two readers, serving neither.
fn serve_blocked(
    manifest: &Manifest,
    owed: &[u32],
    produced_through: Option<u32>,
    reposition: u32,
) -> Action {
    let lowest = owed[0];
    let Some(through) = produced_through else {
        // Nothing produced yet: start where the work is. Starting anywhere but
        // the beginning is itself a reposition.
        return if lowest == 0 {
            Action::Produce { next: 0 }
        } else {
            Action::Reposition { to: lowest }
        };
    };
    match owed.iter().copied().find(|index| *index > through) {
        // Near enough ahead that a copy pipe at several times realtime gets
        // there before a new process finishes starting. What it produces
        // *next* is still its own next gap — reading forward to the target
        // means making everything on the way, minus whatever already exists.
        Some(target) if target <= through.saturating_add(reposition) => {
            let next = manifest
                .next_gap(through.saturating_add(1))
                .unwrap_or(target)
                .min(target);
            Action::Produce { next }
        }
        // Everything owed is behind the producer, or further ahead than a
        // restart costs. Either way the pipeline moves.
        _ => Action::Reposition { to: lowest },
    }
}

/// Suspend if the producer has run past the ahead horizon, idle if it simply
/// has nothing to do.
fn stop(produced_through: Option<u32>, furthest: u32, horizon: u32) -> Action {
    match produced_through {
        Some(through) if through >= furthest.saturating_add(horizon) => Action::Suspend {
            produced_through: through,
            reason: Hold::Ahead { horizon },
        },
        _ => Action::Idle,
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
                video_bytes: 99_400,
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
            working_set: WorkingSet::default(),
        }
    }

    /// The same position, under a working set of `used` against `budget`.
    fn under_pressure(through: Option<u32>, used: u64, budget: u64, held: bool) -> Position {
        Position {
            working_set: WorkingSet {
                used_bytes: used,
                budget_bytes: budget,
                held,
            },
            ..position(through)
        }
    }

    /// A reader that has everything it has asked for.
    fn demand(frontier: u32) -> Demand {
        Demand::idle_at(frontier)
    }

    /// A reader with an open GET waiting on `index`.
    fn waiting(index: u32) -> Demand {
        Demand::waiting_on(index)
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
                reason: Hold::Ahead { horizon },
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
        // earlier pass made it — so the only thing owed really is the far one.
        for index in 0..35 {
            manifest.materialize(index, 1_000, 0);
        }
        // The producer sits at 4. The request at 35 is 31 segments ahead, over
        // the 9-segment reposition gap, so a restart at the boundary beats
        // reading there.
        match decide(&manifest, &[waiting(35)], position(Some(4))) {
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
            decide(&manifest, &[waiting(10)], position(Some(4))),
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
        // decision moved to requests — and note that it takes an actual
        // request to move the producer backwards, not merely a back window
        // that reaches down there.
        let readers = [Demand {
            blocked_on: Some(3),
            frontier: 30,
        }];
        match decide(&manifest, &readers, position(Some(29))) {
            Action::Reposition { to } => assert_eq!(to, 3),
            other => panic!("expected a backwards reposition, got {other:?}"),
        }
    }

    #[test]
    fn a_hole_nobody_is_waiting_on_does_not_pull_the_producer_backwards() {
        // The regression this module was rewritten for. A reader is blocked at
        // 30 with its first-byte deadline running; an old segment at 3 was
        // evicted and is inside the same reader's back window, so it is a
        // segment the reader *could* ask for and has not.
        let mut manifest = manifest(60);
        for index in 0..30 {
            manifest.materialize(index, 1_000, 0);
        }
        manifest.evict(3);
        assert_eq!(
            decide(&manifest, &[waiting(30)], position(Some(29))),
            Action::Produce { next: 30 },
            "the open request outranks a hole nobody has asked for; serving \
             the hole first is how the waiting reader times out"
        );
    }

    #[test]
    fn a_blocked_request_outranks_the_ahead_window() {
        // Two readers on one rendition: one ahead-filling near the start, one
        // blocked far ahead in a stretch an earlier pass already made — except
        // for the segment it is actually waiting on.
        let mut manifest = manifest(80);
        for index in 0..40 {
            manifest.materialize(index, 1_000, 0);
        }
        manifest.evict(38);
        // The ahead-fill reader at 2 puts the horizon at 28, so 38 is well
        // outside it. It is still owed: somebody is holding a connection open
        // for it.
        match decide(&manifest, &[demand(2), waiting(38)], position(Some(39))) {
            Action::Reposition { to } => assert_eq!(to, 38),
            other => panic!("expected the blocked request to win, got {other:?}"),
        }
    }

    #[test]
    fn the_lowest_owed_segment_wins_when_no_single_move_serves_everyone() {
        // Readers blocked at 5 and at 60, producer at 30. Neither is reachable
        // by reading forward inside the reposition gap. Going to 5 is the only
        // move that can still sweep through both; going to 60 strands the
        // reader at 5 for good.
        let mut manifest = manifest(80);
        for index in 0..40 {
            manifest.materialize(index, 1_000, 0);
        }
        manifest.evict(5);
        match decide(&manifest, &[waiting(5), waiting(60)], position(Some(30))) {
            Action::Reposition { to } => assert_eq!(to, 5),
            other => panic!("expected a reposition to the lowest owed, got {other:?}"),
        }
    }

    #[test]
    fn a_request_for_a_segment_that_already_exists_is_not_demand() {
        // A reader can be recorded as blocked a tick after the bytes landed.
        // That is a response about to be written, not work.
        let mut manifest = manifest(40);
        for index in 0..10 {
            manifest.materialize(index, 1_000, 0);
        }
        assert_eq!(
            decide(&manifest, &[waiting(4)], position(Some(9))),
            Action::Produce { next: 10 },
            "an already-materialized request must not drag the producer back"
        );
    }

    #[test]
    fn a_request_past_the_end_of_the_plan_is_not_the_producers_problem() {
        let mut manifest = manifest(20);
        let len = manifest.len() as u32;
        for index in 0..len {
            manifest.materialize(index, 1_000, 0);
        }
        // Off the end of the film: the delivery path owes that reader a 404,
        // and the producer must not go chasing an index that has no plan
        // entry.
        assert_eq!(
            decide(&manifest, &[waiting(len + 5)], position(Some(len - 1))),
            Action::Idle
        );
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

    // ---- the working set -------------------------------------------------

    #[test]
    fn a_full_working_set_with_a_reader_waiting_makes_room_rather_than_stopping() {
        // Suspending here parks the producer against a bound only it can
        // clear, with a reader blocked on a segment behind it. Eviction is
        // what makes room and nothing else is going to run it.
        let mut manifest = manifest(80);
        // Something to evict. Without it the honest answer is `NoRoom`, which
        // the test below covers.
        for index in 0..4 {
            manifest.materialize(index, 1_000, i64::from(index));
        }
        let action = decide(
            &manifest,
            &[Demand::waiting_on(4)],
            under_pressure(Some(3), 12_000, 10_000, false),
        );
        assert_eq!(
            action,
            Action::MakeRoom { wanted: 7_000 },
            "measured to the release line, so one sweep clears the hold"
        );
    }

    #[test]
    fn a_full_working_set_with_nothing_evictable_is_a_stall_and_says_so() {
        // Asking for room again with identical inputs gets the identical
        // answer while the blocked reader's deadline runs out. Only something
        // outside this rendition clears it, and somebody has to be told.
        let manifest = manifest(80);
        match decide(
            &manifest,
            &[Demand::waiting_on(4)],
            under_pressure(Some(3), 12_000, 10_000, false),
        ) {
            Action::Suspend {
                reason: Hold::NoRoom { wanted },
                ..
            } => assert_eq!(wanted, 7_000),
            other => panic!("expected a stall, got {other:?}"),
        }
    }

    #[test]
    fn a_producer_sitting_on_the_budget_line_does_not_thrash() {
        // The whole reason `held` exists. Without hysteresis a producer at the
        // line frees one segment, drops under, produces one, crosses again,
        // and does that once per segment for the length of a film.
        let mut manifest = manifest(80);
        for index in 0..4 {
            manifest.materialize(index, 1_000, i64::from(index));
        }
        let just_under_the_line = 9_999;
        assert_eq!(
            decide(
                &manifest,
                &[Demand::waiting_on(4)],
                under_pressure(Some(3), just_under_the_line, 10_000, false),
            ),
            Action::Produce { next: 4 },
            "under the budget and not held: serve the blocked reader"
        );
        // Now held. It must keep making room until it is under HALF, not
        // until it is back under the line it just crossed.
        assert_eq!(
            decide(
                &manifest,
                &[Demand::waiting_on(4)],
                under_pressure(Some(3), just_under_the_line, 10_000, true),
            ),
            Action::MakeRoom { wanted: 4_999 },
            "held and above half: free to the release line in one sweep"
        );
        assert_eq!(
            decide(
                &manifest,
                &[Demand::waiting_on(4)],
                under_pressure(Some(3), 4_999, 10_000, true),
            ),
            Action::Produce { next: 4 },
            "held and under half: released"
        );
    }

    #[test]
    fn an_unset_budget_never_holds_anything() {
        // A zero budget is "not configured", not "no bytes allowed". Reading
        // it the other way stops every producer on the node.
        let mut manifest = manifest(80);
        manifest.materialize(0, 1_000, 0);
        assert_eq!(
            decide(
                &manifest,
                &[Demand::waiting_on(4)],
                under_pressure(Some(3), u64::MAX, 0, false),
            ),
            Action::Produce { next: 4 }
        );
    }

    #[test]
    fn nothing_attached_is_still_idle_however_full_the_disk_is() {
        // A producer with no reader is reclaimed, not suspended: suspending
        // holds the process open against a bound it is not going to clear.
        let manifest = manifest(80);
        assert_eq!(
            decide(
                &manifest,
                &[],
                under_pressure(Some(3), 12_000, 10_000, false)
            ),
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
            working_set: WorkingSet::default(),
        };
        let long = Position {
            produced_through: Some(0),
            seconds_per_segment: 7.0,
            working_set: WorkingSet::default(),
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
            working_set: WorkingSet::default(),
        };
        assert!(broken.horizon_segments(AHEAD_HORIZON_SECONDS) >= 1);
    }
}
