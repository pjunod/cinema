//! The wait pool behind blocking VOD segment GETs.
//!
//! Plan §2.3's second outcome is a GET that *blocks* until the segment
//! materializes, and review B4's operational half is why that blocking needs
//! a pool rather than a bare `Notify` per request: an unbounded set of parked
//! request futures is a resource the client controls, and a seek storm — or a
//! misbehaving client that opens GETs and walks away — must not be able to
//! grow it without limit. So every wait is *registered* here first, behind a
//! per-session cap and a global cap, and a wait that would exceed either is
//! refused before it parks anything ([`WaitRefused`] → the caller answers a
//! typed `segment_pending` 503 immediately, no wait, no state).
//!
//! Three properties the plan names, and this module owns:
//!
//! - **A client disconnect cancels the wait.** When axum drops the request
//!   future mid-block, the slot must come back *synchronously* — a pool that
//!   waits for a sweeper leaks a future per abandoned request until a seek
//!   storm finds it. Deregistration is a `Drop` impl on a guard the wait
//!   future holds, so cancellation at any await point releases both caps
//!   before the drop returns.
//! - **N waiters on one segment are one unit of demand.** Ten viewers parked
//!   on `(rendition, 512)` do not ask the producer for ten things; they ask
//!   for segment 512 once. [`WaitPool::demands`] preserves every distinct
//!   session/index so scheduling can use the session's accepted playback
//!   intent, rather than mistake the lowest old request for the playhead.
//! - **Every wait ends through exactly one named path** (review B4 deleted
//!   the escape hatch): the segment lands ([`WaitOutcome::Ready`]), the
//!   deadline expires ([`WaitOutcome::Deadline`] → typed retryable 503), the
//!   producer dies ([`WaitOutcome::ProducerFailed`] → typed 502), or the
//!   rendition goes terminal ([`WaitOutcome::Gone`] → 410). Deadline expiry
//!   is an `Ok` outcome, not an error — it is the *common* end of a long
//!   block, and the client's retry ladder is designed around it (§12.7).
//!
//! Wakeups are delivered over per-waiter oneshot channels created under the
//! same lock that registers the waiter, so a `satisfy()` that races
//! registration finds the sender already in the map and the wakeup is
//! buffered — there is no window between "registered" and "sleeping" in
//! which a wakeup can be lost.

// M3 builds the pool before the segment GET handler attaches to it, so
// nothing outside the tests calls it yet.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::oneshot;

const POISONED: &str = "waitpool state mutex poisoned";

/// One blocked segment: which rendition, which plan index.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WaitKey {
    pub rendition: String,
    pub index: u32,
}

/// How a wait ended. Every variant is a named path to a typed response
/// (plan §2.3); none of them is an error from the pool's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The segment materialized while waiting → the caller serves the bytes.
    Ready,
    /// The per-request budget expired → typed `segment_pending` 503.
    Deadline,
    /// The rendition's producer died mid-wait → typed `producer_failed` 502.
    /// Carries the cause so the refusal body can say what actually happened.
    ProducerFailed(String),
    /// The rendition went terminal mid-wait → 410.
    Gone,
}

/// Why a wait was refused at the door. A refused wait touched no state:
/// nothing was registered, nothing needs releasing, the caller answers a
/// typed 503 immediately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitRefused {
    /// The session already has its cap of blocked GETs in flight.
    SessionBusy,
    /// The pool as a whole is at its cap.
    PoolFull,
}

/// One registered waiter: who to wake, and whose session slot to release.
struct Waiter {
    id: u64,
    session: String,
    tx: oneshot::Sender<WaitOutcome>,
}

#[derive(Default)]
struct State {
    waiters: HashMap<WaitKey, Vec<Waiter>>,
    /// Retention outlives the notification: a just-woken request still has
    /// to open its file before eviction can safely unlink it.
    retained: HashMap<u64, WaitKey>,
    per_session: HashMap<String, usize>,
    total: usize,
    next_id: u64,
}

impl State {
    /// Release the cap slots one departing waiter held. The waiter itself
    /// must already be out of `waiters`.
    fn release(&mut self, session: &str) {
        if let Some(n) = self.per_session.get_mut(session) {
            *n -= 1;
            if *n == 0 {
                self.per_session.remove(session);
            }
        }
        self.total -= 1;
    }

