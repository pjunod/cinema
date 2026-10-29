//! Real local-voter API contracts: no synthetic Raft roster or bypassed clock.
use super::*;
use crate::cluster::membership::{
    CacheAdminRevocationCleanupOutcome, ClusterAvailability, ClusterOperationCleanupOutcome,
    MembershipError,
};
use crate::store::{AUTH_PROTOCOL_MAX, AUTH_PROTOCOL_MIN};
use hiqlite::params;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

async fn singleton() -> (tempfile::TempDir, SelectedStore) {
    install_default_crypto_provider();
    let directory = tempfile::tempdir().expect("owned singleton directory");
    let config = membership_test_config(directory.path());
    drop(SqliteStore::open(&directory.path().join(SQLITE_FILENAME)).expect("source SQLite"));
    let selected = startup_observer::select_applied_singleton(&config)
        .await
        .expect("real admitted singleton");
    (directory, selected)
}

fn redemption(token: &crate::cluster::membership::IssuedJoinToken) -> RedeemJoinRequest {
    RedeemJoinRequest {
        token_digest: join_token_digest(&token.token),
        raft_id: token.raft_id,
        node_id: uuid::Uuid::new_v4().to_string(),
        hostname: "prospective-peer".into(),
        raft_address: format!("127.0.0.1:{}", free_test_port()),
        api_address: format!("127.0.0.1:{}", free_test_port()),
        http_base: String::new(),
        schema_version: AUTH_SCHEMA_VERSION,
        protocol_version: AUTH_PROTOCOL_MIN,
        protocol_min: AUTH_PROTOCOL_MIN,
        protocol_max: AUTH_PROTOCOL_MAX,
        live_tv_v1: true,
        sharing: Default::default(),
    }
}

#[test]
fn real_singleton_join_refusals_leave_membership_and_unused_tokens_intact() {
    startup_observer::run_plural_fixture("singleton-join-refusals", join_refusals);
}

async fn join_refusals() {
    let (_directory, selected) = singleton().await;
    let manager = selected.membership_manager();
    let client = selected.local_client().expect("local voter client");
    let status = manager.status().await.expect("current membership");
    assert_eq!(status.availability, ClusterAvailability::SingleNode);
    assert_eq!(status.capacity.voting_nodes, 1);
    assert_eq!(status.capacity.voting_quorum, 1);
    assert_eq!(status.capacity.voting_failure_tolerance, 0);
    assert_eq!(status.capacity.non_voting_replicas, 0);
    assert!(status.recovery.quorum_available && status.recovery.leader_elected);
    assert!(!status.recovery.required);
    assert_eq!(
        manager.active_protocol_range().await.expect("active range"),
        (AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_MIN)
    );
    assert!(matches!(
        manager.issue_learner_token(Duration::from_secs(30)).await,
        Err(MembershipError::LearnerProtocolInactive)
    ));

    let issued = manager
        .issue_token(Duration::from_secs(30))
        .await
        .expect("voter token");
    let next = manager
        .issue_token(Duration::from_secs(30))
        .await
        .expect("independent outstanding token");
    assert!(
        next.raft_id > issued.raft_id,
        "live tokens reserve distinct Raft identities"
    );
    let request = redemption(&issued);
    let mut incompatible = request.clone();
    incompatible.schema_version -= 1;
    assert!(matches!(
        manager.redeem(&incompatible).await,
        Err(MembershipError::Incompatible)
    ));
    let mut wrong_id = request.clone();
    wrong_id.raft_id += 100;
    assert!(matches!(
        manager.redeem(&wrong_id).await,
        Err(MembershipError::InvalidToken)
    ));
    assert!(
        matches!(
            manager.redeem_learner(&request).await,
            Err(MembershipError::InvalidToken)
        ),
        "a voter token cannot choose a learner role"
    );
    let mut unsupported = request.clone();
    unsupported.protocol_min = AUTH_PROTOCOL_MAX + 1;
    unsupported.protocol_max = AUTH_PROTOCOL_MAX + 1;
    unsupported.protocol_version = AUTH_PROTOCOL_MAX + 1;
    assert!(matches!(
        manager.redeem(&unsupported).await,
        Err(MembershipError::Incompatible)
    ));
    let mut unknown = request.clone();
    unknown.token_digest = "f".repeat(64);
    assert!(matches!(
        manager.redeem(&unknown).await,
        Err(MembershipError::InvalidToken)
    ));
    let mut rows = client
        .query_consistent(
            "SELECT state FROM cluster_join_tokens WHERE token_hash=$1",
            params!(&request.token_digest),
        )
        .await
        .expect("unused token state");
    assert_eq!(
        rows[0].get::<String>("state"),
        "issued",
        "failed admission never consumes the token"
    );

    // Exact durable states are owned fixture input; the public API must
    // classify them before attempting to connect to the absent joiner.
    client
        .execute(
            "UPDATE cluster_join_tokens SET expires_at=0 WHERE token_hash=$1",
            params!(&request.token_digest),
        )
        .await
        .expect("expired issued fixture");
    assert!(matches!(
        manager.redeem(&request).await,
        Err(MembershipError::ExpiredToken)
    ));
    let replay = redemption(&next);
    assert_eq!(client.execute("UPDATE cluster_join_tokens SET state='redeemed',node_id=$1,redeemed_at=1 WHERE token_hash=$2", params!(&replay.node_id, &replay.token_digest)).await.expect("completed redemption fixture"), 1, "fixture must update the exact token");
    let replayed = manager.redeem(&replay).await;
    assert!(
        matches!(replayed, Err(MembershipError::ReusedToken)),
        "completed token replay: {replayed:?}"
    );
    assert_eq!(
        manager
            .status()
            .await
            .expect("membership after refusals")
            .nodes
            .len(),
        1
    );

    // A signature from this same node is intentionally not peer authority.
    // Prove that even a correctly signed local proof cannot bypass that rule.
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("timestamp");
    for timestamp in [now, now - 60_000] {
        let auth = manager
            .sign_internal_peer_request(
                &status.local_node_id,
                timestamp,
                &uuid::Uuid::new_v4().to_string(),
                "GET",
                "/_internal/v1/media",
                b"",
            )
            .expect("signed self request");
        assert!(!manager
            .authorize_internal_peer_read_request(&auth, "GET", "/_internal/v1/media", b"")
            .await
            .expect("self proof refused"));
    }
    selected.shutdown().await.expect("stop real singleton");
}

