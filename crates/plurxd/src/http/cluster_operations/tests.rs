use super::*;
use plurx_core::cluster::membership::{ClusterRecoveryStatus, NodeRole};

fn fresh_cache_statuses(read: PeerStatusCacheRead) -> BTreeMap<String, PeerStatusOutcome> {
    match read {
        PeerStatusCacheRead::Fresh(statuses, _) => statuses,
        PeerStatusCacheRead::Unavailable => panic!("cache is unavailable"),
        PeerStatusCacheRead::Stale => panic!("cache is stale"),
    }
}

fn fresh_membership_status(read: MembershipStatusCacheRead) -> MembershipStatus {
    match read {
        MembershipStatusCacheRead::Fresh(status) => *status,
        MembershipStatusCacheRead::Unavailable => panic!("membership cache is unavailable"),
        MembershipStatusCacheRead::Stale => panic!("membership cache is stale"),
    }
}

fn cached_private_transport(
    read: PeerStatusCacheRead,
    node_id: &str,
    expected_observer: u64,
    _aggregate_observed_ms: u64,
) -> SnapshotTransportStatus {
    let (mut statuses, cache_age) = match read {
        PeerStatusCacheRead::Fresh(statuses, age) => (statuses, age),
        PeerStatusCacheRead::Unavailable => panic!("cache is unavailable"),
        PeerStatusCacheRead::Stale => panic!("cache is stale"),
    };
    let transport = statuses
        .remove(node_id)
        .and_then(|outcome| outcome.transport)
        .expect("cached private transport evidence");
    sanitize_transport(
        transport,
        expected_observer,
        u64::try_from(cache_age.as_millis()).unwrap_or(u64::MAX),
    )
    .expect("valid cached private transport evidence")
}

fn test_transport_status(
    observing_node_id: u64,
    sample_age_ms: u64,
    active_deadline_remaining_ms: Option<u64>,
) -> SnapshotTransportStatus {
    serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "observing_node_id": observing_node_id,
        "observed_at_unix_ms": unix_ms(),
        "observations": [{
            "observing_node_id": observing_node_id,
            "peer_node_id": 1,
            "raft_group": "sqlite",
            "boot_id": "test-boot",
            "attempt_id": 7,
            "snapshot_id": "test-snapshot",
            "socket_epoch": 3,
            "direction": "inbound",
            "attempted_offset": 64,
            "acknowledged_offset": null,
            "locally_received_bytes": 64,
            "total_bytes": null,
            "attempt_age_ms": 100,
            "last_acknowledgement_age_ms": null,
            "last_local_receive_age_ms": 10,
            "active_deadline_remaining_ms": active_deadline_remaining_ms,
            "sample_age_ms": sample_age_ms,
            "phase": "transferring",
            "reconnect_count": 0,
            "retry_count": 0,
            "last_error_category": null,
            "operation_owns_work": false
        }]
    }))
    .expect("deserialize transport fixture through the public schema")
}

#[tokio::test(start_paused = true)]
async fn peer_fanout_applies_the_one_second_per_peer_deadline() {
    let peers = (0..MAX_OPERATIONS_PEERS)
        .map(|index| ActivityPeer {
            node_id: format!("node-{index}"),
            raft_id: index as u64,
            http_base: Some(format!("http://node-{index}:8080")),
            reachable: true,
        })
        .collect();
    let started = tokio::time::Instant::now();
    let deadline = started + Duration::from_secs(2);
    let outcomes = collect_peer_statuses_with(peers, deadline, |peer, _peer_deadline| async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        PeerStatusOutcome {
            node_id: peer.node_id,
            state: ObservationState::Answered,
            status: None,
            transport: None,
        }
    })
    .await;

    assert_eq!(
        tokio::time::Instant::now() - started,
        Duration::from_secs(1)
    );
    assert_eq!(outcomes.len(), MAX_OPERATIONS_PEERS);
    assert!(outcomes
        .values()
        .all(|outcome| outcome.state == ObservationState::TimedOut));
}

#[tokio::test]
async fn peer_fanout_never_exceeds_the_eight_peer_bound() {
    let peers = (0..(MAX_OPERATIONS_PEERS + 4))
        .map(|index| ActivityPeer {
            node_id: format!("node-{index}"),
            raft_id: index as u64,
            http_base: Some(format!("http://node-{index}:8080")),
            reachable: true,
        })
        .collect();
    let outcomes = collect_peer_statuses_with(
        peers,
        tokio::time::Instant::now() + Duration::from_secs(2),
        |peer, _peer_deadline| async move {
            PeerStatusOutcome {
                node_id: peer.node_id,
                state: ObservationState::Answered,
                status: None,
                transport: None,
            }
        },
    )
    .await;

    assert_eq!(outcomes.len(), MAX_OPERATIONS_PEERS + 4);
    assert_eq!(
        outcomes
            .values()
            .filter(|outcome| outcome.state == ObservationState::PeerLimit)
            .count(),
        4
    );
    assert_eq!(MAX_OPERATIONS_PEERS, 8);
}

#[tokio::test]
async fn production_collector_labels_directory_overflow_without_probing_it() {
    let peers = (0..(MAX_OPERATIONS_PEERS + 4))
        .map(|index| ActivityPeer {
            node_id: format!("node-{index}"),
            raft_id: index as u64,
            http_base: Some(format!("http://node-{index}:32400")),
            reachable: true,
        })
        .collect();
    let public_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let private_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let outcomes = collect_peer_statuses_with_sources(
        peers,
        tokio::time::Instant::now() + AGGREGATE_TIMEOUT,
        {
            let public_calls = public_calls.clone();
            move |peer, _peer_deadline| {
                public_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async move {
                    PeerStatusOutcome {
                        node_id: peer.node_id,
                        state: ObservationState::Answered,
                        status: None,
                        transport: None,
                    }
                }
            }
        },
        {
            let private_calls = private_calls.clone();
            move |_raft_id| {
                private_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async { Ok(None) }
            }
        },
    )
    .await;

    assert_eq!(outcomes.len(), MAX_OPERATIONS_PEERS + 4);
    assert_eq!(
        public_calls.load(std::sync::atomic::Ordering::Relaxed),
        MAX_OPERATIONS_PEERS
    );
    assert_eq!(
        private_calls.load(std::sync::atomic::Ordering::Relaxed),
        MAX_OPERATIONS_PEERS
    );
    assert_eq!(
        outcomes
            .values()
            .filter(|outcome| outcome.state == ObservationState::PeerLimit)
            .count(),
        4
    );
}

#[tokio::test(start_paused = true)]
async fn peer_status_cache_is_fresh_for_five_seconds_then_expires() {
    let cache = PeerStatusCache::default();
    let statuses = BTreeMap::from([(
        "node-2".to_owned(),
        PeerStatusOutcome {
            node_id: "node-2".to_owned(),
            state: ObservationState::Answered,
            status: None,
            transport: None,
        },
    )]);
    cache.store(&statuses).await;
    assert_eq!(fresh_cache_statuses(cache.fresh().await).len(), 1);

    tokio::time::advance(PEER_STATUS_CACHE_TTL - Duration::from_millis(1)).await;
    assert!(matches!(
        cache.fresh().await,
        PeerStatusCacheRead::Fresh(_, _)
    ));
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(matches!(cache.fresh().await, PeerStatusCacheRead::Stale));
}

#[tokio::test(start_paused = true)]
async fn membership_status_cache_is_fresh_for_five_seconds_then_expires() {
    let cache = MembershipStatusCache::default();
    cache.store(test_membership("node-1", 3)).await;
    assert_eq!(fresh_membership_status(cache.fresh().await).nodes.len(), 3);

    tokio::time::advance(PEER_STATUS_CACHE_TTL - Duration::from_millis(1)).await;
    assert!(matches!(
        cache.fresh().await,
        MembershipStatusCacheRead::Fresh(_)
    ));
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(matches!(
        cache.fresh().await,
        MembershipStatusCacheRead::Stale
    ));
}

#[tokio::test(start_paused = true)]
async fn unclustered_membership_cache_loop_exits_without_refresh_or_retry() {
    let (_app, state) = crate::http::tests::test_app_with_state();
    assert!(!state.membership.is_replicated());
    let cache = state.membership_status_cache.clone();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(membership_status_cache_loop(state, shutdown));

    tokio::task::yield_now().await;
    tokio::time::advance(PEER_STATUS_REFRESH_INTERVAL * 3).await;
    assert!(task.is_finished(), "unclustered loop kept retrying");
    task.await.expect("unclustered cache loop");
    assert!(matches!(
        cache.fresh().await,
        MembershipStatusCacheRead::Unavailable
    ));
}

#[test]
fn automatic_activation_runs_outside_the_membership_projection() {
    let source = include_str!("../cluster_operations.rs")
        .split_once("pub(crate) async fn membership_status_cache_loop")
        .expect("membership cache loop")
        .1
        .split_once("pub(crate) async fn peer_status_cache_loop")
        .expect("membership cache loop end")
        .0;
    assert!(source.contains("cache_admin_revocation_ready()"));
    assert!(!source.contains("activate_if_ready"));
    assert!(include_str!("../../main.rs")
        .contains("internal_auth_revocation::cache_admin_revocation_activation_loop"));
}

#[tokio::test(start_paused = true)]
async fn membership_projection_refresh_is_bounded_and_preserves_last_good_sample() {
    let cache = MembershipStatusCache::default();
    let refresh_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let refresh_started = tokio::time::Instant::now();
    refresh_membership_status_cache_with(&cache, refresh_started, {
        let refresh_calls = refresh_calls.clone();
        move || async move {
            refresh_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(test_membership("node-1", 3))
        }
    })
    .await
    .expect("initial membership projection");
    assert_eq!(refresh_calls.load(std::sync::atomic::Ordering::Relaxed), 1);

    let timed_out =
        refresh_membership_status_cache_with(&cache, tokio::time::Instant::now(), || async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(test_membership("node-1", 1))
        })
        .await;
    assert_eq!(
        timed_out.expect_err("slow membership projection must time out"),
        "membership projection timed out"
    );
    assert_eq!(fresh_membership_status(cache.fresh().await).nodes.len(), 3);
}

