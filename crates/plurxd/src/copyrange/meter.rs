//! Where a growing Source copy's production time goes. One line per published
//! segment, one warning when the producer makes no progress for a whole
//! advertised target duration, and one line when production ends. It only
//! observes: nothing here changes what the producer does or when.
use std::{
    future::Future,
    sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};
use tokio::time::Instant;

/// The read bank's hold state, readable without its lock. The bank's own
/// mutex is held across an `.await` while it changes state, so a reader that
/// tried that lock would see "not held" exactly when it matters. `holds`
/// counts holds begun, so a wait can ask whether the bank held at any point
/// since it started, not only at the instant it is reported.
#[derive(Default)]
pub(crate) struct HoldMirror {
    held: AtomicBool,
    holds: AtomicU64,
}
impl HoldMirror {
    /// Record the bank's state after each transition it commits.
    pub(crate) fn set(&self, held: bool) {
        if held {
            if !self.held.swap(true, Ordering::SeqCst) {
                self.holds.fetch_add(1, Ordering::SeqCst);
            }
        } else {
            self.held.store(false, Ordering::SeqCst);
        }
    }
    /// A mark to compare against later.
    pub(super) fn mark(&self) -> u64 {
        self.holds.load(Ordering::SeqCst)
    }
    pub(super) fn is_held(&self) -> bool {
        self.held.load(Ordering::SeqCst)
    }
    /// Whether the bank is holding now or began a hold since `mark`.
    pub(super) fn held_since(&self, mark: u64) -> bool {
        self.held.load(Ordering::SeqCst) || self.holds.load(Ordering::SeqCst) != mark
    }
}

/// Where a range's production stood at one instant: what it had committed
/// (its init and its published segments, which only ever grow), how many
/// child outputs its mux had accepted, and the bank's hold count. Compared
/// later, it says whether production advanced in between.
///
/// A commit is not the only progress. A healthy producer slower than its
/// viewer spends longer than one wait window between two commits, and its
/// FFmpeg child keeps delivering output the whole time; a stuck one delivers
/// nothing. Each output the mux accepts is that evidence: the same signal the
/// producer's own stall warning (`child_output`) is judged by. Bytes read
/// from the child but not yet accepted by the mux do not count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProductionMark {
    committed: usize,
    outputs: u64,
    holds: u64,
    memory_waits: u64,
}

