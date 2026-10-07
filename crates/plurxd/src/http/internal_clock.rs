//! Authenticated four-timestamp response with causal leader/learner observation.
use crate::state::AppState;
use axum::{
    body::Body,
    extract::State,
    http::{header, Request, Response, StatusCode},
};
use serde::{Deserialize, Serialize};

pub(crate) const PATH: &str = "/_internal/v1/clock";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClockResponse {
    pub node_id: String,
    pub received_unix_ms: i64,
    pub sent_unix_ms: i64,
}

#[cfg(all(test, feature = "cluster-integration-tests"))]
mod causal_tests {
    use super::*;
    use crate::clock_offset::ClockObserver;
    use futures_util::FutureExt;
    use plurx_core::cluster::membership::{
        decode_join_token, join_token_digest, ActivitySigningKey, ClusterPeer, ClusterRole,
        InternalPeerAuth, JoinSecrets, MembershipManager, RedeemJoinRequest,
        StartupMembershipAdmission,
    };
    use plurx_core::cluster::migration::select_daemon_store_observing;
    use plurx_core::config::Config;
    use std::borrow::Cow;
    use std::sync::Arc;
    use std::time::Duration;

    fn config(root: &std::path::Path) -> Config {
        let held: Vec<_> = (0..3)
            .map(|_| std::net::TcpListener::bind("127.0.0.1:0").expect("owned port"))
            .collect();
        let addresses: Vec<_> = held
            .iter()
            .map(|socket| socket.local_addr().expect("owned fixture invariant"))
            .collect();
        let mut config = Config::default();
        config.storage.data_dir = root.into();
        config.server.bind = addresses[0];
        config.cluster.raft_bind = addresses[1];
        config.cluster.api_bind = addresses[2];
        config.cluster.advertise_host = "localhost".into();
        config.cluster.join_url = format!("http://{}", addresses[0]);
        config.cluster.artwork_url = config.cluster.join_url.clone();
        config
    }

    fn clock_app(
        manager: MembershipManager,
        node: String,
        observer: ClockObserver,
    ) -> axum::Router {
        axum::Router::new()
            .route(PATH, axum::routing::get(observation_snapshot))
            .with_state(ObservationContext {
                membership: manager,
                node_id: node,
                observer,
            })
    }

    async fn send(
        client: &reqwest::Client,
        origin: &str,
        auth: &InternalPeerAuth,
    ) -> reqwest::Response {
        use crate::http::peer_transport::*;
        client
            .get(format!("{origin}{PATH}"))
            .header(NODE_HEADER, &auth.node_id)
            .header(TARGET_HEADER, &auth.target_node_id)
            .header(TIMESTAMP_HEADER, auth.timestamp_ms)
            .header(NONCE_HEADER, &auth.nonce)
            .header(SIGNATURE_HEADER, &auth.signature)
            .send()
            .await
            .expect("original two-second signed exchange")
    }

    fn proof(manager: &MembershipManager, target: &str) -> InternalPeerAuth {
        manager
            .sign_internal_peer_request(
                target,
                crate::media_sessions::unix_ms(),
                &uuid::Uuid::new_v4().to_string(),
                "GET",
                PATH,
                &[],
            )
            .expect("actual learner signature")
    }

    async fn verify_positive(
        manager: &MembershipManager,
        auth: &InternalPeerAuth,
        response: reqwest::Response,
    ) {
        use crate::http::peer_transport::{signed_response_payload, RESPONSE_SIGNATURE_HEADER};
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let signature = response
            .headers()
            .get(RESPONSE_SIGNATURE_HEADER)
            .expect("signed timing")
            .to_str()
            .expect("owned fixture invariant")
            .to_owned();
        let body = response.bytes().await.expect("bounded clock body");
        assert!(body.len() <= 1024);
        let timing: ClockResponse = serde_json::from_slice(&body).expect("real timestamps");
        assert_eq!(timing.node_id, auth.target_node_id);
        assert!(timing.sent_unix_ms >= timing.received_unix_ms);
        assert!(manager
            .authorize_internal_peer_member_response(
                &auth.target_node_id,
                &auth.node_id,
                &auth.nonce,
                PATH,
                &signed_response_payload(200, &body),
                &signature
            )
            .await
            .expect("actual response verification"));
    }

