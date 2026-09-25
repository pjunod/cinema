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

/// An encoded rendition that wanted to start was refused an encoder permit.
///
/// A prepared successor (speculative priority) never registers a pool waiter,
/// so nothing a running producer checks would ever make room for it, and on a
/// full pool the viewer's switch could only fail. The one permit it may claim
/// is its own viewer's: ask the predecessor that staged exactly this successor
/// to hand it over, and keep this driver polling until it is claimed.
async fn report_permit_wait(
    shared: &Arc<Shared>,
    rendition: &Arc<Rendition>,
    encoding: &crate::vodencode::Encoding,
) {
    let handoff = if encoding.is_speculative() {
        request_predecessor_handoff(shared, rendition, encoding).await
    } else {
        Err("not_prepared")
    };
    encoding.handoff_wait.store(handoff.is_ok(), Relaxed);
    let Some(refusal) = encoding.last_refusal() else {
        // A policy-read failure registered no capacity wait; it is retried
        // and reported by its own path.
        return;
    };
    let first = !rendition.permit_wait_logged.swap(true, Relaxed);
    if first {
        tracing::info!(
            target: "plurxd::vodserve",
            rendition = %rendition.key,
            priority = ?refusal.priority,
            hardware_used = refusal.pool.hardware_used,
            hardware_limit = refusal.hardware_limit,
            software_used = refusal.pool.software_used,
            software_budget = refusal.software_budget,
            wants_hardware = encoding.resources.hardware_slot,
            wants_threads = encoding.resources.cpu_threads,
            live_waiters = refusal.pool.live_waiting,
            background_holds_permit = refusal.pool.background_active,
            reservations = refusal.pool.reservations,
            over_budget = refusal.over_budget,
            handoff_from = handoff.as_ref().map_or("none", |(key, _)| key.as_str()),
            handoff_reason = handoff.as_ref().err().copied().unwrap_or("requested"),
            "encoded vod rendition is waiting for an encoder permit"
        );
    } else if let Ok((predecessor, true)) = &handoff {
        tracing::info!(
            target: "plurxd::vodserve",
            rendition = %rendition.key,
            handoff_from = %predecessor,
            "prepared successor asked its predecessor for its encoder permit"
        );
    }
}

/// Ask the predecessor whose preparation slot names this exact successor to
/// give back its encoder permit. `Ok((predecessor key, newly started))`, or
/// the reason no handoff was asked for, which the wait line reports.
///
/// Every refusal here is a reason *not* to preempt anybody:
/// - the refusal is ordinary capacity, not an impossible plan (`over_budget`),
///   a live viewer already waiting (`live_waiter`: speculative work never
///   jumps one), or background work (`background_owner`: it yields to live,
///   never to speculative);
/// - this session was primed as a prepared successor (`not_prepared`), and a
///   session on this node is being replaced by exactly that incarnation right
///   now (`no_local_predecessor`; a predecessor on another node is out of
///   scope);
/// - that predecessor's rendition is read by nobody else (`not_exclusive`),
///   is encoded and holds a process (`predecessor_idle`), and returning its
///   permit is enough for this exact plan (`would_not_fit`).
async fn request_predecessor_handoff(
    shared: &Arc<Shared>,
    successor: &Arc<Rendition>,
    encoding: &crate::vodencode::Encoding,
) -> Result<(String, bool), &'static str> {
    let refusal = encoding.last_refusal().ok_or("policy_unavailable")?;
    if refusal.over_budget {
        return Err("over_budget");
    }
    if refusal.pool.background_active {
        return Err("background_owner");
    }
    if refusal.pool.live_waiting > 0 {
        return Err("live_waiter");
    }
    let (incarnation, bound, predecessor) = {
        let sessions = shared.sessions.lock().await;
        let Some((incarnation, playback_id)) = sessions.values().find_map(|session| {
            let incarnation = session.prepared_incarnation.as_ref()?;
            session
                .live_rendition()
                .filter(|rendition| Arc::ptr_eq(rendition, successor))?;
            Some((incarnation.clone(), session.playback_id.clone()))
        }) else {
            return Err("not_prepared");
        };
        // The sessions being replaced by exactly this incarnation. The store
        // stages a preparation only for its predecessor's own user and
        // playback, so this binding is the account scope as well.
        let bound: HashMap<String, Arc<Rendition>> = sessions
            .iter()
            .filter(|(_, session)| {
                session.playback_id == playback_id
                    && session
                        .control
                        .lock()
                        .expect("control lock")
                        .live_preparation_incarnation()
                        == Some(incarnation.as_str())
            })
            .filter_map(|(id, session)| {
                session
                    .live_rendition()
                    .map(|rendition| (id.clone(), Arc::clone(rendition)))
            })
            .collect();
        let predecessor = bound
            .values()
            .find(|rendition| !Arc::ptr_eq(rendition, successor))
            .cloned();
        (incarnation, bound, predecessor)
    };
    let Some(predecessor) = predecessor else {
        return Err("no_local_predecessor");
    };
    let Some(released) = predecessor.recipe.encoding.as_ref() else {
        return Err("predecessor_idle");
    };
    if predecessor.closed.load(Relaxed) {
        return Err("predecessor_idle");
    }
    let exclusive = predecessor
        .readers
        .lock()
        .await
        .keys()
        .all(|id| bound.contains_key(id));
    if !exclusive {
        return Err("not_exclusive");
    }
    if matches!(predecessor.slot.belief().await, Producer::Absent { .. }) {
        // Already yielded and still parked for this handoff: keep this
        // driver retrying without extending the predecessor's parking.
        return match predecessor.live_handoff() {
            Some(request) if request.incarnation == incarnation => {
                Ok((predecessor.key.clone(), false))
            }
            _ => Err("predecessor_idle"),
        };
    }
    if !encoding.fits_after_release(&released.resources) {
        return Err("would_not_fit");
    }
    let fresh = predecessor.request_handoff(&incarnation, successor, encoding.resources);
    predecessor.kick();
    Ok((predecessor.key.clone(), fresh))
}

