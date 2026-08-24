//! The title store's bookkeeping: what is planned, what exists, what is kept.
//!
//! Companion to [`plurx_core::segplan`] (what a rendition's segments *are*) —
//! this is the state that says which of them have bytes on disk right now,
//! which readers are depending on those bytes, and which may be deleted.
//!
//! Review finding B3 is the reason this module exists as its own thing. The
//! plan's first draft carried one bitmap and asked it to mean both "the bytes
//! are here" and "the whole rendition is durably cached", and those two cannot
//! both be true of a large title: eviction clears the first while the second
//! is still being assembled, so either the rendition never completes or a
//! directory with holes gets published as a cache hit. So there are **three**
//! facts here, never fewer (plan §2.4, ledger D12):
//!
//! - **planned** — the segment belongs to this immutable rendition. Comes from
//!   the plan and never changes for the life of the rendition.
//! - **materialized** — bytes exist and may be served *now*. Eviction clears
//!   this. It says nothing about whether the rendition will ever complete.
//! - **admitted** — the whole rendition was atomically published as a cache
//!   hit. Eviction never clears this, because completion demanded every
//!   segment present simultaneously under the completed-cache budget.
//!
//! A title too big for that budget is not a failure and not a lie: it is a
//! **working-set-only rendition** — still VOD-presented, never cache-completed,
//! honestly re-materialized on a later watch. The plan's own 69 Mb/s reference
//! remux is about 62 GB against a 50 GB default cache, so this is the common
//! case for exactly the titles the plan cares most about, not an edge.

// M2 builds this bookkeeping before M3 wires it to the delivery and transcode
// paths, so nothing outside the tests calls it yet. The allow is scoped to
// this module and comes out when those callers arrive.
#![allow(dead_code)]

use std::collections::BTreeMap;

use plurx_core::segplan::{PlanEntryKind, SegmentPlan};

/// What is true of one planned segment right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegState {
    /// Planned, no bytes. Either never produced or evicted since.
    Planned,
    /// Bytes exist and may be served.
    Materialized { bytes: u64, at_ms: i64 },
}

impl SegState {
    pub fn bytes(self) -> u64 {
        match self {
            SegState::Planned => 0,
            SegState::Materialized { bytes, .. } => bytes,
        }
    }

    pub fn is_materialized(self) -> bool {
        matches!(self, SegState::Materialized { .. })
    }
}

/// One reader's dependency on a rendition, in plan indexes.
///
/// The window is what today's retention constant describes in seconds, said in
/// the units the store actually addresses. Nothing inside any attached
/// reader's window may be evicted — which is what turns the 180 s retention
/// value from a client-visible contract into a mere eviction preference: a
/// rewind past it re-materializes instead of 404ing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaderWindow {
    pub back: u32,
    pub playhead: u32,
    pub frontier: u32,
    pub ahead: u32,
}

impl ReaderWindow {
    pub fn covers(&self, index: u32) -> bool {
        let low = self.playhead.saturating_sub(self.back);
        let high = self.frontier.saturating_add(self.ahead);
        index >= low && index <= high
    }
}

/// The two budgets, named separately because they govern different things and
/// conflating them is what B3 caught.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budgets {
    /// Live materialized bytes across the store — the successor to the
    /// `playback.hls_ahead_max_bytes` / `playback.hls_scratch_max_bytes`
    /// session scratch budgets.
    pub working_set_bytes: u64,
    /// The completed cache, `cache.max_gb`.
    pub completed_cache_bytes: u64,
    /// The share of the completed cache one rendition may reserve. A rendition
    /// whose planned total exceeds this is inadmissible: it would have to
    /// evict most of the cache to complete, and then be evicted itself.
    pub admission_share: f64,
}

impl Budgets {
    pub fn admission_threshold(&self) -> u64 {
        let share = self.admission_share.clamp(0.0, 1.0);
        (self.completed_cache_bytes as f64 * share) as u64
    }
}

/// Why a rendition may never be cache-completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inadmissible {
    /// Planned total is over the admission threshold.
    TooLarge { planned: u64, threshold: u64 },
    /// The plan has no entries to admit.
    Empty,
}

/// One rendition's segment bookkeeping.
#[derive(Debug, Clone)]
pub struct Manifest {
    plan: SegmentPlan,
    states: Vec<SegState>,
    /// Set only by an atomic completion, and never cleared by eviction.
    admitted: bool,
    /// Space reserved against the completed-cache budget when backfill began.
    /// Reserved *before* the work, because a rendition that fills the disk and
    /// only then discovers it cannot be admitted has spent the budget twice.
    reserved: bool,
}

