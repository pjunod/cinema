//! Where one processed DV window's wall time went.
//!
//! Each window gets one clock. The daemon marks the stage boundaries it sees
//! (source-offset lane, helper and encoder spawn and exit, capture,
//! verification); the helper writes its own stage rows to `stages.tsv` in the
//! window's evidence directory (open, probe, seek, decoder init, first window
//! picture, GPU init, render). The window ends with one INFO line,
//! `processed window stage timings`, on every outcome, so a deadline miss on
//! a deployed build shows how far the window got.
//!
//! Diagnostic only. A mark is recorded once when a stage ends and nothing
//! reads one back to decide anything: the window's bounds, deadline and
//! outcome are exactly what they are without the clock. The timings are not
//! part of the window's receipt, which is publication proof.
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;

/// One stage boundary of a processed window, as the daemon observes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowStage {
    /// The window asked for its recipe's one-permit source-offset lane.
    LaneRequested,
    LaneAcquired,
    HelperSpawned,
    EncoderSpawned,
    /// The helper's supervised completion settled (any outcome).
    HelperExited,
    /// The encoder's exit status was observed.
    EncoderExited,
    /// Both children were reaped and the private output was accepted.
    Captured,
    /// Observation, finalize, completed plan and receipt all succeeded.
    Verified,
}
const STAGES: usize = 8;

/// Counters and stage ends the helper reported in `stages.tsv`, in
/// microseconds since the helper started. A window stopped at its deadline
/// has only the rows it reached; `last` names the furthest one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HelperStages {
    pub(crate) opened_us: Option<u64>,
    pub(crate) probed_us: Option<u64>,
    pub(crate) seeked_us: Option<u64>,
    pub(crate) decoders_us: Option<u64>,
    pub(crate) first_frame_us: Option<u64>,
    pub(crate) gpu_ready_us: Option<u64>,
    pub(crate) rendered_us: Option<u64>,
    pub(crate) complete_us: Option<u64>,
    /// Decoded pictures discarded before the window start (keyframe preroll).
    pub(crate) preroll: u32,
    pub(crate) paired: u32,
    pub(crate) emitted: u32,
    pub(crate) last: String,
}

impl HelperStages {
    /// Rows are `name\tmicros\tpreroll\tpaired\temitted`. Parsing stops at the
    /// first malformed row and keeps what came before; no rows is `None`.
    pub(crate) fn parse(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut stages = Self::default();
        let mut rows = 0;
        for line in text.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            let [name, micros, preroll, paired, emitted] = fields[..] else {
                break;
            };
            let (Ok(micros), Ok(preroll), Ok(paired), Ok(emitted)) = (
                micros.parse::<u64>(),
                preroll.parse::<u32>(),
                paired.parse::<u32>(),
                emitted.parse::<u32>(),
            ) else {
                break;
            };
            if name.is_empty()
                || name.len() > 32
                || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            {
                break;
            }
            let slot = match name {
                "opened" => Some(&mut stages.opened_us),
                "probed" => Some(&mut stages.probed_us),
                "seeked" => Some(&mut stages.seeked_us),
                "decoders" => Some(&mut stages.decoders_us),
                "first_frame" => Some(&mut stages.first_frame_us),
                "gpu_ready" => Some(&mut stages.gpu_ready_us),
                "rendered" => Some(&mut stages.rendered_us),
                "complete" => Some(&mut stages.complete_us),
                _ => None,
            };
            if let Some(slot) = slot {
                slot.get_or_insert(micros);
            }
            (stages.preroll, stages.paired, stages.emitted) = (preroll, paired, emitted);
            name.clone_into(&mut stages.last);
            rows += 1;
        }
        (rows > 0).then_some(stages)
    }
}

/// One window's stage marks, shared by the window's owner, its streaming
/// spawn and its private-output capture.
#[derive(Debug)]
pub(crate) struct WindowStageClock {
    started: Instant,
    marks: std::sync::Mutex<[Option<Duration>; STAGES]>,
    budget: std::sync::Mutex<Option<Duration>>,
    helper: std::sync::Mutex<Option<HelperStages>>,
    /// The window's place in a window run, `(index, windows)`. A later run
    /// window reuses the run's lane and helper, so it marks neither.
    run: std::sync::Mutex<Option<(u8, u8)>>,
}

