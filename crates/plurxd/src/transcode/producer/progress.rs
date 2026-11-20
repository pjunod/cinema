use super::*;
use plurx_core::process::observation::{Metric, UnknownReason};
use plurx_core::resource_observation::{
    ServiceObservation, ServiceUnit, ServiceWindow, WorkPhase, WorkSample,
};

/// Live encode telemetry from FFmpeg motion and completed rolling output.
///
/// Without it, "slow" and "stalled" are the same observation. The only signal
/// the session machinery had was whether a finished segment was listed yet —
/// a yes/no answer to a question that needs a rate. A 4K HDR session
/// tone-mapping at 0.7x and a session whose GPU has wedged look identical for
/// the first twelve seconds, and the watchdog killed both, restarting the
/// merely-slow one on software that is slower still. The actor now consumes
/// these measurements directly and owns the only progress deadline.
#[derive(Debug)]
pub struct Progress {
    /// Monotonic zero point for `moved_at_ms`.
    pub(super) started: Instant,
    /// Which spawn attempt owns these numbers.
    ///
    /// A killed attempt's stdout reader does not stop the instant the process
    /// does — it can still be draining buffered lines while the replacement is
    /// already running. Without a generation, one of those late lines lands on
    /// the new attempt's telemetry, and a stale `out_time` from a process that
    /// got further along reads as "produced, then frozen": a healthy new
    /// encoder declared stalled by its dead predecessor.
    pub(super) generation: AtomicU64,
    /// Content produced so far, in ms of output timeline; `-1` before the
    /// first block. Relative to the session's own `-ss`, because an input seek
    /// restarts output timestamps at zero.
    ///
    /// This is ENCODER progress, not published media: it includes frames muxed
    /// into the in-progress `.tmp` segment, which no client can fetch. The
    /// producer actor wants exactly that (proof of motion); pacing must not
    /// use it (that is what the segment index is for).
    pub(super) out_time_ms: AtomicI64,
    /// Cumulative encode rate x1000 (`speed=1.85x` -> 1850); `-1` when unknown.
    pub(super) speed_milli: AtomicI64,
    /// `started.elapsed()` when `out_time_ms` last *moved*. Staleness is
    /// measured from here rather than from the last block received: a stuck
    /// ffmpeg keeps emitting blocks, it just stops advancing.
    pub(super) moved_at_ms: AtomicI64,
    /// Baseline for the recent-rate delta: wall clock and output time at the
    /// last usable sample, or [`SAMPLE_UNSET`] before there is one. A sample
    /// too close to it to measure leaves it in place; a sample that spans a
    /// suspend replaces it.
    pub(super) sample_wall_ms: AtomicI64,
    pub(super) sample_out_ms: AtomicI64,
    /// Smoothed recent rate x1000; `-1` until two usable samples exist.
    pub(super) recent_milli: AtomicI64,
    rolling_service: std::sync::Mutex<RollingService>,
}

#[derive(Debug, Default)]
struct RollingService {
    generation: u64,
    flow_revision: u64,
    held: Option<WorkPhase>,
    /// The rolling index keeps historical entries after pruning their files.
    verified_segments: usize,
    verified_end_ms: u64,
    window: ServiceWindow,
}

impl RollingService {
    fn observe(&mut self, phase: WorkPhase, at: Instant) {
        self.window.observe(WorkSample {
            generation: self.generation,
            phase,
            unit: ServiceUnit::MediaMilliseconds,
            completed: self.verified_end_ms,
            deadline_slack_ms: Metric::Unknown(UnknownReason::InsufficientEvidence),
            queued_bytes: Metric::Unknown(UnknownReason::InsufficientEvidence),
            at,
        });
    }

    fn extend(&mut self, index: &SegmentIndex) -> bool {
        let start = self.verified_segments;
        let mut previous = start.checked_sub(1).and_then(|i| index.segs.get(i));
        for segment in index.segs.get(start..).into_iter().flatten() {
            if segment.bytes <= 0
                || !segment.visibility.is_advertised()
                || segment.end_ms <= segment.start_ms
                || previous.map_or(segment.start_ms != 0, |earlier| {
                    earlier.index.checked_add(1) != Some(segment.index)
                        || earlier.end_ms != segment.start_ms
                })
            {
                break;
            }
            self.verified_segments += 1;
            self.verified_end_ms = segment.end_ms as u64;
            previous = Some(segment);
        }
        self.verified_segments > start
    }
}