    /// Remove pending demand and hand the senders back for waking. Admission
    /// slots remain owned through the caller's file open, bounding retention
    /// as well as the number of parked notification receivers.
    fn drain_key(&mut self, key: &WaitKey) -> Vec<Waiter> {
        self.waiters.remove(key).unwrap_or_default()
    }
}

/// Deregisters its waiter when dropped. This is the disconnect path: axum
/// drops the request future when the client goes away, the future drops this
/// guard, and both cap slots come back synchronously — no sweeper, no delay.
struct SlotGuard {
    state: Arc<Mutex<State>>,
    key: WaitKey,
    id: u64,
    session: String,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let mut state = self.state.lock().expect(POISONED);
        if state.retained.remove(&self.id).is_some() {
            state.release(&self.session);
        }
        // The caller now has its file or has abandoned the request. If the
        // notification already arrived, only its pin/slot needed releasing.
        let Some(waiters) = state.waiters.get_mut(&self.key) else {
            return;
        };
        let Some(pos) = waiters.iter().position(|w| w.id == self.id) else {
            return;
        };
        waiters.swap_remove(pos);
        if waiters.is_empty() {
            state.waiters.remove(&self.key);
        }
    }
}

/// A bounded, admitted request. Keep this owner until the caller has opened
/// the response file; dropping it releases both pending demand and retention.
pub struct RegisteredWait {
    _guard: SlotGuard,
    rx: oneshot::Receiver<WaitOutcome>,
}