impl WindowStageClock {
    pub(crate) fn start() -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(),
            marks: std::sync::Mutex::new([None; STAGES]),
            budget: std::sync::Mutex::new(None),
            helper: std::sync::Mutex::new(None),
            run: std::sync::Mutex::new(None),
        })
    }
    /// Record that `stage` ended now. The first mark of a stage stands.
    pub(crate) fn mark(&self, stage: WindowStage) {
        let elapsed = self.started.elapsed();
        let mut marks = self
            .marks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        marks[stage as usize].get_or_insert(elapsed);
    }
    /// Record that `stage` ended at `at`, which may precede the window's own
    /// start (a run's pre-started encoder): that marks it at the start.
    pub(crate) fn mark_at(&self, stage: WindowStage, at: Instant) {
        let elapsed = at.saturating_duration_since(self.started);
        let mut marks = self
            .marks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        marks[stage as usize].get_or_insert(elapsed);
    }
    /// Time from the window's start to the end of `stage`, if it was reached.
    pub(crate) fn at(&self, stage: WindowStage) -> Option<Duration> {
        self.marks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[stage as usize]
    }
    /// The window's deadline, recorded as the budget it left from the start.
    pub(crate) fn deadline(&self, deadline: Instant) {
        self.budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert(deadline.saturating_duration_since(self.started));
    }
    /// Record that this window is window `index` of a `windows`-window run.
    pub(crate) fn run_window(&self, index: u8, windows: u8) {
        self.run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert((index, windows));
    }
    pub(crate) fn helper_reported(&self, stages: Option<HelperStages>) {
        if let Some(stages) = stages {
            self.helper
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .replace(stages);
        }
    }

    pub(crate) fn report(&self, source_open: Option<Duration>) -> WindowStageReport {
        let between =
            |from: WindowStage, to: WindowStage| Some(self.at(to)?.saturating_sub(self.at(from)?));
        let helper = self
            .helper
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let helper_between = |from: Option<u64>, to: Option<u64>| {
            Some(Duration::from_micros(to?.saturating_sub(from?)))
        };
        let (helper_open, helper_probe, helper_seek, decoder_init, preroll, gpu_init, render) =
            helper
                .as_ref()
                .map_or((None, None, None, None, None, None, None), |h| {
                    (
                        h.opened_us.map(Duration::from_micros),
                        helper_between(h.opened_us, h.probed_us),
                        helper_between(h.probed_us, h.seeked_us),
                        helper_between(h.seeked_us.or(h.probed_us), h.decoders_us),
                        helper_between(h.decoders_us, h.first_frame_us),
                        helper_between(h.first_frame_us, h.gpu_ready_us),
                        helper_between(h.gpu_ready_us, h.rendered_us),
                    )
                });
        WindowStageReport {
            budget: *self
                .budget
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            source_open,
            lane_wait: between(WindowStage::LaneRequested, WindowStage::LaneAcquired),
            helper_spawned: self.at(WindowStage::HelperSpawned),
            helper_open,
            helper_probe,
            helper_seek,
            decoder_init,
            preroll,
            preroll_pictures: helper.as_ref().map(|h| h.preroll),
            paired_pictures: helper.as_ref().map(|h| h.paired),
            emitted_pictures: helper.as_ref().map(|h| h.emitted),
            gpu_init,
            render,
            helper_stage: helper.map(|h| h.last),
            helper_exited: self.at(WindowStage::HelperExited),
            encoder_tail: between(WindowStage::HelperExited, WindowStage::EncoderExited),
            verify: between(WindowStage::Captured, WindowStage::Verified),
            run: *self
                .run
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            total: self.started.elapsed(),
        }
    }
}

/// The values of one `processed window stage timings` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowStageReport {
    /// Deadline budget from the window's start (8 s for VOD).
    pub(crate) budget: Option<Duration>,
    /// Source custody open before the window started (outside the budget).
    pub(crate) source_open: Option<Duration>,
    pub(crate) lane_wait: Option<Duration>,
    /// Window start to helper spawn: checks, encoder recipe and lane wait.
    pub(crate) helper_spawned: Option<Duration>,
    /// Helper-reported, from helper start: demuxer open over source custody.
    pub(crate) helper_open: Option<Duration>,
    pub(crate) helper_probe: Option<Duration>,
    pub(crate) helper_seek: Option<Duration>,
    /// VA-API device and decoder open.
    pub(crate) decoder_init: Option<Duration>,
    /// Decode from the keyframe to the first window picture.
    pub(crate) preroll: Option<Duration>,
    pub(crate) preroll_pictures: Option<u32>,
    pub(crate) paired_pictures: Option<u32>,
    pub(crate) emitted_pictures: Option<u32>,
    /// Vulkan and libplacebo start-up at the first window picture.
    pub(crate) gpu_init: Option<Duration>,
    /// GPU ready to the last window picture written to the encoder pipe.
    pub(crate) render: Option<Duration>,
    pub(crate) helper_stage: Option<String>,
    /// Window start to helper exit.
    pub(crate) helper_exited: Option<Duration>,
    /// Helper exit to encoder exit.
    pub(crate) encoder_tail: Option<Duration>,
    /// Observation, finalize, completed plan and receipt.
    pub(crate) verify: Option<Duration>,
    /// `(index, windows)` within a window run; `None` for a single window.
    pub(crate) run: Option<(u8, u8)>,
    pub(crate) total: Duration,
}

