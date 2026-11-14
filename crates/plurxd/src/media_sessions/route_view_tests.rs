//! Playback-start E1c: the route cache over the node's applied view
//! (`docs/streaming/PLAYBACK-START-E1-BUILD.md` §4.4, §8).
//!
//! The cache is driven through [`ReplicaRoutes`], a double of the applied
//! view whose replica image, Authority answers, table revision, proof, image
//! generation, layout and per-fact floors a test moves by hand, so each case
//! states exactly what this node has applied and what a quorum read would
//! say. The double answers without blocking, so the cases run on a paused
//! clock and assert exact counts; only the fence case reads the SQLite store
//! behind the coordinator, on the wall clock.
use super::tests::media_route;
use super::*;
use plurx_core::domain::MediaSessionActivation;
use plurx_core::store::{SqliteStore, ViewFallback};
use std::sync::atomic::{AtomicBool, AtomicU32};

/// A replica of `media_sessions` as the route cache sees it.
pub(crate) struct ReplicaRoutes {
    /// What a quorum read answers.
    authority: StdMutex<HashMap<String, MediaSessionRoute>>,
    /// What this node has applied.
    rows: StdMutex<HashMap<String, MediaSessionRoute>>,
    revision: AtomicU64,
    proof: AtomicBool,
    /// The image is being replaced (an odd generation).
    replacing: AtomicBool,
    /// The principal session layout is installed (the view can answer).
    answers: AtomicBool,
    /// Facts numbered at or below this are applied here.
    applied_facts: AtomicU64,
    next_fact: AtomicU64,
    /// The highest fact `hold_floor` closed the table for.
    held: AtomicU64,
    /// The next this-many quorum reads fail.
    fail_quorum: AtomicU32,
    /// While set, a quorum read waits here until the semaphore is closed.
    quorum_gate: StdMutex<Option<Arc<tokio::sync::Semaphore>>>,
    pub(crate) view_reads: AtomicU64,
    /// `route()` answered from Authority.
    pub(super) authority_reads: AtomicU64,
    /// `quorum_route()` reads.
    pub(super) quorum_reads: AtomicU64,
    pub(super) reloads: AtomicU64,
    /// Every trait call, for the fence guard.
    pub(crate) calls: AtomicU64,
}

impl ReplicaRoutes {
    pub(crate) fn new(proof: bool) -> Arc<Self> {
        Arc::new(Self {
            authority: StdMutex::new(HashMap::new()),
            rows: StdMutex::new(HashMap::new()),
            revision: AtomicU64::new(1),
            proof: AtomicBool::new(proof),
            replacing: AtomicBool::new(false),
            answers: AtomicBool::new(true),
            applied_facts: AtomicU64::new(0),
            next_fact: AtomicU64::new(0),
            held: AtomicU64::new(0),
            fail_quorum: AtomicU32::new(0),
            quorum_gate: StdMutex::new(None),
            view_reads: AtomicU64::new(0),
            authority_reads: AtomicU64::new(0),
            quorum_reads: AtomicU64::new(0),
            reloads: AtomicU64::new(0),
            calls: AtomicU64::new(0),
        })
    }

    /// Apply one entry that writes `route` here: the slot moves.
    pub(crate) fn apply(&self, route: MediaSessionRoute) {
        self.rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(route.session_id.clone(), route);
        self.revision.fetch_add(1, Ordering::AcqRel);
    }

    /// Commit `route` cluster-wide: what a quorum read now answers.
    pub(super) fn commit(&self, route: MediaSessionRoute) {
        self.authority
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(route.session_id.clone(), route);
    }

    /// A confirmation after every fact noted so far is applied here.
    pub(super) fn catch_up(&self) {
        self.applied_facts
            .store(self.next_fact.load(Ordering::Acquire), Ordering::Release);
    }

    pub(super) fn set_proof(&self, proof: bool) {
        self.proof.store(proof, Ordering::Release);
    }

    fn set_replacing(&self, replacing: bool) {
        self.replacing.store(replacing, Ordering::Release);
    }

    fn set_answers(&self, answers: bool) {
        self.answers.store(answers, Ordering::Release);
    }