/// The handoff this rendition is still bound to, if any: the request is live,
/// its successor still exists, and a session reading this rendition is still
/// being replaced by exactly that incarnation. Anything else releases it.
async fn bound_handoff(shared: &Shared, rendition: &Rendition) -> Option<HandoffRequest> {
    rendition.recipe.encoding.as_ref()?;
    let request = rendition.live_handoff()?;
    let successor_live = request
        .successor
        .upgrade()
        .is_some_and(|successor| !successor.closed.load(Relaxed));
    let bound = successor_live && {
        let readers: Vec<String> = rendition.readers.lock().await.keys().cloned().collect();
        let sessions = shared.sessions.lock().await;
        readers.iter().any(|id| {
            sessions.get(id).is_some_and(|session| {
                session.tombstone.is_none()
                    && session
                        .control
                        .lock()
                        .expect("control lock")
                        .live_preparation_incarnation()
                        == Some(request.incarnation.as_str())
            })
        })
    };
    if bound {
        Some(request)
    } else {
        rendition.release_handoff_unless(None);
        None
    }
}

/// Reserve the successor's capacity before this producer's permit is
/// released, so nothing else can start in the gap before it retries.
fn reserve_for_successor(request: &HandoffRequest) -> Option<Arc<Rendition>> {
    let successor = request.successor.upgrade()?;
    if let Some(encoding) = successor.recipe.encoding.as_ref() {
        let token = encoding
            .admissions
            .reserve_for_handoff(&request.wanted, HANDOFF_RESERVATION_TTL);
        encoding.accept_handoff_claim(token);
    }
    Some(successor)
}

fn finish_handoff_yield(
    shared: &Arc<Shared>,
    rendition: &Rendition,
    successor: Option<Arc<Rendition>>,
) {
    tracing::info!(
        target: "plurxd::vodserve",
        rendition = %rendition.key,
        successor = successor.as_ref().map_or("gone", |successor| successor.key.as_str()),
        "gave this viewer's encoder permit to its prepared successor"
    );
    if let Some(successor) = successor {
        successor.kick();
    }
    shared.kick_all();
}