/// Milliseconds with one decimal, or `-` for a stage the window never reached.
struct Ms(Option<Duration>);
impl std::fmt::Display for Ms {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(duration) => write!(f, "{:.1}", duration.as_secs_f64() * 1000.0),
            None => f.write_str("-"),
        }
    }
}
/// `index/windows` for a run window, `-` for a single window.
struct RunAt(Option<(u8, u8)>);
impl std::fmt::Display for RunAt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some((index, windows)) => write!(f, "{index}/{windows}"),
            None => f.write_str("-"),
        }
    }
}
struct Count(Option<u32>);
impl std::fmt::Display for Count {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(count) => write!(f, "{count}"),
            None => f.write_str("-"),
        }
    }
}

impl WindowStageReport {
    /// The window's one INFO line. `outcome` is `completed`, `cancelled` or
    /// `failed`; `cause` is the failure text.
    pub(crate) fn log(
        &self,
        entry: u32,
        mode: plurx_core::transcode::dv_processing::DvProcessingMode,
        outcome: &str,
        cause: Option<&str>,
    ) {
        tracing::info!(target: "plurxd::dv",
            entry, mode = ?mode, outcome, cause = cause.unwrap_or("-"),
            run_window = %RunAt(self.run),
            budget_ms = %Ms(self.budget),
            source_open_ms = %Ms(self.source_open),
            lane_wait_ms = %Ms(self.lane_wait),
            helper_spawned_ms = %Ms(self.helper_spawned),
            helper_open_ms = %Ms(self.helper_open),
            helper_probe_ms = %Ms(self.helper_probe),
            helper_seek_ms = %Ms(self.helper_seek),
            decoder_init_ms = %Ms(self.decoder_init),
            preroll_ms = %Ms(self.preroll),
            preroll_pictures = %Count(self.preroll_pictures),
            paired_pictures = %Count(self.paired_pictures),
            emitted_pictures = %Count(self.emitted_pictures),
            gpu_init_ms = %Ms(self.gpu_init),
            render_ms = %Ms(self.render),
            helper_stage = self.helper_stage.as_deref().unwrap_or("-"),
            helper_exited_ms = %Ms(self.helper_exited),
            encoder_tail_ms = %Ms(self.encoder_tail),
            verify_ms = %Ms(self.verify),
            total_ms = %Ms(Some(self.total)),
            "processed window stage timings");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "started\t10\t0\t0\t0\nopened\t1000\t0\t0\t0\nprobed\t41000\t0\t0\t0\n\
        seeked\t43000\t0\t0\t0\ndecoders\t120000\t0\t0\t0\nfirst_frame\t2120000\t128\t129\t0\n\
        gpu_ready\t2420000\t128\t129\t0\nrendered\t5420000\t128\t177\t48\ncomplete\t5430000\t128\t177\t48\n";

    #[test]
    fn helper_rows_parse_into_stage_ends_and_final_counters() {
        let stages = HelperStages::parse(FULL.as_bytes()).expect("rows");
        assert_eq!(stages.opened_us, Some(1000));
        assert_eq!(stages.decoders_us, Some(120_000));
        assert_eq!(stages.first_frame_us, Some(2_120_000));
        assert_eq!(stages.rendered_us, Some(5_420_000));
        assert_eq!(
            (stages.preroll, stages.paired, stages.emitted),
            (128, 177, 48)
        );
        assert_eq!(stages.last, "complete");
    }

    #[test]
    fn a_window_stopped_mid_preroll_keeps_the_rows_it_reached() {
        let partial = "started\t10\t0\t0\t0\nopened\t1000\t0\t0\t0\nprobed\t41000\t0\t0\t0\n\
            seeked\t43000\t0\t0\t0\ndecoders\t120000\t0\t0\t0\nfirst_fr";
        let stages = HelperStages::parse(partial.as_bytes()).expect("rows");
        assert_eq!(stages.last, "decoders");
        assert_eq!(stages.first_frame_us, None);
        assert_eq!(HelperStages::parse(b""), None);
        assert_eq!(HelperStages::parse(b"garbage"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn report_splits_daemon_and_helper_stages() {
        let clock = WindowStageClock::start();
        clock.deadline(Instant::now() + Duration::from_secs(8));
        clock.mark(WindowStage::LaneRequested);
        tokio::time::advance(Duration::from_millis(250)).await;
        clock.mark(WindowStage::LaneAcquired);
        clock.mark(WindowStage::HelperSpawned);
        tokio::time::advance(Duration::from_millis(5500)).await;
        clock.mark(WindowStage::HelperExited);
        // The first mark of a stage stands.
        tokio::time::advance(Duration::from_millis(100)).await;
        clock.mark(WindowStage::HelperExited);
        clock.mark(WindowStage::EncoderExited);
        clock.mark(WindowStage::Captured);
        tokio::time::advance(Duration::from_millis(40)).await;
        clock.mark(WindowStage::Verified);
        clock.helper_reported(HelperStages::parse(FULL.as_bytes()));
        let report = clock.report(Some(Duration::from_millis(12)));
        assert_eq!(report.budget, Some(Duration::from_secs(8)));
        assert_eq!(report.source_open, Some(Duration::from_millis(12)));
        assert_eq!(report.lane_wait, Some(Duration::from_millis(250)));
        assert_eq!(report.helper_spawned, Some(Duration::from_millis(250)));
        assert_eq!(report.helper_exited, Some(Duration::from_millis(5750)));
        assert_eq!(report.encoder_tail, Some(Duration::from_millis(100)));
        assert_eq!(report.verify, Some(Duration::from_millis(40)));
        assert_eq!(report.helper_open, Some(Duration::from_millis(1)));
        assert_eq!(report.helper_probe, Some(Duration::from_millis(40)));
        assert_eq!(report.helper_seek, Some(Duration::from_millis(2)));
        assert_eq!(report.decoder_init, Some(Duration::from_millis(77)));
        assert_eq!(report.preroll, Some(Duration::from_secs(2)));
        assert_eq!(report.preroll_pictures, Some(128));
        assert_eq!(report.gpu_init, Some(Duration::from_millis(300)));
        assert_eq!(report.render, Some(Duration::from_secs(3)));
        assert_eq!(report.helper_stage.as_deref(), Some("complete"));
        assert_eq!(report.total, Duration::from_millis(5890));
    }

    #[test]
    fn the_window_line_is_info_and_names_every_stage() {
        use tracing_subscriber::prelude::*;
        let logs = Arc::new(crate::logbuf::LogBuffer::new(16));
        let _capture = tracing::subscriber::set_default(
            tracing_subscriber::registry().with(crate::logbuf::BufferLayer(Arc::clone(&logs))),
        );
        let clock = WindowStageClock::start();
        clock.mark(WindowStage::LaneRequested);
        clock.mark(WindowStage::LaneAcquired);
        clock.helper_reported(HelperStages::parse(
            b"started\t10\t0\t0\t0\nopened\t1000\t0\t0\t0\n",
        ));
        clock.report(None).log(
            306,
            plurx_core::transcode::dv_processing::DvProcessingMode::BaseRpu,
            "failed",
            Some("DV private window deadline expired"),
        );
        let lines: Vec<String> = logs
            .tail("info", 16)
            .into_iter()
            .map(|entry| entry.message)
            .filter(|message| message.contains("processed window stage timings"))
            .collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = &lines[0];
        for field in [
            "entry=306",
            "outcome=\"failed\"",
            "deadline expired",
            "budget_ms=-",
            "source_open_ms=-",
            "lane_wait_ms=",
            "helper_spawned_ms=-",
            "helper_open_ms=1.0",
            "helper_probe_ms=-",
            "helper_seek_ms=",
            "decoder_init_ms=",
            "preroll_ms=",
            "preroll_pictures=0",
            "paired_pictures=",
            "emitted_pictures=",
            "gpu_init_ms=",
            "render_ms=",
            "helper_stage=\"opened\"",
            "helper_exited_ms=-",
            "encoder_tail_ms=",
            "verify_ms=",
            "total_ms=",
        ] {
            assert!(line.contains(field), "{field} missing from {line}");
        }
    }
}