    fn hold_quorum(&self) -> Arc<tokio::sync::Semaphore> {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *self
            .quorum_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&gate));
        gate
    }

    fn closed(&self) -> Option<ViewFallback> {
        if self.replacing.load(Ordering::Acquire) {
            return Some(ViewFallback::ImageReplacing);
        }
        if !self.proof.load(Ordering::Acquire) {
            return Some(ViewFallback::NoProof);
        }
        if !self.answers.load(Ordering::Acquire) {
            return Some(ViewFallback::LegacyLayout);
        }
        (self.held.load(Ordering::Acquire) > self.applied_facts.load(Ordering::Acquire))
            .then_some(ViewFallback::Floor)
    }

    fn authority_row(&self, session_id: &str) -> Option<MediaSessionRoute> {
        self.authority
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned()
    }

    fn total(&self, counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Acquire)
    }

    /// Every read this double answered from Authority.
    fn quorum_total(&self) -> u64 {
        self.total(&self.authority_reads) + self.total(&self.quorum_reads)
    }

    fn same_image_uncounted(&self, stamp: RouteStamp) -> bool {
        matches!(stamp, RouteStamp::Test(revision) if revision == self.revision.load(Ordering::Acquire))
    }
}

impl RouteView for ReplicaRoutes {
    fn route<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<RouteViewAnswer, StoreError>> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        Box::pin(async move {
            if self.closed().is_some() {
                self.authority_reads.fetch_add(1, Ordering::AcqRel);
                return Ok(RouteViewAnswer {
                    route: self.authority_row(session_id),
                    stamp: None,
                    source: ViewSource::Authority,
                });
            }
            self.view_reads.fetch_add(1, Ordering::AcqRel);
            let stamp = RouteStamp::Test(self.revision.load(Ordering::Acquire));
            let route = self
                .rows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(session_id)
                .cloned();
            Ok(RouteViewAnswer {
                route,
                stamp: Some(stamp),
                source: ViewSource::View,
            })
        })
    }

    fn check(&self, stamp: RouteStamp) -> StampCheck {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if let Some(reason) = self.closed() {
            return StampCheck::Closed(reason);
        }
        if self.same_image_uncounted(stamp) {
            StampCheck::Current
        } else {
            StampCheck::Stale
        }
    }

    fn same_image(&self, stamp: RouteStamp) -> bool {
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.same_image_uncounted(stamp)
    }

    fn observe(&self) -> RouteFact {
        self.calls.fetch_add(1, Ordering::AcqRel);
        RouteFact::Test(self.next_fact.fetch_add(1, Ordering::AcqRel) + 1)
    }

    fn floor_met(&self, fact: RouteFact) -> bool {
        self.calls.fetch_add(1, Ordering::AcqRel);
        match fact {
            RouteFact::Test(fact) => {
                self.proof.load(Ordering::Acquire)
                    && !self.replacing.load(Ordering::Acquire)
                    && fact <= self.applied_facts.load(Ordering::Acquire)
            }
            RouteFact::Applied(_) => false,
        }
    }

    fn hold_floor(&self, fact: RouteFact) {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if let RouteFact::Test(fact) = fact {
            self.held.fetch_max(fact, Ordering::AcqRel);
        }
    }

    fn note_reload(&self) {
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.reloads.fetch_add(1, Ordering::AcqRel);
    }

    fn answers_routes(&self) -> bool {
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.answers.load(Ordering::Acquire)
    }

    fn quorum_route<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<MediaSessionRoute>, StoreError>> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        Box::pin(async move {
            self.quorum_reads.fetch_add(1, Ordering::AcqRel);
            let gate = self
                .quorum_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(gate) = gate {
                let _ = gate.acquire().await;
            }
            if self
                .fail_quorum
                .try_update(Ordering::AcqRel, Ordering::Acquire, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
            {
                return Err(StoreError::Database("injected quorum failure".to_owned()));
            }
            Ok(self.authority_row(session_id))
        })
    }
}

/// A coordinator over an empty in-memory SQLite store and a replica double.
pub(super) fn coordinator_over_replica(
    proof: bool,
) -> (
    Arc<MediaSessionCoordinator>,
    Arc<ReplicaRoutes>,
    Arc<dyn Store>,
) {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let replica = ReplicaRoutes::new(proof);
    let applied: Arc<dyn RouteView> = replica.clone();
    let coordinator = MediaSessionCoordinator::with_applied_view(
        plurx_core::cluster::membership::MembershipManager::unavailable(),
        Arc::clone(&store),
        applied,
    );
    (coordinator, replica, store)
}

/// A committed active row in the SQLite store, and the route it returns.
async fn committed(store: &Arc<dyn Store>, session_id: &str, owner: &str) -> MediaSessionRoute {
    let user = store
        .create_user(&format!("e1c-{session_id}"), "hash", false)
        .await
        .expect("user")
        .id;
    store
        .activate_media_session(&MediaSessionActivation {
            recovery_epoch: String::new(),
            expected_desired_revision: None,
            incarnation_id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_owned(),
            principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser {
                user_id: user,
            },
            playback_id: format!("e1c-{session_id}"),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: None,
            request_fingerprint: "a".repeat(64),
            owner_node_id: owner.to_owned(),
            recipe_json: "{}".to_owned(),
            response_json: "{}".to_owned(),
            publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: unix_ms(),
            lease_expires_at_ms: unix_ms().saturating_add(600_000),
        })
        .await
        .expect("activate")
        .expect("activation wins")
        .route
}

fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}

fn ended(session_id: &str) -> MediaSessionRoute {
    let mut route = media_route(session_id);
    route.state = "ended".to_owned();
    route.terminal_reason = Some("deleted".to_owned());
    route
}

async fn try_lookup(
    coordinator: &MediaSessionCoordinator,
    session_id: &str,
) -> Result<Option<MediaSessionRoute>, StoreError> {
    coordinator
        .raw_route_before(
            RouteReadSite::Routing,
            session_id,
            tokio::time::Instant::now() + ROUTE_QUERY_DEADLINE,
        )
        .await
}

async fn lookup(
    coordinator: &MediaSessionCoordinator,
    session_id: &str,
) -> Option<MediaSessionRoute> {
    try_lookup(coordinator, session_id)
        .await
        .expect("route lookup")
}

fn state_of(route: Option<MediaSessionRoute>) -> Option<String> {
    route.map(|route| route.state)
}

/// Let spawned lookups on the current-thread runtime run until they park.
async fn settle() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

/// §8 daemon "no clock in a `View` entry": under steady renewals of *other*
/// sessions and ten seconds of paused time, lookups of one session cost local
/// re-reads only — each renewal applied here moves the slot — and no quorum
/// read at all. With no change applied, time alone never ends the entry. The
/// old cache read the Store once a second.
#[tokio::test(start_paused = true)]
async fn a_view_entry_has_no_clock_and_moves_only_with_the_replica() {
    let (coordinator, replica, _store) = coordinator_over_replica(true);
    let watched = id(0xe1c_001);
    replica.apply(media_route(&watched));
    assert_eq!(
        state_of(lookup(&coordinator, &watched).await),
        Some("active".to_owned())
    );
    for second in 0..10 {
        tokio::time::advance(Duration::from_secs(1)).await;
        replica.apply(media_route(&id(0xe1c_100 + second)));
        assert!(lookup(&coordinator, &watched).await.is_some());
    }
    assert_eq!(
        replica.quorum_total(),
        0,
        "no quorum read while the proof holds"
    );
    assert_eq!(
        coordinator.route_cache_metrics.lookups(RouteLookup::Store),
        0
    );
    assert_eq!(replica.total(&replica.view_reads), 11);
    assert_eq!(
        replica.total(&replica.reloads),
        10,
        "one re-read per change"
    );

    // No change applied: ten more seconds and the entry still answers.
    tokio::time::advance(Duration::from_secs(10)).await;
    for _ in 0..5 {
        assert!(lookup(&coordinator, &watched).await.is_some());
    }
    assert_eq!(replica.total(&replica.view_reads), 11);
    assert_eq!(
        coordinator
            .route_cache_metrics
            .lookups(RouteLookup::CacheHit),
        5
    );
}

/// A release, takeover or expiry applied here moves the slot, and the next
/// lookup re-reads the replica locally: no clock advance, no quorum read.
#[tokio::test(start_paused = true)]
async fn a_peer_release_applied_here_moves_the_slot_and_the_next_lookup_rereads() {
    let (coordinator, replica, _store) = coordinator_over_replica(true);
    let session = id(0xe1c_201);
    replica.apply(media_route(&session));
    assert_eq!(
        state_of(lookup(&coordinator, &session).await),
        Some("active".to_owned())
    );
    replica.apply(ended(&session));
    assert_eq!(
        state_of(lookup(&coordinator, &session).await),
        Some("ended".to_owned()),
        "the release a peer made is seen as soon as it applies here"
    );
    let mut taken = media_route(&session);
    taken.owner_node_id = "node-successor".to_owned();
    taken.owner_epoch = 2;
    replica.apply(taken);
    assert_eq!(
        lookup(&coordinator, &session)
            .await
            .map(|route| route.owner_node_id),
        Some("node-successor".to_owned())
    );
    assert_eq!(replica.quorum_total(), 0);
    assert_eq!(replica.total(&replica.view_reads), 3);
    assert_eq!(replica.total(&replica.reloads), 2);
}