/// Whether a wait that saw no commit and no child output met a stuck
/// producer. The read bank's
/// hold excuses it: a holding producer pauses by design, ahead of its viewer.
/// But the hold is the bank's decision from its last demand snapshot, not a
/// sign the producer is alive, so a viewer that asks for media at the
/// production head while held re-anchors the bank to that request
/// ([`Self::reanchored`]). From then until production commits again, a hold
/// excuses nothing.
///
/// Everything it judges by is a mirror the producer updates after each
/// commit, readable without the range's lock: a resource request judging its
/// deadline must not wait behind the very read it timed out on.
pub(super) struct ProductionStall {
    holding: HoldMirror,
    /// The copy waiting for node memory to grow its charge
    /// ([`crate::admission::CopyMemoryPermit::resize`]).
    memory: HoldMirror,
    /// Child outputs the mux has accepted.
    outputs: AtomicU64,
    /// The range's init and published segments, as last committed.
    committed: AtomicUsize,
    /// Whether the range committed its end.
    complete: AtomicBool,
    /// The media end the range has published, or `i64::MIN` before its
    /// origin is known.
    produced_end_ms: AtomicI64,
    /// A viewer waited at the held head: the next read-bank refresh
    /// re-anchors the bank to the produced end.
    reanchor: AtomicBool,
    /// The commit count when the bank was last re-anchored at a held head,
    /// or `usize::MAX`.
    reanchored_at: AtomicUsize,
}
impl Default for ProductionStall {
    fn default() -> Self {
        Self {
            holding: HoldMirror::default(),
            memory: HoldMirror::default(),
            outputs: AtomicU64::new(0),
            committed: AtomicUsize::new(0),
            complete: AtomicBool::new(false),
            produced_end_ms: AtomicI64::new(i64::MIN),
            reanchor: AtomicBool::new(false),
            reanchored_at: AtomicUsize::new(usize::MAX),
        }
    }
}
impl ProductionStall {
    pub(super) fn holding(&self) -> &HoldMirror {
        &self.holding
    }
    /// Whether the copy waits for node memory: set by
    /// [`super::observed::ObservedCopyMux::fit`] while its growth waits.
    pub(super) fn memory_wait(&self) -> &HoldMirror {
        &self.memory
    }
    /// Mirror what the range has committed.
    pub(super) fn record(&self, committed: usize, complete: bool, produced_end_ms: Option<i64>) {
        self.committed.store(committed, Ordering::SeqCst);
        self.complete.store(complete, Ordering::SeqCst);
        self.produced_end_ms
            .store(produced_end_ms.unwrap_or(i64::MIN), Ordering::SeqCst);
    }
    pub(super) fn committed(&self) -> usize {
        self.committed.load(Ordering::SeqCst)
    }
    pub(super) fn complete(&self) -> bool {
        self.complete.load(Ordering::SeqCst)
    }
    /// A viewer waited at the head while the bank held: from now until
    /// production commits again a hold excuses nothing, and the next
    /// read-bank refresh re-anchors the bank to the produced end.
    /// Whether a re-anchor was requested: not before the range has an
    /// origin, when there is no produced end to move the bank to and a hold
    /// must keep excusing a wait.
    pub(super) fn request_reanchor(&self) -> bool {
        if self.produced_end_ms.load(Ordering::SeqCst) == i64::MIN {
            return false;
        }
        self.reanchored(self.committed());
        self.reanchor.store(true, Ordering::SeqCst);
        true
    }
    #[cfg(test)]
    pub(super) fn mirrors_for_test(&self) -> (usize, bool, u64) {
        (
            self.committed(),
            self.complete(),
            self.outputs.load(Ordering::SeqCst),
        )
    }
    #[cfg(test)]
    pub(super) fn produced_end_for_test(&self) -> Option<i64> {
        let end = self.produced_end_ms.load(Ordering::SeqCst);
        (end != i64::MIN).then_some(end)
    }
    /// The range committed its end.
    pub(super) fn complete_now(&self) {
        self.complete.store(true, Ordering::SeqCst);
    }
    /// The viewer's own demand superseded a requested re-anchor.
    pub(super) fn cancel_reanchor(&self) {
        self.reanchor.store(false, Ordering::SeqCst);
    }
    pub(super) fn reanchor_pending(&self) -> bool {
        self.reanchor.load(Ordering::SeqCst)
    }
    /// The produced end a requested re-anchor moves the bank to, once.
    pub(super) fn take_reanchor(&self) -> Option<i64> {
        if !self.reanchor.swap(false, Ordering::SeqCst) {
            return None;
        }
        let end = self.produced_end_ms.load(Ordering::SeqCst);
        (end != i64::MIN).then_some(end)
    }
    pub(super) fn mark(&self, committed: usize) -> ProductionMark {
        ProductionMark {
            committed,
            outputs: self.outputs.load(Ordering::SeqCst),
            holds: self.holding.mark(),
            memory_waits: self.memory.mark(),
        }
    }
    /// The mux accepted one more output from the child.
    pub(super) fn output(&self) {
        self.outputs.fetch_add(1, Ordering::SeqCst);
    }
    /// The bank was re-anchored to a viewer's request at the head, with
    /// `committed` committed.
    pub(super) fn reanchored(&self, committed: usize) {
        self.reanchored_at.store(committed, Ordering::SeqCst);
    }
    /// Stuck since `mark`: still running and unfinished, nothing committed
    /// and no child output accepted, and no hold that still excuses it. A
    /// copy that stopped or finished is not stuck; its callers' own checks
    /// end those. A copy that waited for node memory at any point since
    /// `mark` is not stuck either: it pauses for other copies, and their
    /// ending (or its own stop) ends the wait.
    pub(super) fn stalled_since(
        &self,
        mark: ProductionMark,
        committed: usize,
        complete: bool,
        live: bool,
    ) -> bool {
        if !live
            || complete
            || committed != mark.committed
            || self.outputs.load(Ordering::SeqCst) != mark.outputs
        {
            return false;
        }
        if self.memory.held_since(mark.memory_waits) {
            return false;
        }
        let excused = self.holding.held_since(mark.holds)
            && self.reanchored_at.load(Ordering::SeqCst) != committed;
        !excused
    }
}

/// Production's own accounting between two published segments.
///
/// `child` is time spent waiting for the FFmpeg child's next output, which
/// covers the source read (through the held Source input), demux and the
/// child's own mux. `convert` is plurx's normalisation, Dolby Vision RPU
/// rewrite and segment kernel. `memory` is waiting for node memory to grow
/// the copy's charge while other copies hold it. `commit` is writing and checkpointing what the
/// kernel produced. A slow NAS read shows as `child`, a slow conversion as
/// `convert`, a slow disk as `commit`.
pub(super) struct ProductionMeter {
    incarnation: String,
    range_id: uuid::Uuid,
    started: Instant,
    interval_started: Instant,
    interval_media_start_ms: Option<f64>,
    child: Duration,
    convert: Duration,
    memory: Duration,
    commit: Duration,
    segments: u32,
    media_end_ms: Option<f64>,
}