    async fn wait_rounds(observer: &ClockObserver, count: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while observer.test_audit().len() < count || observer.test_is_running() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("original bounded owner round");
    }

    /// A new real runtime control, not a replay of controller/socket/WAL tests.
    /// No heartbeat, clock sample, applied watch or removal outcome is seeded.
    #[test]
    fn k06_real_signed_learner_final_query_fence_and_owner_causality() {
        // Match the existing actual learner fixture's finite stack envelope;
        // the large source-import futures exceed default libtest stacks.
        let worker = std::thread::Builder::new()
            .name("k06-final-clock-query".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .thread_stack_size(8 * 1024 * 1024)
                    .enable_all()
                    .build()
                    .expect("owned causal fixture runtime")
                    .block_on(Box::pin(real_signed_learner_fixture()));
            })
            .expect("owned causal fixture thread");
        if let Err(panic) = worker.join() {
            std::panic::resume_unwind(panic);
        }
    }

    async fn real_signed_learner_fixture() {
        let source_root = crate::test_tempdir().expect("owned leader root");
        let source_config = config(source_root.path());
        drop(
            plurx_core::store::SqliteStore::open(&source_root.path().join("plurx.db"))
                .expect("schema"),
        );
        let startup = Arc::new(crate::StartupObservationHttp::new(
            source_config.server.bind,
        ));
        let selected = select_daemon_store_observing(&source_config, Some(startup.as_ref()))
            .await
            .expect("actual singleton activation within production phase");
        startup.stop_probe().await;
        let leader = selected.membership_manager();
        let token = leader
            .issue_token(Duration::from_secs(120))
            .await
            .expect("real token");
        let payload = decode_join_token(&token.token).expect("owned token");
        let learner_root = crate::test_tempdir().expect("owned learner root");
        let learner_config = config(learner_root.path());
        let learner_id = uuid::Uuid::new_v4().to_string();
        let (protocol_min, protocol_max) = payload.declared_protocol_range();
        leader
            .redeem(&RedeemJoinRequest {
                token_digest: join_token_digest(&token.token),
                raft_id: token.raft_id,
                node_id: learner_id.clone(),
                hostname: "causal-clock-control".into(),
                raft_address: learner_config.cluster.raft_bind.to_string(),
                api_address: learner_config.cluster.api_bind.to_string(),
                http_base: learner_config.cluster.artwork_url.clone(),
                schema_version: payload.schema_version(),
                protocol_version: protocol_min,
                protocol_min,
                protocol_max,
                live_tv_v1: true,
            })
            .await
            .expect("real authorized staged identity");
        let local = ClusterPeer {
            raft_id: token.raft_id,
            raft_address: learner_config.cluster.raft_bind.to_string(),
            api_address: learner_config.cluster.api_bind.to_string(),
        };
        let mut nodes: Vec<_> = payload
            .bootstrap()
            .iter()
            .map(hiqlite::Node::from)
            .collect();
        nodes.push(hiqlite::Node::from(&local));
        let learner_client = tokio::time::timeout(
            Duration::from_secs(45),
            Box::pin(hiqlite::start_node_for_clock_observation(
                hiqlite::NodeConfig {
                    node_id: token.raft_id,
                    nodes,
                    listen_addr_api: Cow::Borrowed("127.0.0.1"),
                    listen_addr_raft: Cow::Borrowed("127.0.0.1"),
                    data_dir: Cow::Owned(learner_root.path().to_string_lossy().into_owned()),
                    filename_db: Cow::Borrowed("clock-causal.db"),
                    secret_raft: payload.secrets().raft.clone(),
                    secret_api: payload.secrets().api.clone(),
                    tls_raft: Some(hiqlite::tls::ServerTlsConfig::TlsAutoCertificates),
                    tls_api: Some(hiqlite::tls::ServerTlsConfig::TlsAutoCertificates),
                    learner_only: true,
                    health_check_delay_secs: 0,
                    ..plurx_core::cluster::migration::production_hiqlite_defaults_with_read_pool(1)
                },
                Arc::new(StartupMembershipAdmission::default()),
            )),
        )
        .await
        .expect("unchanged 45-second learner budget")
        .expect("actual committed learner");
        let mut identity = selected.identity.clone();
        identity.node_id.clone_from(&learner_id);
        identity.raft_id = token.raft_id;
        let learner = MembershipManager::clock_observation(
            learner_client.clone(),
            selected.replication_monitor(),
            Arc::clone(&selected.store),
            identity,
            local,
            source_config.cluster.join_url.clone(),
            learner_config.cluster.artwork_url.clone(),
            JoinSecrets {
                raft: payload.secrets().raft.clone(),
                api: payload.secrets().api.clone(),
                credential_key: payload.secrets().credential_key.clone(),
            },
            ActivitySigningKey::from_seed_hex(&"13".repeat(32)).expect("test-owned signing key"),
            payload.activation_marker().clone(),
            ClusterRole::Voter,
            learner_root.path().into(),
        )
        .await
        .expect("actual pending observation identity");
        let applied = learner_client
            .local_db_raft_metrics()
            .expect("real applied watch")
            .membership_snapshot();
        assert!(applied.committed && applied.members.contains(&token.raft_id));
        assert!(!applied.voters.contains(&token.raft_id));
        let leadership = leader.clock_leadership_identity().expect("actual leader");
        assert_eq!(
            leadership.current_leader,
            Some(leadership.membership.local_node)
        );
        assert!(leadership.membership.members.contains(&token.raft_id));

        let observer = ClockObserver::new(leader.clone());
        observer.test_hold_periodic(true);
        let stop = tokio_util::sync::CancellationToken::new();
        let owner = {
            let observer = observer.clone();
            let stop = stop.clone();
            tokio::spawn(async move { observer.run(stop).await })
        };
        let source_http = {
            let startup = Arc::clone(&startup);
            let stop = stop.clone();
            let app = clock_app(
                leader.clone(),
                selected.identity.node_id.clone(),
                observer.clone(),
            );
            tokio::spawn(async move { startup.serve_normal(app, stop.cancelled()).await })
        };
        let learner_app = clock_app(
            learner.clone(),
            learner_id.clone(),
            ClockObserver::new(learner.clone()),
        );
        let learner_listener = tokio::net::TcpListener::bind(learner_config.server.bind)
            .await
            .expect("learner HTTP");
        let learner_http = {
            let stop = stop.clone();
            let app = learner_app.clone();
            tokio::spawn(async move {
                axum::serve(learner_listener, app)
                    .with_graceful_shutdown(async move { stop.cancelled().await })
                    .await
            })
        };
        let alternate_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("owned alternate origin");
        let alternate_origin = format!(
            "http://{}",
            alternate_listener
                .local_addr()
                .expect("owned fixture invariant")
        );
        let alternate_http = {
            let stop = stop.clone();
            tokio::spawn(async move {
                axum::serve(alternate_listener, learner_app)
                    .with_graceful_shutdown(async move { stop.cancelled().await })
                    .await
            })
        };

        // Always drain real writers/listeners even if a control assertion fails.
        let controls = async {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(2))
                .build()
                .expect("owned fixture invariant");
            let origin = source_config.cluster.join_url.as_str();
            let auth = proof(&learner, &selected.identity.node_id);
            let companion_auth = proof(&learner, &selected.identity.node_id);
            let round_gate = observer.test_gate_next_round();
            let final_gate = observer.test_gate_final_query();
            let primary = {
                let client = client.clone();
                let origin = origin.to_owned();
                let auth = auth.clone();
                tokio::spawn(async move { send(&client, &origin, &auth).await })
            };
            tokio::time::timeout(Duration::from_secs(2), round_gate.reached())
                .await
                .expect("handler demands owner, timer held");
            let barrier = observer
                .learner_barrier(&learner_id)
                .await
                .expect("owned fixture invariant")
                .expect("actual learner barrier");
            let (entered, entering) = tokio::sync::oneshot::channel();
            let cancelled = {
                let observer = observer.clone();
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    let mut observation = Box::pin(observer.observe_learner(&barrier));
                    assert!(
                        futures_util::poll!(observation.as_mut()).is_pending(),
                        "real waiter joins held owner"
                    );
                    entered.send(()).expect("test waiter admission witness");
                    observation.await
                })
            };
            let companion = {
                let client = client.clone();
                let origin = origin.to_owned();
                let auth = companion_auth.clone();
                tokio::spawn(async move { send(&client, &origin, &auth).await })
            };
            tokio::time::timeout(Duration::from_secs(2), entering)
                .await
                .expect("real waiter admitted")
                .expect("owned fixture invariant");
            cancelled.abort();
            assert!(cancelled
                .await
                .expect_err("cancelled real waiter")
                .is_cancelled());
            for _ in 0..32 {
                observer.test_demand_round();
            }
            round_gate.release();
            tokio::time::timeout(Duration::from_secs(2), final_gate.reached())
                .await
                .expect("real full round precedes final query");
            let audits = observer.test_audit();
            assert_eq!(
                audits.len(),
                1,
                "concurrent callers do not own extra rounds"
            );
            assert!(audits[0].completed);
            assert_eq!(
                audits[0]
                    .directory
                    .get(&learner_id)
                    .expect("owned fixture invariant"),
                &(token.raft_id, learner_config.cluster.artwork_url.clone())
            );
            assert_eq!(audits[0].filter_depths.get(&learner_id), Some(&1));
            final_gate.release();
            verify_positive(
                &learner,
                &auth,
                primary.await.expect("owned fixture invariant"),
            )
            .await;
            verify_positive(
                &learner,
                &companion_auth,
                companion.await.expect("owned fixture invariant"),
            )
            .await;
            let replay = send(&client, origin, &auth).await;
            assert_eq!(
                replay.status(),
                reqwest::StatusCode::UNAUTHORIZED,
                "nonce admitted exactly once"
            );
            assert!(!replay
                .headers()
                .contains_key(crate::http::peer_transport::RESPONSE_SIGNATURE_HEADER));
            assert!(replay
                .bytes()
                .await
                .expect("owned fixture invariant")
                .is_empty());

            // A real periodic trigger and demands share the actual filter owner.
            let periodic_gate = observer.test_gate_next_round();
            observer.test_hold_periodic(false);
            tokio::time::timeout(Duration::from_secs(2), periodic_gate.reached())
                .await
                .expect("actual periodic trigger");
            observer.test_hold_periodic(true);
            for _ in 0..32 {
                observer.test_demand_round();
            }
            periodic_gate.release();
            wait_rounds(&observer, 2).await;
            let audits = observer.test_audit();
            assert_eq!(audits.len(), 2);
            assert!(audits[1].completed);
            assert_eq!(
                audits[1].filter_depths.get(&learner_id),
                Some(&2),
                "same owner's actual samples retained"
            );
            assert_ne!(
                audits[0]
                    .ticket
                    .expect("owned fixture invariant")
                    .state_generation,
                audits[1]
                    .ticket
                    .expect("owned fixture invariant")
                    .state_generation
            );

            let leader_client = selected.local_client().expect("actual leader client");
            assert_eq!(
                leader_client
                    .execute(
                        "UPDATE cluster_node_http SET public_http_url=$1 WHERE node_id=$2",
                        hiqlite::params!(alternate_origin.as_str(), learner_id.as_str())
                    )
                    .await
                    .expect("committed origin change"),
                1
            );
            let changed_auth = proof(&learner, &selected.identity.node_id);
            verify_positive(
                &learner,
                &changed_auth,
                send(&client, origin, &changed_auth).await,
            )
            .await;
            let audits = observer.test_audit();
            assert_eq!(audits.len(), 3);
            assert!(audits[2].completed);
            assert_eq!(
                audits[2].directory.get(&learner_id),
                Some(&(token.raft_id, alternate_origin.clone()))
            );
            assert_eq!(
                audits[2].filter_depths.get(&learner_id),
                Some(&1),
                "actual changed-origin sample starts a new filter"
            );
            assert_ne!(
                audits[1]
                    .ticket
                    .expect("owned fixture invariant")
                    .state_generation,
                audits[2]
                    .ticket
                    .expect("owned fixture invariant")
                    .state_generation
            );

            let gate = observer.test_gate_final_query();
            let fenced_auth = proof(&learner, &selected.identity.node_id);
            let still_fresh = fenced_auth.clone();
            let held = {
                let client = client.clone();
                let origin = origin.to_owned();
                tokio::spawn(async move { send(&client, &origin, &fenced_auth).await })
            };
            tokio::time::timeout(Duration::from_secs(2), gate.reached())
                .await
                .expect("authenticated final query not yet started");
            let before = leader
                .clock_leadership_identity()
                .expect("owned fixture invariant");
            let attempt = uuid::Uuid::new_v4().to_string();
            assert_eq!(observer.test_final_query_results(), vec![true, true, true]);
            let committed = leader_client.txn(vec![
                ("INSERT INTO cluster_node_removal_attempts (node_id, attempt_id) VALUES ($1,$2)".to_owned(),
                    hiqlite::params!(learner_id.as_str(), attempt.as_str())),
                ("INSERT INTO cluster_node_removal_intents (node_id, attempt_id) SELECT $1,$2 \
                    WHERE EXISTS (SELECT 1 FROM cluster_node_removal_attempts WHERE node_id=$1 AND attempt_id=$2) \
                    ON CONFLICT(node_id) DO UPDATE SET attempt_id=excluded.attempt_id".to_owned(),
                    hiqlite::params!(learner_id.as_str(), attempt.as_str())),
                ("INSERT INTO cluster_node_removals (node_id, started_at) SELECT $1,$2 WHERE EXISTS \
                    (SELECT 1 FROM cluster_node_removal_attempts WHERE node_id=$1 AND attempt_id=$3) \
                    ON CONFLICT(node_id) DO NOTHING".to_owned(),
                    hiqlite::params!(learner_id.as_str(), crate::media_sessions::unix_ms(), attempt.as_str())),
                ("DELETE FROM cluster_node_removal_intents WHERE node_id=$1 AND attempt_id=$2".to_owned(),
                    hiqlite::params!(learner_id.as_str(), attempt.as_str())),
            ]).await.expect("real committed sender fence before query snapshot");
            assert_eq!(committed.len(), 4);
            for rows in committed {
                assert_eq!(rows.expect("actual fence transaction statement"), 1);
            }
            assert_eq!(
                leader.clock_leadership_identity().as_ref(),
                Some(&before),
                "fence does not fabricate membership reduction"
            );
            assert!(
                leader.clock_guard().check_evidence().is_ok(),
                "actual completed timing remains bounded; only sender authority was fenced"
            );
            assert!(leader
                .authenticated_clock_request_still_fresh(&still_fresh)
                .expect("actual request freshness"));
            gate.release();
            let refused = held.await.expect("owned fixture invariant");
            assert_eq!(refused.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
            assert!(!refused
                .headers()
                .contains_key(crate::http::peer_transport::RESPONSE_SIGNATURE_HEADER));
            assert!(
                refused
                    .bytes()
                    .await
                    .expect("owned fixture invariant")
                    .is_empty(),
                "refusal carries no trusted timing"
            );
            assert_eq!(
                observer.test_audit().len(),
                3,
                "no timer or passing context replay"
            );
            eprintln!("K06 actual signed handler: applied learner={}, leader term={}, full rounds=3, filter depths=1/2/1, nonce replay=401, final fenced response=503 unsigned; no membership reduction", token.raft_id, before.current_term);
            assert_eq!(
                observer.test_final_query_results(),
                vec![true, true, true, false],
                "refusal reached the actual final query, not a timeout or prior branch"
            );
        };
        let outcome = std::panic::AssertUnwindSafe(controls).catch_unwind().await;
        stop.cancel();
        owner.await.expect("observer terminal");
        source_http
            .await
            .expect("leader HTTP terminal")
            .expect("leader HTTP drained");
        learner_http
            .await
            .expect("learner HTTP terminal")
            .expect("learner HTTP drained");
        alternate_http
            .await
            .expect("alternate HTTP terminal")
            .expect("alternate HTTP drained");
        let learner_drain = tokio::spawn(async move { learner_client.shutdown().await }).await;
        match learner_drain {
            Ok(result) => result.expect("learner writer drained"),
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
            Err(error) => panic!("learner drain: {error}"),
        }
        selected.shutdown().await.expect("leader writer drained");
        if let Err(error) = outcome {
            std::panic::resume_unwind(error);
        }
    }
}