/// The review's B1 sequence through the cache. This node releases session S
/// (index W) while its replica is at W−3, and W−2, W−1 are other sessions'
/// renewals: the release is an `Observed` tombstone, and as W−2 and W−1 apply
/// (moving the slot) no lookup answers active — not from the entry, not from
/// a view read, not after the lease, when a quorum read takes over. The same
/// for a takeover (never the old owner) and an activation (never absent).
/// Once W applies and a confirmation covers the fact, the entry retires and
/// the view answers the terminal row.
#[tokio::test(start_paused = true)]
async fn a_replica_behind_a_local_release_never_revives_the_pre_release_row() {
    let (coordinator, replica, _store) = coordinator_over_replica(true);
    let session = id(0xe1c_301);
    replica.commit(media_route(&session));
    replica.apply(media_route(&session)); // W−3
    assert_eq!(
        state_of(lookup(&coordinator, &session).await),
        Some("active".to_owned())
    );

    // W: the release, committed through this node, published as it returns.
    replica.commit(ended(&session));
    coordinator.cache_terminal_route(ended(&session)).await;
    replica.apply(media_route(&id(0xe1c_302))); // W−2
    replica.apply(media_route(&id(0xe1c_303))); // W−1
    for step in 0..30 {
        let route = lookup(&coordinator, &session).await;
        assert_ne!(
            route.as_ref().map(|route| route.state.as_str()),
            Some("active"),
            "a lagging replica revived the pre-release row at step {step}"
        );
        assert!(matches!(
            coordinator
                .route_resolution_before(
                    &session,
                    "node-c",
                    Instant::now() + Duration::from_secs(1)
                )
                .await
                .expect("resolution"),
            DurableRouteResolution::Terminal(_)
        ));
        tokio::time::advance(Duration::from_millis(50)).await;
    }
    assert!(
        coordinator
            .route_cache_metrics
            .lookups(RouteLookup::Observed)
            > 0
    );
    assert_eq!(
        replica.total(&replica.quorum_reads),
        1,
        "past its lease, the unretired fact is replaced by one quorum read"
    );
    assert_eq!(
        replica.total(&replica.view_reads),
        1,
        "no view read while the release is unretired"
    );

    // W applies here and a confirmation covers it: retired, the view answers.
    replica.apply(ended(&session));
    replica.catch_up();
    assert_eq!(
        state_of(lookup(&coordinator, &session).await),
        Some("ended".to_owned())
    );
    assert_eq!(replica.total(&replica.view_reads), 2);

    // A takeover through this node: never the old owner.
    let taken_id = id(0xe1c_311);
    let mut old = media_route(&taken_id);
    old.owner_node_id = "node-old".to_owned();
    replica.apply(old.clone());
    assert_eq!(
        lookup(&coordinator, &taken_id)
            .await
            .map(|route| route.owner_node_id),
        Some("node-old".to_owned())
    );
    let mut successor = old.clone();
    successor.owner_node_id = "node-new".to_owned();
    successor.owner_epoch += 1;
    coordinator.cache_route(successor).await;
    replica.apply(media_route(&id(0xe1c_312)));
    for _ in 0..5 {
        assert_eq!(
            lookup(&coordinator, &taken_id)
                .await
                .map(|route| route.owner_node_id),
            Some("node-new".to_owned()),
            "a lagging replica answered the old owner"
        );
    }

    // An activation through this node: never absent.
    let activated = id(0xe1c_321);
    assert!(lookup(&coordinator, &activated).await.is_none());
    coordinator.cache_route(media_route(&activated)).await;
    replica.apply(media_route(&id(0xe1c_322)));
    for _ in 0..5 {
        assert!(
            lookup(&coordinator, &activated).await.is_some(),
            "a lagging replica answered absent for a session this node activated"
        );
    }
}

