//! Which helper renders a requested processed window: a live window run
//! whose next window it is, a new run over it and its following plan entries,
//! or a single-window helper.
//!
//! The VOD driver still asks for one plan entry at a time and awaits that
//! window's whole helper/encoder/verify/publish cycle. The run is the
//! recipe's: it is continued only by the exact next window of an identical
//! helper invocation under the same graph reservation, and anything else (a
//! seek, a skipped window, a replaced or terminated rendition, a failed or
//! cancelled window, an idle bound) kills it. A killed run's later windows are
//! simply rendered by a new run.
use super::*;

/// How [`run_private_window`] obtains its producer.
pub(super) enum Launch<'a> {
    Single,
    Start(crate::dv_segment::RunLaunch),
    Continue(&'a mut crate::dv_segment::WindowRun),
}

/// How many of `following` (the plan entries after `entry`, in order) one
/// helper may render after `entry`: contiguous video entries on the frame
/// grid, none with more pictures than `entry` (the helper's per-window picture
/// cap is the first window's), at most [`crate::dv_segment::RUN_MAX`] windows.
pub(crate) fn run_entries(
    entry: &plurx_core::segplan::PlanEntry,
    following: &[plurx_core::segplan::PlanEntry],
    frame_ticks: u64,
) -> usize {
    use plurx_core::segplan::PlanEntryKind;
    if frame_ticks == 0 || entry.kind != PlanEntryKind::Video {
        return 0;
    }
    let cap = entry.duration_ticks / frame_ticks;
    let (mut index, mut end) = (entry.index, entry.end_ticks());
    let mut count = 0;
    for next in following.iter().take(crate::dv_segment::RUN_MAX - 1) {
        let frames = next.duration_ticks / frame_ticks;
        if next.kind != PlanEntryKind::Video
            || Some(next.index) != index.checked_add(1)
            || next.start_ticks != end
            || !next.start_ticks.is_multiple_of(frame_ticks)
            || !next.duration_ticks.is_multiple_of(frame_ticks)
            || frames == 0
            || frames > cap
        {
            break;
        }
        (index, end) = (next.index, next.end_ticks());
        count += 1;
    }
    count
}

/// Only a verified window with windows left keeps its run.
pub(crate) fn keeps_run<T>(outcome: &Result<WindowOutcome<T>, String>, has_more: bool) -> bool {
    has_more && matches!(outcome, Ok(WindowOutcome::Completed(_)))
}

impl RuntimeRecipe {
    /// End the session's window run: at once if it is resting, otherwise
    /// when the window holding it returns. Its helper task still settles the
    /// run's lane, offset, reservation and source reader on reap.
    pub(crate) fn end_window_run(&self) {
        self.window_run.end();
    }

    /// The windows after `entry` one helper renders with it.
    fn run_later_windows(
        &self,
        entry: &plurx_core::segplan::PlanEntry,
        following: &[plurx_core::segplan::PlanEntry],
        grid: plurx_core::transcode::VodFrameGrid,
    ) -> Vec<crate::dv_segment::SegmentWindow> {
        let count = run_entries(entry, following, u64::from(grid.denominator));
        let mut windows = Vec::with_capacity(count);
        let Ok(mut previous) = self.window(entry, grid) else {
            return windows;
        };
        for next in &following[..count] {
            let Ok(window) = self.window(next, grid) else {
                break;
            };
            if window.start != previous.end || window.args().is_err() {
                break;
            }
            windows.push(window);
            previous = window;
        }
        windows
    }