/// A parked predecessor wakes itself when its handoff lapses, so a viewer GET
/// that arrived meanwhile is not left to its whole block budget.
fn arm_handoff_expiry(rendition: &Arc<Rendition>) {
    if rendition.handoff_expiry_armed.swap(true, AcqRel) {
        return;
    }
    let weak = Arc::downgrade(rendition);
    tokio::spawn(async move {
        loop {
            let Some(rendition) = weak.upgrade() else {
                return;
            };
            let until = rendition.live_handoff().map(|request| request.until);
            match until {
                Some(until) => {
                    drop(rendition);
                    tokio::time::sleep_until(tokio::time::Instant::from_std(until)).await;
                }
                None => {
                    rendition.handoff_expiry_armed.store(false, Release);
                    rendition.kick();
                    return;
                }
            }
        }
    });
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
        let handoff = bound_handoff(shared, rendition).await;
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
        let handoff_requested = handoff.is_some();
        let contention = rendition.recipe.encoding.as_ref().map_or(
            Contention {
                live_waiting: false,
                holds_permit: false,
                handoff_waiting: false,
            },
            |encoding| Contention {
                live_waiting: encoding.admissions.live_is_waiting(),
                holds_permit: true,
                handoff_waiting: handoff_requested,
            },
        );
        let step = yield_step(belief, step, contention);
        let yielding_for_handoff = handoff_requested
            && matches!(
                step,
                Step::Terminate {
                    why: Termination::YieldToWaiter
                }
            );
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
                    let successor = handoff.as_ref().map(reserve_for_successor);
                    match perform_driver_step(shared, rendition, step).await {
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(target: "plurxd::vodserve", rendition = %rendition.key, "retiring encoder before admission: {error}");
                            return;
                        }
                    }
                    if let Some(successor) = successor {
                        finish_handoff_yield(shared, rendition, successor);
                    }
                }
                if handoff_requested {
                    // This viewer's own prepared successor is waiting for
                    // exactly this capacity. Competing for it as live work
                    // would register a waiter the speculative successor can
                    // never pass, and would take back what was just yielded.
                    // The handoff's expiry wakes this driver again.
                    encoding.cancel_wait();
                    arm_handoff_expiry(rendition);
                    return;
                }
                let was_waiting = encoding.has_live_wait();
                prepared_permit = encoding.try_permit().await;
                notify_new_vod_live_wait(shared, encoding, was_waiting, prepared_permit.is_some());
                if prepared_permit.is_none() {
                    report_permit_wait(shared, rendition, encoding).await;
                    return;
                }
                rendition.permit_wait_logged.store(false, Relaxed);
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
                    tracing::debug!(target: "plurxd::vodserve", rendition = %rendition.key, "retiring prewarm producer: {error}");
                }
            }
            rendition.kick();
            return;
        }
        update_marker_prewarm_dispatch(rendition, belief, step, &decision);
        if !matches!(step, Step::Start { .. } | Step::Restart { .. }) {
            if let Some(encoding) = &rendition.recipe.encoding {
                encoding.cancel_wait();
                rendition.permit_wait_logged.store(false, Relaxed);
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
                        tracing::warn!(
                            target: "plurxd::vodserve",
                            rendition = %rendition.key, "performing {step:?}: {error}"
                        );
                    }
                }
            }
            Step::Terminate { .. } => {
                rendition.gen_epoch.fetch_add(1, Relaxed);
                let successor = handoff
                    .as_ref()
                    .filter(|_| yielding_for_handoff)
                    .map(reserve_for_successor);
                match perform_driver_step(shared, rendition, step).await {
                    Ok(_) => {}
                    Err(error) => {
                        tracing::debug!(
                            target: "plurxd::vodserve",
                            rendition = %rendition.key, "performing {step:?}: {error}"
                        );
                    }
                }
                if let Some(successor) = successor {
                    finish_handoff_yield(shared, rendition, successor);
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
                        tracing::warn!(
                            target: "plurxd::vodserve",
                            rendition = %rendition.key, "performing {step:?}: {error}"
                        );
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
                                target: "plurxd::vodserve",
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
                                    tracing::warn!(target: "plurxd::vodserve", rendition = %rendition.key, "terminating producer after a zero-progress capacity sweep: {error}");
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "plurxd::vodserve",
                            rendition = %rendition.key, "make_room: {error}"
                        );
                    }
                }
            }
            Step::Report { hold } => {
                tracing::warn!(
                    target: "plurxd::vodserve",
                    rendition = %rendition.key, "producer stalled: {hold:?}"
                );
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