#[tokio::test(start_paused = true)]
async fn capability_refresh_error_and_rollback_clear_cache_only_admin_authority() {
    let cache = crate::http::extract::CacheOnlyAdminProofCache::new(true);
    let digest = plurx_core::auth::hash_token("capability-refresh-admin");
    let admin = plurx_core::domain::User {
        id: 77,
        username: "owner".to_owned(),
        password_hash: String::new(),
        is_admin: true,
        created_at: 1,
    };

    refresh_cache_admin_revocation_capability_with(&cache, tokio::time::Instant::now(), || async {
        Ok(true)
    })
    .await
    .expect("all committed heartbeat proofs");
    let ticket = cache.authentication_ticket();
    cache.record_authenticated(ticket, digest.clone(), &admin);
    assert!(cache.authenticate(&digest));

    let ready = refresh_cache_admin_revocation_capability_with(
        &cache,
        tokio::time::Instant::now(),
        || async { Ok(false) },
    )
    .await
    .expect("an old-binary heartbeat is a valid closed verdict");
    assert!(!ready);
    assert!(!cache.authenticate(&digest));

    refresh_cache_admin_revocation_capability_with(&cache, tokio::time::Instant::now(), || async {
        Ok(true)
    })
    .await
    .expect("roll forward");
    let ticket = cache.authentication_ticket();
    cache.record_authenticated(ticket, digest.clone(), &admin);
    assert!(cache.authenticate(&digest));

    let timeout = refresh_cache_admin_revocation_capability_with(
        &cache,
        tokio::time::Instant::now(),
        || async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(true)
        },
    )
    .await
    .expect_err("a stale capability projection must close authority");
    assert!(timeout.contains("timed out"), "{timeout}");
    assert!(!cache.authenticate(&digest));
    assert!(cache.authentication_ticket().is_none());
}