    /// Render one requested window's private output, through the live run
    /// when this is its next window.
    ///
    /// Only the window's own refusal, cancellation or deadline fails it. A
    /// run that cannot continue (ended, idle-expired, its helper gone) is
    /// killed and the window is rendered by a fresh run or a single helper,
    /// so a run never turns into a refused episode on its own account.
    pub(super) async fn render_window(
        &self,
        request: crate::dv_segment::StreamingSegmentRequest,
        registration_hook: Option<RegistrationHook>,
        entry: &plurx_core::segplan::PlanEntry,
        following: &[plurx_core::segplan::PlanEntry],
        grid: plurx_core::transcode::VodFrameGrid,
        original_settlement: Option<(&std::sync::atomic::AtomicBool, &mut bool)>,
        next_encoder: Option<(u32, Vec<String>)>,
    ) -> Result<WindowOutcome<PrivateWindow>, String> {
        // An original preparation proves its native settlement window by
        // window, and a run would outlive that proof (as it would a
        // background caller's claim), so it always renders singly.
        if original_settlement.is_some() {
            self.window_run.end();
            return run_private_window(
                request,
                registration_hook,
                Launch::Single,
                original_settlement,
            )
            .await
            .map(|(outcome, _)| outcome);
        }
        let window = request.window.ok_or("processed window has no bounds")?;
        let key = crate::dv_segment::request_key(&request)?;
        let frames = request.request.shape.max_frames;
        // What the run needs to start the next window's encoder the moment
        // this window's is reaped.
        let prestart = next_encoder.map(|(at, encoder_args)| crate::dv_segment::PreStartRequest {
            at,
            encoder_args,
            encoder: request.request.encoder.clone(),
            runtime_cache: request.request.runtime_cache.clone(),
            mode: request.mode,
            decode: request.decode,
            wait: self.preparation_budget,
        });
        // An end requested while this window runs is honoured when it returns.
        let ends = self.window_run.ends();
        let (mut held, run) = self.window_run.take().await;
        if let Some(mut run) = run {
            let continued = run.continues(
                &window,
                frames,
                &key,
                &request.request.admission,
                &request.request.encoder_args,
            ) && run
                .ready(
                    &request.request.cancel,
                    request.request.preparation_deadline,
                )
                .await?;
            if continued {
                let outcome = run_private_window(
                    request,
                    registration_hook,
                    Launch::Continue(&mut run),
                    None,
                )
                .await
                .map(|(outcome, _)| outcome);
                if keeps_run(&outcome, run.has_more()) {
                    if let Some(prestart) = prestart {
                        run.prestart(prestart);
                    }
                    run.rest(self.preparation_budget);
                    self.window_run.store(&mut held, run, ends).await;
                } else {
                    run.kill().await;
                }
                return outcome;
            }
            run.kill().await;
        }
        let later = self.run_later_windows(entry, following, grid);
        let reservation = request.request.admission.reservation();
        // A background caller returns its job and source claim when its
        // window returns; a run would outlive both, so it renders singly.
        let launch = match reservation {
            Some(reservation)
                if !later.is_empty()
                    && !request.background
                    && request.request.source_input.is_some() =>
            {
                let windows = u32::try_from(later.len() + 1).unwrap_or(u32::MAX);
                Launch::Start(crate::dv_segment::RunLaunch {
                    later,
                    reservation,
                    // A backstop only: each window is bounded by its own
                    // deadline (at most one budget) and each rest by the
                    // idle bound (one budget), and either ending kills the
                    // run. N windows and N-1 rests fit in 2N-1 budgets, so
                    // 2N+2 never fails a window on the run's account.
                    deadline: tokio::time::Instant::now()
                        + self.preparation_budget.saturating_mul(2 * windows + 2),
                    key,
                })
            }
            _ => Launch::Single,
        };
        let (outcome, run) =
            match run_private_window(request, registration_hook, launch, None).await {
                Ok((outcome, run)) => (Ok(outcome), run),
                Err(cause) => (Err(cause), None),
            };
        if let Some(mut run) = run {
            if keeps_run(&outcome, run.has_more()) {
                if let Some(prestart) = prestart {
                    run.prestart(prestart);
                }
                run.rest(self.preparation_budget);
                self.window_run.store(&mut held, run, ends).await;
            } else {
                run.kill().await;
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::segplan::{PlanCut, PlanEntry, PlanEntryKind};

    const FRAME: u64 = 1001;

    fn entry(index: u32, start_frame: u64, frames: u64) -> PlanEntry {
        PlanEntry {
            index,
            kind: PlanEntryKind::Video,
            start_ticks: start_frame * FRAME,
            duration_ticks: frames * FRAME,
            est_bytes: 0,
            cut: PlanCut::Clean,
            fragments: 1,
        }
    }

    fn plan(windows: u32) -> Vec<PlanEntry> {
        (0..windows)
            .map(|index| entry(index, u64::from(index) * 48, 48))
            .collect()
    }

    #[test]
    fn a_run_covers_up_to_seven_following_contiguous_entries() {
        let plan = plan(12);
        assert_eq!(run_entries(&plan[3], &plan[4..], FRAME), 7);
        // Bounded by the plan's end.
        assert_eq!(run_entries(&plan[9], &plan[10..], FRAME), 2);
        assert_eq!(run_entries(&plan[11], &[], FRAME), 0);
    }

    #[test]
    fn a_run_stops_before_an_entry_one_helper_cannot_render() {
        let mut plan = plan(6);
        // A shorter final entry is still within the first window's cap.
        plan[5] = entry(5, 5 * 48, 30);
        assert_eq!(run_entries(&plan[0], &plan[1..], FRAME), 5);
        // A longer window exceeds the helper's per-window picture cap.
        let mut longer = self::plan(4);
        longer[2] = entry(2, 96, 49);
        assert_eq!(run_entries(&longer[0], &longer[1..], FRAME), 1);
        // A gap or an index jump is not contiguous.
        let mut gap = self::plan(4);
        gap[2] = entry(2, 97, 48);
        assert_eq!(run_entries(&gap[0], &gap[1..], FRAME), 1);
        let mut jump = self::plan(4);
        jump[2].index = 7;
        assert_eq!(run_entries(&jump[0], &jump[1..], FRAME), 1);
        // The audio tail is no video window.
        let mut tail = self::plan(4);
        tail[3].kind = PlanEntryKind::AudioTail;
        assert_eq!(run_entries(&tail[0], &tail[1..], FRAME), 2);
        // Off the frame grid.
        let mut off = self::plan(3);
        off[1].duration_ticks += 1;
        assert_eq!(run_entries(&off[0], &off[1..], FRAME), 0);
    }

    #[test]
    fn only_a_verified_window_with_windows_left_keeps_its_run() {
        let completed: Result<WindowOutcome<()>, String> = Ok(WindowOutcome::Completed(()));
        assert!(keeps_run(&completed, true));
        assert!(!keeps_run(&completed, false));
        assert!(!keeps_run::<()>(&Ok(WindowOutcome::Cancelled), true));
        assert!(!keeps_run::<()>(
            &Err("renderer refused segment".into()),
            true
        ));
    }
}
