//! The directory's cache against a real replicated node (contract lane).
//!
//! A real singleton voter admits a real learner. Each change the roster can
//! see — a row, a Raft membership change, a progress row, a removal fence —
//! must change the very next answer with no sleep, and with no roster write
//! the directory must answer from the one snapshot it holds. A cache that
//! expires on a timer fails the first half; one that reads per call fails
//! the second.

use std::borrow::Cow;

use super::super::*;
use super::*;

#[test]
fn the_directory_answers_the_next_read_after_every_roster_change_and_holds_otherwise() {
    // The real activation and learner futures exceed the default libtest
    // stack; own one explicitly.
    let worker = std::thread::Builder::new()
        .name("directory-invalidation".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("owned fixture runtime")
                .block_on(Box::pin(directory_invalidation_fixture()));
        })
        .expect("owned fixture thread");
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
}

struct SingletonObserver;

impl crate::cluster::migration::StartupClockObserver for SingletonObserver {
    fn start(
        &self,
        manager: MembershipManager,
        _node: String,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), crate::error::StoreError>> + Send + '_>>
    {
        Box::pin(async move {
            let roster = manager
                .clock_peers()
                .await
                .map_err(|error| crate::error::StoreError::Database(error.to_string()))?;
            let guard = manager.clock_guard();
            let round = guard
                .roster_for_peer_directory(&roster)
                .expect("proved singleton");
            assert!(guard.publish(round, BTreeMap::new()), "no remote samples");
            Ok(())
        })
    }
}

fn addresses() -> Vec<std::net::SocketAddr> {
    let held: Vec<_> = (0..3)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").expect("reserve owned fixture port"))
        .collect();
    held.iter()
        .map(|listener| listener.local_addr().expect("owned port"))
        .collect()
}