impl Manifest {
    pub fn new(plan: SegmentPlan) -> Manifest {
        let states = vec![SegState::Planned; plan.entries.len()];
        Manifest {
            plan,
            states,
            admitted: false,
            reserved: false,
        }
    }

    pub fn plan(&self) -> &SegmentPlan {
        &self.plan
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    pub fn state(&self, index: u32) -> Option<SegState> {
        self.states.get(index as usize).copied()
    }

    pub fn is_admitted(&self) -> bool {
        self.admitted
    }

    /// Bytes currently on disk for this rendition.
    pub fn materialized_bytes(&self) -> u64 {
        self.states.iter().map(|state| state.bytes()).sum()
    }

    /// How much of the rendition exists right now.
    pub fn materialized_count(&self) -> usize {
        self.states
            .iter()
            .filter(|state| state.is_materialized())
            .count()
    }

    /// The planned total the admission decision is made against.
    pub fn planned_bytes(&self) -> u64 {
        self.plan.planned_bytes()
    }

    /// Record bytes for one segment.
    pub fn materialize(&mut self, index: u32, bytes: u64, at_ms: i64) -> bool {
        let Some(slot) = self.states.get_mut(index as usize) else {
            return false;
        };
        *slot = SegState::Materialized { bytes, at_ms };
        true
    }

    /// Forget one segment's bytes. Refused for an admitted rendition, because
    /// admission is the promise that every member is present — the whole point
    /// of separating the third fact from the second.
    pub fn evict(&mut self, index: u32) -> bool {
        if self.admitted {
            return false;
        }
        let Some(slot) = self.states.get_mut(index as usize) else {
            return false;
        };
        if !slot.is_materialized() {
            return false;
        }
        *slot = SegState::Planned;
        true
    }

    /// Stop claiming one segment's bytes, whatever this rendition's state.
    ///
    /// Distinct from [`Manifest::evict`] and deliberately not subject to its
    /// refusal. Eviction is a *choice* — give up bytes to make room — and an
    /// admitted rendition may not make it, because admission is the promise
    /// that every member is present. This is the report of a fact: the bytes
    /// are already gone. Refusing here would leave the manifest claiming a
    /// segment that is not on disk, and that claim is served as a cache hit
    /// and 404s a viewer mid-film.
    ///
    /// An admitted rendition that loses a member is a real event and the
    /// caller is expected to say so out loud; see
    /// [`crate::renditiondir::RenditionDir::reconcile`], which is the only
    /// thing that calls this.
    pub fn forget(&mut self, index: u32) -> bool {
        let Some(slot) = self.states.get_mut(index as usize) else {
            return false;
        };
        if !slot.is_materialized() {
            return false;
        }
        *slot = SegState::Planned;
        true
    }

    /// May this rendition ever be published as a cache hit?
    pub fn admissible(&self, budgets: &Budgets) -> Result<(), Inadmissible> {
        if self.states.is_empty() {
            return Err(Inadmissible::Empty);
        }
        let planned = self.planned_bytes();
        let threshold = budgets.admission_threshold();
        if planned > threshold {
            return Err(Inadmissible::TooLarge { planned, threshold });
        }
        Ok(())
    }

    /// Reserve this rendition's planned space before backfill begins.
    pub fn reserve(&mut self, budgets: &Budgets) -> Result<u64, Inadmissible> {
        self.admissible(budgets)?;
        self.reserved = true;
        Ok(self.planned_bytes())
    }

    pub fn is_reserved(&self) -> bool {
        self.reserved
    }

    /// Publish the rendition as a cache hit, or say why not.
    ///
    /// Completion demands every segment materialized **simultaneously**. That
    /// is the whole content of B3: a bitmap that remembered "produced once"
    /// would publish a directory with holes, and a viewer's second watch would
    /// get a cache hit pointing at segments that were evicted while the first
    /// watch was still going.
    pub fn complete(&mut self, budgets: &Budgets) -> Result<u64, CompletionRefused> {
        if let Err(reason) = self.admissible(budgets) {
            return Err(CompletionRefused::Inadmissible(reason));
        }
        let missing = self
            .states
            .iter()
            .enumerate()
            .filter(|(_, state)| !state.is_materialized())
            .map(|(index, _)| index as u32)
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(CompletionRefused::Incomplete {
                missing: missing.len(),
                first: missing[0],
            });
        }
        let bytes = self.materialized_bytes();
        if bytes > budgets.completed_cache_bytes {
            return Err(CompletionRefused::Inadmissible(Inadmissible::TooLarge {
                planned: bytes,
                threshold: budgets.completed_cache_bytes,
            }));
        }
        self.admitted = true;
        Ok(bytes)
    }