#[tokio::test]
async fn full_roster_projection_activates_without_a_credential_mutation() {
    let activations = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_activations = activations.clone();
    let shutdown = tokio_util::sync::CancellationToken::new();
    super::super::internal_auth_revocation::cache_admin_revocation_activation_loop_with(
            shutdown,
            move || {
                observed_activations.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async {
                    Ok(
                        super::super::internal_auth_revocation::CacheAdminRevocationActivation::Complete,
                    )
                }
            },
        )
        .await;
    assert_eq!(activations.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[tokio::test(start_paused = true)]
async fn permanent_activation_stops_the_one_time_worker() {
    let shutdown = tokio_util::sync::CancellationToken::new();
    let activations = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_activations = activations.clone();
    super::super::internal_auth_revocation::cache_admin_revocation_activation_loop_with(
            shutdown,
            move || {
                observed_activations.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async {
                    Ok(
                        super::super::internal_auth_revocation::CacheAdminRevocationActivation::Complete,
                    )
                }
            },
        )
        .await;
    tokio::time::advance(Duration::from_secs(30)).await;
    assert_eq!(activations.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[tokio::test(start_paused = true)]
async fn stuck_automatic_activation_does_not_block_membership_refresh_or_shutdown() {
    let shutdown = tokio_util::sync::CancellationToken::new();
    let activation_shutdown = shutdown.clone();
    let activations = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_activations = activations.clone();
    let activation = tokio::spawn(
        super::super::internal_auth_revocation::cache_admin_revocation_activation_loop_with(
            activation_shutdown,
            move || {
                observed_activations.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                std::future::pending::<
                    Result<
                        super::super::internal_auth_revocation::CacheAdminRevocationActivation,
                        String,
                    >,
                >()
            },
        ),
    );
    tokio::task::yield_now().await;
    assert_eq!(activations.load(std::sync::atomic::Ordering::Relaxed), 1);

    let cache = MembershipStatusCache::default();
    for node_count in [1, 2, 3] {
        refresh_membership_status_cache_with(&cache, tokio::time::Instant::now(), || async move {
            Ok(test_membership("node-1", node_count))
        })
        .await
        .expect("membership projection remains independently refreshable");
        tokio::time::advance(PEER_STATUS_REFRESH_INTERVAL).await;
        assert_eq!(
            fresh_membership_status(cache.fresh().await).nodes.len(),
            node_count
        );
    }

    shutdown.cancel();
    tokio::task::yield_now().await;
    activation.await.expect("activation loop stops on shutdown");
}

#[tokio::test(start_paused = true)]
async fn full_refresh_cycle_keeps_cache_fresh_and_ages_transport_from_local_cache_time() {
    assert!(
        PEER_STATUS_REFRESH_INTERVAL + PEER_DIRECTORY_TIMEOUT + PEER_STATUS_TIMEOUT
            < PEER_STATUS_CACHE_TTL,
        "the refresh schedule needs strict overhead beyond directory and peer deadlines"
    );
    let cache = PeerStatusCache::default();
    cache.store(&BTreeMap::new()).await;
    tokio::time::advance(PEER_STATUS_REFRESH_INTERVAL).await;

    let source_observed_ms = 1_000_000;
    let mut private_transport = test_transport_status(2, 0, Some(10_000));
    private_transport.observed_at_unix_ms = source_observed_ms;
    let refresh_started = tokio::time::Instant::now();
    refresh_peer_status_cache_with(
        &cache,
        refresh_started,
        || async {
            tokio::time::sleep(PEER_DIRECTORY_TIMEOUT - Duration::from_millis(1)).await;
            Ok(vec![ActivityPeer {
                node_id: "node-2".to_owned(),
                raft_id: 2,
                http_base: Some("http://blackhole.invalid:32400".to_owned()),
                reachable: true,
            }])
        },
        move |peers, deadline| {
            let private_transport = private_transport.clone();
            async move {
                collect_peer_statuses_with_sources(
                        peers,
                        deadline,
                        |_peer, _deadline| async move {
                            std::future::pending::<PeerStatusOutcome>().await
                        },
                        move |_raft_id| {
                            let private_transport = private_transport.clone();
                            async move { Ok(Some(private_transport)) }
                        },
                    )
                    .await
            }
        },
    )
    .await
    .expect("bounded refresh");
    assert_eq!(
        tokio::time::Instant::now() - refresh_started,
        PEER_DIRECTORY_TIMEOUT - Duration::from_millis(1) + PEER_STATUS_TIMEOUT
    );

    let elapsed = PEER_STATUS_CACHE_TTL
        - PEER_STATUS_REFRESH_INTERVAL
        - PEER_DIRECTORY_TIMEOUT
        - PEER_STATUS_TIMEOUT
        + Duration::from_millis(1);
    tokio::time::advance(elapsed).await;
    let projected = cached_private_transport(
        cache.fresh().await,
        "node-2",
        2,
        source_observed_ms + PEER_STATUS_CACHE_TTL.as_millis() as u64,
    );
    let observation = &projected.observations[0];
    assert_eq!(observation.sample_age_ms, 1_501);
    assert_eq!(observation.active_deadline_remaining_ms, Some(8_499));
}

#[tokio::test(start_paused = true)]
async fn absent_and_expired_cache_are_unavailable_never_peer_limited() {
    let membership = test_membership("node-1", 2);
    let cache = PeerStatusCache::default();
    let aggregate_observed_ms = unix_ms();
    let rows = join_observations(
        &membership,
        "node-1",
        test_local_status("node-1", Some(1)),
        cache.fresh().await,
        aggregate_observed_ms,
    );
    assert_eq!(rows[1].observation, ObservationState::Unavailable);
    assert_eq!(rows[1].error_class.as_deref(), Some("cache_unavailable"));

    cache.store(&BTreeMap::new()).await;
    tokio::time::advance(PEER_STATUS_CACHE_TTL).await;
    let rows = join_observations(
        &membership,
        "node-1",
        test_local_status("node-1", Some(1)),
        cache.fresh().await,
        aggregate_observed_ms,
    );
    assert_eq!(rows[1].observation, ObservationState::Unavailable);
    assert_eq!(rows[1].error_class.as_deref(), Some("cache_stale"));
    assert!(rows
        .iter()
        .all(|row| row.observation != ObservationState::PeerLimit));
}

#[tokio::test(start_paused = true)]
async fn long_install_crosses_to_stalled_then_expires_after_the_post_deadline_window() {
    let source = test_transport_status(2, 301_000, Some(4_000));

    let mut at_deadline = source.clone();
    age_transport_observations(&mut at_deadline, 4_000);
    let observation = &at_deadline.observations[0];
    assert_eq!(observation.phase, SnapshotTransportPhase::Stalled);
    assert_eq!(observation.active_deadline_remaining_ms, Some(0));
    assert_eq!(observation.sample_age_ms, 0);
    assert_eq!(
        observation.last_error_category.as_deref(),
        Some("snapshot_stalled")
    );

    let mut just_before_expiry = source.clone();
    age_transport_observations(
        &mut just_before_expiry,
        4_000 + TRANSPORT_OBSERVATION_TTL_MS - 1,
    );
    assert_eq!(
        just_before_expiry.observations[0].phase,
        SnapshotTransportPhase::Stalled
    );
    assert_eq!(
        just_before_expiry.observations[0].sample_age_ms,
        TRANSPORT_OBSERVATION_TTL_MS - 1
    );

    let mut expired = source;
    age_transport_observations(&mut expired, 4_000 + TRANSPORT_OBSERVATION_TTL_MS + 1);
    assert!(expired.observations.is_empty());

    let mut already_stalled = test_transport_status(2, TRANSPORT_OBSERVATION_TTL_MS + 1, Some(0));
    already_stalled.observations[0].phase = SnapshotTransportPhase::Stalled;
    age_transport_observations(&mut already_stalled, 0);
    assert!(
        already_stalled.observations.is_empty(),
        "an already-stalled source sample must not receive a fresh retention window"
    );
}

#[tokio::test(start_paused = true)]
async fn cached_fresh_active_transport_stalls_exactly_when_deadline_reaches_zero() {
    let cache = PeerStatusCache::default();
    let source_observed_ms = 1_000_000;
    let mut transport = test_transport_status(2, 1_000, Some(1_000));
    transport.observed_at_unix_ms = source_observed_ms;
    let statuses = BTreeMap::from([(
        "node-2".to_owned(),
        PeerStatusOutcome {
            node_id: "node-2".to_owned(),
            state: ObservationState::Unreachable,
            status: None,
            transport: Some(transport),
        },
    )]);
    cache.store(&statuses).await;

    tokio::time::advance(Duration::from_millis(999)).await;
    let still_active =
        cached_private_transport(cache.fresh().await, "node-2", 2, source_observed_ms + 999);
    let observation = &still_active.observations[0];
    assert_eq!(observation.phase, SnapshotTransportPhase::Transferring);
    assert_eq!(observation.active_deadline_remaining_ms, Some(1));
    assert_eq!(observation.last_error_category, None);

    tokio::time::advance(Duration::from_millis(1)).await;
    let expired =
        cached_private_transport(cache.fresh().await, "node-2", 2, source_observed_ms + 1_000);
    let observation = &expired.observations[0];
    assert_eq!(observation.phase, SnapshotTransportPhase::Stalled);
    assert_eq!(observation.active_deadline_remaining_ms, Some(0));
    assert_eq!(observation.sample_age_ms, 0);
    assert_eq!(
        observation.last_error_category.as_deref(),
        Some("snapshot_stalled")
    );
}

#[test]
fn cached_deadline_less_active_transport_uses_the_producer_fallback_boundary() {
    let source = test_transport_status(2, 1_000, None);

    let mut just_before_stall = source.clone();
    age_transport_observations(&mut just_before_stall, 28_999);
    let observation = &just_before_stall.observations[0];
    assert_eq!(observation.phase, SnapshotTransportPhase::Transferring);
    assert_eq!(observation.active_deadline_remaining_ms, Some(1));
    assert_eq!(observation.sample_age_ms, 29_999);

    let mut at_stall = source.clone();
    age_transport_observations(&mut at_stall, 29_000);
    let observation = &at_stall.observations[0];
    assert_eq!(observation.phase, SnapshotTransportPhase::Stalled);
    assert_eq!(observation.active_deadline_remaining_ms, Some(0));
    assert_eq!(observation.sample_age_ms, 0);

    let mut at_expiry = source.clone();
    age_transport_observations(&mut at_expiry, 29_000 + TRANSPORT_OBSERVATION_TTL_MS);
    assert_eq!(
        at_expiry.observations[0].phase,
        SnapshotTransportPhase::Stalled
    );
    assert_eq!(
        at_expiry.observations[0].sample_age_ms,
        TRANSPORT_OBSERVATION_TTL_MS
    );

    let mut expired = source;
    age_transport_observations(&mut expired, 29_000 + TRANSPORT_OBSERVATION_TTL_MS + 1);
    assert!(expired.observations.is_empty());
}

#[tokio::test(start_paused = true)]
async fn cached_terminal_transport_observation_expires_at_five_minutes() {
    let cache = PeerStatusCache::default();
    let source_observed_ms = 1_000_000;
    let mut transport = test_transport_status(2, 299_000, None);
    transport.observations[0].phase = SnapshotTransportPhase::Failed;
    transport.observed_at_unix_ms = source_observed_ms;
    let statuses = BTreeMap::from([(
        "node-2".to_owned(),
        PeerStatusOutcome {
            node_id: "node-2".to_owned(),
            state: ObservationState::Unreachable,
            status: None,
            transport: Some(transport),
        },
    )]);
    cache.store(&statuses).await;

    tokio::time::advance(Duration::from_millis(999)).await;
    let fresh =
        cached_private_transport(cache.fresh().await, "node-2", 2, source_observed_ms + 999);
    assert_eq!(fresh.observations[0].sample_age_ms, 299_999);

    tokio::time::advance(Duration::from_millis(2)).await;
    let fresh =
        cached_private_transport(cache.fresh().await, "node-2", 2, source_observed_ms + 1_001);
    assert!(fresh.observations.is_empty());
}

#[tokio::test(start_paused = true)]
async fn transport_projection_uses_local_monotonic_age_despite_remote_clock_skew() {
    let source_observed_ms = 1_000_000;
    let mut transport = test_transport_status(2, 1_000, Some(20_000));
    transport.observed_at_unix_ms = source_observed_ms;
    let projected =
        sanitize_transport(transport, 2, 6_000).expect("locally timed authenticated transport");
    let observation = &projected.observations[0];
    assert_eq!(observation.sample_age_ms, 7_000);
    assert_eq!(observation.attempt_age_ms, Some(6_100));
    assert_eq!(observation.last_local_receive_age_ms, Some(6_010));
    assert_eq!(observation.active_deadline_remaining_ms, Some(14_000));

    let mut replayed = test_transport_status(2, 301_000, Some(30_000));
    replayed.observed_at_unix_ms = source_observed_ms;
    let projected = sanitize_transport(replayed, 2, 40_000)
        .expect("local cache age crosses the deadline conservatively");
    assert_eq!(projected.observations.len(), 1);
    assert_eq!(
        projected.observations[0].phase,
        SnapshotTransportPhase::Stalled,
        "a crossed cached deadline remains visible only in its fixed post-deadline window"
    );
    assert_eq!(
        projected.observations[0].active_deadline_remaining_ms,
        Some(0)
    );

    let mut six_minutes_behind = test_transport_status(2, 0, Some(1_000));
    six_minutes_behind.observed_at_unix_ms = source_observed_ms - 6 * 60 * 1_000;
    let cache = PeerStatusCache::default();
    cache
        .store(&BTreeMap::from([(
            "node-2".to_owned(),
            PeerStatusOutcome {
                node_id: "node-2".to_owned(),
                state: ObservationState::Unreachable,
                status: None,
                transport: Some(six_minutes_behind),
            },
        )]))
        .await;
    let behind = cached_private_transport(cache.fresh().await, "node-2", 2, source_observed_ms);
    assert_eq!(behind.observed_at_unix_ms, source_observed_ms - 360_000);
    assert_eq!(behind.observations[0].sample_age_ms, 0);

    let mut five_seconds_ahead = test_transport_status(2, 0, Some(1_000));
    five_seconds_ahead.observed_at_unix_ms = source_observed_ms + 5_001;
    cache
        .store(&BTreeMap::from([(
            "node-2".to_owned(),
            PeerStatusOutcome {
                node_id: "node-2".to_owned(),
                state: ObservationState::Unreachable,
                status: None,
                transport: Some(five_seconds_ahead),
            },
        )]))
        .await;
    let ahead = cached_private_transport(cache.fresh().await, "node-2", 2, source_observed_ms);
    assert_eq!(ahead.observed_at_unix_ms, source_observed_ms + 5_001);
    assert_eq!(ahead.observations[0].sample_age_ms, 0);
}

#[test]
fn peer_status_refresh_keeps_strict_cycle_overhead_margin() {
    assert_eq!(
        PEER_STATUS_CACHE_TTL
            - PEER_STATUS_REFRESH_INTERVAL
            - PEER_DIRECTORY_TIMEOUT
            - PEER_STATUS_TIMEOUT,
        Duration::from_millis(500)
    );
    assert!(PEER_STATUS_REFRESH_INTERVAL < PEER_STATUS_CACHE_TTL);
}

#[tokio::test]
async fn aggregate_request_reads_only_node_owned_projections() {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let (_unused_app, mut state) = crate::http::tests::test_app_with_state();
    state.node_id = "node-1".to_owned();
    let admin = state
        .store
        .create_user("cluster-admin", "unused-test-hash", true)
        .await
        .expect("create admin");
    let token = "cache-only-cluster-status-token";
    let token_digest = plurx_core::auth::hash_token(token);
    state
        .store
        .create_token(&token_digest, admin.id, Some("cluster-status-test"))
        .await
        .expect("create admin token");
    let app = crate::http::router(state.clone());

    // One ordinary Store-backed request publishes the bounded admin
    // proof. The two recovery routes below must use only that digest.
    let authenticated = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/me")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("authenticated request"),
        )
        .await
        .expect("authenticated response");
    assert_eq!(authenticated.status(), StatusCode::OK);
    assert!(state
        .store
        .delete_token(&token_digest)
        .await
        .expect("make Store proof unavailable"));
    assert!(state
        .store
        .user_for_token(&token_digest)
        .await
        .expect("read missing Store proof")
        .is_none());

    // The fixture is deliberately not a replicated cluster. A production
    // request that reaches MembershipManager::status cannot succeed, and
    // its current peer directory is empty. Seed only the two node-owned
    // projections that GET and support are allowed to consume.
    assert!(state.membership.status().await.is_err());
    assert!(state
        .membership
        .operations_peers()
        .await
        .expect("unclustered peer directory")
        .is_empty());
    state
        .membership_status_cache
        .store(test_membership("node-1", 2))
        .await;
    state
        .peer_status_cache
        .store(&BTreeMap::from([(
            "node-2".to_owned(),
            PeerStatusOutcome {
                node_id: "node-2".to_owned(),
                state: ObservationState::Answered,
                status: Some(test_local_status("node-2", Some(2))),
                transport: None,
            },
        )]))
        .await;

    let status_request = Request::builder()
        .uri("/api/v1/cluster/status")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .expect("cluster status request");
    let response = tokio::time::timeout(
        Duration::from_millis(250),
        app.clone().oneshot(status_request),
    )
    .await
    .expect("cache-only status request must not wait for Store")
    .expect("cluster status response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL),
        Some(&HeaderValue::from_static("private, no-store"))
    );
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("cluster status body")
        .to_bytes();
    let aggregate: ClusterOperationsAggregate =
        serde_json::from_slice(&bytes).expect("cluster status JSON");
    assert_eq!(aggregate.nodes.len(), 2);
    assert_eq!(aggregate.nodes[1].observation, ObservationState::Answered);

    let support_request = Request::builder()
        .uri("/api/v1/cluster/support-bundle")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .expect("support bundle request");
    let response = tokio::time::timeout(
        Duration::from_millis(250),
        app.clone().oneshot(support_request),
    )
    .await
    .expect("cache-only support request must not wait for Store")
    .expect("support bundle response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&HeaderValue::from_static("application/zip"))
    );

    // Logout is a mutating route and therefore revalidates the bearer
    // against Store before it may fan out a cluster-wide proof fence.
    // Restore the same token only after the two cache-only reads have
    // proved that their request paths do not touch Store.
    state
        .store
        .create_token(&token_digest, admin.id, Some("cluster-status-test"))
        .await
        .expect("restore token for authenticated logout");
    let logout = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/logout")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("logout request"),
        )
        .await
        .expect("logout response");
    assert_eq!(logout.status(), StatusCode::OK);
    let revoked = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/cluster/status")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("revoked cluster status request"),
        )
        .await
        .expect("revoked cluster status response");
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);

    // Supplemental capability guard: both real handlers above execute the
    // production collector. Keep its narrow wiring explicit so Store and
    // peer transports cannot be slipped in beside the projection reads.
    let source = include_str!("../cluster_operations.rs");
    let collector = source
        .split("pub(crate) async fn collect_aggregate(")
        .nth(1)
        .and_then(|tail| tail.split("/// Safety-changing operations").next())
        .expect("read-only collector source");
    assert!(collector.contains("state.membership_status_cache.fresh()"));
    assert!(collector.contains("state.peer_status_cache.fresh()"));
    assert!(!collector.contains("state.store"));
    assert!(!collector.contains("state.membership.status()"));
    assert!(!collector.contains("operations_peers"));
    assert!(!collector.contains("collect_peer_statuses"));

    let aggregate_handler = source
        .split("pub(crate) async fn aggregate(")
        .nth(1)
        .and_then(|tail| tail.split("/// Complete GET response boundary").next())
        .expect("aggregate handler source");
    assert!(aggregate_handler.contains("_admin: CacheOnlyAdminUser"));
    assert!(!aggregate_handler.contains("_admin: AdminUser"));
    assert!(
        aggregate_handler.contains("aggregate_response_with(|| collect_aggregate(&state)).await")
    );
    assert!(!aggregate_handler.contains("state.membership"));
    assert!(!aggregate_handler.contains("collect_peer_statuses"));

    let support_handler = source
        .split("pub(crate) async fn support_bundle(")
        .nth(1)
        .and_then(|tail| tail.split("/// Complete support response boundary").next())
        .expect("support handler source");
    assert!(support_handler.contains("_admin: CacheOnlyAdminUser"));
    assert!(!support_handler.contains("_admin: AdminUser"));
    assert!(support_handler.contains("|| collect_aggregate(&state)"));
    assert!(support_handler.contains("state.cluster_logs.tail(\"trace\", 200)"));
    assert!(!support_handler.contains("state.membership"));
    assert!(!support_handler.contains("collect_peer_statuses"));

    let extractor_source = include_str!("../extract.rs");
    // Scoped to the impl block itself, not "everything until the tests":
    // a helper added between the two would silently join the slice and
    // make the counting assertion below fail for the wrong reason.
    let cache_extractor = extractor_source
        .split("impl FromRequestParts<AppState> for CacheOnlyAdminUser")
        .nth(1)
        .and_then(|tail| tail.split("\n}\n").next())
        .expect("cache-only admin extractor source");
    // The Store may be a fallback and may never be a prerequisite. The
    // cached answer has to be reached, and returned, before the source
    // mentions the Store at all — otherwise a wedged Store would delay the
    // one read whose purpose is to describe the wedge.
    let cache_answer = cache_extractor
        .find("cache_only_admin_proofs.authenticate(&digest)")
        .expect("cache-only answer");
    let early_return = cache_extractor
        .find("return Ok(Self);")
        .expect("cache-only early return");
    let store_read = cache_extractor
        .find("state.store.user_for_token(&digest)")
        .expect("bounded Store fallback");
    assert!(cache_answer < early_return);
    assert!(early_return < store_read);
    // The publication ticket is captured before the Store read and not
    // after it. Taking it afterwards compiles, passes every test in the
    // suite, and reintroduces the stale-proof race the cache generation
    // exists to close: a proof derived from a pre-revocation Store read
    // would publish once the revocation's generation bump had settled.
    let ticket = cache_extractor
        .find("authentication_ticket()")
        .expect("publication ticket");
    assert!(ticket < store_read);
    // And the fallback is bounded, so a Store that never answers becomes a
    // named refusal rather than a hung request.
    assert!(cache_extractor.contains("CACHE_ONLY_ADMIN_STORE_FALLBACK_TIMEOUT,"));
    assert!(cache_extractor.contains("tokio::time::timeout("));
    // A caller the Store recognises is never told its credential is bad:
    // the only Unauthorized answers here are a request with no token at
    // all and a token the Store has no row for. A known non-admin gets
    // Forbidden, and an unanswerable Store gets the named 503.
    assert_eq!(cache_extractor.matches("ApiError::Unauthorized").count(), 2);
    assert!(cache_extractor.contains("Err(ApiError::Forbidden)"));
    assert!(cache_extractor.contains("cache_only_admin_unavailable()"));
}