impl ProductionMeter {
    pub(super) fn new(incarnation: String, range_id: uuid::Uuid, now: Instant) -> Self {
        Self {
            incarnation,
            range_id,
            started: now,
            interval_started: now,
            interval_media_start_ms: None,
            child: Duration::ZERO,
            convert: Duration::ZERO,
            memory: Duration::ZERO,
            commit: Duration::ZERO,
            segments: 0,
            media_end_ms: None,
        }
    }
    pub(super) fn child(&mut self, spent: Duration) {
        self.child = self.child.saturating_add(spent);
    }
    pub(super) fn convert(&mut self, spent: Duration) {
        self.convert = self.convert.saturating_add(spent);
    }
    pub(super) fn memory(&mut self, spent: Duration) {
        self.memory = self.memory.saturating_add(spent);
    }
    pub(super) fn commit(&mut self, spent: Duration) {
        self.commit = self.commit.saturating_add(spent);
    }

    /// Log one published segment and start the next interval. The rate is
    /// media seconds produced per wall second since the previous segment.
    /// One child output can complete several segments at once; every one
    /// after the first is `batched`: the interval's wall time, split and rate
    /// belong to the first line, and a batched line claims none of them.
    pub(super) fn segment(
        &mut self,
        now: Instant,
        index: u32,
        media_start_ms: f64,
        media_end_ms: f64,
        bytes: u64,
        batched: bool,
    ) {
        if batched {
            tracing::info!(
                target: "plurxd::transcode",
                incarnation = %self.incarnation,
                range = %self.range_id,
                segment = index,
                media_start_ms = media_start_ms.round() as i64,
                media_end_ms = media_end_ms.round() as i64,
                bytes,
                since_start_ms = now.saturating_duration_since(self.started).as_millis() as u64,
                batched = true,
                "Source copy segment published"
            );
            self.segments = self.segments.saturating_add(1);
            self.media_end_ms = Some(media_end_ms);
            self.interval_media_start_ms = Some(media_end_ms);
            return;
        }
        let wall = now.saturating_duration_since(self.interval_started);
        let produced_from = self.interval_media_start_ms.unwrap_or(media_start_ms);
        let media_ms = (media_end_ms - produced_from).max(0.0);
        let rate = if wall.is_zero() {
            None
        } else {
            Some((media_ms / wall.as_secs_f64() / 1000.0 * 100.0).round() / 100.0)
        };
        tracing::info!(
            target: "plurxd::transcode",
            incarnation = %self.incarnation,
            range = %self.range_id,
            segment = index,
            media_start_ms = media_start_ms.round() as i64,
            media_end_ms = media_end_ms.round() as i64,
            bytes,
            wall_ms = wall.as_millis() as u64,
            since_start_ms = now.saturating_duration_since(self.started).as_millis() as u64,
            rate = rate.unwrap_or(f64::NAN),
            child_ms = self.child.as_millis() as u64,
            convert_ms = self.convert.as_millis() as u64,
            memory_ms = self.memory.as_millis() as u64,
            commit_ms = self.commit.as_millis() as u64,
            "Source copy segment published"
        );
        self.segments = self.segments.saturating_add(1);
        self.media_end_ms = Some(media_end_ms);
        self.interval_started = now;
        self.interval_media_start_ms = Some(media_end_ms);
        self.child = Duration::ZERO;
        self.convert = Duration::ZERO;
        self.memory = Duration::ZERO;
        self.commit = Duration::ZERO;
    }

    /// One line for a wait for node memory that outlasted the advertised
    /// target duration: the copy paused behind other copies' holds.
    pub(super) fn waited_for_memory(&self, waited: Duration) {
        tracing::info!(
            target: "plurxd::transcode",
            incarnation = %self.incarnation,
            range = %self.range_id,
            waited_ms = waited.as_millis() as u64,
            segments = self.segments,
            media_end_ms = self.media_end_ms.map_or(-1, |end| end.round() as i64),
            "Source copy producer waited for node memory"
        );
    }

