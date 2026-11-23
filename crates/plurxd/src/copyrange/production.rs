//! A single already-authorized physical producer and its append-only range.
//! The Source actor retains grant/assignment/lease authority; this owner only
//! joins the child and outputs and commits the verified physical observations.
use super::{
    meter::{await_reporting_stall, ProductionMark, ProductionMeter, ProductionStall},
    CopyClockCensus, CopyMuxEvent, CopyRangeFiles, CopyRangePublisher, ObservedCopyMux,
    RunningCopyGeneration,
};
use crate::copy_analysis::CopyRangeCheckpoint;
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio_util::sync::CancellationToken;

struct ReadBank {
    observer: crate::source_custody::SourceObserver,
    anchor_ms: i64,
    snapshot: Option<crate::playback_control::PlaybackDemandSnapshot>,
    observed_at: Instant,
    low_ms: u64,
    high_ms: u64,
    held: bool,
}
/// A generation that ended before its seek landing was verified read only its
/// bounded bootstrap allowance: report the unverified landing, never a later
/// symptom of that ceiling. Refusals that name their own cause keep it.
pub(super) fn landing_failure(
    seek_bootstrap: &crate::source_input::SourceSeekBootstrap,
    error: String,
) -> String {
    if !seek_bootstrap.is_verified()
        && !matches!(
            error.as_str(),
            "copy_target_exceeded"
                | "copy_init_contract_changed"
                | "copy_clock_unknown"
                | "copy_range_cancelled"
        )
    {
        "copy_sync_unverified".to_owned()
    } else {
        error
    }
}

/// A producer a resource wait can watch: what it has committed, and whether
/// it advanced since.
pub(crate) trait CommitProgress {
    type Mark: Copy + std::fmt::Debug + Send;
    fn mark(&self) -> impl std::future::Future<Output = Self::Mark> + Send;
    fn stalled_since(&self, mark: Self::Mark) -> impl std::future::Future<Output = bool> + Send;
}

/// How a wait for a resource the producer has not committed yet ended at
/// its deadline.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DeadlineEnd {
    /// Production advanced during the wait: a slow producer, and the next
    /// request may find the resource.
    Slow,
    /// Nothing was committed in the whole wait window, and the producer did
    /// not hold by design: it is stuck.
    Stalled,
}

/// One resource wait over a producer. The wait window is this waiter's
/// life: what the producer has committed when it begins is what the
/// deadline is judged against. It adds no timer beyond the caller's own
/// deadline.
pub(crate) struct CommitWaiter<'a, P: CommitProgress> {
    progress: &'a P,
    waited: P::Mark,
    deadline: tokio::time::Instant,
}
impl<'a, P: CommitProgress> CommitWaiter<'a, P> {
    pub(crate) async fn begin(progress: &'a P, deadline: tokio::time::Instant) -> Self {
        Self {
            progress,
            waited: progress.mark().await,
            deadline,
        }
    }
    /// Wait for the producer's next change, enabled by the caller before it
    /// last probed, or for the deadline.
    pub(crate) async fn changed(
        &self,
        changed: std::pin::Pin<&mut tokio::sync::futures::Notified<'_>>,
    ) -> Result<(), DeadlineEnd> {
        if tokio::time::timeout_at(self.deadline, changed)
            .await
            .is_ok()
        {
            return Ok(());
        }
        Err(if self.progress.stalled_since(self.waited).await {
            DeadlineEnd::Stalled
        } else {
            DeadlineEnd::Slow
        })
    }
}

impl CommitProgress for CopyRangeProduction {
    type Mark = ProductionMark;
    async fn mark(&self) -> ProductionMark {
        CopyRangeProduction::mark(self).await
    }
    async fn stalled_since(&self, mark: ProductionMark) -> bool {
        CopyRangeProduction::stalled_since(self, mark).await
    }
}

