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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitRefused {
    /// The session already has its cap of blocked GETs in flight.
    SessionBusy,
    /// The pool as a whole is at its cap.
    PoolFull,
}

/// The refusal classes, in [`WaitRefused::index`] order.
///
/// Two fixed strings for two enum variants: the label set cannot grow with
/// traffic, which is the property that lets these be counted per reason at
/// all. Session ids and rendition keys are the labels an attribution counter
/// invites and the ones it must never carry.
const REFUSAL_REASONS: [&str; 2] = ["session_busy", "pool_full"];

impl WaitRefused {
    fn index(self) -> usize {
        match self {
            Self::SessionBusy => 0,
            Self::PoolFull => 1,
        }
    }
}

/// Blocked-GET admission counters, readable without the pool's lock.
///
/// Separated from `State` rather than derived from it, because that lock is
/// taken on every blocked segment GET and on every driver pass that reads
/// demand, and this node's metrics surface is deliberately free of anything a
/// live request can hold — the same rule that keeps the session map out of the
/// transcode snapshot, and that has its own regression. Every field here is
/// read with a plain atomic load.
///
/// Owned by the pool and handed out as a handle rather than kept in a module
/// static, so a test can drive a pool of its own and read exactly what that
/// pool did. The statics this replaced could not be asserted at all under a
/// parallel suite.
#[derive(Debug, Default)]
pub struct BlockedGetMetrics {
    waiting: AtomicUsize,
    cap: AtomicUsize,
    admitted: AtomicU64,
    refused: [AtomicU64; 2],
}

impl BlockedGetMetrics {
    /// The node's blocked-GET picture, read whole.
    ///
    /// Answered whole because no subset answers the question. A refusal count
    /// without the ceiling it is against cannot tell a node at its limit from
    /// one nowhere near it; a ceiling without refusals cannot tell one that is
    /// doing its job from one sized for a different machine; and the two
    /// classes summed cannot tell one client's seek storm from a full node —
    /// which is the entire question this exists to answer.
    pub fn snapshot(&self) -> BlockedGets {
        BlockedGets {
            waiting: self.waiting.load(Relaxed),
            cap: self.cap.load(Relaxed),
            admitted: self.admitted.load(Relaxed),
            refused: [self.refused[0].load(Relaxed), self.refused[1].load(Relaxed)],
        }
    }

    /// Prometheus text for blocked-GET admission.
    pub fn prometheus(&self) -> String {
        render_blocked_gets(self.snapshot())
    }
}

/// What blocked-GET admission looks like right now, and what it has done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockedGets {
    /// Parked right now.
    pub waiting: usize,
    /// The node-wide ceiling those waits are against.
    pub cap: usize,
    /// Admitted since boot.
    pub admitted: u64,
    /// Refused since boot, in [`REFUSAL_REASONS`] order.
    pub refused: [u64; 2],
}

impl BlockedGets {
    /// Refusals for one class, by name — so a caller naming a class cannot
    /// silently read the other one's counter off a wrong index.
    pub fn refused(&self, reason: WaitRefused) -> u64 {
        self.refused[reason.index()]
    }
}