    /// One line for a wait that outlasted the advertised target duration.
    /// `held` says the Source read bank held reads at some point during the
    /// wait. A held bank is the producer running far enough ahead (its
    /// watermarks are two target durations apart, so a healthy hold outlasts
    /// one): debug. A wait with no hold is the child itself being slow: warn.
    pub(super) fn stalled(&self, stage: &'static str, waited: Duration, held: bool) {
        macro_rules! line {
            ($level:ident, $message:literal) => {
                tracing::$level!(
                    target: "plurxd::transcode",
                    incarnation = %self.incarnation,
                    range = %self.range_id,
                    stage,
                    waited_ms = waited.as_millis() as u64,
                    read_bank_held = held,
                    segments = self.segments,
                    media_end_ms = self.media_end_ms.map_or(-1, |end| end.round() as i64),
                    $message
                )
            };
        }
        if held {
            line!(debug, "Source copy producer waited on its read bank's hold");
        } else {
            line!(
                warn,
                "Source copy producer made no progress for a target duration"
            );
        }
    }

    /// The one line production ends with: how far it got and how long it
    /// ran. A failure's producer cause is warned where it is relabelled; this
    /// line carries the outcome code beside the totals.
    pub(super) fn finished(
        &self,
        now: Instant,
        outcome: &Result<(), String>,
        peak_held_bytes: usize,
        charged_bytes: u64,
    ) {
        tracing::info!(
            target: "plurxd::transcode",
            incarnation = %self.incarnation,
            range = %self.range_id,
            outcome = outcome.as_ref().err().map_or("complete", String::as_str),
            segments = self.segments,
            peak_held_bytes,
            charged_bytes,
            media_end_ms = self.media_end_ms.map_or(-1, |end| end.round() as i64),
            elapsed_ms = now.saturating_duration_since(self.started).as_millis() as u64,
            "Source copy production ended"
        );
    }
}

