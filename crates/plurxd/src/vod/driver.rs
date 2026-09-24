use super::*;

pub(super) fn spawn_driver(
    shared: Arc<Shared>,
    rendition: Arc<Rendition>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let encoded_waiting = rendition
                .recipe
                .encoding
                .as_ref()
                .is_some_and(|encoding| encoding.is_waiting());
            let stopped_encoder = rendition.recipe.encoding.is_some()
                && matches!(rendition.slot.belief().await, Producer::Stopped { .. });
            if encoded_waiting || stopped_encoder {
                // One owned driver, not one retry task per GET. Pool releases
                // outside VOD cannot notify this registry. A queued rendition
                // retries admission quickly; a stopped encoder polls slowly
                // enough to yield inside the live start's five-second budget.
                let poll = if encoded_waiting {
                    Duration::from_millis(250)
                } else {
                    #[cfg(test)]
                    rendition.stopped_poll_armed.notify_one();
                    STOPPED_ENCODER_POLL
                };
                tokio::select! {
                    _ = rendition.wake.notified() => {},
                    _ = tokio::time::sleep(poll) => {},
                }
                #[cfg(test)]
                if stopped_encoder && !encoded_waiting {
                    rendition.stopped_poll_fired.notify_one();
                }
            } else {
                rendition.wake.notified().await;
            }
            if rendition.closed.load(Relaxed) {
                if let Some(encoding) = &rendition.recipe.encoding {
                    encoding.cancel_wait();
                }
                rendition.gen_epoch.fetch_add(1, Relaxed);
                let _ = perform_driver_step(
                    &shared,
                    &rendition,
                    Step::Terminate {
                        why: Termination::Idle,
                    },
                )
                .await;
                break;
            }
            driver_pass(&shared, &rendition).await;
        }
    })
}

/// Reclaim what a rendition that has already failed is still holding.
///
/// A recorded failure is first-wins and never cleared for the life of the
/// rendition, and `record_failure` answers every waiter — the pool, and the
/// init `Notify` — at the moment it lands. From that instant the producer is
/// serving nobody and no later pass can change that.
///
/// The bare early return that used to stand in `driver_pass` left two things
/// behind. The child was one. `Action::Idle` is what normally reclaims a
/// producer, and it is only ever reached through the pass that return skipped,
/// so what remained were the dormant purge — which refuses an admitted
/// rendition outright — and the last `Arc<Rendition>` drop, which an in-flight
/// generation task or a parked materialize watchdog can defer for as long as
/// they live. On an admitted rendition that combination reaps nothing: a live
/// ffmpeg, or a SIGSTOP'd one still sitting on its codec session, stayed on the
/// node until the process exited.
///
/// The capacity hold is the other, and it is worth being exact about what it
/// is not. `capacity_hold` is written in exactly one place — the pass below —
/// so the value from the last pass before the failure latched for the life of
/// the rendition. It changes nothing a client sees: `status` answers `failed`
/// ahead of every belief arm, so the stale hold was already shielded from the
/// wire. Clearing it here removes dead state that contradicts the rendition it
/// belongs to, so that the next reader of it — a status reordering, an
/// operator surface, a decision that consults it — is not the one that has to
/// discover it was never true.
///
/// `Termination::Idle` is the existing spelling for "reclaim it"; `after` reads
/// nothing from the `why` and no wire carries it, so this is not a claim that a
/// failed rendition is idle.
async fn retire_failed_rendition(shared: &Shared, rendition: &Arc<Rendition>) {
    *rendition.capacity_hold.lock().expect("capacity hold") = None;
    if matches!(rendition.slot.belief().await, Producer::Absent { .. }) {
        return;
    }
    // Ignored exactly as the dormant purge and generation-end terminations
    // ignore it: a slot that lost its child between the belief read and here
    // is the outcome this asked for.
    let _ = perform_driver_step(
        shared,
        rendition,
        Step::Terminate {
            why: Termination::Idle,
        },
    )
    .await;
}