#[allow(clippy::too_many_lines)]
async fn directory_invalidation_fixture() {
    use crate::cluster::migration::select_daemon_store_observing;
    use crate::config::Config;

    let root = tempfile::tempdir().expect("leader root");
    let ports = addresses();
    let mut config = Config::default();
    config.storage.data_dir = root.path().into();
    config.server.bind = ports[0];
    config.cluster.raft_bind = ports[1];
    config.cluster.api_bind = ports[2];
    config.cluster.advertise_host = "localhost".into();
    config.cluster.join_url = format!("http://{}", ports[0]);
    config.cluster.artwork_url = config.cluster.join_url.clone();
    drop(crate::store::SqliteStore::open(&root.path().join("plurx.db")).expect("source schema"));
    let selected = Box::pin(select_daemon_store_observing(
        &config,
        Some(&SingletonObserver),
    ))
    .await
    .expect("actual singleton activation");
    let manager = selected.membership_manager();
    let mark = selected
        .local_client()
        .expect("a local voter client")
        .local_db_applied_mark()
        .expect("the voter's applied mark");
    let directory = RosterDirectory::new(manager.clone(), Some(mark));
    let leader = manager.replicated_inner().expect("actual leader");
    // One instant for the whole test: rows are written at it and every
    // purpose is evaluated at it, so nothing depends on how long a step took.
    let t0 = unix_ms().expect("t0");

    // No roster write: one read, then the same snapshot, counted as hits.
    let first = directory.snapshot().await.expect("first read");
    assert!(
        first.stamp().is_some(),
        "a committed singleton stamps its read"
    );
    assert_eq!(directory.reads().reload, 1);
    let reads = 25;
    for _ in 0..reads {
        let again = directory.snapshot().await.expect("held read");
        assert!(
            Arc::ptr_eq(&first, &again),
            "no roster write, same snapshot"
        );
    }
    assert_eq!(
        directory.reads(),
        DirectoryReadCounts {
            hit: reads,
            reload: 1,
            uncached: 0
        }
    );
    assert!(first.operations(t0).expect("bounded").is_empty());

    // A real unrelated Raft write advances the applied index but not a roster
    // revision. Production registration must preserve the cached Arc.
    let before_unrelated = directory.reads();
    leader.client.execute(
        "directory_invalidation_fixture",
        "INSERT INTO settings (key, value, updated_at) VALUES ('directory.unrelated', 'yes', 0)",
        params!(),
    ).await.expect("unrelated settings write");
    let unrelated = directory
        .snapshot()
        .await
        .expect("read after unrelated apply");
    assert!(
        Arc::ptr_eq(&first, &unrelated),
        "unrelated apply preserves the roster snapshot"
    );
    assert_eq!(directory.reads().reload, before_unrelated.reload);

    // A row: the redeemed learner's identity, not yet a Raft member.
    let token = manager
        .issue_token(Duration::from_secs(120))
        .await
        .expect("owned join token");
    let learner_root = tempfile::tempdir().expect("learner root");
    let learner_ports = addresses();
    let learner_id = uuid::Uuid::new_v4().to_string();
    let (protocol_min, protocol_max) = manager.active_protocol_range().await.expect("range");
    manager
        .redeem(&RedeemJoinRequest {
            token_digest: join_token_digest(&token.token),
            raft_id: token.raft_id,
            node_id: learner_id.clone(),
            hostname: "directory-fixture".into(),
            raft_address: learner_ports[1].to_string(),
            api_address: learner_ports[2].to_string(),
            http_base: format!("http://{}", learner_ports[0]),
            schema_version: AUTH_SCHEMA_VERSION,
            protocol_version: protocol_min,
            protocol_min,
            protocol_max,
            live_tv_v1: true,
            sharing: SharingJoinCapabilities::for_current_binary(),
        })
        .await
        .expect("actual token-authorized staged identity");
    let staged = directory.snapshot().await.expect("staged read");
    assert!(
        !Arc::ptr_eq(&first, &staged),
        "a written row is a new snapshot"
    );
    assert!(
        staged.operations(t0).expect("bounded").is_empty(),
        "a staged row is not a member until Raft admits it"
    );

    // A Raft membership change: the learner is admitted, and no row the
    // directory reads has to move for the next answer to name it.
    let held = directory.snapshot().await.expect("held staged read");
    assert!(Arc::ptr_eq(&staged, &held));
    let local = ClusterPeer {
        raft_id: token.raft_id,
        raft_address: learner_ports[1].to_string(),
        api_address: learner_ports[2].to_string(),
    };
    let learner_client = Box::pin(tokio::time::timeout(
        Duration::from_secs(45),
        Box::pin(hiqlite::start_node_for_clock_observation(
            hiqlite::NodeConfig {
                node_id: token.raft_id,
                nodes: vec![Node::from(&leader.local), Node::from(&local)],
                listen_addr_api: Cow::Borrowed("127.0.0.1"),
                listen_addr_raft: Cow::Borrowed("127.0.0.1"),
                data_dir: Cow::Owned(learner_root.path().to_string_lossy().into_owned()),
                filename_db: Cow::Borrowed("directory-test.db"),
                secret_raft: leader.secrets.raft.clone(),
                secret_api: leader.secrets.api.clone(),
                tls_raft: Some(hiqlite::tls::ServerTlsConfig::TlsAutoCertificates),
                tls_api: Some(hiqlite::tls::ServerTlsConfig::TlsAutoCertificates),
                learner_only: true,
                health_check_delay_secs: 0,
                ..crate::cluster::migration::production_hiqlite_defaults_with_read_pool(1)
            },
            Arc::new(StartupMembershipAdmission::default()),
        )),
    ))
    .await
    .expect("bounded learner startup")
    .expect("actual learner");
    // The admission commits through Raft; wait for this node's watch to
    // apply it rather than assume it already has.
    let mut watch = leader.local_metrics.clone();
    let admitted = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let membership = watch.membership_snapshot();
            if membership.committed && membership.members.contains(&token.raft_id) {
                return membership;
            }
            assert!(watch.wait_for_change().await, "the Raft watch closed");
        }
    })
    .await
    .expect("the learner's admission commits");
    assert!(!admitted.voters.contains(&token.raft_id));
    let member = directory.snapshot().await.expect("member read");
    assert_eq!(
        member
            .operations(t0)
            .expect("bounded")
            .into_iter()
            .map(|peer| peer.node_id)
            .collect::<Vec<_>>(),
        vec![learner_id.clone()],
        "the admitted learner is a committed member on the next read"
    );
    assert!(member.voters(t0).is_empty(), "a learner is never a voter");

    // The HTTP endpoint has its own registered slot, separate from node and
    // progress updates. The very next read sees the new advertised URL.
    let new_url = format!("http://{}/directory-new", learner_ports[0]);
    assert_eq!(
        leader
            .client
            .execute(
                "directory_invalidation_fixture",
                "UPDATE cluster_node_http SET public_http_url = $1 WHERE node_id = $2",
                params!(new_url.clone(), learner_id.as_str()),
            )
            .await
            .expect("HTTP roster change"),
        1
    );
    let endpoint = directory.snapshot().await.expect("changed endpoint read");
    assert!(!Arc::ptr_eq(&member, &endpoint));
    assert_eq!(
        endpoint.operations(t0).expect("bounded")[0].http_base,
        Some(new_url)
    );

    assert_eq!(member.voter_count(), 1);

    // A progress row: ready, then unready. Each changes the next answer.
    let workers = |snapshot: &RosterSnapshot| {
        snapshot
            .media_workers(t0, true)
            .expect("known roles")
            .into_iter()
            .map(|peer| peer.node_id)
            .collect::<Vec<_>>()
    };
    assert!(workers(&member).is_empty(), "no progress row yet");
    let progress = |ready: bool| {
        format!(
            "INSERT INTO cluster_node_progress (node_id, current_term, bounded_read_ready, \
                 voter_storage_ready, voter_role_persisted, observed_at) \
             VALUES ('{learner_id}', 1, {}, 0, 0, {}) \
             ON CONFLICT(node_id) DO UPDATE SET bounded_read_ready = excluded.bounded_read_ready, \
                 observed_at = excluded.observed_at",
            i64::from(ready),
            t0
        )
    };
    leader
        .client
        .execute("directory_invalidation_fixture", progress(true), params!())
        .await
        .expect("ready progress row");
    let ready = directory.snapshot().await.expect("ready read");
    assert_eq!(workers(&ready), vec![learner_id.clone()]);
    leader
        .client
        .execute("directory_invalidation_fixture", progress(false), params!())
        .await
        .expect("unready progress row");
    let unready = directory.snapshot().await.expect("unready read");
    assert!(
        workers(&unready).is_empty(),
        "an unready row leaves at once"
    );

    // The clock: reachability ages on the same snapshot, with no read.
    leader
        .client
        .execute(
            "directory_invalidation_fixture",
            "UPDATE cluster_nodes SET last_seen_at = $1 WHERE node_id = $2",
            params!(t0, learner_id.as_str()),
        )
        .await
        .expect("fresh heartbeat row");
    let seen = directory.snapshot().await.expect("seen read");
    let before = directory.reads();
    let at = t0;
    let reachable = |snapshot: &RosterSnapshot, at: i64| {
        snapshot
            .operations(at)
            .expect("bounded")
            .iter()
            .all(|peer| peer.reachable)
    };
    assert!(reachable(&seen, at));
    let same = directory.snapshot().await.expect("held read");
    assert!(Arc::ptr_eq(&seen, &same));
    assert!(
        !reachable(&same, at + NODE_REACHABLE_WINDOW_MS + 1_000),
        "31 s later the same snapshot answers unreachable"
    );
    assert_eq!(directory.reads().reload, before.reload, "no read to age");

    // A removal fence, written as the coordinator writes it (an attempt
    // reference, its intent, then the fence, in one transaction): the next
    // answer drops the member.
    let attempt = uuid::Uuid::new_v4().to_string();
    let written = leader
        .client
        .txn(
            "directory_invalidation_fixture",
            vec![
            (
                "INSERT INTO cluster_node_removal_attempts (node_id, attempt_id) VALUES ($1, $2)"
                    .to_owned(),
                params!(learner_id.as_str(), attempt.as_str()),
            ),
            (
                BEGIN_REMOVAL_INTENT_SQL.to_owned(),
                params!(learner_id.as_str(), attempt.as_str()),
            ),
            (
                BEGIN_REMOVAL_FENCE_SQL.to_owned(),
                params!(learner_id.as_str(), t0, attempt.as_str()),
            ),
        ],
        )
        .await
        .expect("removal fence transaction")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("removal fence statements");
    assert_eq!(written, vec![1, 1, 1], "the fence row was written");
    let fenced = directory.snapshot().await.expect("fenced read");
    assert!(
        fenced.operations(t0).expect("bounded").is_empty(),
        "a removal fence leaves at once"
    );
    assert_eq!(directory.reads().uncached, 0, "every read was stamped");

    let stopped = tokio::spawn(async move { learner_client.shutdown().await }).await;
    match stopped {
        Ok(result) => result.expect("learner drain"),
        Err(error) if error.is_panic() => {
            let payload = error.into_panic();
            let text = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied());
            assert_eq!(
                text,
                Some("The global Hiqlite shutdown handler to always listen: SendError { .. }")
            );
        }
        Err(error) => panic!("learner shutdown task: {error}"),
    }
    selected.shutdown().await.expect("leader drain");
}