/// Await `work`, calling `on_stall` once if it is still pending after
/// `target`. The wait itself is unchanged; this only reports it.
pub(super) async fn await_reporting_stall<F: Future>(
    work: F,
    target: Duration,
    on_stall: impl FnOnce(Duration),
) -> F::Output {
    let started = Instant::now();
    tokio::pin!(work);
    tokio::select! {
        biased;
        output = &mut work => return output,
        () = tokio::time::sleep(target) => {}
    }
    on_stall(started.elapsed());
    work.await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn capture() -> (Arc<crate::logbuf::LogBuffer>, impl Sized) {
        use tracing_subscriber::prelude::*;
        let logs = Arc::new(crate::logbuf::LogBuffer::new(16));
        let guard = tracing::subscriber::set_default(
            tracing_subscriber::registry().with(crate::logbuf::BufferLayer(Arc::clone(&logs))),
        );
        (logs, guard)
    }

    fn lines(logs: &crate::logbuf::LogBuffer) -> Vec<String> {
        logs.tail("debug", 16)
            .into_iter()
            .map(|entry| format!("{} {} {}", entry.level, entry.target, entry.message))
            .collect()
    }

    /// The 6751 shape: 19.5 s of media in 24 s, then 7.1 s in 27 s, mostly
    /// waiting on the child. Each line names its own interval's rate and split.
    #[test]
    fn growing_copy_segment_lines_carry_rate_and_where_the_time_went() {
        let (logs, _guard) = capture();
        let start = Instant::now();
        let mut meter = ProductionMeter::new("inc-1".into(), uuid::Uuid::nil(), start);
        meter.child(Duration::from_millis(20_000));
        meter.convert(Duration::from_millis(3_000));
        meter.commit(Duration::from_millis(1_000));
        meter.segment(
            start + Duration::from_secs(24),
            1,
            4_463.0,
            19_478.0,
            37_000_000,
            false,
        );
        meter.child(Duration::from_millis(26_000));
        meter.segment(
            start + Duration::from_secs(51),
            2,
            19_478.0,
            26_610.0,
            17_000_000,
            false,
        );
        meter.segment(
            start + Duration::from_secs(51),
            3,
            26_610.0,
            38_956.0,
            30_000_000,
            true,
        );
        let lines = lines(&logs);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].starts_with("INFO plurxd::transcode Source copy segment published"));
        for field in [
            "incarnation=inc-1",
            "segment=1",
            "media_start_ms=4463",
            "media_end_ms=19478",
            "bytes=37000000",
            "wall_ms=24000",
            "child_ms=20000",
            "convert_ms=3000",
            "commit_ms=1000",
            "rate=0.63",
        ] {
            assert!(lines[0].contains(field), "{field} in {}", lines[0]);
        }
        // The second interval starts where the first one ended.
        for field in [
            "segment=2",
            "wall_ms=27000",
            "since_start_ms=51000",
            "child_ms=26000",
            "convert_ms=0",
            "rate=0.26",
        ] {
            assert!(lines[1].contains(field), "{field} in {}", lines[1]);
        }
        // A segment the same child output completed claims no time or rate.
        assert!(
            lines[2].contains("segment=3") && lines[2].contains("batched=true"),
            "{}",
            lines[2]
        );
        for absent in ["rate=", "wall_ms=", "child_ms="] {
            assert!(!lines[2].contains(absent), "{absent} in {}", lines[2]);
        }
    }

    #[test]
    fn growing_copy_end_line_carries_its_outcome_and_totals() {
        let (logs, _guard) = capture();
        let start = Instant::now();
        let mut meter = ProductionMeter::new("inc-2".into(), uuid::Uuid::nil(), start);
        meter.segment(
            start + Duration::from_secs(5),
            0,
            0.0,
            4_463.0,
            1_000,
            false,
        );
        meter.finished(
            start + Duration::from_secs(9),
            &Err("copy_sync_unverified".into()),
            0,
            0,
        );
        meter.finished(start + Duration::from_secs(9), &Ok(()), 0, 0);
        let lines = lines(&logs);
        let failed = &lines[1];
        assert!(
            failed.starts_with("INFO plurxd::transcode Source copy production ended"),
            "{failed}"
        );
        for field in [
            "outcome=\"copy_sync_unverified\"",
            "segments=1",
            "media_end_ms=4463",
            "elapsed_ms=9000",
        ] {
            assert!(failed.contains(field), "{field} in {failed}");
        }
        assert!(lines[2].contains("outcome=\"complete\""), "{}", lines[2]);
    }

    /// The warning fires once, after the target duration, and the awaited
    /// work still completes with its own output.
    #[tokio::test(start_paused = true)]
    async fn a_wait_past_the_target_duration_is_reported_once_and_not_cut_short() {
        let target = Duration::from_secs(16);
        let mut stalls = Vec::new();
        let output = await_reporting_stall(
            async {
                tokio::time::sleep(Duration::from_secs(40)).await;
                7
            },
            target,
            |waited| stalls.push(waited),
        )
        .await;
        assert_eq!(output, 7);
        assert_eq!(stalls, vec![target]);
        let mut quick = Vec::new();
        let output = await_reporting_stall(async { 3 }, target, |waited| quick.push(waited)).await;
        assert_eq!((output, quick.len()), (3, 0));

        let (logs, _guard) = capture();
        let meter = ProductionMeter::new("inc-3".into(), uuid::Uuid::nil(), Instant::now());
        meter.stalled("child_output", target, false);
        let lines = lines(&logs);
        assert!(
            lines[0].starts_with("WARN plurxd::transcode Source copy producer made no progress"),
            "{lines:?}"
        );
        for field in [
            "stage=\"child_output\"",
            "waited_ms=16000",
            "read_bank_held=false",
            "segments=0",
            "media_end_ms=-1",
        ] {
            assert!(lines[0].contains(field), "{field} in {}", lines[0]);
        }
    }

    /// The production loop's own wiring: a wait marks the hold count, the
    /// bank begins and ends a hold during it, and the report still knows it
    /// was back-pressure. A wait with no hold is reported as the child's.
    #[tokio::test(start_paused = true)]
    async fn a_hold_during_the_wait_makes_the_stall_back_pressure_not_a_warning() {
        let (logs, _guard) = capture();
        let meter = ProductionMeter::new("inc-4".into(), uuid::Uuid::nil(), Instant::now());
        let mirror = std::sync::Arc::new(HoldMirror::default());
        let target = Duration::from_secs(16);
        for hold in [true, false] {
            let mark = mirror.mark();
            let bank = std::sync::Arc::clone(&mirror);
            await_reporting_stall(
                async move {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    if hold {
                        bank.set(true);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        bank.set(false);
                    }
                    tokio::time::sleep(Duration::from_secs(30)).await;
                },
                target,
                |waited| meter.stalled("child_output", waited, mirror.held_since(mark)),
            )
            .await;
        }
        let lines = lines(&logs);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].starts_with(
                "DEBUG plurxd::transcode Source copy producer waited on its read bank's hold"
            ),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("read_bank_held=true"), "{}", lines[0]);
        assert!(
            lines[1].starts_with("WARN plurxd::transcode Source copy producer made no progress"),
            "{}",
            lines[1]
        );
    }
}
