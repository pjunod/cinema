use super::*;

/// A completed speculative window has no foreground reason to retain its
/// ffmpeg process. The ordinary scheduler would stop it at the ahead horizon;
/// retire it instead once attribution is exhausted, unless foreground demand
/// has explicitly taken ownership of the same generation.
pub(super) fn retire_completed_marker_prewarm(
    rendition: &Rendition,
    belief: Producer,
    decision: &MarkerPrewarmDecision,
    step: Step,
) -> Step {
    let epoch = rendition.gen_epoch.load(Relaxed);
    let prewarm_generation =
        rendition.marker_prewarm_generation.load(Acquire) == epoch.saturating_add(1);
    let ahead_hold = matches!(
        decision.action,
        Action::Suspend {
            reason: crate::prodsched::Hold::Ahead { .. },
            ..
        }
    );
    if prewarm_generation
        && rendition.active_marker_prewarms.load(Acquire) == 0
        && decision.candidate.is_none()
        && ahead_hold
        && !matches!(belief, Producer::Absent { .. })
        && matches!(step, Step::Stop | Step::Nothing)
    {
        Step::Terminate {
            why: Termination::Idle,
        }
    } else {
        step
    }
}

/// Fence a live speculative producer before a foreground room-making pass.
/// Returns true when the caller must physically terminate the old generation
/// and retry scheduling before it may evict anything.
pub(super) fn fence_marker_prewarm_before_room(
    rendition: &Rendition,
    belief: Producer,
    step: Step,
) -> bool {
    if !matches!(step, Step::MakeRoom { .. }) || matches!(belief, Producer::Absent { .. }) {
        return false;
    }
    let epoch = rendition.gen_epoch.load(Relaxed);
    let prewarm_generation =
        rendition.marker_prewarm_generation.load(Acquire) == epoch.saturating_add(1);
    if !prewarm_generation {
        return false;
    }
    clear_marker_prewarm_dispatch(rendition);
    rendition.marker_prewarm_generation.store(0, Release);
    rendition.gen_epoch.fetch_add(1, Relaxed);
    true
}

pub(super) fn decide_with_marker_prewarm(
    manifest: &Manifest,
    demands: &[Demand],
    position: Position,
    readers: &[ReaderWindow],
    prewarm_ledgers: &[Arc<StdMutex<MarkerPrewarmLedger>>],
) -> MarkerPrewarmDecision {
    let foreground_action = decide(manifest, demands, position, readers);
    // Real reader demand is always decided first. Only an idle producer or
    // one that would otherwise stop at the ordinary ahead horizon may spend
    // work on a marker destination. Capacity holds never evict or make room
    // for speculative bytes.
    let selected_prewarm = if matches!(
        foreground_action,
        crate::prodsched::Action::Idle
            | crate::prodsched::Action::Suspend {
                reason: crate::prodsched::Hold::Ahead { .. },
                ..
            }
    ) {
        prewarm_ledgers
            .iter()
            .filter_map(|ledger| {
                ledger
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .candidate(manifest)
            })
            .min_by_key(|candidate| candidate.target_entry)
    } else {
        None
    };
    let action = selected_prewarm
        .map(|candidate| {
            let demand = if candidate.target_materialized {
                Demand::idle_at(candidate.target_entry)
            } else {
                // This internal demand gets the same repositioning decision a
                // blocked GET would, but only after the foreground decision
                // above proved the playhead has no work left.
                Demand::waiting_on(candidate.target_entry)
            };
            (candidate, decide(manifest, &[demand], position, readers))
        })
        .filter(|(_, action)| {
            matches!(
                action,
                crate::prodsched::Action::Produce { .. }
                    | crate::prodsched::Action::Reposition { .. }
            )
        });
    let mut owners = Vec::new();
    for ledger in prewarm_ledgers {
        let mut state = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((candidate, _)) = action {
            for record_nonce in state.activate(candidate) {
                owners.push(MarkerPrewarmOwner {
                    ledger: Arc::clone(ledger),
                    record_nonce,
                });
            }
        } else {
            state.deactivate();
        }
    }
    MarkerPrewarmDecision {
        action: action.map_or(foreground_action, |(_, action)| action),
        candidate: action.map(|(candidate, _)| candidate),
        owners,
    }
}

/// Bind speculative work to the exact producer generation that received it.
/// A later control snapshot may disable future prewarming before an in-flight
/// fragment publishes; in that case retain only the producer's immediate
/// reach. A restart for foreground work is a new cause and drops attribution.
pub(super) fn update_marker_prewarm_dispatch(
    rendition: &Rendition,
    belief: Producer,
    step: Step,
    decision: &MarkerPrewarmDecision,
) {
    let current_epoch = rendition.gen_epoch.load(Relaxed);
    let mut dispatch = rendition
        .marker_prewarm_dispatch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(candidate) = decision.candidate.filter(|_| !decision.owners.is_empty()) {
        let producer_epoch = current_epoch.saturating_add(u64::from(matches!(
            step,
            Step::Start { .. } | Step::Restart { .. }
        )));
        *dispatch = Some(MarkerPrewarmDispatch {
            producer_epoch,
            candidate,
            owners: decision.owners.clone(),
        });
        rendition
            .marker_prewarm_generation
            .store(producer_epoch.saturating_add(1), Release);
    } else {
        let foreground_owns_generation = matches!(
            decision.action,
            Action::Produce { .. } | Action::Reposition { .. }
        );
        if foreground_owns_generation || matches!(step, Step::Terminate { .. }) {
            rendition.marker_prewarm_generation.store(0, Release);
        }
        if foreground_owns_generation {
            // The publication that follows is owed to foreground demand, even
            // when it reuses a process prewarm positioned. Keeping the old
            // owners would let foreground work manufacture a prewarm hit.
            *dispatch = None;
        } else {
            let immediate_reach = matches!(step, Step::Nothing | Step::Resume | Step::Stop)
                .then(|| {
                    belief
                        .produced_through()
                        .map(|through| through.saturating_add(1))
                        .or_else(|| belief.positioned_at())
                })
                .flatten();
            let retain = dispatch.as_mut().is_some_and(|existing| {
                if existing.producer_epoch != current_epoch {
                    return false;
                }
                let Some(reach) = immediate_reach else {
                    return false;
                };
                if reach < existing.candidate.target_entry {
                    return false;
                }
                existing.candidate.window_end_entry =
                    existing.candidate.window_end_entry.min(reach);
                true
            });
            if !retain {
                *dispatch = None;
            }
        }
    }
    rendition.active_marker_prewarms.store(
        dispatch.as_ref().map_or(0, |dispatch| {
            u32::try_from(dispatch.owners.len()).unwrap_or(u32::MAX)
        }),
        Release,
    );
}
