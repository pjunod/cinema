use super::*;
use axum::{extract::State, response::IntoResponse, routing::post, Json};
use plurx_core::cluster::membership::{
    FinalizeJoinRequest, LocalServingRole, MembershipManager, RedeemJoinRequest,
};
use plurx_core::config::Config;
use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

async fn redeem_learner(
    State(manager): State<MembershipManager>,
    Json(request): Json<RedeemJoinRequest>,
) -> axum::response::Response {
    match manager.redeem_learner(&request).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn finalize_learner(
    State(manager): State<MembershipManager>,
    Json(request): Json<FinalizeJoinRequest>,
) -> axum::response::Response {
    match manager.finalize_learner(&request).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

fn free_address() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("owned cluster port");
    listener.local_addr().expect("owned cluster address")
}

fn member_config(root: &Path) -> Config {
    let mut config = Config::default();
    config.storage.data_dir = root.to_owned();
    // The voter serves on these sockets, so no other socket can take either
    // port between this bind and its start.
    let raft = std::net::TcpListener::bind("127.0.0.1:0").expect("owned Raft listener");
    let api = std::net::TcpListener::bind("127.0.0.1:0").expect("owned API listener");
    config.cluster.raft_bind = raft.local_addr().expect("owned Raft address");
    config.cluster.api_bind = api.local_addr().expect("owned API address");
    config.cluster.listeners = plurx_core::config::ClusterListeners::owned(raft, api);
    config.cluster.advertise_host = "localhost".into();
    config
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("cluster fixture clock")
            .as_millis(),
    )
    .expect("bounded fixture clock")
}

async fn signed_operation(
    app: &Router,
    signer: &MembershipManager,
    owner: &str,
    method: Method,
    path: &str,
    body: Vec<u8>,
) -> (StatusCode, Vec<u8>) {
    let nonce = uuid::Uuid::new_v4().to_string();
    let auth = signer
        .sign_internal_peer_request(owner, now_ms(), &nonce, method.as_str(), path, &body)
        .expect("actual member signs exact Activity request");
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(peer_transport::NODE_HEADER, auth.node_id.as_str())
        .header(peer_transport::TARGET_HEADER, auth.target_node_id.as_str())
        .header(
            peer_transport::TIMESTAMP_HEADER,
            auth.timestamp_ms.to_string(),
        )
        .header(peer_transport::NONCE_HEADER, auth.nonce.as_str())
        .header(peer_transport::SIGNATURE_HEADER, auth.signature.as_str())
        .body(Body::from(body))
        .expect("signed Activity HTTP request");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("full Activity router");
    let status = response.status();
    let signature = response
        .headers()
        .get(peer_transport::RESPONSE_SIGNATURE_HEADER)
        .expect("owner signs Activity response")
        .to_str()
        .expect("hexadecimal response signature")
        .to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1_048_576)
        .await
        .expect("bounded Activity response")
        .to_vec();
    let payload = peer_transport::signed_response_payload(status.as_u16(), &bytes);
    assert!(
        signer
            .authorize_internal_peer_member_response(
                owner,
                &auth.node_id,
                &nonce,
                path,
                &payload,
                &signature
            )
            .await
            .expect("fresh member response proof"),
        "Activity acknowledgment must bind this owner, request nonce, path and body"
    );
    (status, bytes)
}

async fn refused_by_capacity(app: &Router, method: Method, path: &str, code: &str) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .expect("bounded refused route"),
        )
        .await
        .expect("full router capacity refusal");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("bounded refusal body");
    let answer: serde_json::Value = serde_json::from_slice(&body).expect("typed capacity refusal");
    assert_eq!(answer["code"], code, "{path}");
}

async fn admitted_activity_routes(app: &Router, signer: &MembershipManager, owner: &str) {
    for (method, path) in [
        (Method::GET, internal_activity::PATH),
        (Method::POST, internal_activity::SOURCE_STOP_PATH),
    ] {
        let unsigned = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .expect("unsigned Activity request"),
            )
            .await
            .expect("full router authentication refusal");
        assert_eq!(unsigned.status(), StatusCode::UNAUTHORIZED);
        assert!(!unsigned
            .headers()
            .contains_key(peer_transport::RESPONSE_SIGNATURE_HEADER));
    }
    let (status, body) = signed_operation(
        app,
        signer,
        owner,
        Method::GET,
        internal_activity::PATH,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let snapshot: internal_activity::ActivitySnapshot =
        serde_json::from_slice(&body).expect("signed owner snapshot");
    assert_eq!(snapshot.node_id, owner);
    let stop = serde_json::to_vec(&json!({
        "node_id": owner,
        "stop_id": format!("source-s-{}", "a".repeat(64)),
    }))
    .expect("bounded exact Stop body");
    let (status, body) = signed_operation(
        app,
        signer,
        owner,
        Method::POST,
        internal_activity::SOURCE_STOP_PATH,
        stop,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no processed owner was installed"
    );
    assert!(body.is_empty(), "signed Stop acknowledgment has no payload");
}