/// One exponential-moving-average step over a progress sample, or `None` when
/// the sample should not move the average at all.
///
/// Pure, because the two rejection rules are the whole subtlety and they are
/// invisible in a test that has to sleep to produce a sample: a gap longer
/// than the cutoff spans a suspend (dividing its content by the stopped time
/// invents a slowdown the moment a held session resumes), and a gap shorter
/// than the floor is two adjacent ffmpeg blocks whose jitter is larger than
/// the signal.
pub(super) fn recent_rate_step(prev_ewma: i64, d_wall_ms: i64, d_out_ms: i64) -> Option<i64> {
    if !(RECENT_SAMPLE_MIN_MS..=RECENT_SAMPLE_MAX_GAP_MS).contains(&d_wall_ms) || d_out_ms < 0 {
        return None;
    }
    let instant = (d_out_ms * 1000) / d_wall_ms;
    Some(if prev_ewma < 0 {
        instant
    } else {
        // Weighted toward history: a single slow segment on a variable-bitrate
        // film should bend the number, not spike it.
        (prev_ewma * 7 + instant * 3) / 10
    })
}

impl Progress {
    pub fn new() -> Progress {
        Progress {
            started: Instant::now(),
            generation: AtomicU64::new(0),
            out_time_ms: AtomicI64::new(-1),
            speed_milli: AtomicI64::new(-1),
            moved_at_ms: AtomicI64::new(0),
            sample_wall_ms: AtomicI64::new(SAMPLE_UNSET),
            sample_out_ms: AtomicI64::new(SAMPLE_UNSET),
            recent_milli: AtomicI64::new(-1),
            rolling_service: std::sync::Mutex::new(RollingService::default()),
        }
    }

