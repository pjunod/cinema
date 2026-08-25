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
//!   for segment 512 once. Coalescing is [`WaitPool::blocked_on`] returning
//!   the *lowest* blocked index per rendition — the single number that feeds
//!   `prodsched::Demand::waiting_on` — not ten entries in a queue.
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

    /// Remove every waiter on `key` and hand them back for waking. Cap slots
    /// are released here, under the lock, so `len()` never counts a waiter
    /// that has already been answered.
    fn drain_key(&mut self, key: &WaitKey) -> Vec<Waiter> {
        let Some(waiters) = self.waiters.remove(key) else {
            return Vec::new();
        };
        for w in &waiters {
            self.release(&w.session);
        }
        waiters
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
        // Already answered (satisfy/fail/close removed us first)? Then the
        // answering side released the slots and there is nothing to do.
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
        state.release(&self.session);
    }
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
    pub async fn wait(
        &self,
        key: WaitKey,
        session: &str,
        deadline: Duration,
    ) -> Result<WaitOutcome, WaitRefused> {
        // Registration is synchronous — no await between the cap check and
        // the guard existing — so a cancellation can never strand a slot.
        let (_guard, mut rx) = self.register(key, session)?;
        match tokio::time::timeout(deadline, &mut rx).await {
            Ok(Ok(outcome)) => Ok(outcome),
            // The sender only drops without sending if the pool's state is
            // torn down while a waiter is parked, which cannot happen while
            // the pool that admitted the waiter is alive. Answer Gone rather
            // than panic: a terminal answer is the truthful degraded mode.
            Ok(Err(_)) => Ok(WaitOutcome::Gone),
            Err(_elapsed) => Ok(match rx.try_recv() {
                // The answer landed in the same instant the deadline fired;
                // it already cost the producer the work, so serve it.
                Ok(outcome) => outcome,
                Err(_) => WaitOutcome::Deadline,
            }),
        }
    }

    fn register(
        &self,
        key: WaitKey,
        session: &str,
    ) -> Result<(SlotGuard, oneshot::Receiver<WaitOutcome>), WaitRefused> {
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
        *state.per_session.entry(session.to_string()).or_insert(0) += 1;
        state.total += 1;
        drop(state);
        let guard = SlotGuard {
            state: Arc::clone(&self.state),
            key,
            id,
            session: session.to_string(),
        };
        Ok((guard, rx))
    }

    /// A segment materialized: wake every waiter on `(rendition, index)`.
    pub fn satisfy(&self, rendition: &str, index: u32) {
        let key = WaitKey {
            rendition: rendition.to_string(),
            index,
        };
        let waiters = self.lock().drain_key(&key);
        for w in waiters {
            // A receiver gone mid-send is a disconnect that raced the
            // wakeup; its guard already ran or is about to find nothing.
            let _ = w.tx.send(WaitOutcome::Ready);
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

    /// The lowest plan index anyone is blocked on for this rendition, if
    /// any. This is what feeds `prodsched::Demand::waiting_on` — ten waiters
    /// on one index are ONE demand, and coalescing is this method returning
    /// one index, not ten entries.
    pub fn blocked_on(&self, rendition: &str) -> Option<u32> {
        self.lock()
            .waiters
            .keys()
            .filter(|k| k.rendition == rendition)
            .map(|k| k.index)
            .min()
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