pub(crate) struct CopyRangeProduction {
    pub(crate) files: AsyncMutex<CopyRangeFiles>,
    pub(crate) changed: Notify,
    publisher: CopyRangePublisher,
    stop: CancellationToken,
    outcome: Mutex<Option<Result<(), String>>>,
    bank: AsyncMutex<ReadBank>,
    /// `bank.held`, readable without the bank's lock, and whether a wait
    /// that saw no commit met a stuck producer.
    stall: ProductionStall,
    seek_bootstrap: crate::source_input::SourceSeekBootstrap,
    wanted_ms: i64,
}
impl CopyRangeProduction {
    /// Must be retained by the actual serving actor until wait_stopped joins
    /// every physical operation. HTTP waiter cancellation does not own it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        files: CopyRangeFiles,
        running: RunningCopyGeneration,
        mux: ObservedCopyMux,
        census: Arc<CopyClockCensus>,
        canonical_title_start: u64,
        observer: crate::source_custody::SourceObserver,
        initial_anchor_ms: i64,
        watermarks: (u64, u64),
        seek_bootstrap: crate::source_input::SourceSeekBootstrap,
    ) -> Arc<Self> {
        let publisher = files.publisher();
        let production = Arc::new(Self {
            files: AsyncMutex::new(files),
            changed: Notify::new(),
            publisher,
            stop: CancellationToken::new(),
            outcome: Mutex::new(None),
            seek_bootstrap,
            wanted_ms: initial_anchor_ms,
            bank: AsyncMutex::new(ReadBank {
                observer,
                anchor_ms: initial_anchor_ms,
                snapshot: None,
                observed_at: Instant::now(),
                low_ms: watermarks.0,
                high_ms: watermarks.1,
                held: false,
            }),
            stall: ProductionStall::default(),
        });
        let task = Arc::clone(&production);
        tokio::spawn(async move {
            task.run(running, mux, census, canonical_title_start).await;
        });
        production
    }
    pub(crate) async fn observe_demand(
        &self,
        demand: crate::playback_control::PlaybackDemandSnapshot,
        watermarks: (u64, u64),
    ) -> Result<(), String> {
        {
            let mut bank = self.bank.lock().await;
            bank.anchor_ms = demand.buffer_anchor_ms();
            bank.snapshot = Some(demand);
            bank.observed_at = Instant::now();
            bank.low_ms = watermarks.0;
            bank.high_ms = watermarks.1;
        }
        self.refresh_read_bank().await
    }
    /// Existing Source IO arbitration closes the same gate checked by every
    /// actual positioned read. Holding retains custody/admission; it never
    /// equates a stale lease or paused observer with physical settlement.
    pub(crate) async fn refresh_read_bank(&self) -> Result<(), String> {
        self.refresh_read_bank_inner(true).await
    }
    async fn refresh_read_bank_inner(&self, publish_observation: bool) -> Result<(), String> {
        let mut bank = self.bank.lock().await;
        if bank.low_ms == 0 || bank.high_ms <= bank.low_ms {
            return Err("copy_source_watermarks_invalid".into());
        }
        let age = bank.observed_at.elapsed();
        let position = bank
            .snapshot
            .as_ref()
            .map(|demand| crate::playback_control::rolling_estimated_position_ms(demand, age))
            .unwrap_or(bank.anchor_ms)
            .max(0);
        let fresh = bank.snapshot.as_ref().is_some_and(|demand| {
            age <= std::time::Duration::from_secs(5)
                && demand.render_state == crate::playback_control::RenderState::Rendering
        });
        let files = self.files.lock().await;
        let lead = files.media_origin_ms().map(|origin| {
            let milliseconds = files
                .segments()
                .iter()
                .map(|segment| segment.seconds * 1000.0)
                .sum::<f64>();
            ((origin as f64 + milliseconds - position as f64)
                .floor()
                .max(0.0)
                .min(u64::MAX as f64)) as u64
        });
        let complete = files.complete();
        drop(files);
        if complete {
            return Ok(());
        }
        let hold = lead.is_some_and(|lead| {
            if bank.held {
                lead > bank.low_ms
            } else {
                lead >= bank.high_ms
            }
        });
        if bank.held && !hold {
            bank.observer.refill().await?;
            bank.held = false;
            self.stall.holding().set(false);
            return Ok(());
        }
        // Segment checkpoints perform local arithmetic only until an actual
        // physical hold transition. They never replicate per-segment progress.
        if hold == bank.held && !publish_observation {
            return Ok(());
        }
        // The mirror shows the decided hold while the observer records it,
        // so a wait judged meanwhile sees production pausing, not stuck.
        self.stall.holding().set(hold);
        if let Err(error) = bank
            .observer
            .rolling(lead, bank.low_ms, bank.high_ms, hold, fresh)
            .await
        {
            self.stall.holding().set(bank.held);
            return Err(error);
        }
        bank.held = hold;
        Ok(())
    }
    pub(crate) fn is_live(&self) -> bool {
        !self.stop.is_cancelled() && self.failed().is_none()
    }
    fn committed(files: &CopyRangeFiles) -> usize {
        files.segments().len() + usize::from(files.canonical_init().is_some())
    }
    pub(crate) async fn mark(&self) -> ProductionMark {
        let files = self.files.lock().await;
        self.stall.mark(Self::committed(&files))
    }
    /// Whether production is stuck since `mark` ([`ProductionStall`]).
    pub(crate) async fn stalled_since(&self, mark: ProductionMark) -> bool {
        let live = self.is_live();
        let files = self.files.lock().await;
        self.stall
            .stalled_since(mark, Self::committed(&files), files.complete(), live)
    }
    pub(crate) fn is_holding(&self) -> bool {
        self.stall.holding().is_held()
    }
    /// A viewer asked for media at the production head while the bank
    /// held: the viewer is at least there, whatever its last demand
    /// snapshot said (a control exchange refused, a player not rendering, an
    /// extrapolation capped). Re-anchor the bank's position to the produced
    /// end and decide the hold again; until production commits again, a
    /// hold no longer excuses a wait.
    pub(crate) async fn reanchor_at_head(&self) -> Result<(), String> {
        let (end, committed) = {
            let files = self.files.lock().await;
            let end = files.media_origin_ms().map(|origin| {
                let produced = files
                    .segments()
                    .iter()
                    .map(|segment| segment.seconds * 1000.0)
                    .sum::<f64>();
                (origin as f64 + produced).floor() as i64
            });
            (end, Self::committed(&files))
        };
        let Some(end) = end else {
            return Ok(());
        };
        {
            let mut bank = self.bank.lock().await;
            bank.anchor_ms = end;
            bank.snapshot = None;
            bank.observed_at = Instant::now();
        }
        self.stall.reanchored(committed);
        self.refresh_read_bank_inner(true).await
    }
    pub(crate) fn retire(&self) {
        self.publisher.retire();
        self.stop.cancel();
        self.changed.notify_waiters();
    }
    pub(crate) fn failed(&self) -> Option<String> {
        self.outcome
            .lock()
            .expect("copy range outcome")
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .cloned()
    }
    pub(crate) async fn wait_stopped(&self) -> Result<(), String> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.outcome.lock().expect("copy range outcome").clone() {
                return result;
            }
            changed.await;
        }
    }
    pub(crate) async fn mark_rendered(&self) -> Result<Option<Arc<CopyRangeCheckpoint>>, String> {
        let result = self.files.lock().await.mark_rendered(&self.stop).await?;
        self.changed.notify_waiters();
        Ok(result)
    }
    /// `forward_ms` comes from the existing transport/rate policy. The title
    /// request is measured from the container format start, independently of
    /// the signed AV presentation origin. A short EOF needs no invented media.
    pub(crate) async fn wait_eligible(
        &self,
        requested_ms: i64,
        forward_ms: u64,
        deadline: Instant,
    ) -> Result<i64, String> {
        if requested_ms < 0 || forward_ms == 0 {
            return Err("copy_range_start_invalid".into());
        }
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let files = self.files.lock().await;
                if let Some(origin) = files.media_origin_ms() {
                    let duration = files
                        .segments()
                        .iter()
                        .map(|segment| segment.seconds)
                        .sum::<f64>();
                    if duration.is_finite() {
                        let end = origin as f64 + duration * 1000.0;
                        let request = requested_ms as f64;
                        if !files.segments().is_empty()
                            && end > request
                            && (end - request >= forward_ms as f64 || files.complete())
                        {
                            return Ok(origin);
                        }
                        if files.complete() {
                            return Err("copy_range_seek_past_end".into());
                        }
                    }
                }
            }
            if let Some(error) = self.failed() {
                return Err(error);
            }
            tokio::select! {biased;()=self.stop.cancelled()=>return Err("copy_range_cancelled".into()),()=tokio::time::sleep_until(tokio::time::Instant::from_std(deadline))=>return Err("copy_range_start_deadline".into()),()=changed=>{}}
        }
    }
    async fn apply(
        &self,
        events: Vec<CopyMuxEvent>,
        census: &CopyClockCensus,
        canonical_title_start: u64,
        meter: &mut ProductionMeter,
    ) -> Result<(), String> {
        // Segments after the first from one child output are batched.
        let mut batched = false;
        for event in events {
            let committing = tokio::time::Instant::now();
            let mut published = None;
            let mut files = self.files.lock().await;
            match event {
                CopyMuxEvent::Init { canonical, av } => {
                    files.set_init(canonical, av, &self.stop).await?
                }
                CopyMuxEvent::Origin {
                    first_packet,
                    canonical_dts,
                    av_dts,
                    timescale,
                } => {
                    census.verify_seek_landing(first_packet, self.wanted_ms)?;
                    files.set_origin(census.media_origin_ms(
                        canonical_dts,
                        canonical_title_start,
                        av_dts,
                        timescale,
                    )?)?;
                    self.seek_bootstrap.verified_landing();
                }
                CopyMuxEvent::Segment(output) => {
                    let index = output.segment.output_index;
                    let seconds = output.segment.seconds;
                    let bytes = output.segment.media.bytes.len() as u64;
                    let media_start_ms = files.media_origin_ms().unwrap_or(0) as f64
                        + files
                            .segments()
                            .iter()
                            .map(|segment| segment.seconds * 1000.0)
                            .sum::<f64>();
                    files.commit_segment(output, &self.stop).await?;
                    published = Some((
                        index,
                        media_start_ms,
                        media_start_ms + seconds * 1000.0,
                        bytes,
                    ));
                }
            }
            drop(files);
            self.refresh_read_bank_inner(false).await?;
            self.changed.notify_waiters();
            meter.commit(committing.elapsed());
            if let Some((index, start, end, bytes)) = published {
                meter.segment(
                    tokio::time::Instant::now(),
                    index,
                    start,
                    end,
                    bytes,
                    batched,
                );
                batched = true;
            }
        }
        Ok(())
    }
    async fn run(
        self: Arc<Self>,
        mut running: RunningCopyGeneration,
        mut mux: ObservedCopyMux,
        census: Arc<CopyClockCensus>,
        canonical_title_start: u64,
    ) {
        let (incarnation, range_id) = self.files.lock().await.log_identity();
        let mut meter = ProductionMeter::new(incarnation, range_id, tokio::time::Instant::now());
        // The advertised target duration: a wait longer than one segment's
        // worth of wall time is worth a line, whatever the cause.
        let target = std::time::Duration::from_secs(u64::from(
            plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS,
        ));
        let observed=async{
            loop {
                let pipes=running.pipes.as_mut().ok_or("copy_output_readers_missing")?;
                let waiting=tokio::time::Instant::now();
                let holds=self.stall.holding().mark();
                let next=await_reporting_stall(pipes.next(),target,|waited|{
                    meter.stalled("child_output",waited,self.stall.holding().held_since(holds));
                });
                let event=tokio::select!{biased;()=self.stop.cancelled()=>return Err("copy_range_cancelled".to_owned()),event=next=>event.ok_or("copy_output_readers_lost")?};
                meter.child(waiting.elapsed());
                let eof=pipes.all_eof();
                let converting=tokio::time::Instant::now();
                let events=mux.feed(event)?;
                meter.convert(converting.elapsed());
                self.apply(events,&census,canonical_title_start,&mut meter).await?;
                if eof{break;}
            }
            running.slot.request_registered_completion(&running.registration).await.map_err(|_|"copy_reap_unavailable".to_owned())?;
            let status=tokio::select!{biased;()=self.stop.cancelled()=>return Err("copy_range_cancelled".to_owned()),status=running.registration.wait_child_exit()=>status};
            if !status.success(){return Err("copy_child_failed".into());}
            running.join_completed_outputs().await?;
            let (tail,inspection)=mux.finish()?;
            self.apply(tail,&census,canonical_title_start,&mut meter).await?;
            // The child is successful and every output task/parser is joined
            // before EOF publication. The registered writer remains retained
            // until after this final atomic commit acknowledges below.
            self.files.lock().await.finish(inspection,&self.stop).await?;
            self.changed.notify_waiters();
            Ok::<(),String>(())
        }.await;
        let observed = observed.map_err(|error| {
            let reported = landing_failure(&self.seek_bootstrap, error.clone());
            // A failed range otherwise surfaces only as a later Unavailable
            // status or control answer; the cause is named once, here, with
            // the producer's own code before any landing relabel.
            if error != "copy_range_cancelled" {
                tracing::warn!(target: "plurxd::transcode", reason = %error, reported = %reported, wanted_ms = self.wanted_ms, "Source copy range production failed");
            }
            reported
        });
        if observed.is_err() {
            self.publisher.retire();
        }
        let settled = running.settle(observed.is_ok()).await;
        let result = match (observed, settled) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(receipt)) if !receipt.exit_success() => Err("copy_child_failed".into()),
            (Ok(()), Ok(_)) => Ok(()),
        };
        meter.finished(tokio::time::Instant::now(), &result);
        *self.outcome.lock().expect("copy range outcome") = Some(result);
        self.changed.notify_waiters();
    }
}