fn record_performed_step(shared: &Shared, rendition: &Rendition, step: Step) {
    match step {
        Step::Stop => rendition.ahead_hold.store(true, Release),
        Step::Resume | Step::Start { .. } | Step::Restart { .. } => {
            rendition.ahead_hold.store(false, Release);
        }
        Step::Terminate {
            why: Termination::YieldToWaiter,
        } => rendition.ahead_hold.store(true, Release),
        _ => {}
    }
    let why = match step {
        Step::Terminate {
            why: Termination::Idle,
        } => Some(VodProducerTermination::Idle),
        Step::Terminate {
            why: Termination::IndefiniteHold,
        } => Some(VodProducerTermination::IndefiniteHold),
        Step::Terminate {
            why: Termination::YieldToWaiter,
        } => Some(VodProducerTermination::YieldToWaiter),
        Step::Restart { .. } => Some(VodProducerTermination::Restart),
        _ => None,
    };
    if let Some(why) = why {
        shared.pool.metrics_handle().count_producer_termination(why);
    }
}

pub(super) fn notify_new_vod_live_wait(
    shared: &Arc<Shared>,
    encoding: &crate::vodencode::Encoding,
    was_waiting: bool,
    admitted: bool,
) {
    if !admitted && !was_waiting && encoding.has_live_wait() {
        // Registration is node-wide news: a stopped encoded rendition may be
        // holding exactly this permit. Policy-read retries are deliberately
        // excluded because they registered no capacity waiter.
        shared.kick_all();
    }
}

pub(super) async fn perform_driver_step(
    shared: &Shared,
    rendition: &Rendition,
    step: Step,
) -> std::io::Result<Performed> {
    let performed = rendition.slot.perform(step, || {}).await?;
    record_performed_step(shared, rendition, step);
    Ok(performed)
}