    /// Which materialized segments may be evicted, coldest first, to free
    /// `wanted` bytes — skipping anything inside any attached reader's window.
    ///
    /// Returns fewer than `wanted` bytes' worth when the readers hold the
    /// rest. That is the honest answer: the caller's next move is to refuse a
    /// new producer, not to evict something a viewer is about to read.
    pub fn eviction_candidates(&self, readers: &[ReaderWindow], wanted: u64) -> Vec<u32> {
        if self.admitted || wanted == 0 {
            return Vec::new();
        }
        let mut by_age: BTreeMap<(i64, u32), (u32, u64)> = BTreeMap::new();
        for (index, state) in self.states.iter().enumerate() {
            let SegState::Materialized { bytes, at_ms } = *state else {
                continue;
            };
            let index = index as u32;
            if readers.iter().any(|reader| reader.covers(index)) {
                continue;
            }
            by_age.insert((at_ms, index), (index, bytes));
        }
        let mut freed = 0u64;
        let mut chosen = Vec::new();
        for (index, bytes) in by_age.into_values() {
            if freed >= wanted {
                break;
            }
            freed += bytes;
            chosen.push(index);
        }
        chosen
    }

    /// The first planned-but-absent segment at or after `from`, which is what
    /// a producer asked for demand at `from` has to make next.
    pub fn next_gap(&self, from: u32) -> Option<u32> {
        self.states
            .iter()
            .enumerate()
            .skip(from as usize)
            .find(|(_, state)| !state.is_materialized())
            .map(|(index, _)| index as u32)
    }