#[cfg(test)]
mod commit_wait_tests {
    use super::{CommitProgress, CommitWaiter, DeadlineEnd, ProductionMark, ProductionStall};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;
    use tokio::sync::Notify;

    /// A producer's commit count, judged by the real [`ProductionStall`] a
    /// [`super::CopyRangeProduction`] owns: only its range and child are
    /// stood in for.
    #[derive(Default)]
    struct Producer {
        committed: AtomicUsize,
        stall: ProductionStall,
        changed: Notify,
    }
    impl Producer {
        fn commit(&self) {
            self.committed.fetch_add(1, Ordering::SeqCst);
            self.changed.notify_waiters();
        }
        fn committed(&self) -> usize {
            self.committed.load(Ordering::SeqCst)
        }
    }
    impl CommitProgress for Producer {
        type Mark = ProductionMark;
        async fn mark(&self) -> ProductionMark {
            self.stall.mark(self.committed())
        }
        async fn stalled_since(&self, mark: ProductionMark) -> bool {
            self.stall
                .stalled_since(mark, self.committed(), false, true)
        }
    }

    /// The Source resource wait window.
    const WINDOW: Duration = Duration::from_secs(25);

    /// One resource request for the `wanted`th commit, as the growing
    /// segment wait makes it: probe, then wait for a change or the deadline.
    async fn request(producer: &Producer, wanted: usize) -> Result<usize, DeadlineEnd> {
        let waiter = CommitWaiter::begin(producer, tokio::time::Instant::now() + WINDOW).await;
        loop {
            let changed = producer.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if producer.committed() >= wanted {
                return Ok(producer.committed());
            }
            waiter.changed(changed).await?;
        }
    }