    /// Start a new attempt: forget everything measured so far and return the
    /// generation the new process's reader must quote to be believed.
    ///
    /// Called when a session respawns (the hardware->software fallback). The
    /// replacement writes its own timeline from the same seek point, so
    /// carrying the dead process's numbers would make its actor progress
    /// deadline look expired -- and the generation bump is what stops the dead
    /// process's reader from putting them back.
    pub fn begin_attempt(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Relaxed) + 1;
        self.reset_attempt(generation);
        generation
    }

    /// Reset the compatibility telemetry to an attempt allocated by the
    /// rolling actor. Offline producers still use [`Self::begin_attempt`]; a
    /// live session must share the actor's attempt fence with publication and
    /// response commits so a predecessor cannot mutate its successor.
    pub(super) fn begin_fenced_attempt(&self, generation: u64) {
        self.generation.store(generation, Relaxed);
        self.reset_attempt(generation);
        if let Ok(mut service) = self.rolling_service.lock() {
            *service = RollingService {
                generation,
                ..RollingService::default()
            };
        }
    }

    pub(super) fn rolling_flow_revision(&self, generation: u64) -> Option<u64> {
        let service = self.rolling_service.lock().ok()?;
        (generation != 0 && service.generation == generation).then_some(service.flow_revision)
    }

    /// An applied signal breaks the output rate interval before another
    /// index refresh can commit. A resume waits for a new output baseline.
    pub(super) fn note_rolling_flow(&self, generation: u64, held: Option<WorkPhase>, at: Instant) {
        let Ok(mut service) = self.rolling_service.lock() else {
            return;
        };
        if generation == 0 || service.generation != generation {
            return;
        }
        service.flow_revision = service.flow_revision.wrapping_add(1);
        service.held = held;
        service.window = ServiceWindow::default();
        if let Some(phase) = held {
            service.observe(phase, at);
        }
    }

    pub(super) fn note_rolling_segments(
        &self,
        generation: u64,
        flow_revision: u64,
        index: &SegmentIndex,
        rebuilt: bool,
        input_paced: bool,
        at: Instant,
    ) {
        let Ok(mut service) = self.rolling_service.lock() else {
            return;
        };
        if service.generation != generation {
            return;
        }
        if rebuilt {
            service.verified_segments = 0;
            service.verified_end_ms = 0;
            service.window = ServiceWindow::default();
        }
        if service.flow_revision == flow_revision && service.extend(index) {
            let phase = service.held.unwrap_or(if input_paced {
                WorkPhase::InputPaced
            } else {
                WorkPhase::Producing
            });
            service.observe(phase, at);
        }
    }

    pub(super) fn rolling_resource_service(
        &self,
        generation: u64,
        now: Instant,
    ) -> Option<ServiceObservation> {
        let service = self.rolling_service.lock().ok()?;
        (service.generation == generation)
            .then(|| service.window.snapshot(now))
            .flatten()
    }

    pub(super) fn reset_attempt(&self, generation: u64) {
        debug_assert!(generation > 0);
        self.out_time_ms.store(-1, Relaxed);
        self.speed_milli.store(-1, Relaxed);
        self.recent_milli.store(-1, Relaxed);
        self.sample_wall_ms.store(SAMPLE_UNSET, Relaxed);
        self.sample_out_ms.store(SAMPLE_UNSET, Relaxed);
        self.moved_at_ms
            .store(self.started.elapsed().as_millis() as i64, Relaxed);
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation.load(Relaxed)
    }

    pub(super) fn note_out_time(&self, ms: i64) {
        self.note_out_time_at(self.started.elapsed().as_millis() as i64, ms);
    }

    /// [`Self::note_out_time`] at an explicit reading of the attempt's wall
    /// clock, so the sampler's baseline rules can be tested as a sequence
    /// without sleeping between blocks.
    pub(super) fn note_out_time_at(&self, now: i64, ms: i64) {
        if self.out_time_ms.swap(ms, Relaxed) == ms {
            return; // a repeated timestamp is not progress
        }
        self.moved_at_ms.store(now, Relaxed);

        // Recent rate: content produced per second of wall clock, smoothed.
        // ffmpeg's own `speed=` is cumulative over the whole session, which
        // hides a slowdown behind a fast start and reads as nonsense across a
        // suspend -- and it is the recent number that predicts whether the
        // viewer's reserve is about to drain.
        let last_wall = self.sample_wall_ms.load(Relaxed);
        let last_out = self.sample_out_ms.load(Relaxed);
        let have_baseline = last_wall != SAMPLE_UNSET && last_out != SAMPLE_UNSET;
        if have_baseline && now - last_wall < RECENT_SAMPLE_MIN_MS {
            // Too close to the baseline to measure, so the baseline stays.
            // The content this block reports is real: moving the baseline
            // past it would drop that content from every later sample, and a
            // late block followed by its catch-up block would read as a
            // producer slower than it is -- low enough to fail a healthy
            // 1.0x session's capacity check.
            return;
        }
        self.sample_wall_ms.store(now, Relaxed);
        self.sample_out_ms.store(ms, Relaxed);
        if !have_baseline {
            return; // the first sample only establishes a baseline
        }
        let prev = self.recent_milli.load(Relaxed);
        if let Some(next) = recent_rate_step(prev, now - last_wall, ms - last_out) {
            self.recent_milli.store(next, Relaxed);
        }
    }

    /// How long output has not advanced. Also the answer before the first
    /// block ever arrives, which is what makes a session that never opened its
    /// input measurable by the same rule as one that died halfway.
    pub(super) fn stalled_for(&self) -> Duration {
        let now = self.started.elapsed().as_millis() as i64;
        Duration::from_millis((now - self.moved_at_ms.load(Relaxed)).max(0) as u64)
    }

    pub fn out_time_ms(&self) -> Option<i64> {
        Some(self.out_time_ms.load(Relaxed)).filter(|v| *v >= 0)
    }

    pub fn speed(&self) -> Option<f64> {
        self.speed_milli().map(|v| v as f64 / 1000.0)
    }

    pub(super) fn speed_milli(&self) -> Option<i64> {
        Some(self.speed_milli.load(Relaxed)).filter(|v| *v >= 0)
    }

    /// The rate over the last few seconds, which is the one that predicts a
    /// stall. Falls back to nothing rather than to the cumulative figure --
    /// reporting a session's lifetime average as "recent" is how a slowdown
    /// stays invisible.
    pub fn recent_speed(&self) -> Option<f64> {
        self.recent_speed_milli().map(|v| v as f64 / 1000.0)
    }

    pub(super) fn recent_speed_milli(&self) -> Option<i64> {
        Some(self.recent_milli.load(Relaxed)).filter(|v| *v >= 0)
    }

    /// Restart the motion clock without touching anything measured.
    ///
    /// Called when a suspended session is resumed. `moved_at_ms` stopped
    /// advancing the moment the SIGSTOP landed — correctly, nothing was
    /// moving — so the first stall check after SIGCONT would otherwise read
    /// the whole suspension as "output has not advanced for minutes" and fail
    /// a healthy session that simply had not emitted its first post-resume
    /// progress block yet. The recent-rate sampler needs no equivalent: a
    /// sample spanning the suspension is already rejected by
    /// [`RECENT_SAMPLE_MAX_GAP_MS`].
    pub(super) fn touch(&self) {
        self.moved_at_ms
            .store(self.started.elapsed().as_millis() as i64, Relaxed);
    }
}

