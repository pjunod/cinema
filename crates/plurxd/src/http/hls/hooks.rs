use super::*;

use crate::seam_hooks::HookFuture;

/// The points of the HLS route group that a race test can pause at, delay,
/// fault or observe, and the one alternate path it can choose
/// (TRANSCODE-DECOMPOSITION-PLAN §3.9, M8, Decision D-M8-J).
///
/// The routes are free functions: the only owner every one of them reaches is
/// [`AppState`], which holds these in a [`crate::seam_hooks::HookSlot`] in
/// every build. Production never fills the slot, so every point reads
/// [`NoopHlsRouteHooks`]: the awaited points are ready at their first poll,
/// the faults never fire, the records do nothing and a prepared successor is
/// always primed. The paths below therefore have the same await points in the
/// test and release binaries. A paused or delayed hook's timing is still a
/// test artefact: what this makes identical is the set of await points, not
/// scheduling. `Any` is a supertrait only so a test can reach the test hooks
/// behind the state.
pub(crate) trait HlsRouteHooks: std::any::Any + Send + Sync {
    /// A create has recorded the viewer's ask and holds the revision it
    /// recorded, before it admits serving authority and activates.
    fn after_create_ask_recorded(&self) -> HookFuture<'_>;
    /// Whether the staged-ledger read of a control exchange for
    /// `incarnation_id` fails, in place of its result.
    fn staged_read_fault(&self, incarnation_id: &str) -> bool;
    /// A control exchange that may have dispatched a preparation candidate
    /// for `playback_id` is about to read the pending map to answer.
    fn before_dispatch_answer(&self, playback_id: &str) -> HookFuture<'_>;
    /// A preparation settlement for `incarnation_id` has its executor and
    /// retry deadline, before it settles anything.
    fn before_preparation_settlement(&self, incarnation_id: &str) -> HookFuture<'_>;
    /// Whether the next commit attempt of the settlement for
    /// `incarnation_id` fails transiently, so it backs off and retries.
    fn preparation_settlement_fault(&self, incarnation_id: &str) -> bool;
    /// The detached preparation candidate for `incarnation_id` is finished,
    /// on every exit.
    fn preparation_candidate_finished(&self, incarnation_id: &str);
    /// A preparation candidate for `playback_id` has read its source and is
    /// about to plan.
    fn before_preparation_planning(&self, playback_id: &str) -> HookFuture<'_>;
    /// Whether planning the candidate for `playback_id` is refused, taking the
    /// exit an unplannable candidate takes.
    fn refuses_preparation_planning(&self, playback_id: &str) -> bool;
    /// Whether an admitted candidate stages and primes its successor, as
    /// production always does. `false` stages the durable row only, on this
    /// node, as a selection change: decision tests use synthetic media rows
    /// and never launch ffmpeg.
    fn primes_prepared_successor(&self) -> bool;
    /// A priming successor for `playback_id` is built, before it registers
    /// for cancellation.
    fn before_preparation_registered(&self, playback_id: &str) -> HookFuture<'_>;
    /// A release of `session` has closed its publication fence, before the
    /// durable End.
    fn after_release_fence_closed(&self, session: &str) -> HookFuture<'_>;
    /// A release of `session` has committed its durable tombstone and
    /// projected it locally, before the remote owner or the VOD reader is
    /// told.
    fn after_release_tombstoned(&self, session: &str) -> HookFuture<'_>;
    /// Whether the durable End of `session` is commit-unknown, in place of
    /// the Store's answer.
    fn release_commit_unknown(&self, session: &str) -> bool;
    /// A status request for `session` resolved to a local owner and is about
    /// to query its actor.
    fn status_local_lookup(&self, session: &str);
}

/// What production installs: every point is already ready, no fault fires,
/// nothing is recorded, and every admitted successor is primed.
pub(crate) struct NoopHlsRouteHooks;

