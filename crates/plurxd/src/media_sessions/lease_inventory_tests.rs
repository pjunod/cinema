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

    let idle_watch = selected.applied_view().watch(&["media_sessions"]);
    assert!(
        idle_watch.unchanged(),
        "fixture must exercise a real trusted mark"
    );

    // Idle: no worker, no remembered lease, no seed, no owned row.
    let (routes, authority) = authority_reads_of(lease_tick_inventory(
        &state,
        true,
        now_ms,
        None,
        tokio::time::Instant::now() + LEASE_RENEWAL_DEADLINE,
    ))
    .await;
    assert_eq!(routes.expect("an idle inventory").len(), 0);
    assert_eq!(
        authority,
        Vec::new(),
        "an idle tick must not read the authority"
    );

    // A quiet table eliminates even the repeat local hint SQL. Other table
    // writes are irrelevant; a session row below invalidates this same watch.
    let counts = plurx_core::store::HttpStoreOperationCounts::default();
    let quiet = plurx_core::store::scope_http_store_operations(
        counts.clone(),
        lease_tick_inventory(
            &state,
            true,
            now_ms,
            Some(&idle_watch),
            tokio::time::Instant::now() + LEASE_RENEWAL_DEADLINE,
        ),
    )
    .await
    .expect("quiet empty inventory");
    assert!(quiet.is_empty());
    assert!(counts
        .method_snapshot()
        .into_iter()
        .all(
            |(method, _)| method != "owned_media_sessions" && method != "owned_media_sessions_hint"
        ));

    // A lease the loop remembers always takes the inventory: loss of a
    // remembered generation is only seen there.
    let (routes, authority) = authority_reads_of(lease_tick_inventory(
        &state,
        false,
        now_ms,
        None,
        tokio::time::Instant::now() + LEASE_RENEWAL_DEADLINE,
    ))
    .await;
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
    assert!(!idle_watch.unchanged(), "the new session invalidates quiet");
    let (routes, authority) = authority_reads_of(lease_tick_inventory(
        &state,
        true,
        now_ms,
        Some(&idle_watch),
        tokio::time::Instant::now() + LEASE_RENEWAL_DEADLINE,
    ))
    .await;
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