    /// A stuck producer with a player polling it ends within one wait
    /// window: the first request that waits a whole window over no commit
    /// is stuck, however often the player asks.
    #[tokio::test(start_paused = true)]
    async fn a_stalled_producer_ends_within_one_wait_window() {
        let producer = Producer::default();
        producer.commit();
        let began = tokio::time::Instant::now();
        assert_eq!(request(&producer, 2).await, Err(DeadlineEnd::Stalled));
        assert_eq!(began.elapsed(), WINDOW);
    }

    /// A producer slower than the player, committing a segment every 20 s
    /// while the player waits 25 s for one 60 s out, passes each window it
    /// advanced in and serves the segment once committed.
    #[tokio::test(start_paused = true)]
    async fn a_slow_but_advancing_producer_stays_transient() {
        let producer = Arc::new(Producer::default());
        let producing = Arc::clone(&producer);
        let task = tokio::spawn(async move {
            for _ in 0..3 {
                tokio::time::sleep(Duration::from_secs(20)).await;
                producing.commit();
            }
        });
        let mut outcomes = Vec::new();
        loop {
            let outcome = request(&producer, 3).await;
            let served = outcome.is_ok();
            outcomes.push(outcome);
            if served {
                break;
            }
        }
        assert_eq!(
            outcomes,
            [Err(DeadlineEnd::Slow), Err(DeadlineEnd::Slow), Ok(3)]
        );
        task.await.expect("producer");
    }