impl HlsRouteHooks for NoopHlsRouteHooks {
    fn after_create_ask_recorded(&self) -> HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }

    fn staged_read_fault(&self, _: &str) -> bool {
        false
    }

    fn before_dispatch_answer(&self, _: &str) -> HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }

    fn before_preparation_settlement(&self, _: &str) -> HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }

    fn preparation_settlement_fault(&self, _: &str) -> bool {
        false
    }

    fn preparation_candidate_finished(&self, _: &str) {}

    fn before_preparation_planning(&self, _: &str) -> HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }

    fn refuses_preparation_planning(&self, _: &str) -> bool {
        false
    }

    fn primes_prepared_successor(&self) -> bool {
        true
    }

    fn before_preparation_registered(&self, _: &str) -> HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }

    fn after_release_fence_closed(&self, _: &str) -> HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }

    fn after_release_tombstoned(&self, _: &str) -> HookFuture<'_> {
        Box::pin(crate::seam_hooks::HookReady)
    }

    fn release_commit_unknown(&self, _: &str) -> bool {
        false
    }

    fn status_local_lookup(&self, _: &str) {}
}

/// The route group's slot as [`AppState`] holds it: empty, so it reads
/// [`NoopHlsRouteHooks`], until a test fills it.
pub(crate) fn unfilled_hls_route_hooks() -> Arc<crate::seam_hooks::HookSlot<dyn HlsRouteHooks>> {
    Arc::new(crate::seam_hooks::HookSlot::new(&NoopHlsRouteHooks))
}

/// Records that the detached preparation candidate for one incarnation has
/// finished, when it is dropped on any of the candidate's exits.
pub(super) struct PreparationCandidateFinished<'a> {
    pub(super) hooks: &'a dyn HlsRouteHooks,
    pub(super) incarnation_id: &'a str,
}

impl Drop for PreparationCandidateFinished<'_> {
    fn drop(&mut self) {
        self.hooks
            .preparation_candidate_finished(self.incarnation_id);
    }
}

#[cfg(test)]
type Keyed<T> = std::sync::Mutex<std::collections::HashMap<String, T>>;
#[cfg(test)]
type KeySet = std::sync::Mutex<std::collections::HashSet<String>>;

/// The route group's test hooks: the tables the routes' module statics used
/// to be, keyed by session, incarnation or playback id, now per state.
///
/// Every state built by the test-only [`AppState::new`] holds these with
/// priming turned off, so its admitted candidates stage without priming, as
/// every test build did before, until a test calls
/// [`prime_prepared_successors`]. A state built by the production constructor
/// reads the no-op until a test arms a point on it; arming installs these
/// answering priming as the no-op did, so a test can hold a point on the
/// production path without leaving it.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct HlsRouteTestHooks {
    create_ask_recorded: crate::seam_hooks::PauseSlot,
    staged_read_faults: KeySet,
    dispatch_answer_pauses: Keyed<Arc<crate::seam_hooks::AsyncPause>>,
    dispatch_settle_waits: KeySet,
    preparation_settlement_delays: Keyed<Duration>,
    preparation_settlement_faults: Keyed<usize>,
    completed_preparation_candidates: KeySet,
    preparation_planning_faults: Keyed<(Duration, bool)>,
    preparation_planning_refusals: KeySet,
    primes_prepared_successors: std::sync::atomic::AtomicBool,
    preparation_registration_delays: Keyed<Duration>,
    preparation_registration_pauses: Keyed<Arc<crate::seam_hooks::AsyncPause>>,
    release_fence_pauses: Keyed<Arc<crate::seam_hooks::AsyncPause>>,
    release_tombstone_pauses: Keyed<Arc<crate::seam_hooks::AsyncPause>>,
    release_errors: KeySet,
    status_lookup_observers: Keyed<Arc<std::sync::atomic::AtomicUsize>>,
}

#[cfg(test)]
fn locked<T>(table: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    table
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
fn delayed(delay: Option<Duration>) -> HookFuture<'static> {
    Box::pin(async move {
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
    })
}

#[cfg(test)]
fn held(pause: Option<Arc<crate::seam_hooks::AsyncPause>>) -> HookFuture<'static> {
    Box::pin(async move {
        if let Some(pause) = pause {
            pause.hold().await;
        }
    })
}