pub(crate) async fn snapshot(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, StatusCode> {
    snapshot_for_membership(
        &state.membership,
        &state.clock_observer,
        &state.node_id,
        request,
    )
    .await
}

/// One existing exact-request authorizer, shared by pending observation and
/// normal service. This context deliberately owns no application services.
#[derive(Clone)]
pub(crate) struct ObservationContext {
    pub(crate) membership: plurx_core::cluster::membership::MembershipManager,
    pub(crate) node_id: String,
    pub(crate) observer: crate::clock_offset::ClockObserver,
}

pub(crate) async fn observation_snapshot(
    State(state): State<ObservationContext>,
    request: Request<Body>,
) -> Result<Response<Body>, StatusCode> {
    snapshot_for_membership(&state.membership, &state.observer, &state.node_id, request).await
}

async fn snapshot_for_membership(
    membership: &plurx_core::cluster::membership::MembershipManager,
    observer: &crate::clock_offset::ClockObserver,
    node_id: &str,
    request: Request<Body>,
) -> Result<Response<Body>, StatusCode> {
    let received_unix_ms = crate::media_sessions::unix_ms();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let guard = membership.clock_guard();
    let clock_generation = guard.ticket().clock_generation;
    let auth = super::peer_transport::exact_auth_from_headers(request.headers())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    // The exact proof is for an empty GET. Never ignore an unsigned raw body.
    tokio::time::timeout_at(deadline, axum::body::to_bytes(request.into_body(), 0))
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if !tokio::time::timeout_at(
        deadline,
        membership.authorize_internal_peer_request(&auth, "GET", PATH, &[]),
    )
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    .unwrap_or(false)
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if !observer.belongs_to(membership) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let barrier = tokio::time::timeout_at(deadline, observer.learner_barrier(&auth.node_id))
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if let Some(barrier) = barrier {
        let completed = tokio::time::timeout_at(deadline, observer.observe_learner(&barrier))
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        tokio::time::timeout_at(
            deadline,
            observer.revalidate_learner(&barrier, completed, &auth),
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    }
    // No nonce is consumed again. All inverse service time remains between
    // t2/t3 and is subtracted from RTT by the unchanged four-stamp arithmetic.
    if tokio::time::Instant::now() >= deadline
        || guard.ticket().clock_generation != clock_generation
        || !membership
            .authenticated_clock_request_still_fresh(&auth)
            .unwrap_or(false)
    {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let sent_unix_ms = crate::media_sessions::unix_ms();
    let body = serde_json::to_vec(&ClockResponse {
        node_id: node_id.to_owned(),
        received_unix_ms,
        sent_unix_ms,
    })
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let payload = super::peer_transport::signed_response_payload(StatusCode::OK.as_u16(), &body);
    let signature = membership
        .sign_internal_peer_response(&auth.node_id, &auth.nonce, PATH, &payload)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(super::peer_transport::RESPONSE_SIGNATURE_HEADER, signature)
        .body(Body::from(body))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