#[tokio::test]
async fn mutation_preflight_refreshes_roster_directory_and_bounded_peer_observations() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let membership_calls = std::sync::Arc::new(AtomicUsize::new(0));
    let directory_calls = std::sync::Arc::new(AtomicUsize::new(0));
    let public_calls = std::sync::Arc::new(AtomicUsize::new(0));
    let private_calls = std::sync::Arc::new(AtomicUsize::new(0));
    let peers = (2..=13)
        .map(|index| ActivityPeer {
            node_id: format!("node-{index}"),
            raft_id: index,
            http_base: Some(format!("http://node-{index}:32400")),
            reachable: true,
        })
        .collect::<Vec<_>>();
    let deadline = tokio::time::Instant::now() + AGGREGATE_TIMEOUT;

    let aggregate = collect_current_aggregate_with_sources(
        "node-1",
        deadline,
        {
            let membership_calls = membership_calls.clone();
            move || async move {
                membership_calls.fetch_add(1, Ordering::Relaxed);
                Ok(test_membership("node-1", 13))
            }
        },
        {
            let directory_calls = directory_calls.clone();
            move || {
                let peers = peers.clone();
                async move {
                    directory_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(peers)
                }
            }
        },
        || async { test_local_status("node-1", Some(1)) },
        {
            let public_calls = public_calls.clone();
            let private_calls = private_calls.clone();
            move |peers, received_deadline| async move {
                assert_eq!(received_deadline, deadline);
                collect_peer_statuses_with_sources(
                    peers,
                    received_deadline,
                    move |peer, _peer_deadline| {
                        public_calls.fetch_add(1, Ordering::Relaxed);
                        async move {
                            PeerStatusOutcome {
                                node_id: peer.node_id,
                                state: ObservationState::Answered,
                                status: None,
                                transport: None,
                            }
                        }
                    },
                    move |_raft_id| {
                        private_calls.fetch_add(1, Ordering::Relaxed);
                        async { Ok(None) }
                    },
                )
                .await
            }
        },
    )
    .await
    .expect("fresh bounded mutation preflight");

    assert_eq!(membership_calls.load(Ordering::Relaxed), 1);
    assert_eq!(directory_calls.load(Ordering::Relaxed), 1);
    assert_eq!(public_calls.load(Ordering::Relaxed), MAX_OPERATIONS_PEERS);
    assert_eq!(private_calls.load(Ordering::Relaxed), MAX_OPERATIONS_PEERS);
    assert_eq!(aggregate.nodes.len(), 13);
    assert_eq!(
        aggregate
            .nodes
            .iter()
            .filter(|node| node.observation == ObservationState::PeerLimit)
            .count(),
        4
    );

    let source = include_str!("../cluster_operations.rs");
    let acquisition = source
        .split_once("pub(crate) async fn acquire_planned_outage_preflight(")
        .expect("planned-outage acquisition")
        .1
        .split_once("#[cfg(test)]\nasync fn acquire_then_collect_preflight")
        .expect("planned-outage acquisition end")
        .0;
    let prepared_claim = acquisition
        .find("prepare_restart_preparation_claim")
        .expect("prepare exact claim before commit");
    let guard = acquisition
        .find("PlannedOutageLeaseGuard::new")
        .expect("guard exact prepared claim");
    let detached_owner = acquisition
        .find("run_planned_outage_task(async move")
        .expect("detach replicated acquisition owner");
    let replicated_commit = acquisition
        .find("commit_cluster_operation_lease(lease.claim())")
        .expect("commit the guard-owned exact claim");
    assert!(prepared_claim < guard && guard < detached_owner && detached_owner < replicated_commit);
    let exact_release = source
        .split_once("pub(crate) async fn release_replicated_claim")
        .expect("exact planned-outage release")
        .1
        .split_once("pub(crate) async fn release(mut self)")
        .expect("exact planned-outage release end")
        .0;
    let local_latch = exact_release
        .find("retain_local_fence()")
        .expect("synchronous unresolved-release latch");
    let replicated_release = exact_release
        .find("release_exact_claim_with_retry(&self.membership, lease, &self.shutdown)")
        .expect("bounded replicated exact release owner");
    assert!(local_latch < replicated_release);
    let release_retry = source
        .split_once("async fn release_exact_claim_with_retry(")
        .expect("exact-release retry owner")
        .1
        .split_once("async fn release_current_restart_with_retry(")
        .expect("exact-release retry owner end")
        .0;
    assert!(release_retry.contains("release_cluster_operation_lease(lease)"));
    assert!(release_retry.contains("wait_for_planned_outage_cleanup_retry"));
    let current_collector = source
        .split_once("pub(crate) async fn collect_current_aggregate(")
        .expect("current preflight collector")
        .1
        .split_once("async fn collect_current_aggregate_with_sources")
        .expect("current preflight collector end")
        .0;
    assert!(current_collector.contains("membership_for_status"));
    assert!(current_collector.contains(".status()"));
    assert!(current_collector.contains("membership_for_directory"));
    assert!(current_collector.contains(".operations_peers()"));
    assert!(current_collector.contains("PeerTransport::new"));
    assert!(current_collector.contains("collect_peer_statuses("));
    let restart = source
        .split_once("pub(crate) async fn prepare_restart(")
        .expect("restart handler")
        .1
        .split_once("fn restart_preparation_response(")
        .expect("restart handler end")
        .0;
    assert!(restart.contains("acquire_planned_outage_preflight("));
    assert!(restart.contains("PlannedOutageOperation::Restart"));
    assert!(restart.contains("lease.arm_local_fence(local_fence)"));
    assert!(restart.contains("run_serialized_planned_outage_task"));
    assert!(restart.contains("lease.release().await"));
    assert!(restart.contains("lease.disarm()"));
    let maintenance_source = include_str!("../cluster.rs");
    let maintenance = maintenance_source
        .split_once("pub async fn enter_maintenance(")
        .expect("maintenance handler")
        .1
        .split_once("pub async fn exit_maintenance(")
        .expect("maintenance handler end")
        .0;
    assert!(maintenance.contains("acquire_planned_outage_preflight("));
    assert!(maintenance.contains("PlannedOutageOperation::Maintenance"));
    assert!(maintenance.contains("lease.arm_local_fence(local_fence)"));
    assert!(maintenance.contains("run_serialized_planned_outage_task"));
    assert!(maintenance.contains("reconcile_local_maintenance_commit()"));
    assert!(maintenance.contains("lease.release().await"));
    assert!(maintenance.contains("lease.disarm()"));
}