/// One pass: demand → decide → step → carry it out.
pub(super) async fn driver_pass(shared: &Arc<Shared>, rendition: &Arc<Rendition>) {
    let mut prepared_permit = None;
    loop {
        if rendition.closed.load(Relaxed) {
            if let Some(encoding) = &rendition.recipe.encoding {
                encoding.cancel_wait();
            }
            return;
        }
        if rendition.failure().is_some() {
            if let Some(encoding) = &rendition.recipe.encoding {
                encoding.cancel_wait();
            }
            rendition.gen_epoch.fetch_add(1, Relaxed);
            clear_marker_prewarm_dispatch(rendition);
            retire_failed_rendition(shared, rendition).await;
            return;
        }
        let belief = rendition.slot.belief().await;
        // Reader windows may require asynchronous state reads. Take that
        // snapshot before the manifest lock so cached GET publication never
        // waits behind policy or admission I/O.
        let windows = eviction_windows(shared, rendition).await;
        let mut manifest = rendition.manifest.lock().await;
        let (demands, prewarm_ledgers) = {
            let readers = rendition.readers.lock().await;
            let demands = playback_demands(&shared.pool, rendition, &readers, &manifest);
            let ledgers = readers
                .values()
                .map(|reader| Arc::clone(&reader.marker_prewarm))
                .collect::<Vec<_>>();
            (demands, ledgers)
        };
        let position = Position {
            produced_through: belief.produced_through(),
            positioned_at: belief.positioned_at(),
            seconds_per_segment: rendition.seconds_per_segment,
            ahead_held: rendition.ahead_hold.load(Acquire),
            working_set: WorkingSet {
                used_bytes: shared.working_set.load(Relaxed),
                budget_bytes: rendition.working_set_budget,
                held: matches!(
                    belief,
                    Producer::Stopped {
                        reason: crate::prodsched::Hold::WorkingSetFull { .. }
                            | crate::prodsched::Hold::NoRoom { .. },
                        ..
                    }
                ),
            },
        };
        let decision =
            decide_with_marker_prewarm(&manifest, &demands, position, &windows, &prewarm_ledgers);
        // Record only this pass's decision. A later non-capacity decision
        // clears the reason, so the status cannot outlive the condition that
        // produced it.
        *rendition.capacity_hold.lock().expect("capacity hold") = match decision.action {
            Action::Suspend { reason, .. } if !crate::prodexec::clears_on_its_own(reason) => {
                Some(reason)
            }
            _ => None,
        };
        let step = retire_completed_marker_prewarm(
            rendition,
            belief,
            &decision,
            next_step(belief, decision.action),
        );
        let contention = rendition.recipe.encoding.as_ref().map_or(
            Contention {
                live_waiting: false,
                holds_permit: false,
            },
            |encoding| Contention {
                live_waiting: encoding.admissions.live_is_waiting(),
                holds_permit: true,
            },
        );
        let step = yield_step(belief, step, contention);
        if matches!(step, Step::Start { .. } | Step::Restart { .. }) && prepared_permit.is_none() {
            if let Some(encoding) = &rendition.recipe.encoding {
                // The old child may own this pool's only permit. Retire it before
                // admission, but never hold the manifest over process or Store I/O:
                // already-materialized GETs and in-flight publication must drain.
                if matches!(step, Step::Restart { .. }) {
                    rendition.gen_epoch.fetch_add(1, Relaxed);
                    clear_marker_prewarm_dispatch(rendition);
                }
                drop(manifest);
                if matches!(step, Step::Restart { .. }) {
                    match perform_driver_step(shared, rendition, step).await {
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(rendition = %rendition.key, "retiring encoder before admission: {error}");
                            return;
                        }
                    }
                }
                let was_waiting = encoding.has_live_wait();
                prepared_permit = encoding.try_permit().await;
                notify_new_vod_live_wait(shared, encoding, was_waiting, prepared_permit.is_some());
                if prepared_permit.is_none() {
                    return;
                }
                // Admission is not permission to execute the old decision. A
                // seek/cancellation/publication may have changed it while waiting.
                // Re-read producer belief and current admitted/accepted demand.
                continue;
            }
        }
        if fence_marker_prewarm_before_room(rendition, belief, step) {
            // Capacity belongs to blocked foreground demand. Fence and retire a
            // speculative generation before freeing bytes, so queued prewarm
            // output cannot consume the room between this pass and the next.
            let terminate = Step::Terminate {
                why: Termination::IndefiniteHold,
            };
            match perform_driver_step(shared, rendition, terminate).await {
                Ok(_) => {}
                Err(error) => {
                    tracing::debug!(rendition = %rendition.key, "retiring prewarm producer: {error}");
                }
            }
            rendition.kick();
            return;
        }
        update_marker_prewarm_dispatch(rendition, belief, step, &decision);
        if !matches!(step, Step::Start { .. } | Step::Restart { .. }) {
            if let Some(encoding) = &rendition.recipe.encoding {
                encoding.cancel_wait();
            }
        }
        match step {
            Step::Nothing => {}
            Step::Stop | Step::Resume => {
                // TODO(m3-wire): session progress clock — the manager's motion
                // clock replaces this no-op touch when it attaches.
                match perform_driver_step(shared, rendition, step).await {
                    Ok(_) => {}
                    Err(error) => {
                        clear_marker_prewarm_dispatch(rendition);
                        tracing::warn!(rendition = %rendition.key, "performing {step:?}: {error}");
                    }
                }
            }
            Step::Terminate { .. } => {
                rendition.gen_epoch.fetch_add(1, Relaxed);
                match perform_driver_step(shared, rendition, step).await {
                    Ok(_) => {}
                    Err(error) => {
                        tracing::debug!(rendition = %rendition.key, "performing {step:?}: {error}");
                    }
                }
            }
            Step::Start { .. } | Step::Restart { .. } => {
                rendition.gen_epoch.fetch_add(1, Relaxed);
                drop(manifest);
                match perform_driver_step(shared, rendition, step).await {
                    Ok(Performed::NeedsSpawn { at }) => {
                        spawn_generation(shared, rendition, at, prepared_permit.take()).await;
                    }
                    Ok(Performed::Done) => {}
                    Err(error) => {
                        clear_marker_prewarm_dispatch(rendition);
                        tracing::warn!(rendition = %rendition.key, "performing {step:?}: {error}");
                    }
                }
            }
            Step::MakeRoom { wanted } => {
                match rendition
                    .dir
                    .make_room(&mut manifest, &windows, wanted)
                    .await
                {
                    Ok(freed) => {
                        sub_saturating(&shared.working_set, freed.bytes);
                        if let Some(error) = freed.error {
                            tracing::warn!(
                                rendition = %rendition.key,
                                freed = freed.bytes,
                                "eviction sweep stopped early: {error}"
                            );
                        }
                        // Room may now exist; decide again promptly — and the
                        // freed bytes are node-wide news, so every other
                        // rendition's driver re-examines its hold too.
                        if freed.bytes > 0 {
                            rendition.kick();
                            shared.kick_all();
                        } else if !matches!(belief, Producer::Absent { .. }) {
                            // Protected bytes make this an indefinite capacity
                            // hold. Fence queued writes and release the producer;
                            // leaving it running would immediately exceed the
                            // same bound that the zero-progress sweep proved.
                            rendition.gen_epoch.fetch_add(1, Relaxed);
                            match perform_driver_step(
                                shared,
                                rendition,
                                Step::Terminate {
                                    why: Termination::IndefiniteHold,
                                },
                            )
                            .await
                            {
                                Ok(_) => {}
                                Err(error) => {
                                    tracing::warn!(rendition = %rendition.key, "terminating producer after a zero-progress capacity sweep: {error}");
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(rendition = %rendition.key, "make_room: {error}");
                    }
                }
            }
            Step::Report { hold } => {
                tracing::warn!(rendition = %rendition.key, "producer stalled: {hold:?}");
            }
        }
        break;
    }
}

/// Preserve all admitted obligations, using accepted control solely to rank
/// current playback ahead of requests outside its buffer window.
pub(super) fn playback_demands(
    pool: &WaitPool,
    rendition: &Rendition,
    readers: &HashMap<String, Reader>,
    manifest: &Manifest,
) -> Vec<Demand> {
    let blocked = pool.demands(&rendition.key);
    let mut nearest = HashMap::new();
    let mut oldest = HashMap::new();
    for request in &blocked {
        // Publication precedes pool.satisfy. A newly materialized nearest
        // request must not hide the next missing GET during that interval.
        if !manifest
            .state(request.index)
            .is_some_and(|state| !state.is_materialized())
        {
            continue;
        }
        let first = oldest
            .entry(&request.session)
            .or_insert(request.arrival_order);
        *first = (*first).min(request.arrival_order);
        if let Some(reader) = readers.get(&request.session).filter(|reader| {
            reader.control_sequence.is_some()
                && reader_window(reader, rendition.seconds_per_segment).covers(request.index)
        }) {
            let distance = request.index.abs_diff(reader.frontier);
            let closest = nearest.entry(&request.session).or_insert(distance);
            *closest = (*closest).min(distance);
        }
    }
    let mut demands = blocked
        .iter()
        .map(|blocked| {
            let mut demand = Demand::waiting_on(blocked.index);
            demand.arrival_order = Some(blocked.arrival_order);
            demand.foreground = match (readers.get(&blocked.session), nearest.get(&blocked.session))
            {
                (Some(reader), Some(nearest)) => {
                    blocked.index.abs_diff(reader.frontier) == *nearest
                }
                _ => oldest.get(&blocked.session) == Some(&blocked.arrival_order),
            };
            demand
        })
        .collect::<Vec<_>>();
    demands.extend(
        readers
            .values()
            .map(|reader| Demand::idle_at(reader.frontier)),
    );
    demands
}

pub(super) async fn eviction_windows(shared: &Shared, rendition: &Rendition) -> Vec<ReaderWindow> {
    let mut windows = rendition.reader_windows().await;
    windows.extend(
        shared
            .pool
            .retained(&rendition.key)
            .into_iter()
            .map(|index| ReaderWindow {
                back: 0,
                playhead: index,
                frontier: index,
                ahead: 0,
            }),
    );
    windows
}
