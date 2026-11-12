//! Playback-start E1c: the route cache over the node's applied view
//! (`docs/streaming/PLAYBACK-START-E1-BUILD.md` §4.4, §8).
//!
//! The cache is driven through [`ReplicaRoutes`], a double of the applied
//! view whose replica image, table revision, proof and per-fact floors a
//! test moves by hand, so each case states exactly what this node has
//! applied. Authority is the SQLite store behind the coordinator; cases on a
//! paused clock make no Store call, so no deadline can fire under them.
use super::tests::media_route;
use super::*;
use plurx_core::domain::MediaSessionActivation;
use plurx_core::store::{SqliteStore, ViewFallback};
use std::sync::atomic::AtomicBool;

/// A replica of `media_sessions` as the route cache sees it.
pub(super) struct ReplicaRoutes {
    authority: Arc<dyn Store>,
    /// What this node has applied.
    rows: StdMutex<HashMap<String, MediaSessionRoute>>,
    revision: AtomicU64,
    proof: AtomicBool,
    /// Facts numbered at or below this are applied here.
    applied_facts: AtomicU64,
    next_fact: AtomicU64,
    /// The highest fact `hold_floor` closed the table for.
    held: AtomicU64,
    pub(super) view_reads: AtomicU64,
    pub(super) authority_reads: AtomicU64,
    pub(super) reloads: AtomicU64,
    /// Every trait call, for the fence guard.
    pub(super) calls: AtomicU64,
}

impl ReplicaRoutes {
    pub(super) fn new(authority: Arc<dyn Store>, proof: bool) -> Arc<Self> {
        Arc::new(Self {
            authority,
            rows: StdMutex::new(HashMap::new()),
            revision: AtomicU64::new(1),
            proof: AtomicBool::new(proof),
            applied_facts: AtomicU64::new(0),
            next_fact: AtomicU64::new(0),
            held: AtomicU64::new(0),
            view_reads: AtomicU64::new(0),
            authority_reads: AtomicU64::new(0),
            reloads: AtomicU64::new(0),
            calls: AtomicU64::new(0),
        })
    }

    /// Apply one entry that writes `route` here: the slot moves.
    pub(super) fn apply(&self, route: MediaSessionRoute) {
        self.rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(route.session_id.clone(), route);
        self.revision.fetch_add(1, Ordering::AcqRel);
    }

    /// A confirmation after every fact noted so far is applied here.
    pub(super) fn catch_up(&self) {
        self.applied_facts
            .store(self.next_fact.load(Ordering::Acquire), Ordering::Release);
    }

    pub(super) fn set_proof(&self, proof: bool) {
        self.proof.store(proof, Ordering::Release);
    }

    fn closed(&self) -> Option<ViewFallback> {
        if !self.proof.load(Ordering::Acquire) {
            return Some(ViewFallback::NoProof);
        }
        (self.held.load(Ordering::Acquire) > self.applied_facts.load(Ordering::Acquire))
            .then_some(ViewFallback::Floor)
    }

    fn total(&self, counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Acquire)
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
                    route: self.authority.media_session_route(session_id).await?,
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
}