    /// Audio-tail entries carry no video and are produced by the same pass
    /// that finishes the last video segment, so a scheduler must not chase
    /// them as if they were independently seekable.
    pub fn is_audio_tail(&self, index: u32) -> bool {
        self.plan
            .entry(index)
            .is_some_and(|entry| entry.kind == PlanEntryKind::AudioTail)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionRefused {
    Inadmissible(Inadmissible),
    Incomplete { missing: usize, first: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::fmp4::CutClass;
    use plurx_core::fmp4::CutPolicy;
    use plurx_core::segplan::{plan_copy, FragmentIndex, IndexRow, SourceIdentity, TrackDurations};

    const GB: u64 = 1024 * 1024 * 1024;

    fn budgets() -> Budgets {
        Budgets {
            working_set_bytes: 8 * GB,
            completed_cache_bytes: 50 * GB,
            admission_share: 0.5,
        }
    }

    fn plan(fragments: usize, bytes: u32) -> SegmentPlan {
        let mut rows = Vec::new();
        let mut dts = 0;
        for i in 0..fragments {
            let duration = if i % 2 == 0 { 28_016 } else { 28_032 };
            rows.push(IndexRow {
                dts,
                duration,
                bytes,
                video_bytes: bytes.saturating_sub(600),
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
        // 1,751 ms a fragment: the 28,016/28,032-tick durations above at a
        // 16 kHz timescale, which is what a 23.976 fps 42-frame GOP really is.
        let ms = fragments as i64 * 1_751;
        plan_copy(
            &index,
            &policy,
            &TrackDurations {
                video_ms: ms,
                audio_ms: ms,
                audio_bits_per_second: 256_000,
            },
        )
    }

    fn manifest(fragments: usize, bytes: u32) -> Manifest {
        Manifest::new(plan(fragments, bytes))
    }

    fn fill(manifest: &mut Manifest) {
        for index in 0..manifest.len() as u32 {
            manifest.materialize(index, 1_000, index as i64);
        }
    }

    #[test]
    fn a_fresh_manifest_is_all_planned_and_nothing_else() {
        let manifest = manifest(40, 100_000);
        assert!(manifest.len() > 1);
        assert_eq!(manifest.materialized_count(), 0);
        assert_eq!(manifest.materialized_bytes(), 0);
        assert!(!manifest.is_admitted());
    }

    #[test]
    fn eviction_clears_materialized_and_never_touches_admitted() {
        let mut manifest = manifest(12, 100_000);
        fill(&mut manifest);
        assert!(manifest.evict(1));
        assert_eq!(manifest.state(1), Some(SegState::Planned));

        fill(&mut manifest);
        manifest.complete(&budgets()).expect("small title admits");
        assert!(manifest.is_admitted());
        assert!(
            !manifest.evict(1),
            "an admitted rendition promised every member is present; eviction \
             would turn that promise into a directory with holes"
        );
    }

    #[test]
    fn completion_demands_every_segment_at_once() {
        let mut manifest = manifest(12, 100_000);
        fill(&mut manifest);
        manifest.evict(3);
        match manifest.complete(&budgets()) {
            Err(CompletionRefused::Incomplete { missing, first }) => {
                assert_eq!(missing, 1);
                assert_eq!(first, 3);
            }
            other => panic!("a hole must refuse completion, got {other:?}"),
        }
        assert!(!manifest.is_admitted());
    }

    /// The B3 case, in the shape the plan names: a title bigger than the
    /// budget is honestly working-set-only rather than falsely complete.
    #[test]
    fn an_over_budget_title_is_never_admitted_however_full_it_gets() {
        // The plan's own reference case, scaled: a title whose planned total is
        // several times the cache it would have to fit in.
        let tight = Budgets {
            completed_cache_bytes: GB,
            ..budgets()
        };
        let mut manifest = manifest(60, 64_000_000);
        let planned = manifest.planned_bytes();
        assert!(
            planned > tight.admission_threshold(),
            "the fixture must actually exceed the threshold: {planned}"
        );
        assert!(matches!(
            manifest.admissible(&tight),
            Err(Inadmissible::TooLarge { .. })
        ));
        assert!(matches!(
            manifest.reserve(&tight),
            Err(Inadmissible::TooLarge { .. })
        ));
        fill(&mut manifest);
        assert!(matches!(
            manifest.complete(&tight),
            Err(CompletionRefused::Inadmissible(_))
        ));
        assert!(!manifest.is_admitted());
        // Still perfectly serveable. Working-set-only is a presentation, not a
        // failure: the segments exist and may be read.
        assert_eq!(manifest.materialized_count(), manifest.len());
    }

    #[test]
    fn space_is_reserved_before_backfill_not_after() {
        let mut manifest = manifest(12, 100_000);
        assert!(!manifest.is_reserved());
        let reserved = manifest.reserve(&budgets()).expect("small title reserves");
        assert_eq!(reserved, manifest.planned_bytes());
        assert!(manifest.is_reserved());
    }

    #[test]
    fn eviction_never_takes_a_segment_inside_a_reader_window() {
        let mut manifest = manifest(30, 100_000);
        fill(&mut manifest);
        let reader = ReaderWindow {
            back: 1,
            playhead: 3,
            frontier: 4,
            ahead: 2,
        };
        let chosen = manifest.eviction_candidates(&[reader], u64::MAX);
        for index in 2..=6u32 {
            assert!(
                !chosen.contains(&index),
                "segment {index} is inside the reader's window {reader:?}"
            );
        }
        assert!(
            !chosen.is_empty(),
            "everything outside the window is fair game"
        );
    }

    #[test]
    fn eviction_takes_the_coldest_first() {
        let mut manifest = manifest(20, 100_000);
        for index in 0..manifest.len() as u32 {
            // Reverse ages: the last segment is the coldest.
            manifest.materialize(index, 1_000, 1_000 - index as i64);
        }
        let chosen = manifest.eviction_candidates(&[], 2_000);
        assert_eq!(chosen.len(), 2);
        let last = manifest.len() as u32 - 1;
        assert!(
            chosen.contains(&last),
            "the coldest goes first, got {chosen:?}"
        );
    }

    #[test]
    fn eviction_gives_back_less_than_asked_rather_than_taking_a_readers_bytes() {
        let mut manifest = manifest(8, 100_000);
        fill(&mut manifest);
        let all = ReaderWindow {
            back: 100,
            playhead: 0,
            frontier: manifest.len() as u32,
            ahead: 100,
        };
        assert!(
            manifest.eviction_candidates(&[all], u64::MAX).is_empty(),
            "a reader covering the whole rendition leaves nothing to evict, and \
             saying so is the honest answer — the caller refuses a new producer \
             rather than deleting bytes a viewer is about to read"
        );
    }

    #[test]
    fn the_next_gap_is_what_a_producer_must_make() {
        let mut manifest = manifest(20, 100_000);
        fill(&mut manifest);
        assert_eq!(manifest.next_gap(0), None);
        manifest.evict(5);
        assert_eq!(manifest.next_gap(0), Some(5));
        assert_eq!(manifest.next_gap(6), None);
    }

    #[test]
    fn an_admitted_rendition_offers_nothing_to_eviction() {
        let mut manifest = manifest(12, 100_000);
        fill(&mut manifest);
        manifest.complete(&budgets()).expect("admits");
        assert!(manifest.eviction_candidates(&[], u64::MAX).is_empty());
    }

    #[test]
    fn an_empty_plan_is_inadmissible_rather_than_trivially_complete() {
        let mut empty = Manifest::new(SegmentPlan {
            version: 1,
            timescale: 16_000,
            entries: Vec::new(),
            target_duration: 0,
        });
        assert_eq!(empty.admissible(&budgets()), Err(Inadmissible::Empty));
        assert!(matches!(
            empty.complete(&budgets()),
            Err(CompletionRefused::Inadmissible(Inadmissible::Empty))
        ));
    }
}