#[tokio::test(start_paused = true)]
async fn mutation_preflight_ages_local_transport_across_peer_collection() {
    let deadline = tokio::time::Instant::now() + AGGREGATE_TIMEOUT;
    let aggregate = collect_current_aggregate_with_sources(
        "node-1",
        deadline,
        || async { Ok(test_membership("node-1", 1)) },
        || async { Ok(Vec::new()) },
        || async {
            let mut status = test_local_status("node-1", Some(1));
            status.transport = Some(test_transport_status(1, 0, Some(500)));
            status
        },
        |_peers, received_deadline| async move {
            assert_eq!(received_deadline, deadline);
            tokio::time::advance(Duration::from_secs(1)).await;
            BTreeMap::new()
        },
    )
    .await
    .expect("fresh mutation preflight");

    let observation = aggregate.nodes[0]
        .transport
        .as_ref()
        .and_then(|transport| transport.observations.first())
        .expect("local transport observation");
    assert_eq!(observation.phase, SnapshotTransportPhase::Stalled);
    assert_eq!(observation.active_deadline_remaining_ms, Some(0));
    assert_eq!(
        observation.sample_age_ms, 500,
        "stalled retention starts when the 500 ms deadline is crossed, not when collection starts"
    );
    assert_eq!(
        observation.last_error_category.as_deref(),
        Some("snapshot_stalled")
    );
}

async fn wait_for_local_planned_outage_cleanup(serving: &crate::serving_fence::ServingFence) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if !serving.restart_drain_status(0).await.new_admissions_blocked {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("planned-outage Drop cleanup must clear the local fence");
}

#[tokio::test]
async fn cancelled_acquisition_waiter_releases_only_after_ambiguous_outcome_is_known() {
    use plurx_core::cluster::migration::status::ReplicationMonitor;

    let serving =
        crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
    let local_fence = serving
        .begin_restart_preparation_until(unix_ms().saturating_add(60_000))
        .await
        .expect("restart fence token");
    let acquisition_started = std::sync::Arc::new(tokio::sync::Notify::new());
    let publish_outcome = std::sync::Arc::new(tokio::sync::Notify::new());
    let waiter = tokio::spawn(run_planned_outage_task({
        let serving = serving.clone();
        let acquisition_started = acquisition_started.clone();
        let publish_outcome = publish_outcome.clone();
        async move {
            acquisition_started.notify_one();
            publish_outcome.notified().await;
            Ok(PlannedOutageLeaseGuard::local_fence_only_for_test(
                serving,
                local_fence,
            ))
        }
    }));
    acquisition_started.notified().await;

    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    assert!(
        serving.restart_drain_status(0).await.new_admissions_blocked,
        "cancellation must not guess the outcome or clean up before acquisition returns"
    );

    publish_outcome.notify_one();
    wait_for_local_planned_outage_cleanup(&serving).await;
}

#[tokio::test]
async fn abort_during_drain_cancels_the_guard_owned_local_fence() {
    use plurx_core::cluster::migration::status::ReplicationMonitor;

    let serving =
        crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
    let local_fence = serving
        .begin_restart_preparation_until(unix_ms().saturating_add(60_000))
        .await
        .expect("restart fence token");
    let drain_started = std::sync::Arc::new(tokio::sync::Notify::new());
    let drain = tokio::spawn({
        let serving = serving.clone();
        let drain_started = drain_started.clone();
        async move {
            let _guard = PlannedOutageLeaseGuard::local_fence_only_for_test(serving, local_fence);
            drain_started.notify_one();
            std::future::pending::<()>().await;
        }
    });
    drain_started.notified().await;

    drain.abort();
    assert!(drain.await.expect_err("drain is cancelled").is_cancelled());
    wait_for_local_planned_outage_cleanup(&serving).await;
}