fn render_blocked_gets(snapshot: BlockedGets) -> String {
    use std::fmt::Write;
    let mut out = String::from(
        "# HELP plurx_vod_blocked_gets_waiting Segment GETs parked waiting for media right now.\n\
         # TYPE plurx_vod_blocked_gets_waiting gauge\n",
    );
    let _ = writeln!(out, "plurx_vod_blocked_gets_waiting {}", snapshot.waiting);
    out.push_str(
        "# HELP plurx_vod_blocked_get_cap Node-wide ceiling those waits are admitted against.\n\
         # TYPE plurx_vod_blocked_get_cap gauge\n",
    );
    let _ = writeln!(out, "plurx_vod_blocked_get_cap {}", snapshot.cap);
    out.push_str(
        "# HELP plurx_vod_blocked_gets_admitted_total Segment GETs admitted to the wait pool.\n\
         # TYPE plurx_vod_blocked_gets_admitted_total counter\n",
    );
    let _ = writeln!(
        out,
        "plurx_vod_blocked_gets_admitted_total {}",
        snapshot.admitted
    );
    out.push_str(
        "# HELP plurx_vod_blocked_get_refusals_total Segment GETs refused at admission, by bounded cap class.\n\
         # TYPE plurx_vod_blocked_get_refusals_total counter\n",
    );
    for (index, reason) in REFUSAL_REASONS.iter().enumerate() {
        let _ = writeln!(
            out,
            "plurx_vod_blocked_get_refusals_total{{reason=\"{reason}\"}} {}",
            snapshot.refused[index]
        );
    }
    out
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
    /// Carried so the gauge falls on every path a slot comes back, the
    /// disconnect one included — this destructor is that path.
    metrics: Arc<BlockedGetMetrics>,
    key: WaitKey,
    id: u64,
    session: String,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let mut state = self.state.lock().expect(POISONED);
        if state.retained.remove(&self.id).is_some() {
            state.release(&self.session);
            // The gauge's only decrement, paired with the only increment, and
            // taken under the same lock that owns `total` so the two cannot
            // drift.
            self.metrics.waiting.fetch_sub(1, Relaxed);
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
    /// Node-wide, and settable at runtime: the right number is a property of
    /// the deployment rather than of the code, so an operator watching
    /// `pool_full` refusals can raise or lower it without a restart. Read
    /// once per admission, so a change takes effect on the next blocked GET
    /// and never retroactively refuses a request already parked.
    global_cap: AtomicUsize,
    /// Fixed, because it bounds one viewer rather than the node. A viewer who
    /// could raise their own ceiling is not capped.
    per_session_cap: usize,
    /// What the operator surfaces read. An `Arc` so a scrape holds the
    /// counters directly rather than reaching back through whichever
    /// `VodServe` the transcode manager currently owns — it replaces its own
    /// on the cluster boot path.
    metrics: Arc<BlockedGetMetrics>,
}

impl WaitPool {
    pub fn new(global_cap: usize, per_session_cap: usize) -> WaitPool {
        WaitPool {
            state: Arc::new(Mutex::new(State::default())),
            global_cap: AtomicUsize::new(global_cap),
            per_session_cap,
            metrics: Arc::new(BlockedGetMetrics {
                cap: AtomicUsize::new(global_cap),
                ..BlockedGetMetrics::default()
            }),
        }
    }

    /// A cheap clone of the counters, for the metrics and system surfaces.
    pub fn metrics_handle(&self) -> Arc<BlockedGetMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Apply the configured node-wide cap.
    ///
    /// Lowering it below what is already parked refuses the *next* admission
    /// rather than evicting a request that is already waiting: a parked GET
    /// holds a client's response open, and answering it early to satisfy a
    /// number an operator has just changed would turn a settings edit into a
    /// visible playback failure.
    pub fn set_global_cap(&self, cap: usize) {
        let cap = cap.max(1);
        self.global_cap.store(cap, Relaxed);
        // Published with the same store, so the ceiling an operator reads is
        // the one the next admission will actually use rather than the one
        // the settings row happens to hold.
        self.metrics.cap.store(cap, Relaxed);
    }

    /// The ceiling currently in force, for the tests that pin the floor.
    ///
    /// Still test-only, and no longer a gap: the number an operator needs is
    /// published by `BlockedGetMetrics`, beside the refusals it explains,
    /// rather than by an accessor on the pool that a scrape would have to take
    /// a request-path lock to reach.
    #[cfg(test)]
    pub fn global_cap(&self) -> usize {
        self.global_cap.load(Relaxed)
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
        // Counted at the door, where the answer is exact: a refused wait
        // touches no state, so there is no later path on which the refusal
        // could be undone and no double-count to guard against. The two
        // classes are counted apart deliberately — folding them together is
        // what makes "was that one storm or a full node" unanswerable, and
        // the per-session refusal deliberately wins even on a full node, so
        // conflating them would inflate the very number an operator sizes the
        // node cap from.
        if state.per_session.get(session).copied().unwrap_or(0) >= self.per_session_cap {
            self.metrics.refused[WaitRefused::SessionBusy.index()].fetch_add(1, Relaxed);
            return Err(WaitRefused::SessionBusy);
        }
        if state.total >= self.global_cap.load(Relaxed) {
            self.metrics.refused[WaitRefused::PoolFull.index()].fetch_add(1, Relaxed);
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
        // Mirrored under the same lock that owns `total`, so the gauge cannot
        // drift from it: every increment here is matched by the release in
        // `State::release`, which runs under this lock too and on every path
        // a slot comes back — including the disconnect one, through the guard.
        self.metrics.waiting.fetch_add(1, Relaxed);
        self.metrics.admitted.fetch_add(1, Relaxed);
        drop(state);
        let guard = SlotGuard {
            state: Arc::clone(&self.state),
            metrics: Arc::clone(&self.metrics),
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

    /// The node-wide cap is the operator's, and takes effect on the next
    /// admission rather than on requests already parked.
    ///
    /// Lowering it below what is waiting must not answer a live request
    /// early: a parked GET holds a client's response open, and turning a
    /// settings edit into a visible playback failure is worse than briefly
    /// running over the new number.
    #[test]
    fn the_node_cap_is_settable_and_never_evicts_what_is_already_waiting() {
        let pool = WaitPool::new(64, 4);
        let _first = pool.register(key(1), "a").expect("first");
        let _second = pool.register(key(2), "b").expect("second");
        assert_eq!(pool.len(), 2);

        pool.set_global_cap(1);
        assert_eq!(pool.global_cap(), 1);
        assert_eq!(
            pool.len(),
            2,
            "the requests already parked are not answered early"
        );
        assert!(
            matches!(pool.register(key(3), "c"), Err(WaitRefused::PoolFull)),
            "the new ceiling binds the next admission"
        );

        // A zero would refuse every blocked GET, so every seek past the
        // materialized run would answer 503 immediately. The floor is what
        // stops a settings edit from reading like "no limit" and behaving
        // like "no playback".
        pool.set_global_cap(0);
        assert_eq!(pool.global_cap(), 1, "the cap floors at one, never zero");
    }

    /// What the two caps actually promise, including what they do not.
    ///
    /// The per-session cap bounds one viewer: a session at its own ceiling is
    /// refused `SessionBusy` while node room remains, so a newcomer takes that
    /// room. That refusal wins even when the node is *also* full, because the
    /// two answers mean different things to a client — `SessionBusy` says slow
    /// down, `PoolFull` says the node is out — and conflating them would
    /// inflate the very counter an operator sizes the node cap from.
    ///
    /// What neither cap provides is a fair share. Admission is
    /// first-come-first-served under a per-session ceiling, so with sixteen
    /// sessions each holding four, a seventeenth viewer holding *none* is
    /// refused. This states that outcome rather than hiding it: the
    /// fair-service item is still open, and a test implying otherwise would
    /// close it by assertion.
    #[test]
    fn the_two_caps_bound_a_viewer_and_the_node_but_do_not_share_fairly() {
        let pool = WaitPool::new(6, 4);
        let mut storm = Vec::new();
        for index in 0..4 {
            storm.push(
                pool.register(key(index), "storm")
                    .expect("within its own cap"),
            );
        }
        assert!(
            matches!(
                pool.register(key(9), "storm"),
                Err(WaitRefused::SessionBusy)
            ),
            "one viewer is bounded by its own cap, not by the node's"
        );
        assert_eq!(pool.len(), 4, "and the node still has room");

        let _quiet = pool
            .register(key(20), "quiet")
            .expect("a newcomer takes the room the storm could not");
        let _also = pool.register(key(21), "quiet").expect("and the rest of it");

        // A session at its own ceiling on a node that is also full still
        // hears `SessionBusy`. The per-session bound is the more specific
        // truth and the one a client can act on.
        assert!(
            matches!(
                pool.register(key(30), "storm"),
                Err(WaitRefused::SessionBusy)
            ),
            "the per-session refusal is not swallowed by the node being full"
        );
        // And a viewer holding nothing at all is refused for the node — the
        // fair-share gap, stated rather than hidden.
        assert!(
            matches!(pool.register(key(22), "third"), Err(WaitRefused::PoolFull)),
            "first-come-first-served: a newcomer with no slots is still refused"
        );
        drop(storm);
        assert!(
            pool.register(key(23), "third").is_ok(),
            "and the room comes back when the storm's requests finish"
        );
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

        // Retirement answers the waiter; it does not hand back the admission
        // slot. Retention is released by the response dropping its guard, and
        // an implementation that cleared it here would leak both the
        // per-session and the global cap for the life of the pool.
        assert_eq!(pool.len(), 3, "retiring answers waiters, it does not unpin");
        drop(leaving);
        assert_eq!(
            pool.len(),
            2,
            "the retired request's slot comes back on drop"
        );

        pool.satisfy("abcd1234", 1);
        assert_eq!(
            same_index.wait(secs(1)).await,
            WaitOutcome::Ready,
            "the other session waiting on the same segment is served normally"
        );
        pool.satisfy("abcd1234", 2);
        assert_eq!(staying.wait(secs(1)).await, WaitOutcome::Ready);
        drop(same_index);
        drop(staying);
        assert!(
            pool.is_empty(),
            "every slot returns once its request is done"
        );
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

    /// The node can say which cap turned a request away, and against what.
    ///
    /// This is the operator's question — *was that 503 one seek storm, many
    /// healthy viewers, or a ceiling sized for a different node* — and until
    /// these counters existed nothing on the node could answer any part of it.
    /// The refusal reached a log line that named neither cap, and both classes
    /// answered the same typed code.
    ///
    /// The five numbers are asserted together because no subset is an answer.
    /// Refusals without the ceiling cannot tell a node at its limit from one
    /// nowhere near it; the ceiling without refusals cannot tell one that is
    /// working from one sized for a smaller machine; and the classes summed
    /// cannot tell a single client asking for too much from a full node —
    /// which is the distinction that decides whether raising the setting helps
    /// at all.
    #[test]
    fn refusals_are_attributed_to_the_cap_that_produced_them() {
        let pool = WaitPool::new(3, 2);
        let metrics = pool.metrics_handle();
        assert_eq!(
            metrics.snapshot(),
            BlockedGets {
                waiting: 0,
                cap: 3,
                admitted: 0,
                refused: [0, 0],
            },
            "a fresh pool publishes its ceiling before anything has happened"
        );

        let storm = vec![
            pool.register(key(1), "storm").expect("first"),
            pool.register(key(2), "storm").expect("second"),
        ];
        assert!(matches!(
            pool.register(key(3), "storm"),
            Err(WaitRefused::SessionBusy)
        ));
        let quiet = pool.register(key(4), "quiet").expect("node room remains");
        assert!(matches!(
            pool.register(key(5), "third"),
            Err(WaitRefused::PoolFull)
        ));

        let after = metrics.snapshot();
        assert_eq!(
            after,
            BlockedGets {
                waiting: 3,
                cap: 3,
                admitted: 3,
                refused: [1, 1],
            },
            "three admitted against a ceiling of three, and one refusal of each class"
        );
        assert_eq!(after.refused(WaitRefused::SessionBusy), 1);
        assert_eq!(after.refused(WaitRefused::PoolFull), 1);

        // The gauge follows release, including the path a disconnect takes:
        // dropping a registered wait is what a dropped request future does.
        drop(quiet);
        assert_eq!(metrics.snapshot().waiting, 2);
        drop(storm);
        let idle = metrics.snapshot();
        assert_eq!(idle.waiting, 0, "an idle pool is parking nothing");
        assert_eq!(
            (idle.admitted, idle.refused),
            (3, [1, 1]),
            "and the counters are cumulative, not a picture of right now"
        );

        pool.set_global_cap(9);
        assert_eq!(
            metrics.snapshot().cap,
            9,
            "an operator's change is published with the store that applies it"
        );
    }

    /// The counters name classes, never sessions or renditions.
    ///
    /// Those are exactly the labels an attribution counter invites and the
    /// ones that make a metrics endpoint a cardinality bomb, so the render is
    /// held to the two fixed reason strings and nothing else.
    #[test]
    fn the_blocked_get_render_carries_only_bounded_labels() {
        let pool = WaitPool::new(1, 1);
        let metrics = pool.metrics_handle();
        let admitted = pool
            .register(key(1), "a-very-distinctive-session-id")
            .expect("first");
        assert!(matches!(
            pool.register(
                WaitKey {
                    rendition: "a-very-distinctive-rendition-key".into(),
                    index: 2
                },
                // The same session, so the per-session cap answers first —
                // with a global cap of one, any other session would be refused
                // for the node instead and this would be testing a different
                // class than it says.
                "a-very-distinctive-session-id"
            ),
            Err(WaitRefused::SessionBusy)
        ));

        let text = metrics.prometheus();
        assert!(text.contains("plurx_vod_blocked_gets_waiting 1"));
        assert!(text.contains("plurx_vod_blocked_get_cap 1"));
        assert!(text.contains("plurx_vod_blocked_gets_admitted_total 1"));
        assert!(text.contains("plurx_vod_blocked_get_refusals_total{reason=\"session_busy\"} 1"));
        assert!(text.contains("plurx_vod_blocked_get_refusals_total{reason=\"pool_full\"} 0"));
        assert!(
            !text.contains("distinctive"),
            "session ids and rendition keys must never become labels"
        );
        assert!(!text.contains("session=") && !text.contains("rendition="));
        drop(admitted);
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
