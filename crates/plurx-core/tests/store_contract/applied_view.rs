//! Playback-start E1a: the node-local applied view on both backends.
//!
//! The replicated cases run against an embedded singleton in this process —
//! the three-voter contract cluster is reached through a remote client, which
//! has no applied mark by design — with the production node defaults, so the
//! registered tables are the ones the daemon registers. The serving proof is
//! the deterministic validation proof the catalogue contracts use; the mark,
//! the replica and the Authority path are real.
use super::*;
#[cfg(feature = "cluster-read-cost-validation")]
use plurx_core::cluster::migration::status::PassiveRaftMetrics;
use plurx_core::store::{AppliedTable, AppliedView, ViewFallback, ViewSource};
#[cfg(feature = "cluster-read-cost-validation")]
use plurx_core::store::{SettingsStore, StampCheck};

#[tokio::test]
async fn applied_view_on_sqlite_forwards_to_the_only_copy() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    store
        .put_settings(&[("e1.left", "L"), ("e1.right", "R")])
        .await
        .expect("seed settings");
    let view = AppliedView::authority(Arc::clone(&store));
    let read = view
        .settings(&["e1.left", "e1.right", "e1.absent"])
        .await
        .expect("standalone settings");
    assert_eq!(read.source, ViewSource::View);
    assert_eq!(
        read.values,
        store
            .get_settings(&["e1.left", "e1.right"])
            .await
            .expect("authority settings")
    );
    assert!(
        read.stamp.is_none(),
        "SQLite hands out no stamp: there is no mark"
    );
    assert_eq!(
        view.setting("e1.left").await.expect("view read").as_deref(),
        Some("L")
    );
    assert!(view.switch("e1.absent", true).await.expect("view read"));
    // The store is the only copy: every floor is met, even one in the future.
    let fact = view.note_fact(AppliedTable::Settings);
    assert!(view.floor_met(fact));
    assert_eq!(
        view.settings(&["e1.left"]).await.expect("view read").source,
        ViewSource::View
    );
    let observation = view.observation();
    assert_eq!(observation.mode, "standalone");
    assert!(observation.proof_present);
    assert_eq!(observation.total(ViewSource::Authority), 0);
    assert_eq!(observation.total(ViewSource::View), 4);

    let remote = AppliedView::remote_authority(Arc::clone(&store));
    let read = remote.settings(&["e1.left"]).await.expect("view read");
    assert_eq!(read.source, ViewSource::Authority);
    assert!(!remote.floor_met(remote.note_fact(AppliedTable::Settings)));
    assert_eq!(
        remote.observation().last_fallback,
        Some(ViewFallback::NoProof)
    );
}

#[cfg(feature = "cluster-read-cost-validation")]
struct EmbeddedSingleton {
    _root: tempfile::TempDir,
    client: Client,
    store: Arc<HiqliteAuthStore>,
}

#[cfg(feature = "cluster-read-cost-validation")]
impl EmbeddedSingleton {
    async fn start() -> Self {
        install_contract_crypto_provider();
        let root = tempfile::tempdir().expect("applied-view singleton root");
        let ports = contract_free_ports(2);
        let client = Box::pin(hiqlite::start_node(NodeConfig {
            node_id: 1,
            nodes: vec![Node {
                id: 1,
                addr_raft: format!("127.0.0.1:{}", ports[0]),
                addr_api: format!("127.0.0.1:{}", ports[1]),
            }],
            listen_addr_api: Cow::Borrowed("127.0.0.1"),
            listen_addr_raft: Cow::Borrowed("127.0.0.1"),
            data_dir: Cow::Owned(root.path().join("node").to_string_lossy().into_owned()),
            filename_db: Cow::Borrowed("applied-view.db"),
            secret_api: CONTRACT_API_SECRET.to_owned(),
            secret_raft: CONTRACT_RAFT_SECRET.to_owned(),
            tls_api: None,
            tls_raft: None,
            ..plurx_core::cluster::migration::production_hiqlite_defaults_with_read_pool(2)
        }))
        .await
        .expect("start embedded applied-view singleton");
        let store = Box::pin(HiqliteAuthStore::bootstrap(
            client.clone(),
            CONTRACT_INSTANCE_ID,
            &root.path().join("telemetry.db"),
        ))
        .await
        .expect("bootstrap embedded applied-view store");
        Self {
            _root: root,
            client,
            store: Arc::new(store),
        }
    }

    fn applied_index(&self) -> u64 {
        self.client
            .local_db_applied_mark()
            .expect("embedded client owns the mark")
            .applied_index()
    }

    fn view(&self, metrics: PassiveRaftMetrics) -> AppliedView {
        let authority: Arc<dyn Store> = self.store.clone();
        AppliedView::validation_replicated(
            authority,
            Arc::clone(&self.store),
            metrics,
            &self.client,
        )
    }
}