#[test]
fn real_singleton_outage_and_cache_admin_claims_release_only_their_owned_attempt() {
    startup_observer::run_plural_fixture("singleton-operation-claims", operation_claims);
}

async fn operation_claims() {
    let (_directory, selected) = singleton().await;
    // Heartbeat progress is evidence from the production local Raft watch and
    // quorum watermark sampler, not a fixture-written readiness bit.
    let sampler_shutdown = tokio_util::sync::CancellationToken::new();
    let _sampler_guard = sampler_shutdown.clone().drop_guard();
    let stop = sampler_shutdown.clone();
    let monitor = selected.replication_monitor();
    let sampler = tokio::spawn(monitor.passive_metrics_loop(async move {
        stop.cancelled().await;
    }));
    let manager = selected.membership_manager();
    let node_id = selected.identity.node_id.clone();
    let first =
        MembershipManager::prepare_restart_preparation_claim(&node_id, Duration::from_secs(30))
            .expect("first restart claim");
    let loser =
        MembershipManager::prepare_restart_preparation_claim(&node_id, Duration::from_secs(30))
            .expect("competing restart claim");
    manager
        .commit_cluster_operation_lease(&first)
        .await
        .expect("first claim commits");
    assert!(matches!(
        manager.commit_cluster_operation_lease(&loser).await,
        Err(MembershipError::ClusterOperationPending)
    ));
    assert_eq!(
        manager
            .release_cluster_operation_lease(&loser)
            .await
            .expect("loser cleanup"),
        ClusterOperationCleanupOutcome::Confirmed
    );
    assert!(
        matches!(
            manager.commit_cluster_operation_lease(&loser).await,
            Err(MembershipError::ClusterOperationPending)
        ),
        "a released losing attempt cannot be revived"
    );
    assert_eq!(
        manager
            .release_cluster_operation_lease(&first)
            .await
            .expect("owned release"),
        ClusterOperationCleanupOutcome::Confirmed
    );
    let successor =
        MembershipManager::prepare_restart_preparation_claim(&node_id, Duration::from_secs(30))
            .expect("successor claim");
    manager
        .commit_cluster_operation_lease(&successor)
        .await
        .expect("successor commits");
    manager
        .release_cluster_operation_lease(&first)
        .await
        .expect("delayed predecessor cleanup");
    let blocked =
        MembershipManager::prepare_restart_preparation_claim(&node_id, Duration::from_secs(30))
            .expect("third claim");
    assert!(
        matches!(
            manager.commit_cluster_operation_lease(&blocked).await,
            Err(MembershipError::ClusterOperationPending)
        ),
        "delayed cleanup cannot clear the successor"
    );
    assert_eq!(
        manager
            .release_restart_preparation(&node_id)
            .await
            .expect("cancel current restart"),
        ClusterOperationCleanupOutcome::Confirmed
    );
    assert!(
        matches!(
            manager.commit_cluster_operation_lease(&successor).await,
            Err(MembershipError::ClusterOperationPending)
        ),
        "cancelled restart cannot return"
    );

    let mut cache =
        MembershipManager::prepare_cache_admin_revocation_claim(&node_id, Duration::from_secs(30))
            .expect("cache-admin claim");
    manager
        .commit_cache_admin_revocation_lease(&mut cache)
        .await
        .expect("cache-admin exclusion commits");
    assert!(manager
        .cache_admin_revocation_claim_applied(cache.claim_id())
        .await
        .expect("exact claim applied"));
    let mut cache_loser =
        MembershipManager::prepare_cache_admin_revocation_claim(&node_id, Duration::from_secs(30))
            .expect("competing cache claim");
    assert!(matches!(
        manager
            .commit_cache_admin_revocation_lease(&mut cache_loser)
            .await,
        Err(MembershipError::ClusterOperationPending)
    ));
    assert_eq!(
        manager
            .release_cache_admin_revocation_lease(&cache_loser)
            .await
            .expect("cache loser cleanup"),
        CacheAdminRevocationCleanupOutcome::Confirmed
    );
    assert!(manager
        .cache_admin_revocation_claim_applied(cache.claim_id())
        .await
        .expect("winner survives cleanup"));
    assert_eq!(
        manager
            .release_cache_admin_revocation_lease(&cache)
            .await
            .expect("cache winner cleanup"),
        CacheAdminRevocationCleanupOutcome::Confirmed
    );
    assert!(!manager
        .cache_admin_revocation_claim_applied(cache.claim_id())
        .await
        .expect("released claim absent"));

    // A real live owner must remain visible while maintenance drains. The
    // status query binds its capability and lease timestamp independently.
    let viewer = selected
        .store
        .create_user("maintenance-viewer", "hash", false)
        .await
        .expect("viewer");
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("timestamp");
    let activation = crate::domain::MediaSessionActivation {
        incarnation_id: uuid::Uuid::new_v4().to_string(),
        session_id: uuid::Uuid::new_v4().to_string(),
        principal: crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: viewer.id },
        playback_id: uuid::Uuid::new_v4().to_string(),
        recovery_epoch: String::new(),
        expected_predecessor_incarnation_id: None,
        fence_predecessor: false,
        request_id: None,
        request_fingerprint: "a".repeat(64),
        owner_node_id: node_id.clone(),
        recipe_json: "{}".into(),
        response_json: "{}".into(),
        publication_ready_at_ms: crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: 0,
        now_ms: now,
        lease_expires_at_ms: now + 120_000,
        expected_desired_revision: None,
    };
    selected
        .store
        .activate_media_session(&activation)
        .await
        .expect("activate exact local owner")
        .expect("activation wins");
    selected
        .store
        .settle_media_session_activation(
            &activation,
            crate::domain::MediaSessionActivationSettlement::Confirm {
                publication_ready_at_ms: 0,
            },
            now,
        )
        .await
        .expect("settle exact owner")
        .expect("confirmed owner");
    assert_eq!(
        manager
            .status()
            .await
            .expect("live owner projection")
            .nodes
            .iter()
            .find(|node| node.node_id == node_id)
            .expect("local node")
            .active_media_sessions,
        1,
        "membership status counts unexpired active ownership"
    );

    let maintenance =
        MembershipManager::prepare_maintenance_preparation_claim(&node_id, Duration::from_secs(30))
            .expect("maintenance claim");
    manager
        .commit_cluster_operation_lease(&maintenance)
        .await
        .expect("maintenance preparation commits");
    let entered = manager
        .enter_maintenance(&node_id, &maintenance)
        .await
        .expect("sole voter can enter maintenance");
    assert!(
        entered
            .nodes
            .iter()
            .find(|node| node.node_id == node_id)
            .expect("local node")
            .maintenance
    );
    assert!(manager.local_maintenance_active());
    assert!(
        !manager.may_run_cluster_jobs().await,
        "maintenance fences background authority"
    );
    assert!(manager
        .reconcile_local_maintenance_commit()
        .await
        .expect("durable maintenance observation"));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        manager.heartbeat().await.expect("maintenance heartbeat");
        let status = manager.status().await.expect("acknowledged maintenance");
        if status.nodes.iter().any(|node| {
            node.node_id == node_id
                && node.maintenance_acknowledged
                && node.apply_lag_entries == Some(0)
        }) {
            let local = status
                .nodes
                .iter()
                .find(|node| node.node_id == node_id)
                .expect("local node");
            assert_eq!(local.active_media_sessions, 1);
            assert!(
                !local.maintenance_ready,
                "an acknowledged live owner still prevents resumption"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "current process must acknowledge a caught-up drained singleton: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(
        matches!(
            manager.exit_maintenance(&node_id).await,
            Err(MembershipError::MaintenanceResumeUnsafe(_))
        ),
        "a real active owner cannot be waved through maintenance readiness"
    );
    let ended_at = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("timestamp");
    selected
        .store
        .end_media_session(&activation.session_id, "deleted", ended_at)
        .await
        .expect("end real owner")
        .expect("ended route");
    // Durable End advances Raft independently of the passive watermark and
    // acknowledged heartbeat. Wait for its real zero-lag observation rather
    // than treating transaction completion as maintenance readiness.
    let drained_deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let status = manager.status().await.expect("drained maintenance status");
        let local = status
            .nodes
            .iter()
            .find(|node| node.node_id == node_id)
            .expect("local node");
        assert_eq!(
            local.active_media_sessions, 0,
            "durable End removes active ownership"
        );
        if local.maintenance_ready {
            break;
        }
        assert!(
            tokio::time::Instant::now() < drained_deadline,
            "real passive progress and heartbeat must observe the drained owner: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let resumed = manager
        .exit_maintenance(&node_id)
        .await
        .expect("resume acknowledged singleton");
    assert!(
        !resumed
            .nodes
            .iter()
            .find(|node| node.node_id == node_id)
            .expect("local node")
            .maintenance
    );
    assert!(!manager.local_maintenance_active());
    assert!(manager.may_run_cluster_jobs().await);
    assert!(!manager
        .reconcile_local_maintenance_commit()
        .await
        .expect("maintenance cleared"));
    assert!(matches!(
        manager.exit_maintenance("absent-node").await,
        Err(MembershipError::NodeNotFound)
    ));
    sampler_shutdown.cancel();
    sampler.await.expect("join production progress sampler");
    selected.shutdown().await.expect("stop singleton");
}

#[test]
fn real_singleton_artwork_repair_retirement_fences_old_generations() {
    startup_observer::run_plural_fixture("singleton-artwork-repair", artwork_claims);
}

async fn artwork_claims() {
    let (_directory, selected) = singleton().await;
    let manager = selected.membership_manager();
    let item_id = 412;
    assert!(!manager
        .observe_artwork_source_repair(item_id)
        .await
        .expect("no previous repair"));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let claim = loop {
        if let Some(claim) = manager
            .claim_artwork_source_repair(item_id, Duration::from_secs(30))
            .await
            .expect("leader repair claim")
        {
            break claim;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "sole live leader must acquire the first repair"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(claim.fence().owner_node_id, selected.identity.node_id);
    assert_eq!(claim.fence().generation, 1);
    assert!(claim.deadline() > Instant::now());
    assert!(manager
        .observe_artwork_source_repair(item_id)
        .await
        .expect("observe exact repair"));
    assert!(manager
        .claim_artwork_source_repair(item_id, Duration::from_secs(30))
        .await
        .expect("live repair refusal")
        .is_none());
    let mut wrong = claim.fence().clone();
    wrong.owner_node_id = "other-node".into();
    assert!(!manager
        .retire_artwork_source_repair(&wrong)
        .await
        .expect("foreign fence refused"));
    assert!(manager
        .retire_artwork_source_repair(claim.fence())
        .await
        .expect("owned repair retirement"));
    assert!(!manager
        .retire_artwork_source_repair(claim.fence())
        .await
        .expect("old generation cannot retire again"));
    assert!(manager
        .observe_artwork_source_repair(item_id)
        .await
        .expect("retired generation establishes a new observation"));
    assert!(manager
        .claim_artwork_source_repair(item_id, Duration::from_secs(30))
        .await
        .expect("new observation must wait its own lease")
        .is_none());
    selected.shutdown().await.expect("stop singleton");
}