/// A real Raft learner, including its durable maintenance phase, must reach
/// the authenticated Activity handlers through the production router. The
/// signed 404 proves Stop delivery and identity, not renderer acceptance;
/// Source actor tests separately prove an owned operation's physical Stop.
#[test]
fn committed_learner_and_maintenance_owner_admit_signed_activity_and_stop() {
    crate::sharing_fixture_clock::run_plural_fixture("activity-learner-router", || async {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let observed = crate::sharing_fixture_clock::MeasuredPeers::default();
        let source_root = crate::test_tempdir().expect("owned Activity voter root");
        let learner_root = crate::test_tempdir().expect("owned Activity learner root");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("owned join gateway");
        let mut source_config = member_config(source_root.path());
        source_config.server.bind = listener.local_addr().expect("join gateway address");
        source_config.cluster.join_url = format!("http://{}", source_config.server.bind);
        drop(
            SqliteStore::open(
                &source_root
                    .path()
                    .join(plurx_core::cluster::migration::SQLITE_FILENAME),
            )
            .expect("SQLite predecessor"),
        );
        let source = observed
            .select(&source_config, false)
            .await
            .expect("actual selected voter");
        let signer = source.membership_manager();
        let gateway = Router::new()
            .route("/api/v1/cluster/learner/join/redeem", post(redeem_learner))
            .route(
                "/api/v1/cluster/learner/join/finalize",
                post(finalize_learner),
            )
            .with_state(signer.clone())
            .merge(crate::sharing_fixture_clock::MeasuredPeers::route(
                signer.clone(),
            ));
        let join_task = tokio::spawn(async move {
            axum::serve(listener, gateway)
                .await
                .expect("owned learner join gateway");
        });
        signer
            .activate_learner_protocol()
            .await
            .expect("enable committed learner admission");
        let token = signer
            .issue_learner_token(Duration::from_secs(120))
            .await
            .expect("actual learner join token");
        let token_file = learner_root.path().join("join.token");
        std::fs::write(&token_file, format!("{}\n", token.token)).expect("owned join token file");
        let mut learner_config = member_config(learner_root.path());
        learner_config.server.bind = free_address();
        learner_config.cluster.join_token_file = token_file;
        let learner = observed
            .select(&learner_config, true)
            .await
            .expect("actual committed learner");
        let owner = learner.identity.node_id.clone();
        let metrics = source
            .local_client()
            .expect("actual voter client")
            .metrics_db()
            .await
            .expect("committed member roster");
        assert!(metrics
            .membership_config
            .nodes()
            .any(|(id, _)| *id == token.raft_id));
        assert!(!metrics
            .membership_config
            .voter_ids()
            .any(|id| id == token.raft_id));
        let membership = learner.membership_manager();
        signer
            .heartbeat()
            .await
            .expect("refresh source member liveness");
        membership
            .heartbeat()
            .await
            .expect("publish learner route role");
        let mut state = test_state_with_store(Arc::clone(&learner.store), owner.clone());
        state.replace_membership(membership.clone());
        state.replace_replication(learner.replication_monitor());
        assert_eq!(
            state.cluster.admission_role().await.expect("learner role"),
            LocalServingRole::Learner
        );
        state.catalogue = learner.catalogue_reader();
        let app = router(state);
        admitted_activity_routes(&app, &signer, &owner).await;
        for (method, path) in [
            (Method::POST, internal_activity::PATH),
            (Method::GET, internal_activity::SOURCE_STOP_PATH),
            (
                Method::POST,
                "/api/v1/shared/imports/7/files/8/hls/sessions",
            ),
        ] {
            refused_by_capacity(&app, method, path, "learner_route_ineligible").await;
        }
        source
            .local_client()
            .expect("voter maintenance client")
            .execute(
                "INSERT INTO cluster_node_maintenance (node_id, requested_at, acknowledged_at) VALUES ($1, $2, NULL)",
                hiqlite::params!(owner.as_str(), now_ms()),
            )
            .await
            .expect("actual durable maintenance row");
        assert!(membership
            .reconcile_local_maintenance_commit()
            .await
            .expect("publish maintenance role"));
        admitted_activity_routes(&app, &signer, &owner).await;
        for (method, path) in [
            (Method::POST, internal_activity::PATH),
            (Method::GET, internal_activity::SOURCE_STOP_PATH),
            (
                Method::POST,
                "/api/v1/shared/imports/7/files/8/hls/sessions",
            ),
        ] {
            refused_by_capacity(&app, method, path, "node_maintenance").await;
        }
        learner.shutdown().await.expect("drain owned learner");
        source.shutdown().await.expect("drain owned voter");
        join_task.abort();
        assert!(join_task
            .await
            .expect_err("joined gateway aborted")
            .is_cancelled());
        assert!(observed.drain_all().await.is_empty());
    });
}