/// With a proof, N reads cost zero consistent calls and match Authority; with
/// the proof revoked while the local statement runs, exactly one.
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test]
async fn applied_view_matches_authority_and_falls_back_exactly_once() {
    let node = EmbeddedSingleton::start().await;
    node.store
        .put_settings(&[("e1.left", "L"), ("e1.right", "R")])
        .await
        .expect("seed settings");
    let keys = ["e1.left", "e1.right", "e1.absent"];
    let expected = node.store.get_settings(&keys).await.expect("Authority");
    assert_eq!(
        expected.len(),
        2,
        "parity must exercise present and absent keys"
    );

    let metrics = PassiveRaftMetrics::validation_bounded_ready_applied(node.applied_index());
    let view = node.view(metrics.clone());
    assert_eq!(view.observation().mode, "replicated");
    metrics.validation_confirm_now(node.applied_index());
    node.store.validation_reset_operation_counts();
    let mut digests = BTreeSet::new();
    for _ in 0..8 {
        let read = view.settings(&keys).await.expect("view settings");
        assert_eq!(read.source, ViewSource::View);
        assert_eq!(read.values, expected);
        digests.insert(read.digest.to_hex());
    }
    let counts = node.store.validation_operation_counts();
    assert_eq!(
        counts.consistent_query_calls, 0,
        "a proven view never asks the leader"
    );
    assert!(
        counts.non_consistent_query_calls >= 8,
        "the view read the replica"
    );
    assert_eq!(digests.len(), 1);

    view.validation_revoke_after_next_local();
    node.store.validation_reset_operation_counts();
    let read = view.settings(&keys).await.expect("revoked read");
    assert_eq!(read.source, ViewSource::Authority);
    assert_eq!(read.values, expected);
    assert_eq!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls,
        1
    );
    assert_eq!(
        view.observation().last_fallback,
        Some(ViewFallback::RevokedDuringRead)
    );
    assert!(
        digests.contains(&read.digest.to_hex()),
        "Authority and view digest the same committed values identically"
    );

    // The revoked proof stays revoked: the next read is Authority for that.
    node.store.validation_reset_operation_counts();
    assert_eq!(
        view.settings(&keys).await.expect("view read").source,
        ViewSource::Authority
    );
    assert_eq!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls,
        1
    );
    assert_eq!(
        view.observation().last_fallback,
        Some(ViewFallback::NoProof)
    );
    let observation = view.observation();
    assert_eq!(observation.total(ViewSource::View), 8);
    assert_eq!(observation.total(ViewSource::Authority), 2);
}

/// The floor: a fact this process learned at `t` closes the view for its
/// table until a confirmation started after `t` is applied here — even while
/// a valid proof says the node is caught up. This is the shape of the review's
/// B1 sequence: the proof is not enough when the replica is behind a fact.
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test]
async fn applied_view_floor_holds_authority_until_the_replica_contains_the_fact() {
    let node = EmbeddedSingleton::start().await;
    let metrics = PassiveRaftMetrics::validation_bounded_ready_applied(node.applied_index());
    let view = node.view(metrics.clone());
    metrics.validation_confirm_now(node.applied_index());

    node.store
        .put_setting("e1.saved", "first")
        .await
        .expect("save");
    view.note_fact(AppliedTable::Settings);
    node.store.validation_reset_operation_counts();
    assert_eq!(
        view.setting("e1.saved")
            .await
            .expect("view read")
            .as_deref(),
        Some("first")
    );
    assert_eq!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls,
        1
    );
    assert_eq!(view.observation().last_fallback, Some(ViewFallback::Floor));

    // A confirmation started after the save, at an index this replica has
    // applied: the view reopens and answers the saved value locally.
    metrics.validation_confirm_now(node.applied_index());
    node.store.validation_reset_operation_counts();
    let read = view.settings(&["e1.saved"]).await.expect("view read");
    assert_eq!(read.source, ViewSource::View);
    assert_eq!(
        read.values.get("e1.saved").map(String::as_str),
        Some("first")
    );
    assert_eq!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls,
        0
    );

    // A later fact, and a confirmation above anything this replica applied:
    // the proof is valid, the floor is not met, Authority answers.
    node.store
        .put_setting("e1.saved", "second")
        .await
        .expect("save again");
    let second = view.note_fact(AppliedTable::Settings);
    metrics.validation_confirm_now(node.applied_index() + 1_000);
    node.store.validation_reset_operation_counts();
    assert_eq!(
        view.setting("e1.saved")
            .await
            .expect("view read")
            .as_deref(),
        Some("second")
    );
    assert_eq!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls,
        1
    );
    assert_eq!(view.observation().last_fallback, Some(ViewFallback::Floor));
    assert!(!view.floor_met(second));
    // The stamp check agrees: the view is closed for the settings table.
    assert_eq!(
        view.check(&read.stamp.expect("a view answer is stamped")),
        StampCheck::Closed(ViewFallback::Floor)
    );
}