fn publication_phase_fixture(count: usize) -> Vec<OwnedMediaSessionLease> {
    (0..count)
        .map(|id| OwnedMediaSessionLease {
            principal: PlaybackPrincipal::LocalUser { user_id: 7 },
            incarnation_id: format!("phase-incarnation-{id}"),
            session_id: format!("phase-session-{id}"),
            owner_epoch: 1,
            lease_expires_at_ms: 13_000,
            drain_deadline_ms: None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn stalled_publication_batches_cannot_spend_another_cycle_or_starve_renewal_decisions() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let started = tokio::time::Instant::now();
    let cycle_deadline = started + LEASE_RENEWAL_DEADLINE;
    // Inventory and renewal are deliberately nonzero work. Optional work
    // must spend only the remainder, rather than another four seconds.
    tokio::time::sleep(Duration::from_secs(1)).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let renewed_at = tokio::time::Instant::now();
    let entered = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    run_publication_reconciliation_phase(
        publication_phase_fixture(LEASE_RENEWAL_FANOUT * 2 + 1),
        cycle_deadline,
        {
            let entered = Arc::clone(&entered);
            let completed = Arc::clone(&completed);
            move |_| {
                entered.fetch_add(1, Ordering::SeqCst);
                let completed = Arc::clone(&completed);
                async move {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    completed.fetch_add(1, Ordering::SeqCst);
                }
            }
        },
    )
    .await;
    assert!(renewed_at < cycle_deadline);
    assert_eq!(tokio::time::Instant::now(), cycle_deadline);
    assert_eq!(entered.load(Ordering::SeqCst), LEASE_RENEWAL_FANOUT);
    assert_eq!(completed.load(Ordering::SeqCst), 0);
    // The next cycle can decide immediately. A surviving fixed lease still
    // has its original runway; an expired one cannot obtain another renewal.
    let next_cycle = tokio::time::Instant::now() + LEASE_RENEWAL_DEADLINE;
    assert!(renewal_has_full_runway(13_000, 5_000, next_cycle));
    assert!(!renewal_has_full_runway(4_000, 5_000, next_cycle));
    assert!(!renewal_has_full_runway(13_000, 5_000, cycle_deadline));
}

#[tokio::test(start_paused = true)]
async fn an_exhausted_publication_phase_constructs_no_new_store_operations() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let entered = Arc::new(AtomicUsize::new(0));
    run_publication_reconciliation_phase(
        publication_phase_fixture(LEASE_RENEWAL_FANOUT + 1),
        tokio::time::Instant::now(),
        {
            let entered = Arc::clone(&entered);
            move |_| {
                entered.fetch_add(1, Ordering::SeqCst);
                std::future::ready(())
            }
        },
    )
    .await;
    assert_eq!(entered.load(Ordering::SeqCst), 0);
}

#[test]
fn renewal_and_publication_work_share_the_existing_cycle_deadline_and_eligibility() {
    let source = include_str!("../media_sessions.rs");
    let loop_body = source
        .split_once("pub(crate) async fn lease_loop(state: AppState) {")
        .expect("the actual lease owner")
        .1
        .split_once("const TAKEOVER_SETTINGS_REFRESH:")
        .expect("lease owner end")
        .0;
    let capture = loop_body
        .find("let cycle_deadline =")
        .expect("one captured cycle deadline");
    let inventory = loop_body
        .find("let routes = lease_tick_inventory(")
        .expect("owner inventory");
    let renewal = loop_body
        .find("let renewal_deadline = cycle_deadline;")
        .expect("renewal consumes remaining cycle");
    let outcomes = loop_body
        .find("in renewal_outcomes {")
        .expect("renewal result and fail-closed fencing");
    let publication = loop_body
        .find("reconcile_owned_publication_fences(")
        .expect("optional publication phase");
    let publication_setup = loop_body
        .find("let publication_candidates =")
        .expect("exclude newly fenced owners");
    let fenced_publication = &loop_body[publication_setup..];
    let fence = fenced_publication
        .find("fence_and_reap_sessions(&state, cleanup, cycle_deadline).await;")
        .expect("fence before optional work on the same cycle");
    let optional = fenced_publication
        .find("reconcile_owned_publication_fences(")
        .expect("optional phase after fencing");
    assert!(
        capture < inventory && inventory < renewal && renewal < outcomes && outcomes < publication
    );
    assert!(fence < optional);
    assert!(loop_body.contains("publication_reconciliation_candidates(&routes, &live, &cleanup)"));
    assert!(loop_body.contains("renewal_has_full_runway("));
    assert!(!loop_body.contains("let renewal_deadline = tokio::time::Instant::now()"));
    for backend in [
        include_str!("../../../plurx-core/src/store/hiqlite_sessions.rs"),
        include_str!("../../../plurx-core/src/store/sqlite/sessions.rs"),
    ] {
        assert!(backend.contains("request.state = 'starting'"));
        assert!(backend.contains("staged.staged_incarnation_id = media_sessions.incarnation_id"));
    }
}

#[test]
fn publication_reconciliation_excludes_only_the_exact_newly_fenced_owner() {
    let old = publication_phase_fixture(1).remove(0);
    let mut successor = old.clone();
    successor.owner_epoch += 1;
    let live = HashSet::from([old.session_id.clone()]);
    let candidates = publication_reconciliation_candidates(
        &[old.clone(), successor.clone()],
        &live,
        &[(
            old.incarnation_id,
            old.session_id,
            old.owner_epoch,
            false,
            "ambiguous renewal",
        )],
    );
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].owner_epoch, successor.owner_epoch);
}

#[test]
fn a_late_renewal_receipt_keeps_its_original_expiry_and_fences_after_it() {
    let dispatched = 1_000;
    let fixed_expiry = dispatched + LEASE_TTL_MS;
    assert!(renewal_receipt_is_live(dispatched, dispatched + 4_000));
    assert!(renewal_receipt_is_live(dispatched, fixed_expiry - 1));
    assert!(!renewal_receipt_is_live(dispatched, fixed_expiry));
    assert!(!renewal_receipt_is_live(dispatched, fixed_expiry + 1));
}