#[cfg(test)]
impl HlsRouteHooks for HlsRouteTestHooks {
    fn after_create_ask_recorded(&self) -> HookFuture<'_> {
        self.create_ask_recorded.hold()
    }

    fn staged_read_fault(&self, incarnation_id: &str) -> bool {
        locked(&self.staged_read_faults).remove(incarnation_id)
    }

    fn before_dispatch_answer(&self, playback_id: &str) -> HookFuture<'_> {
        let pause = locked(&self.dispatch_answer_pauses).remove(playback_id);
        let settle = locked(&self.dispatch_settle_waits).remove(playback_id);
        let playback_id = playback_id.to_owned();
        Box::pin(async move {
            if let Some(pause) = pause {
                pause.hold().await;
            }
            if !settle {
                return;
            }
            // Bounded by wall-clock time, as an `AsyncPause` is: a candidate
            // that never finishes lets the exchange go, and the test's own
            // assertion about the pending map then fails.
            let deadline = std::time::Instant::now() + crate::seam_hooks::ASYNC_PAUSE_BOUND;
            while pending_candidate_for_playback(&playback_id).is_some() {
                if std::time::Instant::now() >= deadline {
                    eprintln!(
                        "the dispatch answer stopped waiting for {playback_id}'s candidate after {:?}",
                        crate::seam_hooks::ASYNC_PAUSE_BOUND
                    );
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
    }

    fn before_preparation_settlement(&self, incarnation_id: &str) -> HookFuture<'_> {
        delayed(locked(&self.preparation_settlement_delays).remove(incarnation_id))
    }

    fn preparation_settlement_fault(&self, incarnation_id: &str) -> bool {
        let mut faults = locked(&self.preparation_settlement_faults);
        let Some(remaining) = faults.get_mut(incarnation_id) else {
            return false;
        };
        if *remaining == 0 {
            faults.remove(incarnation_id);
            return false;
        }
        *remaining -= 1;
        true
    }

    fn preparation_candidate_finished(&self, incarnation_id: &str) {
        locked(&self.completed_preparation_candidates).insert(incarnation_id.to_owned());
    }

    fn before_preparation_planning(&self, playback_id: &str) -> HookFuture<'_> {
        let fault = locked(&self.preparation_planning_faults).remove(playback_id);
        if let Some((_, true)) = fault {
            locked(&self.preparation_planning_refusals).insert(playback_id.to_owned());
        }
        delayed(fault.map(|(delay, _)| delay))
    }

    fn refuses_preparation_planning(&self, playback_id: &str) -> bool {
        locked(&self.preparation_planning_refusals).remove(playback_id)
    }

    fn primes_prepared_successor(&self) -> bool {
        self.primes_prepared_successors
            .load(std::sync::atomic::Ordering::Acquire)
    }

    fn before_preparation_registered(&self, playback_id: &str) -> HookFuture<'_> {
        let delay = delayed(locked(&self.preparation_registration_delays).remove(playback_id));
        let pause = held(locked(&self.preparation_registration_pauses).remove(playback_id));
        Box::pin(async move {
            delay.await;
            pause.await;
        })
    }

    fn after_release_fence_closed(&self, session: &str) -> HookFuture<'_> {
        held(locked(&self.release_fence_pauses).remove(session))
    }

    fn after_release_tombstoned(&self, session: &str) -> HookFuture<'_> {
        held(locked(&self.release_tombstone_pauses).remove(session))
    }

    fn release_commit_unknown(&self, session: &str) -> bool {
        locked(&self.release_errors).remove(session)
    }

    fn status_local_lookup(&self, session: &str) {
        if let Some(observer) = locked(&self.status_lookup_observers).get(session) {
            observer.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// `state`'s route test hooks, installed on first use.
///
/// Installing them changes no path by itself: they start out answering
/// [`HlsRouteHooks::primes_prepared_successor`] as the hooks they replace
/// did, so arming any point on a production-built state keeps its admitted
/// candidates priming.
#[cfg(test)]
pub(crate) fn hls_route_test_hooks(state: &AppState) -> &HlsRouteTestHooks {
    let primes = state.hls_route_hooks.get().primes_prepared_successor();
    let hooks: &dyn std::any::Any = state.hls_route_hooks.get_or_install(|| {
        Box::new(HlsRouteTestHooks {
            primes_prepared_successors: std::sync::atomic::AtomicBool::new(primes),
            ..HlsRouteTestHooks::default()
        })
    });
    hooks
        .downcast_ref()
        .expect("the state's HLS route hook slot holds HlsRouteTestHooks")
}

/// Hold the next create on `state` after it records the viewer's ask and
/// before its activation runs.
///
/// The property it exists to prove cannot be observed any other way. `create`
/// carries the revision it *recorded* into the activation rather than one read
/// at activation time, and those two values are identical except in the window
/// between them — so a test that cannot stop inside that window cannot tell the
/// correct implementation from the broken one. Modelled on the replicated
/// store's `ACTIVATION_POINTER_READ_PAUSE`, which exists for the same reason.
/// One create at a time takes the armed pause.
#[cfg(test)]
pub(crate) fn pause_create_after_ask_recorded(
    state: &AppState,
) -> Arc<crate::seam_hooks::AsyncPause> {
    hls_route_test_hooks(state)
        .create_ask_recorded
        .arm("create after the ask is recorded")
}

/// Whether `state`'s admitted preparation candidates stage and prime their
/// successor (`true`, as production always does) or stage the durable row
/// only, on this node, as a selection change (`false`, what
/// [`AppState::new`] sets up).
#[cfg(test)]
pub(crate) fn prime_prepared_successors(state: &AppState, primes: bool) {
    hls_route_test_hooks(state)
        .primes_prepared_successors
        .store(primes, std::sync::atomic::Ordering::Release);
}

#[cfg(test)]
pub(super) fn fail_next_staged_read(state: &AppState, incarnation_id: &str) {
    locked(&hls_route_test_hooks(state).staged_read_faults).insert(incarnation_id.to_owned());
}

/// Make the next control exchange for `playback_id` wait, before it answers,
/// until the candidate it dispatched has finished and dropped its marker.
///
/// The first `staging` clause covers a race the emit rule has with the task
/// it just spawned: the candidate can finish, and its guard drop, before the
/// emit rule reads the pending map. In production that window is a few
/// instructions wide and cannot be widened from outside. Arming this makes
/// the exchange wait for exactly that to have happened, which is the only way
/// the clause is under test rather than merely present.
#[cfg(test)]
pub(super) fn wait_for_the_dispatched_candidate(state: &AppState, playback_id: &str) {
    locked(&hls_route_test_hooks(state).dispatch_settle_waits).insert(playback_id.to_owned());
}

/// Hold the next control exchange for `playback_id` just before it reads
/// the pending map to compose its `preparation` answer.
#[cfg(test)]
pub(super) fn pause_before_dispatch_answer(
    state: &AppState,
    playback_id: &str,
) -> Arc<crate::seam_hooks::AsyncPause> {
    let pause = crate::seam_hooks::AsyncPause::new("exchange before its preparation answer");
    locked(&hls_route_test_hooks(state).dispatch_answer_pauses)
        .insert(playback_id.to_owned(), Arc::clone(&pause));
    pause
}

#[cfg(test)]
pub(super) fn delay_next_preparation_settlement(
    state: &AppState,
    incarnation_id: &str,
    delay: Duration,
) {
    locked(&hls_route_test_hooks(state).preparation_settlement_delays)
        .insert(incarnation_id.to_owned(), delay);
}

#[cfg(test)]
pub(super) fn fail_next_preparation_settlements(
    state: &AppState,
    incarnation_id: &str,
    count: usize,
) {
    locked(&hls_route_test_hooks(state).preparation_settlement_faults)
        .insert(incarnation_id.to_owned(), count);
}

/// How many of the transient settlement failures armed for
/// `incarnation_id` have not yet been taken by a settlement attempt.
#[cfg(test)]
pub(super) fn untaken_preparation_settlement_faults(
    state: &AppState,
    incarnation_id: &str,
) -> usize {
    locked(&hls_route_test_hooks(state).preparation_settlement_faults)
        .get(incarnation_id)
        .copied()
        .unwrap_or(0)
}

#[cfg(test)]
pub(super) fn take_preparation_candidate_completion(
    state: &AppState,
    incarnation_id: &str,
) -> bool {
    locked(&hls_route_test_hooks(state).completed_preparation_candidates).remove(incarnation_id)
}

/// Delay the next candidate for `playback_id` at its planning step, and
/// refuse it there if `refuse`.
///
/// Planning is the last thing a candidate does before it can reach either
/// the ledger or the registry, so the window in which a client's exchange
/// finds neither is the one this widens far enough to drive real exchanges
/// through. The refusal half stands in for a candidate that turns out to be
/// unplannable, without needing a source row contrived to make the planner
/// fail for some unrelated reason.
#[cfg(test)]
pub(super) fn fault_preparation_planning(
    state: &AppState,
    playback_id: &str,
    delay: Duration,
    refuse: bool,
) {
    locked(&hls_route_test_hooks(state).preparation_planning_faults)
        .insert(playback_id.to_owned(), (delay, refuse));
}

/// Delay the next priming successor for `playback_id` just before it
/// registers for cancellation.
///
/// Production has no suspension point between a staging task's last await
/// and that registration, which is exactly why the supersession flag is read
/// once more *after* it. A test cannot land a supersession inside a window
/// that does not exist, so this makes one: the task parks here, the ordinary
/// `cancel_preparations_for_superseded_predecessor` runs, and the read after
/// registration is then the only thing in the process that can still see it.
#[cfg(test)]
pub(super) fn delay_preparation_registration(state: &AppState, playback_id: &str, delay: Duration) {
    locked(&hls_route_test_hooks(state).preparation_registration_delays)
        .insert(playback_id.to_owned(), delay);
}

/// Hold the next priming successor for `playback_id` just before it
/// registers for cancellation. Only the priming path reaches this point, so
/// reaching it is how a test tells that an admitted candidate primed.
#[cfg(test)]
pub(super) fn pause_before_preparation_registered(
    state: &AppState,
    playback_id: &str,
) -> Arc<crate::seam_hooks::AsyncPause> {
    let pause = crate::seam_hooks::AsyncPause::new("priming successor before its registration");
    locked(&hls_route_test_hooks(state).preparation_registration_pauses)
        .insert(playback_id.to_owned(), Arc::clone(&pause));
    pause
}

/// Hold the next release of `session` after it closes its publication fence.
#[cfg(test)]
pub(super) fn pause_release_after_fence(
    state: &AppState,
    session: &str,
) -> Arc<crate::seam_hooks::AsyncPause> {
    let pause = crate::seam_hooks::AsyncPause::new("release after its publication fence");
    locked(&hls_route_test_hooks(state).release_fence_pauses)
        .insert(session.to_owned(), Arc::clone(&pause));
    pause
}

/// Hold the next release of `session` after its durable tombstone.
#[cfg(test)]
pub(super) fn pause_release_after_tombstone(
    state: &AppState,
    session: &str,
) -> Arc<crate::seam_hooks::AsyncPause> {
    let pause = crate::seam_hooks::AsyncPause::new("release after its durable tombstone");
    locked(&hls_route_test_hooks(state).release_tombstone_pauses)
        .insert(session.to_owned(), Arc::clone(&pause));
    pause
}

#[cfg(test)]
pub(super) fn inject_release_error(state: &AppState, session: &str) {
    locked(&hls_route_test_hooks(state).release_errors).insert(session.to_owned());
}

/// Count the status lookups of `session` that reach its local actor.
#[cfg(test)]
pub(super) fn observe_status_lookups(
    state: &AppState,
    session: &str,
) -> Arc<std::sync::atomic::AtomicUsize> {
    let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    locked(&hls_route_test_hooks(state).status_lookup_observers)
        .insert(session.to_owned(), Arc::clone(&lookups));
    lookups
}