#[tokio::test]
async fn cancelled_maintenance_waiter_does_not_cancel_the_owned_commit() {
    let commit_started = std::sync::Arc::new(tokio::sync::Notify::new());
    let finish_commit = std::sync::Arc::new(tokio::sync::Notify::new());
    let committed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let waiter = tokio::spawn(run_planned_outage_task({
        let commit_started = commit_started.clone();
        let finish_commit = finish_commit.clone();
        let committed = committed.clone();
        async move {
            commit_started.notify_one();
            finish_commit.notified().await;
            committed.store(true, std::sync::atomic::Ordering::Release);
            Ok::<_, ApiError>(())
        }
    }));
    commit_started.notified().await;

    waiter.abort();
    assert!(waiter
        .await
        .expect_err("waiter is cancelled")
        .is_cancelled());
    assert!(!committed.load(std::sync::atomic::Ordering::Acquire));
    finish_commit.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !committed.load(std::sync::atomic::Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached maintenance owner must finish after HTTP cancellation");
}

#[tokio::test]
async fn cancelled_waiter_keeps_the_serialized_operation_gate_with_its_owner() {
    use plurx_core::cluster::migration::status::ReplicationMonitor;

    let serving =
        crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
    let first_started = std::sync::Arc::new(tokio::sync::Notify::new());
    let finish_first = std::sync::Arc::new(tokio::sync::Notify::new());
    let first_finished = std::sync::Arc::new(tokio::sync::Notify::new());
    let second_started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let first_waiter = tokio::spawn({
        let serving = serving.clone();
        let first_started = first_started.clone();
        let finish_first = finish_first.clone();
        let first_finished = first_finished.clone();
        async move {
            run_serialized_planned_outage_task(&serving, async move {
                first_started.notify_one();
                finish_first.notified().await;
                first_finished.notify_one();
                Ok::<_, ApiError>(())
            })
            .await
        }
    });
    first_started.notified().await;
    first_waiter.abort();
    assert!(first_waiter
        .await
        .expect_err("HTTP waiter is cancelled")
        .is_cancelled());

    for _ in 0..1_024 {
        let second_started = second_started.clone();
        let error = run_serialized_planned_outage_task(&serving, async move {
            second_started.store(true, std::sync::atomic::Ordering::Release);
            Ok::<_, ApiError>(())
        })
        .await
        .expect_err("a contending request must fail fast");
        assert!(matches!(
            error,
            ApiError::Typed {
                code: "planned_outage_operation_pending",
                ..
            }
        ));
    }
    assert!(
        !second_started.load(std::sync::atomic::Ordering::Acquire),
        "a later cancel/prepare must neither queue nor cross the detached first owner"
    );

    finish_first.notify_one();
    first_finished.notified().await;
    let admitted_second = second_started.clone();
    run_serialized_planned_outage_task(&serving, async move {
        admitted_second.store(true, std::sync::atomic::Ordering::Release);
        Ok::<_, ApiError>(())
    })
    .await
    .expect("second serialized operation after owner completion");
    assert!(second_started.load(std::sync::atomic::Ordering::Acquire));
}

#[tokio::test]
async fn cleanup_retry_backoff_is_capped_and_shutdown_interruptible() {
    let mut delay = PLANNED_OUTAGE_CLEANUP_RETRY_INITIAL;
    for _ in 0..16 {
        delay = next_planned_outage_cleanup_delay(delay);
    }
    assert_eq!(delay, PLANNED_OUTAGE_CLEANUP_RETRY_MAX);

    let shutdown = tokio_util::sync::CancellationToken::new();
    shutdown.cancel();
    assert!(
        wait_for_planned_outage_cleanup_retry(&shutdown, Duration::from_secs(60))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn planned_outage_lease_blocks_membership_mutation_during_fresh_preflight() {
    let lifecycle_fence = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    let mutation_crossed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let (lease, ()) = acquire_then_collect_preflight(
        {
            let lifecycle_fence = lifecycle_fence.clone();
            move || async move { Ok::<_, ApiError>(lifecycle_fence.lock_owned().await) }
        },
        {
            let lifecycle_fence = lifecycle_fence.clone();
            let mutation_crossed = mutation_crossed.clone();
            move || async move {
                let mutation =
                    tokio::spawn(async move { lifecycle_fence.try_lock_owned().is_ok() });
                mutation_crossed.store(
                    mutation.await.expect("membership mutation attempt"),
                    std::sync::atomic::Ordering::Relaxed,
                );
                Ok::<_, ApiError>(())
            }
        },
        drop,
    )
    .await
    .expect("lease-protected fresh preflight");

    assert!(
        !mutation_crossed.load(std::sync::atomic::Ordering::Relaxed),
        "a membership lifecycle mutation must not cross the preflight"
    );
    drop(lease);
    assert!(lifecycle_fence.try_lock_owned().is_ok());
}

#[tokio::test]
async fn failed_fresh_preflight_releases_its_planned_outage_lease() {
    let lifecycle_fence = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let result = acquire_then_collect_preflight(
        {
            let lifecycle_fence = lifecycle_fence.clone();
            move || async move { Ok::<_, ApiError>(lifecycle_fence.lock_owned().await) }
        },
        || async {
            Err::<(), _>(ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "test_preflight_failed",
                "test preflight failed",
            ))
        },
        {
            let released = released.clone();
            move |lease| {
                drop(lease);
                released.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        },
    )
    .await;

    assert!(result.is_err());
    assert!(released.load(std::sync::atomic::Ordering::Relaxed));
    assert!(lifecycle_fence.try_lock_owned().is_ok());
}

#[tokio::test]
async fn aborted_fresh_preflight_releases_the_exact_planned_outage_claim() {
    let collect_started = std::sync::Arc::new(tokio::sync::Notify::new());
    let released = std::sync::Arc::new(std::sync::Mutex::new(None));
    let task = tokio::spawn(acquire_then_collect_preflight(
        || async { Ok::<_, ApiError>("exact-claim-a".to_owned()) },
        {
            let collect_started = collect_started.clone();
            move || async move {
                collect_started.notify_one();
                std::future::pending::<Result<(), ApiError>>().await
            }
        },
        {
            let released = released.clone();
            move |claim| {
                *released.lock().expect("release observation") = Some(claim);
            }
        },
    ));

    collect_started.notified().await;
    task.abort();
    let _ = task.await;
    assert_eq!(
        released.lock().expect("release observation").as_deref(),
        Some("exact-claim-a")
    );
}

#[tokio::test(start_paused = true)]
async fn mutation_preflight_bounds_the_current_membership_read() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let directory_calls = std::sync::Arc::new(AtomicUsize::new(0));
    let probe_calls = std::sync::Arc::new(AtomicUsize::new(0));
    let started = tokio::time::Instant::now();
    let result = collect_current_aggregate_with_sources(
        "node-1",
        started + AGGREGATE_TIMEOUT,
        || async { std::future::pending::<Result<MembershipStatus, String>>().await },
        {
            let directory_calls = directory_calls.clone();
            move || async move {
                directory_calls.fetch_add(1, Ordering::Relaxed);
                Ok(Vec::new())
            }
        },
        || async { test_local_status("node-1", Some(1)) },
        {
            let probe_calls = probe_calls.clone();
            move |_peers, _deadline| async move {
                probe_calls.fetch_add(1, Ordering::Relaxed);
                BTreeMap::new()
            }
        },
    )
    .await;

    match result.expect_err("hung membership read must fail closed") {
        ApiError::Typed { status, code, .. } => {
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(code, "cluster_roster_timed_out");
        }
        error => panic!("unexpected membership deadline error: {error:?}"),
    }
    assert_eq!(
        tokio::time::Instant::now() - started,
        PEER_DIRECTORY_TIMEOUT
    );
    assert_eq!(directory_calls.load(Ordering::Relaxed), 0);
    assert_eq!(probe_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn support_text_redacts_credentials_paths_and_secret_shapes() {
    for secret in [
        "Authorization: Bearer household-token",
        "join token plxjoin:v1:secret",
        "cluster_secret=abc",
        "password=hunter2",
        "signature=012345",
        "api_key=tmdb-secret",
        "username=alice",
        "opened /srv/media/private/movie.mkv",
        r"opened C:\media\private\movie.mkv",
    ] {
        let redacted = redact_operator_text(secret);
        assert_eq!(redacted, "[redacted potentially sensitive operator text]");
        assert!(!redacted.contains(secret));
    }
}

#[test]
fn supervisor_commands_require_an_active_and_fully_drained_preparation() {
    let idle = crate::serving_fence::RestartDrainStatus {
        new_admissions_blocked: false,
        admissions_in_flight: 0,
        expires_at_unix_ms: None,
        drained: true,
    };
    assert!(restart_preparation_response("node-a", 0, idle)
        .restart_commands
        .is_empty());

    let draining = crate::serving_fence::RestartDrainStatus {
        new_admissions_blocked: true,
        admissions_in_flight: 1,
        expires_at_unix_ms: Some(unix_ms() + 60_000),
        drained: false,
    };
    assert!(restart_preparation_response("node-a", 0, draining)
        .restart_commands
        .is_empty());

    let drained = crate::serving_fence::RestartDrainStatus {
        admissions_in_flight: 0,
        drained: true,
        ..draining
    };
    assert!(!restart_preparation_response("node-a", 0, drained)
        .restart_commands
        .is_empty());
}

#[test]
fn restart_preparation_claims_and_releases_the_replicated_outage_slot() {
    let source = include_str!("../cluster_operations.rs")
        .split_once("pub(crate) async fn prepare_restart(")
        .expect("restart preparation")
        .1
        .split_once("fn restart_preparation_response(")
        .expect("restart preparation handlers end")
        .0;
    let claim = source
        .find("acquire_planned_outage_preflight")
        .expect("replicated lease acquisition");
    let bound = source
        .find("preparation_expiry_unix_ms")
        .expect("committed lease bounds local fence");
    let local_fence = source
        .find("begin_restart_preparation")
        .expect("local admission fence");
    let commands = source
        .find("restart_preparation_response")
        .expect("reboot-ready response");
    assert!(claim < bound && bound < local_fence && local_fence < commands);
    assert!(source.contains("lease.release().await"));
    assert!(source.contains("lease.disarm()"));
    assert!(source.contains("retain_restart_cancellation"));
    assert!(source.contains("run_serialized_planned_outage_task"));
    assert!(source.contains("release_current_restart_with_retry"));
}

#[test]
fn identity_mismatch_discards_peer_local_state() {
    let member = ClusterNodeRecord {
        node_id: "node-a".to_owned(),
        hostname: "a".to_owned(),
        advertised_host: "a".to_owned(),
        raft_id: 7,
        role: NodeRole::Voter,
        is_voter: true,
        is_leader: false,
        reachable: true,
        last_seen_at: 1,
        removal_pending: false,
        learner_protocol_ready: true,
        bounded_read_ready: true,
        apply_lag_entries: Some(0),
        storage_headroom_bytes: Some(1),
        voter_storage_ready: true,
        maintenance: false,
        maintenance_requested_at: None,
        maintenance_acknowledged: false,
        maintenance_ready: false,
        active_media_sessions: 0,
    };
    let membership = MembershipStatus {
        local_node_id: "local".to_owned(),
        availability: plurx_core::cluster::membership::ClusterAvailability::SingleNode,
        nodes: vec![member],
        replication: test_replication_status(),
        capacity: plurx_core::cluster::membership::ClusterCapacityStatus {
            voting_nodes: 1,
            voting_quorum: 1,
            voting_failure_tolerance: 0,
            non_voting_replicas: 0,
            ready_read_workers: 0,
        },
        protocol: plurx_core::cluster::membership::ClusterProtocolStatus {
            active_min: 1,
            active_max: 1,
            binary_min: 1,
            binary_max: 1,
            learner_protocol_active: false,
            learner_protocol_pending: Vec::new(),
        },
        recovery: ClusterRecoveryStatus {
            required: false,
            quorum_available: true,
            reachable_voters: 1,
            required_voters: 1,
            leader_elected: true,
            permanent_majority_loss_supported: false,
        },
    };
    let mut status = test_local_status("node-a", Some(8));
    status.transport = Some(test_transport_status(7, 0, Some(1_000)));
    let mut remote = BTreeMap::new();
    remote.insert(
        "node-a".to_owned(),
        PeerStatusOutcome {
            node_id: "node-a".to_owned(),
            state: ObservationState::Answered,
            status: Some(status),
            transport: None,
        },
    );
    let rows = join_observations(
        &membership,
        "local",
        test_local_status("local", Some(1)),
        remote,
        unix_ms(),
    );
    assert_eq!(rows[0].observation, ObservationState::IdentityMismatch);
    assert!(rows[0].status.is_none());
    assert!(rows[0].transport.is_none());
}

#[test]
fn public_transport_fallback_requires_matching_observer_identity() {
    let membership = test_membership("node-1", 2);
    let mut peer = test_local_status("node-2", Some(2));
    peer.transport = Some(test_transport_status(7, 0, Some(1_000)));
    let mut remote = BTreeMap::new();
    remote.insert(
        "node-2".to_owned(),
        PeerStatusOutcome {
            node_id: "node-2".to_owned(),
            state: ObservationState::Answered,
            status: Some(peer),
            transport: None,
        },
    );

    let rows = join_observations(
        &membership,
        "node-1",
        test_local_status("node-1", Some(1)),
        remote,
        unix_ms(),
    );
    assert_eq!(rows[1].observation, ObservationState::Answered);
    assert!(rows[1].status.is_some());
    assert!(rows[1]
        .status
        .as_ref()
        .is_some_and(|status| status.transport.is_none()));
    assert!(rows[1].transport.is_none());

    let mut peer = test_local_status("node-2", Some(2));
    peer.transport = Some(test_transport_status(2, 0, Some(1_000)));
    let mut remote = BTreeMap::new();
    remote.insert(
        "node-2".to_owned(),
        PeerStatusOutcome {
            node_id: "node-2".to_owned(),
            state: ObservationState::Answered,
            status: Some(peer),
            transport: None,
        },
    );
    let rows = join_observations(
        &membership,
        "node-1",
        test_local_status("node-1", Some(1)),
        remote,
        unix_ms(),
    );
    assert_eq!(
        rows[1]
            .transport
            .as_ref()
            .map(|transport| transport.observing_node_id),
        Some(2)
    );

    let mut peer = test_local_status("node-2", Some(2));
    let mut transport = test_transport_status(2, 0, Some(1_000));
    transport.observations[0].observing_node_id = 7;
    peer.transport = Some(transport);
    let mut remote = BTreeMap::new();
    remote.insert(
        "node-2".to_owned(),
        PeerStatusOutcome {
            node_id: "node-2".to_owned(),
            state: ObservationState::Answered,
            status: Some(peer),
            transport: None,
        },
    );
    let rows = join_observations(
        &membership,
        "node-1",
        test_local_status("node-1", Some(1)),
        remote,
        unix_ms(),
    );
    assert!(rows[1]
        .transport
        .as_ref()
        .is_some_and(|transport| transport.observations.is_empty()));
    assert!(rows[1]
        .status
        .as_ref()
        .is_some_and(|status| status.transport.is_none()));
}

#[test]
fn remote_status_freshness_uses_local_monotonic_age_despite_clock_skew() {
    let membership = test_membership("node-1", 2);
    let aggregate_observed_ms = unix_ms();

    for observed_at_unix_ms in [
        aggregate_observed_ms.saturating_sub(60_000),
        aggregate_observed_ms.saturating_add(60_000),
    ] {
        let mut peer = test_local_status("node-2", Some(2));
        peer.observed_at_unix_ms = observed_at_unix_ms;
        let remote = BTreeMap::from([(
            "node-2".to_owned(),
            PeerStatusOutcome {
                node_id: "node-2".to_owned(),
                state: ObservationState::Answered,
                status: Some(peer),
                transport: None,
            },
        )]);

        let rows = join_observations(
            &membership,
            "node-1",
            test_local_status("node-1", Some(1)),
            PeerStatusCacheRead::Fresh(remote, Duration::from_millis(250)),
            aggregate_observed_ms,
        );

        assert_eq!(rows[1].observation, ObservationState::Answered);
        assert_eq!(rows[1].sample_age_ms, Some(250));
        assert_eq!(
            rows[1]
                .status
                .as_ref()
                .map(|status| status.observed_at_unix_ms),
            Some(observed_at_unix_ms),
            "the peer timestamp remains diagnostic metadata"
        );
    }
}

#[test]
fn stale_peer_sample_uses_local_elapsed_time_and_cannot_become_healthy() {
    let membership = test_membership("node-1", 2);
    let mut peer = test_local_status("node-2", Some(2));
    peer.observed_at_unix_ms = unix_ms().saturating_add(60_000);
    let mut remote = BTreeMap::new();
    remote.insert(
        "node-2".to_owned(),
        PeerStatusOutcome {
            node_id: "node-2".to_owned(),
            state: ObservationState::Answered,
            status: Some(peer),
            transport: None,
        },
    );

    let rows = join_observations(
        &membership,
        "node-1",
        test_local_status("node-1", Some(1)),
        PeerStatusCacheRead::Fresh(
            remote,
            Duration::from_millis(MAX_FRESH_AGE_MS.saturating_add(1)),
        ),
        unix_ms(),
    );

    assert_eq!(rows[1].observation, ObservationState::InvalidResponse);
    assert_eq!(rows[1].error_class.as_deref(), Some("stale_peer_sample"));
    assert!(!rollout_verdict(&membership, &rows).safe_to_restart_one);
}

#[tokio::test]
async fn production_collector_preserves_private_transport_when_public_listener_is_closed() {
    let membership = test_membership("node-1", 2);
    let peers = vec![ActivityPeer {
        node_id: "node-2".to_owned(),
        raft_id: 2,
        http_base: Some("http://127.0.0.1:32400".to_owned()),
        reachable: true,
    }];
    let private_transport = test_transport_status(2, 0, Some(1_000));
    let remote = collect_peer_statuses_with_sources(
        peers,
        tokio::time::Instant::now() + Duration::from_secs(2),
        |peer, _deadline| async move {
            assert_eq!(peer.node_id, "node-2");
            PeerStatusOutcome {
                node_id: peer.node_id,
                state: ObservationState::Unreachable,
                status: None,
                transport: None,
            }
        },
        move |raft_id| {
            let private_transport = private_transport.clone();
            async move {
                assert_eq!(raft_id, 2);
                Ok(Some(private_transport))
            }
        },
    )
    .await;
    let rows = join_observations(
        &membership,
        "node-1",
        test_local_status("node-1", Some(1)),
        remote,
        unix_ms(),
    );
    assert_eq!(rows[1].observation, ObservationState::Unreachable);
    assert_eq!(
        rows[1]
            .transport
            .as_ref()
            .map(|status| status.observing_node_id),
        Some(2)
    );
    assert_eq!(
        rows[1]
            .transport
            .as_ref()
            .and_then(|status| status.observations.first())
            .map(|observation| observation.locally_received_bytes),
        Some(Some(64))
    );
}

#[tokio::test(start_paused = true)]
async fn peer_transport_age_retains_request_to_receipt_uncertainty() {
    let peers = vec![
        ActivityPeer {
            node_id: "node-2".to_owned(),
            raft_id: 2,
            http_base: None,
            reachable: true,
        },
        ActivityPeer {
            node_id: "node-3".to_owned(),
            raft_id: 3,
            http_base: None,
            reachable: true,
        },
    ];
    let mut remote = collect_peer_statuses_with_sources(
        peers,
        tokio::time::Instant::now() + Duration::from_secs(2),
        |peer, _deadline| async move {
            PeerStatusOutcome {
                node_id: peer.node_id,
                state: ObservationState::Unreachable,
                status: None,
                transport: None,
            }
        },
        |raft_id| async move {
            let mut transport = test_transport_status(raft_id, 0, Some(20_000));
            transport.observations[0].attempt_age_ms = Some(if raft_id == 2 { 1_000 } else { 500 });
            if raft_id == 2 {
                // Construct the older peer sample before delaying its
                // delivery. Sleeping first would test delayed sampling and
                // would not reproduce response-transit uncertainty.
                tokio::time::sleep(Duration::from_millis(900)).await;
            }
            Ok(Some(transport))
        },
    )
    .await;

    let old = sanitize_transport(
        remote
            .remove("node-2")
            .and_then(|outcome| outcome.transport)
            .expect("early private transport"),
        2,
        0,
    )
    .expect("valid early private transport");
    let successor = sanitize_transport(
        remote
            .remove("node-3")
            .and_then(|outcome| outcome.transport)
            .expect("delayed private transport"),
        3,
        0,
    )
    .expect("valid delayed private transport");

    assert_eq!(old.observations[0].attempt_age_ms, Some(1_900));
    assert_eq!(successor.observations[0].attempt_age_ms, Some(1_400));
    assert_eq!(old.observations[0].age_uncertainty_ms, 900);
    assert_eq!(successor.observations[0].age_uncertainty_ms, 0);
    assert!(
        old.observations[0]
            .attempt_age_ms
            .expect("older attempt age")
            .saturating_sub(old.observations[0].age_uncertainty_ms)
            < successor.observations[0]
                .attempt_age_ms
                .expect("successor attempt age"),
        "the delayed predecessor and successor intervals must overlap"
    );
}

#[tokio::test(start_paused = true)]
async fn slow_public_probe_cannot_discard_completed_private_transport() {
    let peers = vec![ActivityPeer {
        node_id: "node-2".to_owned(),
        raft_id: 2,
        http_base: Some("http://blackhole.invalid:32400".to_owned()),
        reachable: true,
    }];
    let private_transport = test_transport_status(2, 0, Some(1_000));
    let remote = collect_peer_statuses_with_sources(
        peers,
        tokio::time::Instant::now() + Duration::from_secs(2),
        |_peer, _deadline| async move { std::future::pending::<PeerStatusOutcome>().await },
        move |raft_id| {
            let private_transport = private_transport.clone();
            async move {
                assert_eq!(raft_id, 2);
                Ok(Some(private_transport))
            }
        },
    )
    .await;
    let outcome = remote.get("node-2").expect("timed-out public outcome");
    assert_eq!(outcome.state, ObservationState::TimedOut);
    assert_eq!(
        outcome
            .transport
            .as_ref()
            .map(|transport| transport.observing_node_id),
        Some(2)
    );
}

#[test]
fn every_aggregator_selects_the_same_roster_order_candidate() {
    let mut membership = test_membership("node-1", 3);
    let observations = test_observations(&membership);
    let first = rollout_verdict(&membership, &observations);
    assert!(first.safe_to_restart_one);
    assert_eq!(first.candidate_node_id.as_deref(), Some("node-2"));

    membership.local_node_id = "node-3".to_owned();
    let second = rollout_verdict(&membership, &observations);
    assert!(second.safe_to_restart_one);
    assert_eq!(second.candidate_node_id, first.candidate_node_id);
}

#[test]
fn an_active_restart_preparation_blocks_a_second_restart() {
    let membership = test_membership("node-1", 3);
    let mut observations = test_observations(&membership);
    observations[1]
        .status
        .as_mut()
        .expect("healthy fixture status")
        .media
        .new_admissions_blocked = true;

    let verdict = rollout_verdict(&membership, &observations);
    assert!(!verdict.safe_to_restart_one);
    assert!(verdict.candidate_node_id.is_none());
    assert!(verdict
        .blockers
        .iter()
        .any(|finding| finding.code == "restart_preparation_active"));
}

#[test]
fn maintenance_entry_admits_the_leader_a_busy_follower_and_a_stable_learner() {
    let mut membership = test_membership("node-1", 3);
    membership.nodes.push(ClusterNodeRecord {
        node_id: "node-4".to_owned(),
        hostname: "node-4".to_owned(),
        advertised_host: "node-4:8080".to_owned(),
        raft_id: 4,
        role: NodeRole::Learner,
        is_voter: false,
        is_leader: false,
        reachable: true,
        last_seen_at: 1,
        removal_pending: false,
        learner_protocol_ready: true,
        bounded_read_ready: true,
        apply_lag_entries: Some(0),
        storage_headroom_bytes: Some(1),
        voter_storage_ready: false,
        maintenance: false,
        maintenance_requested_at: None,
        maintenance_acknowledged: false,
        maintenance_ready: false,
        active_media_sessions: 0,
    });
    membership.capacity.non_voting_replicas = 1;
    membership.capacity.ready_read_workers = 1;
    let mut observations = test_observations(&membership);
    observations[1]
        .status
        .as_mut()
        .expect("busy follower status")
        .media = MediaDrainStatus {
        local_active_sessions: 2,
        drained: false,
        new_admissions_blocked: false,
        admissions_in_flight: 0,
        preparation_expires_at_unix_ms: None,
        direct_play_connections: DirectPlayConnectionStatus::UnknownRequiresProxy,
        restart_commands: Vec::new(),
    };

    let verdicts = maintenance_entry_verdicts(&membership, &observations);
    for node_id in ["node-1", "node-2", "node-4"] {
        let verdict = verdicts
            .iter()
            .find(|verdict| verdict.node_id == node_id)
            .expect("target verdict");
        assert!(
            verdict.safe_to_enter,
            "{node_id} should enter maintenance: {:?}",
            verdict.blockers
        );
    }
}

#[test]
fn maintenance_entry_subtracts_only_a_voters_vote() {
    let mut membership = test_membership("node-1", 2);
    membership.nodes.push(ClusterNodeRecord {
        node_id: "node-3".to_owned(),
        hostname: "node-3".to_owned(),
        advertised_host: "node-3:8080".to_owned(),
        raft_id: 3,
        role: NodeRole::Learner,
        is_voter: false,
        is_leader: false,
        reachable: true,
        last_seen_at: 1,
        removal_pending: false,
        learner_protocol_ready: true,
        bounded_read_ready: true,
        apply_lag_entries: Some(0),
        storage_headroom_bytes: Some(1),
        voter_storage_ready: false,
        maintenance: false,
        maintenance_requested_at: None,
        maintenance_acknowledged: false,
        maintenance_ready: false,
        active_media_sessions: 0,
    });
    let observations = test_observations(&membership);
    let verdicts = maintenance_entry_verdicts(&membership, &observations);
    let voter = verdicts
        .iter()
        .find(|verdict| verdict.node_id == "node-1")
        .expect("voter verdict");
    assert!(!voter.safe_to_enter);
    assert!(voter
        .blockers
        .iter()
        .any(|finding| finding.code == "maintenance_would_lose_quorum"));
    assert!(
        verdicts
            .iter()
            .find(|verdict| verdict.node_id == "node-3")
            .expect("learner verdict")
            .safe_to_enter
    );
}

#[test]
fn one_unreachable_voter_in_a_four_voter_roster_is_not_restart_safe() {
    let membership = test_membership("node-1", 4);
    let mut observations = test_observations(&membership);
    observations[3].observation = ObservationState::Unreachable;
    observations[3].status = None;
    observations[3].error_class = Some("unreachable".to_owned());

    let verdict = rollout_verdict(&membership, &observations);
    assert!(!verdict.safe_to_restart_one);
    assert!(verdict
        .blockers
        .iter()
        .any(|finding| finding.code == "voter_not_observed"));
}

#[test]
fn election_protocol_wal_and_build_skew_are_classified_conservatively() {
    let membership = test_membership("node-1", 3);

    let mut mixed_builds = test_observations(&membership);
    mixed_builds[2]
        .status
        .as_mut()
        .expect("healthy fixture status")
        .build = "test-next".to_owned();
    let verdict = rollout_verdict(&membership, &mixed_builds);
    assert!(verdict.safe_to_restart_one);
    assert!(verdict
        .warnings
        .iter()
        .any(|finding| finding.code == "mixed_builds"));

    let mut election = test_observations(&membership);
    election[2]
        .status
        .as_mut()
        .expect("healthy fixture status")
        .raft
        .current_term = Some(2);
    assert!(rollout_verdict(&membership, &election)
        .blockers
        .iter()
        .any(|finding| finding.code == "leader_term_disagreement"));

    let mut protocol = test_observations(&membership);
    protocol[2]
        .status
        .as_mut()
        .expect("healthy fixture status")
        .protocol_min = 2;
    protocol[2]
        .status
        .as_mut()
        .expect("healthy fixture status")
        .protocol_max = 2;
    assert!(rollout_verdict(&membership, &protocol)
        .blockers
        .iter()
        .any(|finding| finding.code == "protocol_incompatible"));

    let mut wal_error = test_observations(&membership);
    wal_error[2]
        .status
        .as_mut()
        .expect("healthy fixture status")
        .wal
        .snapshot
        .as_mut()
        .expect("healthy fixture WAL")
        .last_error = Some(plurx_core::cluster::migration::status::BoundedWalError {
        observed_at_unix_ms: unix_ms(),
        message: "injected test failure".to_owned(),
    });
    assert!(rollout_verdict(&membership, &wal_error)
        .blockers
        .iter()
        .any(|finding| finding.code == "wal_not_healthy"));
}

fn test_local_status(node_id: &str, raft_id: Option<u64>) -> ClusterNodeOperationsStatus {
    ClusterNodeOperationsStatus {
        schema_version: 1,
        observed_at_unix_ms: unix_ms(),
        node_id: node_id.to_owned(),
        raft_id,
        hostname: node_id.to_owned(),
        build: "test".to_owned(),
        protocol_min: 1,
        protocol_max: 1,
        process: ProcessStatus { live: true },
        serving: ReadinessEvaluation {
            ready: true,
            reason: None,
        },
        raft: RaftStatus {
            running: Some(true),
            sample_valid: true,
            sample_age_seconds: Some(0),
            current_term: Some(1),
            leader_id: Some(1),
            is_leader: Some(false),
            applied_index: Some(1),
            commit_index: Some(1),
            apply_lag_entries: Some(0),
            watermark_valid: true,
            watermark_age_millis: Some(0),
            observation_errors: 0,
            watermark_errors: 0,
        },
        wal: WalStatus {
            available: true,
            reason: None,
            snapshot: Some(WalStatusSnapshot {
                state: WalRuntimeState::Open,
                lock_owned: true,
                unclean_start_observed: false,
                sync_policy: "immediate".to_owned(),
                segment_size_bytes: 1_048_576,
                segment_count: 1,
                allocated_bytes: 1_048_576,
                first_retained_index: Some(1),
                last_log_index: Some(1),
                last_purged_index: None,
                last_durable_index: Some(1),
                last_sync_unix_ms: Some(unix_ms()),
                last_compaction_unix_ms: None,
                last_recovery: None,
                last_error: None,
            }),
        },
        snapshot: SnapshotStatus::from(None),
        transport: None,
        media: MediaDrainStatus {
            local_active_sessions: 0,
            drained: true,
            new_admissions_blocked: false,
            admissions_in_flight: 0,
            preparation_expires_at_unix_ms: None,
            direct_play_connections: DirectPlayConnectionStatus::UnknownRequiresProxy,
            restart_commands: Vec::new(),
        },
    }
}

fn test_membership(local_node_id: &str, voters: usize) -> MembershipStatus {
    let nodes = (1..=voters)
        .map(|index| ClusterNodeRecord {
            node_id: format!("node-{index}"),
            hostname: format!("node-{index}"),
            advertised_host: format!("node-{index}:8080"),
            raft_id: u64::try_from(index).expect("small fixture index"),
            role: NodeRole::Voter,
            is_voter: true,
            is_leader: index == 1,
            reachable: true,
            last_seen_at: 1,
            removal_pending: false,
            learner_protocol_ready: true,
            bounded_read_ready: true,
            apply_lag_entries: Some(0),
            storage_headroom_bytes: Some(1),
            voter_storage_ready: true,
            maintenance: false,
            maintenance_requested_at: None,
            maintenance_acknowledged: false,
            maintenance_ready: false,
            active_media_sessions: 0,
        })
        .collect::<Vec<_>>();
    let quorum = voters / 2 + 1;
    MembershipStatus {
        local_node_id: local_node_id.to_owned(),
        availability: plurx_core::cluster::membership::ClusterAvailability::SingleNode,
        nodes,
        replication: test_replication_status(),
        capacity: plurx_core::cluster::membership::ClusterCapacityStatus {
            voting_nodes: voters,
            voting_quorum: quorum,
            voting_failure_tolerance: voters.saturating_sub(quorum),
            non_voting_replicas: 0,
            ready_read_workers: 0,
        },
        protocol: plurx_core::cluster::membership::ClusterProtocolStatus {
            active_min: 1,
            active_max: 1,
            binary_min: 1,
            binary_max: 1,
            learner_protocol_active: false,
            learner_protocol_pending: Vec::new(),
        },
        recovery: ClusterRecoveryStatus {
            required: false,
            quorum_available: true,
            reachable_voters: voters,
            required_voters: quorum,
            leader_elected: true,
            permanent_majority_loss_supported: false,
        },
    }
}

fn test_observations(membership: &MembershipStatus) -> Vec<ClusterNodeObservation> {
    membership
        .nodes
        .iter()
        .map(|member| {
            let mut status = test_local_status(&member.node_id, Some(member.raft_id));
            status.raft.is_leader = Some(member.raft_id == 1);
            ClusterNodeObservation {
                membership: member.clone(),
                observation: ObservationState::Answered,
                sample_age_ms: Some(0),
                error_class: None,
                status: Some(status),
                transport: None,
            }
        })
        .collect()
}

fn test_replication_status() -> plurx_core::cluster::migration::status::ReplicationStatus {
    serde_json::from_value(serde_json::json!({
        "backend": "replicated",
        "health": "in_sync",
        "clustered": true,
        "last_applied_term": 1,
        "last_applied_index": 1,
        "behind_by": 0,
        "last_converged_at": 1,
        "checked_at": 1,
        "explanation": "test"
    }))
    .expect("test replication status")
}