/// A stamp stays current until its own table moves; a write to another table
/// does not touch it, and a write to the table makes it stale with no clock.
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test]
async fn applied_view_stamp_moves_with_its_own_table_only() {
    let node = EmbeddedSingleton::start().await;
    node.store.put_setting("e1.k", "v1").await.expect("seed");
    let metrics = PassiveRaftMetrics::validation_bounded_ready_applied(node.applied_index());
    let view = node.view(metrics.clone());
    metrics.validation_confirm_now(node.applied_index());
    let stamp = view
        .settings(&["e1.k"])
        .await
        .expect("view read")
        .stamp
        .expect("a view answer is stamped");
    assert_eq!(view.check(&stamp), StampCheck::Current);
    let before = view.observation();

    node.store
        .create_library(&NewLibrary {
            name: "E1 unrelated write".into(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/e1/unrelated")],
            anime: false,
        })
        .await
        .expect("unrelated write");
    metrics.validation_confirm_now(node.applied_index());
    assert_eq!(
        view.check(&stamp),
        StampCheck::Current,
        "another table moved, not this one"
    );

    node.store
        .put_setting("e1.k", "v2")
        .await
        .expect("settings write");
    metrics.validation_confirm_now(node.applied_index());
    assert_eq!(view.check(&stamp), StampCheck::Stale);
    let after = view.observation();
    let revision = |observation: &plurx_core::store::AppliedViewObservation, label: &str| {
        observation
            .revisions
            .iter()
            .find(|(view, _)| *view == label)
            .map(|(_, revision)| *revision)
            .expect("registered view table")
    };
    assert!(revision(&after, "settings") > revision(&before, "settings"));
    assert_eq!(
        revision(&after, "route"),
        revision(&before, "route"),
        "a settings write leaves the route slot alone"
    );
    assert_eq!(after.generation_stable(), Some(true));
    assert_eq!(
        view.setting("e1.k").await.expect("view read").as_deref(),
        Some("v2")
    );
}

/// A replica that is really behind its own commit: the singleton's writer
/// holds a committed settings entry before it applies (the instance-local
/// precommit hold). While held, the mark and the view still show the applied
/// image (stamp current, old value, no consistent read: the bounded staleness
/// of §4.2 "Bound"); the moment the entry applies the stamp is stale, with no
/// clock advance, and the view answers the new value.
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test]
async fn applied_view_stamp_goes_stale_the_moment_a_held_entry_applies() {
    const HELD: &str = "UPDATE settings SET value = 'v2' WHERE key = 'e1.held'";
    let node = EmbeddedSingleton::start().await;
    node.store.put_setting("e1.held", "v1").await.expect("seed");
    let applied = node.applied_index();
    let metrics = PassiveRaftMetrics::validation_bounded_ready_applied(applied);
    let view = node.view(metrics.clone());
    metrics.validation_confirm_now(applied);
    let stamp = view
        .settings(&["e1.held"])
        .await
        .expect("view read")
        .stamp
        .expect("a view answer is stamped");

    let (entered, release) = node
        .client
        .validation_hold_transaction_before_commit(
            HELD.to_owned(),
            std::time::Instant::now() + Duration::from_secs(30),
        )
        .await
        .expect("register the precommit hold");
    let client = node.client.clone();
    let write = tokio::spawn(async move { client.txn([(HELD, hiqlite::params!())]).await });
    entered.await.expect("the writer holds the committed entry");

    assert_eq!(node.applied_index(), applied, "held: not applied here yet");
    assert_eq!(view.check(&stamp), StampCheck::Current);
    node.store.validation_reset_operation_counts();
    assert_eq!(
        view.setting("e1.held").await.expect("view read").as_deref(),
        Some("v1"),
        "the view serves what this replica has applied"
    );
    assert_eq!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls,
        0
    );

    release.send(()).expect("release the writer");
    write
        .await
        .expect("join the held write")
        .expect("held write")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("commit the held write");
    assert!(node.applied_index() > applied);
    assert_eq!(
        view.check(&stamp),
        StampCheck::Stale,
        "stale the moment the entry applies, with no clock advance"
    );
    metrics.validation_confirm_now(node.applied_index());
    let read = view.settings(&["e1.held"]).await.expect("view read");
    assert_eq!(read.source, ViewSource::View);
    assert_eq!(read.values.get("e1.held").map(String::as_str), Some("v2"));
}