impl RegisteredWait {
    pub async fn wait(&mut self, deadline: Duration) -> WaitOutcome {
        match tokio::time::timeout(deadline, &mut self.rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => WaitOutcome::Gone,
            Err(_) => self.rx.try_recv().unwrap_or(WaitOutcome::Deadline),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitDemand {
    pub session: String,
    pub index: u32,
    /// First admitted request still waiting for this session/index. A later
    /// prefetch cannot continually jump ahead of an older viewer's request.
    pub arrival_order: u64,
}

/// The pool itself. Shared via `Arc` between the segment GET handler (which
/// waits), the materialization path (which satisfies), the producer watchdog
/// (which fails), and the lifecycle path (which closes).
pub struct WaitPool {
    state: Arc<Mutex<State>>,
    global_cap: usize,
    per_session_cap: usize,
}

impl WaitPool {
    pub fn new(global_cap: usize, per_session_cap: usize) -> WaitPool {
        WaitPool {
            state: Arc::new(Mutex::new(State::default())),
            global_cap,
            per_session_cap,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect(POISONED)
    }

    /// Register and wait. Returns `Err` before waiting when a cap is hit —
    /// a refused wait touches no state. The wait future deregisters itself
    /// when dropped (client disconnect cancels the wait and releases both
    /// caps), and ends through exactly one of the four named outcomes.
    #[cfg(test)]
    pub async fn wait(
        &self,
        key: WaitKey,
        session: &str,
        deadline: Duration,
    ) -> Result<WaitOutcome, WaitRefused> {
        // Registration is synchronous — no await between the cap check and
        // the guard existing — so a cancellation can never strand a slot.
        Ok(self.register(key, session)?.wait(deadline).await)
    }

    pub fn register(&self, key: WaitKey, session: &str) -> Result<RegisteredWait, WaitRefused> {
        let mut state = self.lock();
        if state.per_session.get(session).copied().unwrap_or(0) >= self.per_session_cap {
            return Err(WaitRefused::SessionBusy);
        }
        if state.total >= self.global_cap {
            return Err(WaitRefused::PoolFull);
        }
        let id = state.next_id;
        state.next_id += 1;
        let (tx, rx) = oneshot::channel();
        state.waiters.entry(key.clone()).or_default().push(Waiter {
            id,
            session: session.to_string(),
            tx,
        });
        state.retained.insert(id, key.clone());
        *state.per_session.entry(session.to_string()).or_insert(0) += 1;
        state.total += 1;
        drop(state);
        let guard = SlotGuard {
            state: Arc::clone(&self.state),
            key,
            id,
            session: session.to_string(),
        };
        Ok(RegisteredWait { _guard: guard, rx })
    }

    /// A segment materialized: wake every waiter on `(rendition, index)`.
    pub fn satisfy(&self, rendition: &str, index: u32) {
        self.wake_key(rendition, index, WaitOutcome::Ready);
    }

    /// A demanded entry missed its service deadline. That is not evidence
    /// that another entry or the rendition's producer has failed.
    pub fn fail_entry(&self, rendition: &str, index: u32, cause: &str) {
        self.wake_key(
            rendition,
            index,
            WaitOutcome::ProducerFailed(cause.to_owned()),
        );
    }

    fn wake_key(&self, rendition: &str, index: u32, outcome: WaitOutcome) {
        let key = WaitKey {
            rendition: rendition.to_string(),
            index,
        };
        let waiters = self.lock().drain_key(&key);
        for w in waiters {
            // A receiver gone mid-send is a disconnect that raced the
            // wakeup; its guard already ran or is about to find nothing.
            let _ = w.tx.send(outcome.clone());
        }
    }

    /// The rendition's producer failed: wake every waiter on the rendition
    /// with `ProducerFailed(cause)`, whatever index each was blocked on.
    pub fn fail(&self, rendition: &str, cause: &str) {
        self.wake_rendition(rendition, WaitOutcome::ProducerFailed(cause.to_string()));
    }

    /// The rendition went terminal: wake every waiter on it with `Gone`.
    pub fn close(&self, rendition: &str) {
        self.wake_rendition(rendition, WaitOutcome::Gone);
    }

    /// Retire everything one departing session still has parked on this
    /// rendition.
    ///
    /// A detached viewer's registered GET otherwise stays in `demands` with no
    /// reader behind it, and the scheduler's fallback marks a session's oldest
    /// wait *foreground* precisely because it cannot see a reader to rank it
    /// against — so an abandoned request outranks a present viewer's and takes
    /// the producer with it until its HTTP deadline expires. Waking them
    /// `Gone` is the same answer their own disconnect would have produced, and
    /// each response dropping its guard is what returns the cap slots.
    pub fn retire_session(&self, rendition: &str, session: &str) {
        let mut drained = Vec::new();
        {
            let mut state = self.lock();
            let keys: Vec<WaitKey> = state
                .waiters
                .keys()
                .filter(|key| key.rendition == rendition)
                .cloned()
                .collect();
            for key in keys {
                let Some(waiters) = state.waiters.get_mut(&key) else {
                    continue;
                };
                let mut kept = Vec::with_capacity(waiters.len());
                for waiter in waiters.drain(..) {
                    if waiter.session == session {
                        drained.push(waiter);
                    } else {
                        kept.push(waiter);
                    }
                }
                if kept.is_empty() {
                    state.waiters.remove(&key);
                } else {
                    *waiters = kept;
                }
            }
        }
        for waiter in drained {
            let _ = waiter.tx.send(WaitOutcome::Gone);
        }
    }

    fn wake_rendition(&self, rendition: &str, outcome: WaitOutcome) {
        let mut drained = Vec::new();
        {
            let mut state = self.lock();
            let keys: Vec<WaitKey> = state
                .waiters
                .keys()
                .filter(|k| k.rendition == rendition)
                .cloned()
                .collect();
            for key in keys {
                drained.extend(state.drain_key(&key));
            }
        }
        for w in drained {
            let _ = w.tx.send(outcome.clone());
        }
    }

    /// A diagnostic lowest blocked index. Lifecycle code uses its presence
    /// to distinguish owed work from speculative completion; the scheduler
    /// uses `demands()` because this projection loses session ownership.
    pub fn blocked_on(&self, rendition: &str) -> Option<u32> {
        self.lock()
            .waiters
            .keys()
            .filter(|k| k.rendition == rendition)
            .map(|k| k.index)
            .min()
    }

    pub fn demands(&self, rendition: &str) -> Vec<WaitDemand> {
        let mut coalesced = HashMap::new();
        let state = self.lock();
        for (key, waiters) in state
            .waiters
            .iter()
            .filter(|(key, _)| key.rendition == rendition)
        {
            for waiter in waiters {
                let order = coalesced
                    .entry((waiter.session.clone(), key.index))
                    .or_insert(waiter.id);
                *order = (*order).min(waiter.id);
            }
        }
        let mut demands = coalesced
            .into_iter()
            .map(|((session, index), arrival_order)| WaitDemand {
                session,
                index,
                arrival_order,
            })
            .collect::<Vec<_>>();
        demands.sort_unstable_by(|a, b| (&a.session, a.index).cmp(&(&b.session, b.index)));
        demands.dedup();
        demands
    }

    pub fn retained(&self, rendition: &str) -> Vec<u32> {
        let mut indexes = self
            .lock()
            .retained
            .values()
            .filter(|key| key.rendition == rendition)
            .map(|key| key.index)
            .collect::<Vec<_>>();
        indexes.sort_unstable();
        indexes.dedup();
        indexes
    }

    /// Number of registered waiters (for telemetry and tests).
    #[allow(dead_code)] // telemetry/test-facing
    pub fn len(&self) -> usize {
        self.lock().total
    }

    #[allow(dead_code)] // telemetry/test-facing
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::Poll;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn key(index: u32) -> WaitKey {
        WaitKey {
            rendition: "abcd1234".into(),
            index,
        }
    }

    /// Poll a pinned future exactly once. Registration happens on the first
    /// poll of `wait()`, so this is how a test parks a waiter without
    /// spawning — and how it proves what a *single* poll does or does not do.
    async fn poll_once<F>(fut: &mut F) -> Poll<F::Output>
    where
        F: Future + Unpin,
    {
        std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut *fut).poll(cx))).await
    }

    #[tokio::test]
    async fn a_notified_request_keeps_its_bounded_pin_until_the_file_is_open() {
        let pool = WaitPool::new(4, 1);
        let mut request = pool.register(key(5), "viewer").expect("admitted");
        pool.satisfy("abcd1234", 5);
        assert!(pool.demands("abcd1234").is_empty());
        assert_eq!(request.wait(secs(1)).await, WaitOutcome::Ready);
        assert_eq!(pool.retained("abcd1234"), vec![5]);
        assert_eq!(
            pool.wait(key(6), "viewer", secs(1)).await,
            Err(WaitRefused::SessionBusy)
        );
        drop(request);
        assert!(pool.retained("abcd1234").is_empty());
        assert!(pool.is_empty());
    }

    #[tokio::test]
    async fn entry_deadline_does_not_fail_other_demand_and_cancellation_removes_exactly_one() {
        let pool = WaitPool::new(4, 4);
        let mut old = pool.register(key(3), "viewer").expect("old admitted");
        let target = pool.register(key(90), "viewer").expect("target admitted");
        let other = pool
            .register(key(8), "other")
            .expect("other viewer admitted");
        assert_eq!(pool.demands("abcd1234").len(), 3);
        pool.fail_entry("abcd1234", 3, "deadline");
        assert_eq!(
            old.wait(secs(1)).await,
            WaitOutcome::ProducerFailed("deadline".into())
        );
        assert_eq!(pool.demands("abcd1234").len(), 2);
        drop(target);
        assert_eq!(
            pool.demands("abcd1234"),
            vec![WaitDemand {
                session: "other".into(),
                index: 8,
                arrival_order: 2,
            }]
        );
        drop((old, other));
        assert!(pool.is_empty());
    }

    #[tokio::test]
    async fn a_client_disconnect_cancels_the_wait_and_releases_its_slot() {
        // per_session_cap of 1 so admission of the second wait proves the
        // first one's slot actually came back, not just that len() says so.
        let pool = WaitPool::new(64, 1);
        let mut fut = Box::pin(pool.wait(key(5), "sess-a", secs(8)));
        assert_eq!(poll_once(&mut fut).await, Poll::Pending);
        assert_eq!(pool.len(), 1);

        // The client disconnects: axum drops the request future mid-wait.
        drop(fut);
        assert_eq!(pool.len(), 0, "the dropped wait must deregister itself");
        assert!(pool.is_empty());

        // The freed slot admits a subsequent wait from the same session.
        let mut again = Box::pin(pool.wait(key(5), "sess-a", secs(8)));
        assert_eq!(poll_once(&mut again).await, Poll::Pending);
        assert_eq!(pool.len(), 1);
        drop(again);
        assert!(pool.is_empty());
    }

    #[tokio::test]
    async fn a_producer_death_mid_wait_answers_every_waiter_typed() {
        let pool = WaitPool::new(64, 4);
        let mut a = Box::pin(pool.wait(key(3), "sess-a", secs(8)));
        let mut b = Box::pin(pool.wait(key(4), "sess-a", secs(8)));
        let mut c = Box::pin(pool.wait(key(9), "sess-b", secs(8)));
        assert_eq!(poll_once(&mut a).await, Poll::Pending);
        assert_eq!(poll_once(&mut b).await, Poll::Pending);
        assert_eq!(poll_once(&mut c).await, Poll::Pending);
        assert_eq!(pool.len(), 3);

        pool.fail("abcd1234", "ffmpeg exited with status 1");

        let want = Ok(WaitOutcome::ProducerFailed(
            "ffmpeg exited with status 1".into(),
        ));
        assert_eq!(a.await, want);
        assert_eq!(b.await, want);
        assert_eq!(c.await, want);
        assert!(
            pool.is_empty(),
            "answered waiters must not linger in the pool"
        );
    }

    #[tokio::test]
    async fn ten_concurrent_waiters_on_one_segment_coalesce_to_one_demand() {
        let pool = WaitPool::new(64, 16);
        let sessions: Vec<String> = (0..10).map(|i| format!("viewer-{i}")).collect();
        let mut waiters = Vec::new();
        for session in &sessions {
            let mut fut = Box::pin(pool.wait(key(512), session, secs(8)));
            assert_eq!(poll_once(&mut fut).await, Poll::Pending);
            waiters.push(fut);
        }
        assert_eq!(pool.len(), 10);

        // Ten waiters, ONE demand: the producer is asked for index 512 once.
        assert_eq!(pool.blocked_on("abcd1234"), Some(512));

        pool.satisfy("abcd1234", 512);
        for fut in waiters {
            assert_eq!(fut.await, Ok(WaitOutcome::Ready));
        }
        assert_eq!(pool.blocked_on("abcd1234"), None);
        assert!(pool.is_empty());
    }

    #[tokio::test]
    async fn a_twenty_seek_storm_stays_inside_the_blocked_get_caps() {
        let pool = WaitPool::new(64, 4);
        let mut admitted = Vec::new();
        let mut refused = 0;
        for i in 0..20u32 {
            let mut fut = Box::pin(pool.wait(key(100 + i), "storm", secs(8)));
            match poll_once(&mut fut).await {
                Poll::Pending => admitted.push((100 + i, fut)),
                Poll::Ready(Err(WaitRefused::SessionBusy)) => refused += 1,
                Poll::Ready(other) => panic!("unexpected storm outcome: {other:?}"),
            }
        }
        assert_eq!(admitted.len(), 4, "at most per_session_cap waits admitted");
        assert_eq!(refused, 16, "the rest refused SessionBusy, immediately");
        assert_eq!(pool.len(), 4, "a refused wait must touch no state");

        // Another session is unaffected by the storm's session cap.
        let mut bystander = Box::pin(pool.wait(key(7), "calm", secs(8)));
        assert_eq!(poll_once(&mut bystander).await, Poll::Pending);
        assert_eq!(pool.len(), 5);
        drop(bystander);

        // Two waits end by materialization, two by disconnect: nothing leaks.
        let (idx_a, fut_a) = admitted.remove(0);
        let (idx_b, fut_b) = admitted.remove(0);
        pool.satisfy("abcd1234", idx_a);
        pool.satisfy("abcd1234", idx_b);
        assert_eq!(fut_a.await, Ok(WaitOutcome::Ready));
        assert_eq!(fut_b.await, Ok(WaitOutcome::Ready));
        drop(admitted);
        assert!(pool.is_empty(), "the storm must leave nothing behind");
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_expiry_answers_deadline_as_an_outcome_not_an_error() {
        let pool = Arc::new(WaitPool::new(64, 4));
        let started = tokio::time::Instant::now();
        let handle = {
            let pool = Arc::clone(&pool);
            tokio::spawn(async move { pool.wait(key(5), "sess-a", secs(8)).await })
        };
        assert_eq!(
            handle.await.expect("waiter task"),
            Ok(WaitOutcome::Deadline)
        );
        assert!(
            started.elapsed() >= secs(8),
            "the deadline is hard: expiry must not arrive early"
        );
        assert!(pool.is_empty(), "an expired wait must deregister itself");
    }

    #[tokio::test]
    async fn a_satisfy_landing_before_the_waiter_sleeps_is_not_a_lost_wakeup() {
        let pool = WaitPool::new(64, 4);
        let mut fut = Box::pin(pool.wait(key(5), "sess-a", secs(8)));
        // One poll registers the waiter; it has not been woken since.
        assert_eq!(poll_once(&mut fut).await, Poll::Pending);

        // The segment lands before the waiter is ever polled again — the
        // wakeup must be buffered, not lost.
        pool.satisfy("abcd1234", 5);
        assert_eq!(fut.await, Ok(WaitOutcome::Ready));
        assert!(pool.is_empty());
    }

    #[tokio::test]
    async fn a_rendition_going_terminal_answers_every_waiter_gone() {
        let pool = WaitPool::new(64, 4);
        let mut a = Box::pin(pool.wait(key(3), "sess-a", secs(8)));
        let mut b = Box::pin(pool.wait(key(8), "sess-b", secs(8)));
        assert_eq!(poll_once(&mut a).await, Poll::Pending);
        assert_eq!(poll_once(&mut b).await, Poll::Pending);

        pool.close("abcd1234");
        assert_eq!(a.await, Ok(WaitOutcome::Gone));
        assert_eq!(b.await, Ok(WaitOutcome::Gone));
        assert!(pool.is_empty());
    }

    /// A departing session's parked requests leave with it.
    ///
    /// Left behind they stay in `demands` with no reader to rank them
    /// against, and the scheduler's fallback marks a session's oldest wait
    /// foreground for exactly that reason — so an abandoned request outranks a
    /// present viewer's and aims the producer at media nobody is watching.
    /// Only that session goes; everyone else's waits survive.
    #[tokio::test]
    async fn a_departing_session_takes_its_own_waits_and_nobody_elses() {
        let pool = WaitPool::new(64, 4);
        let mut leaving = pool.register(key(1), "gone").expect("leaving wait");
        let mut staying = pool.register(key(2), "here").expect("staying wait");
        let mut same_index = pool.register(key(1), "here").expect("shared index");
        assert_eq!(pool.demands("abcd1234").len(), 3);

        pool.retire_session("abcd1234", "gone");

        assert_eq!(
            leaving.wait(secs(1)).await,
            WaitOutcome::Gone,
            "the departing session's waiter is answered, not left to time out"
        );
        let surviving = pool.demands("abcd1234");
        assert_eq!(surviving.len(), 2, "only the departing session left");
        assert!(
            surviving.iter().all(|demand| demand.session == "here"),
            "a shared index must not take another session's waiter with it"
        );

        pool.satisfy("abcd1234", 1);
        assert_eq!(
            same_index.wait(secs(1)).await,
            WaitOutcome::Ready,
            "the other session waiting on the same segment is served normally"
        );
        pool.satisfy("abcd1234", 2);
        assert_eq!(staying.wait(secs(1)).await, WaitOutcome::Ready);
    }

    #[tokio::test]
    async fn blocked_on_reports_the_lowest_index_across_waiters() {
        let pool = WaitPool::new(64, 4);
        let mut a = Box::pin(pool.wait(key(7), "sess-a", secs(8)));
        let mut b = Box::pin(pool.wait(key(3), "sess-a", secs(8)));
        let mut c = Box::pin(pool.wait(key(9), "sess-b", secs(8)));
        assert_eq!(poll_once(&mut a).await, Poll::Pending);
        assert_eq!(poll_once(&mut b).await, Poll::Pending);
        assert_eq!(poll_once(&mut c).await, Poll::Pending);

        assert_eq!(pool.blocked_on("abcd1234"), Some(3));
        assert_eq!(pool.blocked_on("some-other-rendition"), None);

        // The lowest demand satisfied, the next-lowest becomes the demand.
        pool.satisfy("abcd1234", 3);
        assert_eq!(b.await, Ok(WaitOutcome::Ready));
        assert_eq!(pool.blocked_on("abcd1234"), Some(7));
        drop((a, c));
        assert!(pool.is_empty());
    }

    #[tokio::test]
    async fn the_global_cap_refuses_pool_full_without_touching_state() {
        let pool = WaitPool::new(2, 10);
        let mut a = Box::pin(pool.wait(key(1), "sess-a", secs(8)));
        let mut b = Box::pin(pool.wait(key(2), "sess-b", secs(8)));
        assert_eq!(poll_once(&mut a).await, Poll::Pending);
        assert_eq!(poll_once(&mut b).await, Poll::Pending);

        assert_eq!(
            pool.wait(key(3), "sess-c", secs(8)).await,
            Err(WaitRefused::PoolFull)
        );
        assert_eq!(pool.len(), 2, "a refused wait must touch no state");
        drop((a, b));
        assert!(pool.is_empty());
    }
}