/// Review B1: an unretired tombstone past its lease stays in the cache until
/// the quorum read that replaces it lands. A failed read, a read that misses
/// its deadline, a request dropped mid-read and a publication racing the read
/// each leave a fact in place; the replica, still holding the pre-release
/// row, is never read.
#[tokio::test(start_paused = true)]
async fn an_unretired_tombstone_survives_every_failed_replacement() {
    let (coordinator, replica, _store) = coordinator_over_replica(true);
    let session = id(0xe1c_331);
    replica.apply(media_route(&session)); // the pre-release row, held here
    replica.commit(ended(&session));
    coordinator.cache_terminal_route(ended(&session)).await;
    let tombstone = || async {
        coordinator
            .cached_route_entry(&session)
            .await
            .expect("the tombstone is still cached")
            .map(|route| route.state)
    };
    let not_active = |route: Option<MediaSessionRoute>, case: &str| {
        assert_ne!(
            route.as_ref().map(|route| route.state.as_str()),
            Some("active"),
            "{case}: the replica revived the pre-release row"
        );
    };
    tokio::time::advance(OBSERVED_ROUTE_LEASE).await;

    // An error.
    replica.fail_quorum.store(1, Ordering::Release);
    assert!(try_lookup(&coordinator, &session).await.is_err());
    assert_eq!(tombstone().await, Some("ended".to_owned()), "error");
    not_active(lookup(&coordinator, &session).await, "after an error");
    tokio::time::advance(OBSERVED_ROUTE_LEASE).await;

    // A read that misses its deadline.
    let gate = replica.hold_quorum();
    let timed = coordinator.raw_route_before(
        RouteReadSite::Routing,
        &session,
        tokio::time::Instant::now() + Duration::from_millis(100),
    );
    let advance = async {
        settle().await;
        tokio::time::advance(Duration::from_millis(200)).await;
    };
    let (result, ()) = tokio::join!(timed, advance);
    assert!(result.is_err(), "the read missed its deadline");
    assert_eq!(tombstone().await, Some("ended".to_owned()), "timeout");

    // A request dropped while its read is in flight.
    let dropped = {
        let coordinator = Arc::clone(&coordinator);
        let session = session.clone();
        tokio::spawn(async move { try_lookup(&coordinator, &session).await })
    };
    settle().await;
    dropped.abort();
    assert!(dropped.await.is_err(), "the lookup was cancelled");
    assert_eq!(tombstone().await, Some("ended".to_owned()), "cancel");

    // A publication racing the read: the read defers to it.
    let raced = {
        let coordinator = Arc::clone(&coordinator);
        let session = session.clone();
        tokio::spawn(async move { try_lookup(&coordinator, &session).await })
    };
    settle().await;
    let mut successor = ended(&session);
    successor.incarnation_id = "incarnation-successor".to_owned();
    coordinator.cache_route(successor).await;
    gate.close();
    let answer = raced.await.expect("task").expect("lookup");
    assert_eq!(
        answer.map(|route| route.incarnation_id),
        Some("incarnation-successor".to_owned()),
        "shard race: the publication wins over the read that predates it"
    );
    *replica
        .quorum_gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;

    not_active(lookup(&coordinator, &session).await, "after every case");
    assert_eq!(
        replica.total(&replica.view_reads),
        0,
        "the replica holding the pre-release row was never read"
    );
}

/// Review B2: a fact is retired only into a view that can answer. In the
/// legacy layout — a healthy proof, every floor met, and the view answering
/// Authority — an `Observed` entry lives its whole lease, so routing costs
/// one quorum read per session per lease, as before E1c, not one per
/// sampling period.
#[tokio::test(start_paused = true)]
async fn the_legacy_layout_costs_one_quorum_read_per_session_per_lease() {
    let (coordinator, replica, _store) = coordinator_over_replica(true);
    replica.set_answers(false);
    let session = id(0xe1c_341);
    replica.commit(media_route(&session));
    let mut requests = 0_u64;
    let started = tokio::time::Instant::now();
    while started.elapsed() < Duration::from_millis(2_200) {
        replica.catch_up(); // every fact is applied here at once
        assert!(lookup(&coordinator, &session).await.is_some());
        requests += 1;
        tokio::time::advance(Duration::from_millis(40)).await;
    }
    assert_eq!(requests, 55);
    assert_eq!(
        replica.quorum_total(),
        3,
        "one quorum read at 0 s, 1 s and 2 s, however soon each fact applies"
    );
    assert_eq!(replica.total(&replica.view_reads), 0);
}

/// §8 "degraded mode": with no proof (lag, a leader gap, an old leader, a
/// remote-authority process) the cache is today's: one quorum read per
/// session per lease, not one per request — and the proof's return reopens
/// the view by itself.
#[tokio::test(start_paused = true)]
async fn without_a_proof_quorum_reads_are_one_per_session_per_lease() {
    let (coordinator, replica, _store) = coordinator_over_replica(false);
    let session = id(0xe1c_401);
    replica.commit(media_route(&session));
    let started = tokio::time::Instant::now();
    let mut requests = 0_u64;
    while started.elapsed() < Duration::from_millis(2_200) {
        assert!(lookup(&coordinator, &session).await.is_some());
        requests += 1;
        tokio::time::advance(Duration::from_millis(40)).await;
    }
    assert_eq!(requests, 55);
    assert_eq!(replica.quorum_total(), 3);
    assert_eq!(
        coordinator.route_cache_metrics.lookups(RouteLookup::Store),
        3
    );
    assert_eq!(replica.total(&replica.view_reads), 0);
    assert_eq!(
        coordinator
            .route_cache_metrics
            .lookups(RouteLookup::Observed),
        requests - 3
    );

    // The proof returns by itself: once a confirmation covers the fact, the
    // entry retires and the replica answers.
    replica.apply(media_route(&session));
    replica.set_proof(true);
    replica.catch_up();
    assert!(lookup(&coordinator, &session).await.is_some());
    assert_eq!(replica.total(&replica.view_reads), 1);
    assert_eq!(replica.quorum_total(), 3);
}