    /// A producer holding ahead of its viewer by design commits nothing,
    /// and that is not a stall, whether the hold began before the wait or
    /// during it.
    #[tokio::test(start_paused = true)]
    async fn a_holding_producer_is_not_stuck() {
        let producer = Producer::default();
        producer.stall.holding().set(true);
        assert_eq!(request(&producer, 1).await, Err(DeadlineEnd::Slow));
        producer.stall.holding().set(false);
        let waiter = CommitWaiter::begin(&producer, tokio::time::Instant::now() + WINDOW).await;
        producer.stall.holding().set(true);
        producer.stall.holding().set(false);
        let changed = producer.changed.notified();
        tokio::pin!(changed);
        assert_eq!(waiter.changed(changed).await, Err(DeadlineEnd::Slow));
    }

    /// A hold is the bank's decision from its last demand snapshot, not a
    /// sign the producer is alive. Once the bank is re-anchored to a viewer
    /// waiting at the head, a hold that persists excuses nothing until
    /// production commits again; a commit restores the excuse.
    #[tokio::test(start_paused = true)]
    async fn a_reanchored_hold_that_persists_is_stuck() {
        let producer = Producer::default();
        producer.commit();
        producer.stall.holding().set(true);
        assert_eq!(request(&producer, 2).await, Err(DeadlineEnd::Slow));
        producer.stall.reanchored(producer.committed());
        assert_eq!(request(&producer, 2).await, Err(DeadlineEnd::Stalled));
        producer.commit();
        assert_eq!(request(&producer, 3).await, Err(DeadlineEnd::Slow));
    }

    /// A copy that finished or stopped committed nothing too, and is not
    /// stuck: the callers' own checks end those.
    #[test]
    fn only_a_running_unfinished_copy_can_be_stuck() {
        let stall = ProductionStall::default();
        let mark = stall.mark(4);
        assert!(stall.stalled_since(mark, 4, false, true));
        assert!(!stall.stalled_since(mark, 4, true, true));
        assert!(!stall.stalled_since(mark, 4, false, false));
        assert!(!stall.stalled_since(mark, 5, false, true));
    }
}