#[cfg(test)]
mod rolling_service_tests {
    use super::*;

    #[test]
    fn rolling_verified_service_excludes_uncommitted_tail_and_fences_flow_and_replacement() {
        let progress = Progress::new();
        progress.begin_fenced_attempt(7);
        let now = Instant::now();
        let at = |seconds| now + Duration::from_secs(seconds);
        let first_flow = progress.rolling_flow_revision(7).expect("original flow");
        let mut index = SegmentIndex {
            segs: parse_playlist(concat!(
                "#EXTM3U\n#EXTINF:1,\nseg00017.ts\n#EXTINF:1,\nseg00018.ts\n",
                "#EXTINF:1,\nseg00019.ts\n#EXTINF:1,\nseg00020.ts\n",
            )),
            revision: 0,
        };
        assert_eq!(index.segs.len(), 4);
        let note_original = |index: &SegmentIndex, rebuilt, seconds| {
            progress.note_rolling_segments(7, first_flow, index, rebuilt, false, at(seconds));
        };
        let observe_original = |seconds| progress.rolling_resource_service(7, at(seconds));

        index.segs[0].bytes = 128;
        index.segs[2].bytes = 128;
        note_original(&index, false, 0);
        let baseline = observe_original(0).expect("first complete segment");
        assert_eq!(baseline.completed, 1000);
        assert_eq!(
            baseline.units_per_second,
            Metric::Unknown(UnknownReason::InsufficientEvidence)
        );

        note_original(&index, false, 3);
        assert_eq!(
            observe_original(16)
                .expect("stale original sample")
                .units_per_second,
            Metric::Unknown(UnknownReason::Stale)
        );

        index.segs[1].bytes = 128;
        note_original(&index, false, 4);
        index.segs[3].bytes = 128;
        note_original(&index, false, 5);
        let produced = observe_original(5).expect("sustained original output");
        assert_eq!(produced.completed, 4000);
        assert_eq!(produced.units_per_second, Metric::Known(600));

        progress.note_rolling_flow(7, Some(WorkPhase::Paused), at(6));
        let held = observe_original(6).expect("applied hold");
        assert_eq!(held.phase, WorkPhase::Paused);
        assert_eq!(
            held.units_per_second,
            Metric::Unknown(UnknownReason::InsufficientEvidence)
        );

        progress.note_rolling_flow(7, None, at(7));
        note_original(&index, true, 8);
        assert!(observe_original(8).is_none());

        progress.begin_fenced_attempt(8);
        note_original(&index, false, 9);
        assert!(observe_original(9).is_none());
        let next_flow = progress.rolling_flow_revision(8).expect("replacement flow");
        progress.note_rolling_segments(8, next_flow, &index, false, true, at(10));
        let paced = progress
            .rolling_resource_service(8, at(10))
            .expect("replacement only");
        assert_eq!(paced.phase, WorkPhase::InputPaced);
        assert_eq!(paced.advancing_samples, 0);
        assert_eq!(
            paced.units_per_second,
            Metric::Unknown(UnknownReason::InsufficientEvidence)
        );
    }
}