/// Review S4: while the image is being replaced (an odd generation) a `View`
/// entry answers from Authority and an unretired `Observed` entry is neither
/// retired nor replaced by a view answer, even with its confirmation applied;
/// both recover once the image is stable.
#[tokio::test(start_paused = true)]
async fn an_image_replacement_closes_view_entries_and_keeps_facts() {
    let (coordinator, replica, _store) = coordinator_over_replica(true);
    let viewed = id(0xe1c_451);
    let fact = id(0xe1c_452);
    replica.apply(media_route(&viewed));
    replica.commit(media_route(&viewed));
    replica.apply(media_route(&fact)); // the replica's pre-release row
    assert!(lookup(&coordinator, &viewed).await.is_some());
    coordinator.cache_terminal_route(ended(&fact)).await;

    replica.set_replacing(true);
    replica.catch_up();
    assert!(lookup(&coordinator, &viewed).await.is_some());
    assert_eq!(
        replica.total(&replica.authority_reads),
        1,
        "a View entry answers from Authority while the image is replaced"
    );
    for _ in 0..3 {
        assert_eq!(
            state_of(lookup(&coordinator, &fact).await),
            Some("ended".to_owned()),
            "a fact is not retired into a replacing image"
        );
    }
    assert_eq!(replica.total(&replica.view_reads), 1);

    replica.set_replacing(false);
    replica.apply(ended(&fact));
    replica.catch_up();
    assert_eq!(
        state_of(lookup(&coordinator, &fact).await),
        Some("ended".to_owned())
    );
    assert!(lookup(&coordinator, &viewed).await.is_some());
    assert_eq!(
        replica.total(&replica.view_reads),
        3,
        "both answer from the stable image again"
    );
}

/// Review S4: the proof lost between the lookup's exclusive check and its
/// read. The read answers from Authority and its answer is kept as a fact,
/// never as a view answer — for a stale `View` entry and for a retired fact.
#[tokio::test(start_paused = true)]
async fn a_proof_lost_mid_lookup_keeps_the_answer_as_a_fact() {
    let (coordinator, replica, _store) = coordinator_over_replica(true);
    let viewed = id(0xe1c_461);
    let retired = id(0xe1c_462);
    replica.apply(media_route(&viewed));
    replica.commit(media_route(&viewed));
    replica.commit(media_route(&retired));
    assert!(lookup(&coordinator, &viewed).await.is_some());
    coordinator.cache_route(media_route(&retired)).await;
    replica.apply(media_route(&id(0xe1c_463))); // the View entry is stale
    replica.catch_up(); // the fact is retirable

    for session in [&viewed, &retired] {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *coordinator
            .route_store_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&gate));
        let held = {
            let coordinator = Arc::clone(&coordinator);
            let session = session.clone();
            tokio::spawn(async move { lookup(&coordinator, &session).await })
        };
        settle().await;
        replica.set_proof(false);
        gate.close();
        assert!(held.await.expect("task").is_some());
        *coordinator
            .route_store_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        assert!(
            coordinator.cached_route_is_a_fact(session).await,
            "an Authority answer is cached as a fact"
        );
        replica.set_proof(true);
    }
    assert_eq!(replica.total(&replica.view_reads), 1, "only the first read");
}

/// §8 "cache-miss storm", and review S3: under one proof, ids past the cache's
/// capacity cost local reads only, evict view answers, and never evict the
/// release this node published or close the view for the table.
#[tokio::test(start_paused = true)]
async fn a_cache_miss_storm_past_capacity_keeps_facts_and_costs_no_quorum_read() {
    let (coordinator, replica, _store) = coordinator_over_replica(true);
    let published = id(0xe1c_501);
    replica.apply(media_route(&published)); // the pre-release row
    coordinator.cache_terminal_route(ended(&published)).await;
    let storm = MAX_ROUTE_CACHE_ENTRIES as u64 + 500;
    for n in 0..storm {
        assert!(lookup(&coordinator, &uuid::Uuid::new_v4().to_string())
            .await
            .is_none());
        assert_eq!(n + 1, replica.total(&replica.view_reads));
    }
    assert_eq!(replica.quorum_total(), 0);
    assert_eq!(
        coordinator.route_cache_metrics.lookups(RouteLookup::Store),
        0
    );
    assert_eq!(
        coordinator.route_cache_metrics.lookups(RouteLookup::View),
        storm
    );
    assert_eq!(
        coordinator
            .cached_route_entry(&published)
            .await
            .map(state_of),
        Some(Some("ended".to_owned())),
        "the published release outlives the storm"
    );
    assert_eq!(replica.closed(), None, "no eviction closed the view");
}

