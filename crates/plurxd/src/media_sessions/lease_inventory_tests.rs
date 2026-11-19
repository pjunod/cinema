//! A4: the lease loop's owner inventory on an idle node.
//!
//! On the replicated store the inventory is a linearizable read. These run
//! the tick's inventory step against an embedded singleton voter and count
//! the authority reads it issues.

use super::*;
use crate::state::{AppState, Dirs};
use plurx_core::{
    config::Config,
    domain::{MediaSessionActivation, MediaSessionActivationSettlement},
    playback_principal::PlaybackPrincipal,
};

/// Authority reads `future` issued, by the Store method that issued them.
async fn authority_reads_of<T>(
    future: impl std::future::Future<Output = T>,
) -> (T, Vec<(&'static str, u64)>) {
    let counts = plurx_core::store::HttpStoreOperationCounts::default();
    let output = plurx_core::store::scope_http_store_operations(counts.clone(), future).await;
    let authority = counts
        .method_snapshot()
        .into_iter()
        .filter(|(_, [_, authority, _])| *authority > 0)
        .map(|(method, [_, authority, _])| (method, authority))
        .collect();
    (output, authority)
}

fn singleton_config(directory: &std::path::Path) -> Config {
    let mut config = Config::default();
    config.storage.data_dir = directory.join("database");
    let raft = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("Raft port");
    let api = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("API port");
    config.cluster.raft_bind = raft.local_addr().expect("Raft address");
    config.cluster.api_bind = api.local_addr().expect("API address");
    config.cluster.advertise_host = "localhost".to_owned();
    drop((raft, api));
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_lease_tick_takes_no_inventory_and_an_owned_row_still_does() {
    let root = crate::test_tempdir().expect("lease inventory fixture");
    let selected =
        crate::sharing_fixture_clock::select_applied_singleton(&singleton_config(root.path()))
            .await
            .expect("embedded singleton voter");
    let dirs = Dirs {
        artwork: root.path().join("artwork"),
        transcode: root.path().join("transcode"),
        cache: root.path().join("cache"),
        subs: root.path().join("subs"),
        runtime_cache: root.path().join("runtime"),
        renditions: root.path().join("renditions"),
    };
    let state = AppState::new_unhooked(
        "lease-inventory-test".into(),
        Arc::clone(&selected.store),
        dirs,
        "lease-node".into(),
        Default::default(),
        Default::default(),
        Arc::new(crate::logbuf::LogBuffer::new(16)),
    );
    let now_ms = unix_ms();

    // Idle: no worker, no remembered lease, no seed, no owned row.
    let (routes, authority) = authority_reads_of(lease_tick_inventory(&state, true, now_ms)).await;
    assert_eq!(routes.expect("an idle inventory").len(), 0);
    assert_eq!(
        authority,
        Vec::new(),
        "an idle tick must not read the authority"
    );

    // A lease the loop remembers always takes the inventory: loss of a
    // remembered generation is only seen there.
    let (routes, authority) = authority_reads_of(lease_tick_inventory(&state, false, now_ms)).await;
    assert_eq!(routes.expect("a remembered lease's inventory").len(), 0);
    assert!(
        authority
            .iter()
            .any(|(method, _)| *method == "owned_media_sessions"),
        "a remembered lease takes today's read: {authority:?}"
    );

    // A row this replica holds for this node — one an earlier process left,
    // or a takeover — is inventoried, so it is still renewed or settled.
    let user = selected
        .store
        .create_user("lease-viewer", "hash", false)
        .await
        .expect("viewer");
    let activation = MediaSessionActivation {
        incarnation_id: uuid::Uuid::new_v4().to_string(),
        session_id: uuid::Uuid::new_v4().to_string(),
        principal: PlaybackPrincipal::LocalUser { user_id: user.id },
        playback_id: uuid::Uuid::new_v4().to_string(),
        recovery_epoch: String::new(),
        expected_predecessor_incarnation_id: None,
        fence_predecessor: false,
        request_id: None,
        request_fingerprint: "a".repeat(64),
        owner_node_id: "lease-node".into(),
        recipe_json: "{}".into(),
        response_json: "{}".into(),
        publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: 0,
        now_ms,
        lease_expires_at_ms: now_ms + 900_000,
        expected_desired_revision: None,
    };
    selected
        .store
        .activate_media_session(&activation)
        .await
        .expect("activate")
        .expect("activation wins");
    selected
        .store
        .settle_media_session_activation(
            &activation,
            MediaSessionActivationSettlement::Confirm {
                publication_ready_at_ms: 0,
            },
            now_ms,
        )
        .await
        .expect("confirm")
        .expect("confirmed route");
    let (routes, authority) = authority_reads_of(lease_tick_inventory(&state, true, now_ms)).await;
    let routes = routes.expect("an owned row's inventory");
    assert!(
        routes
            .iter()
            .any(|route| route.incarnation_id == activation.incarnation_id),
        "the owned row is inventoried"
    );
    assert!(
        authority
            .iter()
            .any(|(method, _)| *method == "owned_media_sessions"),
        "an owned row takes today's read: {authority:?}"
    );

    drop(state);
    selected.shutdown().await.expect("stop the voter");
}
