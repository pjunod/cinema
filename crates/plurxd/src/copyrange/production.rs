//! A single already-authorized physical producer and its append-only range.
//! The Source actor retains grant/assignment/lease authority; this owner only
//! joins the child and outputs and commits the verified physical observations.
use super::{
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
pub(crate) struct CopyRangeProduction {
    pub(crate) files: AsyncMutex<CopyRangeFiles>,
    pub(crate) changed: Notify,
    publisher: CopyRangePublisher,
    stop: CancellationToken,
    outcome: Mutex<Option<Result<(), String>>>,
    bank: AsyncMutex<ReadBank>,
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
        });
        let task = Arc::clone(&production);
        tokio::spawn(async move {
            task.run(running, mux, census, canonical_title_start).await;
        });
        production
    }
    /// A production with no generation behind it, for driving the read bank
    /// against a bank the test sets. Nothing produces and nothing settles.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn banked_for_test(
        files: CopyRangeFiles,
        observer: crate::source_custody::SourceObserver,
        watermarks: (u64, u64),
        seek_bootstrap: crate::source_input::SourceSeekBootstrap,
    ) -> Arc<Self> {
        let publisher = files.publisher();
        Arc::new(Self {
            files: AsyncMutex::new(files),
            changed: Notify::new(),
            publisher,
            stop: CancellationToken::new(),
            outcome: Mutex::new(None),
            seek_bootstrap,
            wanted_ms: 0,
            bank: AsyncMutex::new(ReadBank {
                observer,
                anchor_ms: 0,
                snapshot: None,
                observed_at: Instant::now(),
                low_ms: watermarks.0,
                high_ms: watermarks.1,
                held: false,
            }),
        })
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
            return Ok(());
        }
        // Segment checkpoints perform local arithmetic only until an actual
        // physical hold transition. They never replicate per-segment progress.
        if hold == bank.held && !publish_observation {
            return Ok(());
        }
        bank.observer
            .rolling(lead, bank.low_ms, bank.high_ms, hold, fresh)
            .await?;
        bank.held = hold;
        Ok(())
    }
    pub(crate) fn is_live(&self) -> bool {
        !self.stop.is_cancelled() && self.failed().is_none()
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
    ) -> Result<(), String> {
        for event in events {
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
                    files.commit_segment(output, &self.stop).await?;
                }
            }
            drop(files);
            self.refresh_read_bank_inner(false).await?;
            self.changed.notify_waiters();
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
        let observed=async{
            loop {
                let pipes=running.pipes.as_mut().ok_or("copy_output_readers_missing")?;
                let event=tokio::select!{biased;()=self.stop.cancelled()=>return Err("copy_range_cancelled".to_owned()),event=pipes.next()=>event.ok_or("copy_output_readers_lost")?};
                let eof=pipes.all_eof();
                self.apply(mux.feed(event)?,&census,canonical_title_start).await?;
                if eof{break;}
            }
            running.slot.request_registered_completion(&running.registration).await.map_err(|_|"copy_reap_unavailable".to_owned())?;
            let status=tokio::select!{biased;()=self.stop.cancelled()=>return Err("copy_range_cancelled".to_owned()),status=running.registration.wait_child_exit()=>status};
            if !status.success(){return Err("copy_child_failed".into());}
            running.join_completed_outputs().await?;
            let (tail,inspection)=mux.finish()?;
            self.apply(tail,&census,canonical_title_start).await?;
            // The child is successful and every output task/parser is joined
            // before EOF publication. The registered writer remains retained
            // until after this final atomic commit acknowledges below.
            self.files.lock().await.finish(inspection,&self.stop).await?;
            self.changed.notify_waiters();
            Ok::<(),String>(())
        }.await;
        let observed = observed.map_err(|error| landing_failure(&self.seek_bootstrap, error));
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
        *self.outcome.lock().expect("copy range outcome") = Some(result);
        self.changed.notify_waiters();
    }
}