impl ReplicaRoutes {
    fn same_image_uncounted(&self, stamp: RouteStamp) -> bool {
        matches!(stamp, RouteStamp::Test(revision) if revision == self.revision.load(Ordering::Acquire))
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
    let replica = ReplicaRoutes::new(Arc::clone(&store), proof);
    let applied: Arc<dyn RouteView> = replica.clone();
    let coordinator = MediaSessionCoordinator::with_applied_view(
        plurx_core::cluster::membership::MembershipManager::unavailable(),
        Arc::clone(&store),
        applied,
    );
    (coordinator, replica, store)
}

/// A committed active row in the Authority store, and the route it returns.
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

async fn lookup(
    coordinator: &MediaSessionCoordinator,
    session_id: &str,
) -> Option<MediaSessionRoute> {
    coordinator
        .raw_route_before(
            RouteReadSite::Routing,
            session_id,
            tokio::time::Instant::now() + ROUTE_QUERY_DEADLINE,
        )
        .await
        .expect("route lookup")
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
        lookup(&coordinator, &watched)
            .await
            .map(|route| route.state),
        Some("active".to_owned())
    );
    for second in 0..10 {
        tokio::time::advance(Duration::from_secs(1)).await;
        replica.apply(media_route(&id(0xe1c_100 + second)));
        assert!(lookup(&coordinator, &watched).await.is_some());
    }
    assert_eq!(replica.total(&replica.authority_reads), 0);
    assert_eq!(
        coordinator.route_cache_metrics.lookups(RouteLookup::Store),
        0,
        "no quorum read while the proof holds"
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
#[tokio::test]
async fn a_peer_release_applied_here_moves_the_slot_and_the_next_lookup_rereads() {
    let (coordinator, replica, _store) = coordinator_over_replica(true);
    let session = id(0xe1c_201);
    replica.apply(media_route(&session));
    assert_eq!(
        lookup(&coordinator, &session)
            .await
            .map(|route| route.state),
        Some("active".to_owned())
    );
    let mut ended = media_route(&session);
    ended.state = "ended".to_owned();
    replica.apply(ended);
    assert_eq!(
        lookup(&coordinator, &session)
            .await
            .map(|route| route.state),
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
    assert_eq!(replica.total(&replica.authority_reads), 0);
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
#[tokio::test]
async fn a_replica_behind_a_local_release_never_revives_the_pre_release_row() {
    let (coordinator, replica, store) = coordinator_over_replica(true);
    let session = id(0xe1c_301);
    let active = committed(&store, &session, "node-c").await;
    replica.apply(active.clone()); // W−3
    assert_eq!(
        lookup(&coordinator, &session)
            .await
            .map(|route| route.state),
        Some("active".to_owned())
    );

    // W: the release, committed through this node, published as it returns.
    let released = store
        .end_media_session(&session, "client", unix_ms())
        .await
        .expect("end")
        .expect("ended row");
    coordinator.cache_terminal_route(released.clone()).await;
    replica.apply(media_route(&id(0xe1c_302))); // W−2
    replica.apply(media_route(&id(0xe1c_303))); // W−1
    let deadline = tokio::time::Instant::now() + OBSERVED_ROUTE_LEASE + Duration::from_millis(300);
    let mut checks = 0;
    while tokio::time::Instant::now() < deadline {
        let route = lookup(&coordinator, &session).await;
        assert_ne!(
            route.as_ref().map(|route| route.state.as_str()),
            Some("active"),
            "a lagging replica revived the pre-release row"
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
        checks += 1;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(checks > 10);
    assert!(
        coordinator
            .route_cache_metrics
            .lookups(RouteLookup::Observed)
            > 0,
        "the tombstone answered while it was inside its lease"
    );
    assert!(
        coordinator.route_cache_metrics.lookups(RouteLookup::Store) >= 1,
        "past the lease, an unretired fact is replaced by a quorum read"
    );
    assert_eq!(
        replica.total(&replica.view_reads),
        1,
        "no view read while the release is unretired"
    );

    // W applies here and a confirmation covers it: retired, the view answers.
    replica.apply(released);
    replica.catch_up();
    assert_eq!(
        lookup(&coordinator, &session)
            .await
            .map(|route| route.state),
        Some("ended".to_owned())
    );
    assert_eq!(replica.total(&replica.view_reads), 2);

    // A takeover through this node: never the old owner.
    let taken_id = id(0xe1c_311);
    let old = committed(&store, &taken_id, "node-old").await;
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

/// §8 "degraded mode": with no proof (lag, a leader gap, an old leader, a
/// remote-authority process) the cache is today's: one quorum read per
/// session per lease, not one per request.
#[tokio::test]
async fn without_a_proof_quorum_reads_are_one_per_session_per_lease() {
    let (coordinator, replica, store) = coordinator_over_replica(false);
    let session = id(0xe1c_401);
    let route = committed(&store, &session, "node-c").await;
    let started = tokio::time::Instant::now();
    let mut requests = 0_u64;
    while started.elapsed() < Duration::from_millis(2_200) {
        assert!(lookup(&coordinator, &session).await.is_some());
        requests += 1;
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let quorum = coordinator.route_cache_metrics.lookups(RouteLookup::Store);
    let leases = started.elapsed().as_millis() / OBSERVED_ROUTE_LEASE.as_millis() + 1;
    assert!(requests >= 30, "{requests} requests");
    assert!(
        (2..=u64::try_from(leases).expect("leases")).contains(&quorum),
        "{quorum} quorum reads for {requests} requests over {leases} leases"
    );
    assert_eq!(replica.total(&replica.view_reads), 0);
    assert_eq!(
        coordinator
            .route_cache_metrics
            .lookups(RouteLookup::Observed),
        requests - quorum
    );

    // The proof returns by itself: once a confirmation covers the fact, the
    // entry retires and the replica answers.
    replica.apply(route);
    replica.set_proof(true);
    replica.catch_up();
    assert!(lookup(&coordinator, &session).await.is_some());
    assert_eq!(replica.total(&replica.view_reads), 1);
    assert_eq!(
        coordinator.route_cache_metrics.lookups(RouteLookup::Store),
        quorum
    );
}

/// §8 "cache-miss storm": 4,096 random capabilities under one proof cost
/// local reads only. The old cache made each a Store read.
#[tokio::test]
async fn a_cache_miss_storm_under_one_proof_costs_no_quorum_read() {
    let (coordinator, replica, _store) = coordinator_over_replica(true);
    for n in 0..4_096 {
        assert!(lookup(&coordinator, &uuid::Uuid::new_v4().to_string())
            .await
            .is_none());
        assert_eq!(n + 1, replica.total(&replica.view_reads));
    }
    assert_eq!(replica.total(&replica.authority_reads), 0);
    assert_eq!(
        coordinator.route_cache_metrics.lookups(RouteLookup::Store),
        0
    );
    assert_eq!(
        coordinator.route_cache_metrics.lookups(RouteLookup::View),
        4_096
    );
}

/// §4.4-3: no sweep runs below capacity; at capacity one scan evicts a
/// stale entry if any, else the lowest insert sequence, and an `Observed`
/// entry evicted before its floor is met closes the view for the table.
#[tokio::test]
async fn the_route_cache_evicts_at_capacity_stale_first_then_oldest() {
    let (coordinator, replica, _store) = coordinator_over_replica(true);
    let entries = || coordinator.routes.try_lock().expect("idle cache").len();
    let prune_count = || {
        let exposition = coordinator.route_cache_metrics.prometheus();
        exposition
            .lines()
            .find_map(|line| line.strip_prefix("plurx_media_session_route_prune_entries_count "))
            .expect("prune count")
            .parse::<u64>()
            .expect("integer")
    };
    // The oldest entry is a fact this node published.
    coordinator.cache_route(media_route(&id(1))).await;
    for n in 2..=MAX_ROUTE_CACHE_ENTRIES as u32 {
        assert!(lookup(&coordinator, &id(n)).await.is_none());
    }
    assert_eq!(entries(), MAX_ROUTE_CACHE_ENTRIES);
    assert_eq!(prune_count(), 0, "no scan below capacity");

    // Full, nothing stale: the lowest sequence goes — the unretired fact —
    // and the view closes for the table until the replica holds it.
    assert!(lookup(&coordinator, &id(0xe1c_501)).await.is_none());
    assert_eq!(entries(), MAX_ROUTE_CACHE_ENTRIES);
    assert_eq!(prune_count(), 1);
    assert!(!coordinator
        .routes
        .try_lock()
        .expect("idle")
        .contains_key(&id(1)));
    assert_eq!(replica.closed(), Some(ViewFallback::Floor));
    replica.catch_up();
    assert_eq!(replica.closed(), None);

    // The table moves: every view entry is stale, the first one scanned goes.
    replica.apply(media_route(&id(0xe1c_502)));
    let before = coordinator.routes.try_lock().expect("idle").clone();
    assert!(lookup(&coordinator, &id(0xe1c_503)).await.is_none());
    let after = coordinator.routes.try_lock().expect("idle").clone();
    assert_eq!(after.len(), MAX_ROUTE_CACHE_ENTRIES);
    assert_eq!(prune_count(), 2);
    let evicted: Vec<_> = before
        .keys()
        .filter(|key| !after.contains_key(*key))
        .collect();
    assert_eq!(evicted.len(), 1);
    assert!(matches!(before[evicted[0]].origin, RouteOrigin::View(_)));
}

/// §8 guard "a fence never reads the view": the final verdicts, the control
/// route, the status bracket and reclassification, and the prepared-media
/// authorisation make no call on the applied view at all.
#[tokio::test]
async fn a_fence_never_reads_the_view() {
    let (coordinator, replica, store) = coordinator_over_replica(true);
    let session = id(0xe1c_601);
    committed(&store, &session, "node-c").await;
    let deadline = || Instant::now() + Duration::from_secs(1);
    coordinator
        .authoritative_route_resolution_before(&session, "node-c", deadline())
        .await
        .expect("verdict");
    coordinator
        .authoritative_media_route_resolution_before(&session, "node-c", deadline())
        .await
        .expect("media verdict");
    for site in [
        RouteReadSite::StatusBracket,
        RouteReadSite::StatusReclassify,
    ] {
        coordinator
            .status_media_route_resolution_before(site, &session, "node-c", deadline())
            .await
            .expect("status fence");
    }
    coordinator.control_route(&session).await.expect("control");
    assert_eq!(
        replica.total(&replica.calls),
        0,
        "a fence asked the applied view"
    );
    // The ordinary routing lookup is the one that does.
    assert!(lookup(&coordinator, &session).await.is_none());
    assert!(replica.total(&replica.calls) > 0);
}

/// The source half of the guard: the fence bodies never name the view.
#[test]
fn the_fence_bodies_never_name_the_applied_view() {
    let source = include_str!("../media_sessions.rs");
    for function in [
        "async fn authoritative_route_resolution_at(",
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
            "raw_route_before",
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