/// §4.4-3 and review S3: no scan runs below capacity. At capacity the scan
/// evicts a stale `View` entry if any, else the oldest entry that needs no
/// floor, and an unretired fact only when every entry is one — the oldest,
/// with the view closed for the table until the replica holds it.
#[tokio::test(start_paused = true)]
async fn the_route_cache_evicts_at_capacity_stale_first_and_facts_last() {
    let (coordinator, replica, _store) = coordinator_over_replica(true);
    let entries = || coordinator.routes.try_lock().expect("idle cache").len();
    let has = |session: String| {
        coordinator
            .routes
            .try_lock()
            .expect("idle cache")
            .contains_key(&session)
    };
    let prune_count = || {
        let exposition = coordinator.route_cache_metrics.prometheus();
        exposition
            .lines()
            .find_map(|line| line.strip_prefix("plurx_media_session_route_prune_entries_count "))
            .expect("prune count")
            .parse::<u64>()
            .expect("integer")
    };
    // The oldest entry is a fact this node published; then view answers.
    coordinator.cache_route(media_route(&id(1))).await;
    for n in 2..=MAX_ROUTE_CACHE_ENTRIES as u32 {
        assert!(lookup(&coordinator, &id(n)).await.is_none());
    }
    assert_eq!(entries(), MAX_ROUTE_CACHE_ENTRIES);
    assert_eq!(prune_count(), 0, "no scan below capacity");

    // Full, nothing stale: the oldest view answer goes, not the older fact.
    assert!(lookup(&coordinator, &id(0x10_000)).await.is_none());
    assert_eq!(entries(), MAX_ROUTE_CACHE_ENTRIES);
    assert_eq!(prune_count(), 1);
    assert!(has(id(1)), "the fact is kept");
    assert!(!has(id(2)), "the oldest view answer went");
    assert_eq!(replica.closed(), None);

    // The table moves: a stale view answer goes.
    replica.apply(media_route(&id(0x10_001)));
    let before = coordinator.routes.try_lock().expect("idle").clone();
    assert!(lookup(&coordinator, &id(0x10_002)).await.is_none());
    let after = coordinator.routes.try_lock().expect("idle").clone();
    assert_eq!(prune_count(), 2);
    let evicted: Vec<_> = before
        .keys()
        .filter(|key| !after.contains_key(*key))
        .collect();
    assert_eq!(evicted.len(), 1);
    assert!(matches!(before[evicted[0]].origin, RouteOrigin::View(_)));
    assert!(has(id(1)));

    // Every entry an unretired fact: the oldest goes and the view closes.
    for n in 0..MAX_ROUTE_CACHE_ENTRIES as u32 {
        coordinator
            .cache_route(media_route(&id(0x20_000 + n)))
            .await;
    }
    assert!(
        !has(id(1)),
        "the oldest fact went once only facts were left"
    );
    assert_eq!(replica.closed(), Some(ViewFallback::Floor));
    replica.catch_up();
    assert_eq!(replica.closed(), None);
}

/// §8 guard "a fence never reads the view", with the cache populated by a
/// view answer that disagrees with Authority: the final verdicts, the
/// prepared-media authorisation (the route is behind its publication fence,
/// so its store branch runs), the status bracket and reclassification, the
/// control route and a release make no read through the applied view and
/// answer Authority's row.
#[tokio::test]
async fn a_fence_never_reads_the_view_even_with_a_cached_answer() {
    let (coordinator, replica, store) = coordinator_over_replica(true);
    let session = id(0xe1c_601);
    let durable = committed(&store, &session, "node-c").await;
    let mut stale = durable.clone();
    stale.owner_node_id = "node-stale".to_owned();
    replica.apply(stale);
    assert_eq!(
        lookup(&coordinator, &session)
            .await
            .map(|route| route.owner_node_id),
        Some("node-stale".to_owned()),
        "the cache holds a view answer that disagrees with Authority"
    );
    let reads = || replica.total(&replica.view_reads) + replica.quorum_total();
    let calls = replica.total(&replica.calls);
    let deadline = || Instant::now() + Duration::from_secs(1);
    let owner = |resolution: DurableRouteResolution| match resolution {
        DurableRouteResolution::ActiveLocal(route)
        | DurableRouteResolution::ActiveRemote(route)
        | DurableRouteResolution::OwnerTransition(route)
        | DurableRouteResolution::Terminal(route) => Some(route.owner_node_id),
        DurableRouteResolution::Absent => None,
    };
    assert_eq!(
        owner(
            coordinator
                .authoritative_route_resolution_before(&session, "node-c", deadline())
                .await
                .expect("verdict")
        ),
        Some("node-c".to_owned())
    );
    assert_eq!(
        owner(
            coordinator
                .authoritative_media_route_resolution_before(&session, "node-c", deadline())
                .await
                .expect("media verdict")
        ),
        Some("node-c".to_owned())
    );
    for site in [
        RouteReadSite::StatusBracket,
        RouteReadSite::StatusReclassify,
    ] {
        assert_eq!(
            owner(
                coordinator
                    .status_media_route_resolution_before(site, &session, "node-c", deadline())
                    .await
                    .expect("status fence")
            ),
            Some("node-c".to_owned())
        );
    }
    assert_eq!(
        coordinator
            .control_route(&session)
            .await
            .expect("control")
            .map(|route| route.owner_node_id),
        Some("node-c".to_owned())
    );
    assert_eq!(
        replica.total(&replica.calls),
        calls,
        "a fence asked the applied view"
    );
    // A release publishes into the cache (one per-row fact) and reads nothing.
    let ReleaseAdmission::Won(_) = coordinator.begin_release_reconciliation(&session).await else {
        panic!("the release is admitted");
    };
    coordinator.complete_release_absent(&session).await;
    assert_eq!(reads(), 1, "only the priming view read");
}

/// The source half of the guard (review S2): no fence names any route-cache
/// or applied-view entry point, and every caller of those entry points is the
/// pinned set (`http/applied_route_inventory.rs`).
#[test]
fn the_fence_bodies_never_name_the_applied_view() {
    let source = include_str!("../media_sessions.rs");
    for function in [
        "async fn authoritative_route_resolution_at(",
        "async fn authoritative_media_route_resolution_at(",
        "pub(crate) async fn authoritative_route_resolution_before(",
        "pub(crate) async fn authoritative_media_route_resolution_before(",
        "pub(crate) async fn control_route(",
        "async fn authorize_prepared_media_before(",
        "pub(crate) async fn status_media_route_resolution_before(",
    ] {
        let start = source
            .find(function)
            .unwrap_or_else(|| panic!("{function}"));
        let body = &source[start..];
        let end = body.find("\n    }\n").expect("body end");
        let body = &body[..end];
        for name in [
            "applied",
            "read_media_session_route_view",
            "read_media_session_route_quorum",
            "raw_route_before",
            ".route_resolution_at(",
            ".media_route_resolution_at(",
            ".cached_route(",
            "cached_route_is_a_fact",
            ".lock_routes(",
            "self.routes",
            "RouteView",
        ] {
            assert!(!body.contains(name), "{function} names {name}");
        }
        assert!(
            ["read_media_session_route(", "self.store", "authoritative_"]
                .iter()
                .any(|read| body.contains(read)),
            "{function} reads the Store"
        );
    }
}

/// While one read for a capability is outstanding, every other lookup of it
/// waits on its shard and is answered by the entry that read filled.
#[tokio::test]
async fn concurrent_lookups_of_one_session_share_one_view_read() {
    const LOOKUPS: u64 = 8;
    let (coordinator, replica, _store) = coordinator_over_replica(true);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *coordinator
        .route_store_gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&gate));
    let session = id(0xe1c_701);
    let lookups = (0..LOOKUPS)
        .map(|_| {
            let coordinator = Arc::clone(&coordinator);
            let session = session.clone();
            tokio::spawn(async move { coordinator.route(&session).await })
        })
        .collect::<Vec<_>>();
    for _ in 0..256 {
        tokio::task::yield_now().await;
    }
    gate.close();
    for lookup in lookups {
        assert!(lookup.await.expect("task").expect("lookup").is_none());
    }
    assert_eq!(replica.total(&replica.view_reads), 1);
    assert_eq!(
        coordinator
            .route_cache_metrics
            .lookups(RouteLookup::SingleFlightHit),
        LOOKUPS - 1
    );
}

/// Review S5: on SQLite the store is the only copy and hands out no stamp;
/// its answer is held for one lease, as the cache before E1c held it, so a
/// standalone server's repeated lookups are map hits.
#[tokio::test]
async fn sqlite_answers_are_held_for_one_lease() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let coordinator = MediaSessionCoordinator::new(
        plurx_core::cluster::membership::MembershipManager::unavailable(),
        Arc::clone(&store),
    );
    let session = id(0xe1c_801);
    let durable = committed(&store, &session, "node-c").await;
    for _ in 0..5 {
        assert_eq!(
            lookup(&coordinator, &session)
                .await
                .map(|route| route.incarnation_id),
            Some(durable.incarnation_id.clone())
        );
    }
    assert_eq!(
        coordinator.route_cache_metrics.lookups(RouteLookup::View),
        1
    );
    assert_eq!(
        coordinator
            .route_cache_metrics
            .lookups(RouteLookup::Observed),
        4
    );
}
