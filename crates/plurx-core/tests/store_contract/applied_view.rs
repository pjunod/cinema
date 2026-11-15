//! Playback-start E1a: the node-local applied view on both backends.
//!
//! The replicated cases run against an embedded singleton in this process —
//! the three-voter contract cluster is reached through a remote client, which
//! has no applied mark by design — with the production node defaults, so the
//! registered tables are the ones the daemon registers. The serving proof is
//! the deterministic validation proof the catalogue contracts use; the mark,
//! the replica and the Authority path are real.
//!
//! E1b adds the settings memo and the writer-raised settings floor: a save
//! through this store closes the view with no `note_fact` from the caller.
//! Its lagging-replica case needs a replica that is really behind an
//! acknowledged save, so it runs an in-process learner (local client, real
//! mark, held apply) beside two child voters.
use super::*;
#[cfg(feature = "cluster-read-cost-validation")]
use plurx_core::cluster::migration::status::PassiveRaftMetrics;
use plurx_core::store::{
    AppliedTable, AppliedView, PlaybackPlanningSnapshot, ViewFallback, ViewSource,
};
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
        "with no memo loaded, each keyed read reads its keys from the replica"
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

/// The saving node's floor (E1b, §4.3), on the embedded singleton: a save
/// through this store closes the settings view by itself — no caller notes
/// the fact — until a watermark confirmation that started after it has been
/// applied here, and every settings writer arms it. A singleton has always
/// applied what it acknowledged, so this proves the source, not that a stale
/// value is refused: `applied_view_follower_save_never_reads_its_stale_replica`
/// proves that on a learner whose apply is held.
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test]
async fn applied_view_settings_save_closes_the_view_until_a_later_confirmation() {
    let node = EmbeddedSingleton::start().await;
    node.store
        .put_setting("e1b.saved", "before")
        .await
        .expect("seed");
    let metrics = PassiveRaftMetrics::validation_bounded_ready_applied(node.applied_index());
    let view = node.view(metrics.clone());
    metrics.validation_confirm_now(node.applied_index());
    assert_eq!(
        view.settings(&["e1b.saved"])
            .await
            .expect("view read")
            .source,
        ViewSource::View
    );

    // The save: no `note_fact` here — the store raises the floor itself.
    node.store
        .put_setting("e1b.saved", "after")
        .await
        .expect("save");
    node.store.validation_reset_operation_counts();
    let read = view.settings(&["e1b.saved"]).await.expect("read back");
    assert_eq!(
        read.values.get("e1b.saved").map(String::as_str),
        Some("after")
    );
    assert_eq!(
        read.source,
        ViewSource::Authority,
        "the saving node reads its own save from Authority while the floor is unmet"
    );
    assert_eq!(view.observation().last_fallback, Some(ViewFallback::Floor));
    assert_eq!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls,
        1
    );

    // The next confirmation this replica has applied reopens the view.
    metrics.validation_confirm_now(node.applied_index());
    let read = view.settings(&["e1b.saved"]).await.expect("view read");
    assert_eq!(read.source, ViewSource::View);
    assert_eq!(
        read.values.get("e1b.saved").map(String::as_str),
        Some("after")
    );

    // Every settings writer arms it, not only `put_setting`.
    node.store
        .put_settings(&[("e1b.saved", "batch"), ("e1b.other", "o")])
        .await
        .expect("batch save");
    assert_eq!(
        view.settings(&["e1b.saved"])
            .await
            .expect("read back")
            .source,
        ViewSource::Authority
    );
    assert_eq!(
        node.store
            .get_or_init_setting("e1b.seeded", "s")
            .await
            .expect("seed"),
        "s"
    );
    metrics.validation_confirm_now(node.applied_index());
    assert_eq!(
        view.settings(&["e1b.saved"])
            .await
            .expect("view read")
            .source,
        ViewSource::View
    );
    node.store
        .get_or_init_setting("e1b.seeded", "ignored")
        .await
        .expect("seed again");
    assert_eq!(
        view.settings(&["e1b.seeded"])
            .await
            .expect("read back")
            .source,
        ViewSource::Authority,
        "a conditional settings write closes the view whether or not it changed a row"
    );
}

/// The settings memo (E1b, §4.3). A snapshot loads the whole table in one
/// local read; keyed reads and snapshots answer from it while its stamp
/// holds, and another table's writes leave it alone. After a settings write
/// a keyed read reads its keys locally and does not reload the table (job
/// cursors share it); only a snapshot reloads, one at a time — a snapshot
/// that finds a reload in flight answers from Authority instead of waiting.
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test]
async fn applied_view_settings_memo_reloads_only_for_a_snapshot_and_never_queues() {
    let node = EmbeddedSingleton::start().await;
    node.store
        .put_settings(&[("e1b.a", "1"), ("e1b.b", "2")])
        .await
        .expect("seed");
    let metrics = PassiveRaftMetrics::validation_bounded_ready_applied(node.applied_index());
    let view = node.view(metrics.clone());
    metrics.validation_confirm_now(node.applied_index());

    node.store.validation_reset_operation_counts();
    let snapshot = view.settings_snapshot().await.expect("view snapshot");
    assert_eq!(snapshot.source, ViewSource::View);
    for _ in 0..16 {
        assert_eq!(
            view.setting("e1b.a").await.expect("view read").as_deref(),
            Some("1")
        );
    }
    let counts = node.store.validation_operation_counts();
    assert_eq!(
        counts.non_consistent_query_calls, 1,
        "one snapshot and sixteen keyed reads, one local statement"
    );
    assert_eq!(counts.consistent_query_calls, 0);
    assert_eq!(
        *snapshot.values,
        node.store
            .settings_snapshot()
            .await
            .expect("Authority snapshot"),
        "the memo is the whole table, as Authority reads it"
    );
    assert_eq!(reloads_of(&view), 1);

    node.store
        .create_library(&NewLibrary {
            name: "E1b unrelated write".into(),
            kind: LibraryKind::Movies,
            paths: vec![PathBuf::from("/e1b/unrelated")],
            anime: false,
        })
        .await
        .expect("unrelated write");
    metrics.validation_confirm_now(node.applied_index());
    node.store.validation_reset_operation_counts();
    assert_eq!(
        view.setting("e1b.b").await.expect("view read").as_deref(),
        Some("2")
    );
    assert_eq!(
        node.store
            .validation_operation_counts()
            .non_consistent_query_calls,
        0,
        "another table's write keeps the memo"
    );

    node.store
        .put_setting("e1b.b", "3")
        .await
        .expect("settings write");
    metrics.validation_confirm_now(node.applied_index());
    node.store.validation_reset_operation_counts();
    for _ in 0..4 {
        assert_eq!(
            view.setting("e1b.b").await.expect("view read").as_deref(),
            Some("3")
        );
    }
    assert_eq!(reloads_of(&view), 1, "a keyed read never reloads the table");
    assert_eq!(
        node.store
            .validation_operation_counts()
            .non_consistent_query_calls,
        4,
        "each keyed read of a stale memo reads its own keys"
    );

    node.store.validation_reset_operation_counts();
    let snapshots = (0..16).map(|_| {
        let view = view.clone();
        tokio::spawn(async move { view.settings_snapshot().await })
    });
    let mut authority = 0;
    for read in snapshots.collect::<Vec<_>>() {
        let read = read.await.expect("join").expect("snapshot");
        assert_eq!(read.values.get("e1b.b").map(String::as_str), Some("3"));
        if read.source == ViewSource::Authority {
            authority += 1;
        }
    }
    assert_eq!(
        reloads_of(&view),
        2,
        "one reload for sixteen concurrent snapshots"
    );
    let counts = node.store.validation_operation_counts();
    assert_eq!(
        counts.consistent_query_calls, authority,
        "a snapshot that met the reload in flight read Authority, once"
    );
    if authority > 0 {
        assert!(view
            .observation()
            .reads
            .iter()
            .any(|(table, source, reason, count)| *table == "settings"
                && *source == "authority"
                && *reason == "reload_in_flight"
                && *count == authority));
    }
}

/// No convoy (§4.2 "no wait", review S4): while one snapshot's memo reload
/// is held in flight, another snapshot answers at once from Authority with
/// reason `reload_in_flight`, and a keyed read reads its keys locally; the
/// held reload then completes and fills the memo.
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applied_view_snapshot_never_waits_on_another_readers_reload() {
    let node = EmbeddedSingleton::start().await;
    node.store.put_setting("e1b.c", "1").await.expect("seed");
    let metrics = PassiveRaftMetrics::validation_bounded_ready_applied(node.applied_index());
    let view = node.view(metrics.clone());
    metrics.validation_confirm_now(node.applied_index());

    let (held, release) = view.validation_pause_next_reload();
    let first = {
        let view = view.clone();
        tokio::spawn(async move { view.settings_snapshot().await })
    };
    held.await.expect("the first snapshot holds its reload");
    node.store.validation_reset_operation_counts();
    let second = tokio::time::timeout(Duration::from_secs(5), view.settings_snapshot())
        .await
        .expect("a second snapshot does not wait on the held reload")
        .expect("second snapshot");
    assert_eq!(second.source, ViewSource::Authority);
    assert_eq!(second.values.get("e1b.c").map(String::as_str), Some("1"));
    assert_eq!(
        view.observation().last_fallback,
        Some(ViewFallback::ReloadInFlight)
    );
    let keyed = tokio::time::timeout(Duration::from_secs(5), view.settings(&["e1b.c"]))
        .await
        .expect("a keyed read does not wait either")
        .expect("keyed read");
    assert_eq!(keyed.source, ViewSource::View);
    assert_eq!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls,
        1,
        "only the second snapshot asked the leader"
    );

    release.send(()).expect("release the held reload");
    let first = first.await.expect("join").expect("first snapshot");
    assert_eq!(first.source, ViewSource::View);
    assert_eq!(reloads_of(&view), 1);
    assert_eq!(
        view.settings_snapshot().await.expect("memo hit").source,
        ViewSource::View
    );
    assert_eq!(reloads_of(&view), 1);
}

#[cfg(feature = "cluster-read-cost-validation")]
fn reloads_of(view: &AppliedView) -> u64 {
    view.observation()
        .reloads
        .iter()
        .find(|(label, _)| *label == "settings")
        .map(|(_, count)| *count)
        .expect("the settings view counts reloads")
}

/// A learner in this process — local client, real applied mark, production
/// registered tables — beside two child voters that hold the quorum.
#[cfg(feature = "cluster-read-cost-validation")]
struct LaggingLearner {
    client: Client,
    store: Arc<HiqliteAuthStore>,
    _cluster: ContractCluster,
}

#[cfg(feature = "cluster-read-cost-validation")]
impl LaggingLearner {
    async fn start() -> Self {
        const ATTEMPTS: usize = 5;
        install_contract_crypto_provider();
        for attempt in 1..=ATTEMPTS {
            let (cluster, specs) = match ContractCluster::try_start_sized(2, 1).await {
                Ok(started) => started,
                Err(ContractStartError::PortCollision) if attempt < ATTEMPTS => continue,
                Err(ContractStartError::PortCollision) => {
                    panic!("lagging-learner voters exhausted {ATTEMPTS} port attempts")
                }
                Err(ContractStartError::Failed(error)) => {
                    panic!("lagging-learner voters failed to start: {error}")
                }
            };
            // The plurx schema goes in through the voters, as production's
            // voter installs it; the learner then opens the replicated store.
            drop(open_contract_hiqlite_store(&cluster).await);
            let learner = specs.last().expect("the learner spec").clone();
            let data_dir = cluster._root.path().join(format!("node-{}", learner.id));
            std::fs::create_dir_all(&data_dir).expect("learner data directory");
            let started = hiqlite::start_node(NodeConfig {
                node_id: learner.id,
                learner_only: true,
                nodes: specs
                    .iter()
                    .map(|node| Node {
                        id: node.id,
                        addr_raft: node.raft.clone(),
                        addr_api: node.api.clone(),
                    })
                    .collect(),
                listen_addr_api: Cow::Borrowed("127.0.0.1"),
                listen_addr_raft: Cow::Borrowed("127.0.0.1"),
                data_dir: Cow::Owned(data_dir.to_string_lossy().into_owned()),
                filename_db: Cow::Borrowed("contract.db"),
                secret_raft: CONTRACT_RAFT_SECRET.to_owned(),
                secret_api: CONTRACT_API_SECRET.to_owned(),
                tls_raft: Some(ServerTlsConfig::TlsAutoCertificates),
                tls_api: Some(ServerTlsConfig::TlsAutoCertificates),
                health_check_delay_secs: 0,
                wal_size: HIQLITE_WAL_SIZE_BYTES,
                raft_config: NodeConfig::default_raft_config(10_000),
                ..plurx_core::cluster::migration::production_hiqlite_defaults_with_read_pool(2)
            })
            .await;
            let client = match started {
                Ok(client) => client,
                Err(error)
                    if attempt < ATTEMPTS
                        && error.to_string().to_ascii_lowercase().contains("in use") =>
                {
                    continue
                }
                Err(error) => panic!("start the in-process learner: {error}"),
            };
            tokio::time::timeout(Duration::from_secs(60), client.wait_until_healthy_db())
                .await
                .expect("the learner joins within a minute");
            let store = HiqliteAuthStore::open(
                client.clone(),
                &cluster._root.path().join("learner-telemetry.db"),
            )
            .await
            .expect("open the replicated store on the learner");
            return Self {
                client,
                store: Arc::new(store),
                _cluster: cluster,
            };
        }
        unreachable!("the start loop returns or panics")
    }

    fn mark(&self) -> hiqlite::AppliedMarkHandle {
        self.client
            .local_db_applied_mark()
            .expect("the learner's client is local")
    }

    /// The learner's own replica, read directly: what a lagging view would see.
    async fn replica_value(&self, key: &str) -> Option<String> {
        self.client
            .query_map::<SettingValue, _>(
                "SELECT value FROM settings WHERE key = $1",
                hiqlite::params!(key.to_owned()),
            )
            .await
            .expect("local read of the learner's replica")
            .into_iter()
            .next()
            .map(|row| row.value)
    }

    async fn await_replica(&self, key: &str, value: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while self.replica_value(key).await.as_deref() != Some(value) {
            assert!(
                std::time::Instant::now() < deadline,
                "the learner applies {key}={value} within 30 s"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[cfg(feature = "cluster-read-cost-validation")]
struct SettingValue {
    value: String,
}

#[cfg(feature = "cluster-read-cost-validation")]
impl From<&mut hiqlite::Row<'_>> for SettingValue {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self {
            value: row.get("value"),
        }
    }
}

/// Read-your-write through a lagging replica (contract §8, the review's B1
/// shape for settings). A learner saves through its own store while its own
/// apply of that save is held, so its replica really is older than a write
/// it has had acknowledged — and the serving proof, confirmed at an index the
/// learner has not applied, is valid. Its applied view must answer the saved
/// value (from Authority), never the old one its replica still holds; once
/// the entry applies and a confirmation covers it, the view answers locally.
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applied_view_follower_save_never_reads_its_stale_replica() {
    const KEY: &str = "e1b.lagging";
    // put_settings' statement, which the learner's writer holds.
    const SAVE: &str = "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, $3) \
                        ON CONFLICT(key) DO UPDATE SET \
                        value = excluded.value, updated_at = excluded.updated_at";
    let _case = HIQLITE_CASE.lock().await;
    let node = LaggingLearner::start().await;
    node.store.put_setting(KEY, "old").await.expect("seed");
    node.await_replica(KEY, "old").await;
    let mark = node.mark();
    let metrics = PassiveRaftMetrics::validation_bounded_ready_applied(mark.applied_index());
    let authority: Arc<dyn Store> = node.store.clone();
    let view = AppliedView::validation_replicated(
        authority,
        Arc::clone(&node.store),
        metrics.clone(),
        &node.client,
    );
    metrics.validation_confirm_now(mark.applied_index());
    let read = view.settings(&[KEY]).await.expect("view read");
    assert_eq!(read.source, ViewSource::View);
    assert_eq!(read.values.get(KEY).map(String::as_str), Some("old"));

    let (entered, release) = node
        .client
        .validation_hold_transaction_before_commit(
            SAVE.to_owned(),
            std::time::Instant::now() + Duration::from_secs(45),
        )
        .await
        .expect("hold the learner's apply of the save");
    node.store
        .put_settings(&[(KEY, "new")])
        .await
        .expect("the voters commit the learner's save");
    tokio::time::timeout(Duration::from_secs(30), entered)
        .await
        .expect("the learner's writer reaches the held save")
        .expect("hold entered");
    assert_eq!(
        node.replica_value(KEY).await.as_deref(),
        Some("old"),
        "the learner's replica is behind its own acknowledged save"
    );
    // A confirmation started after the save, at the save's index at least
    // (the entry the writer holds follows the last one it applied), which
    // this replica has not applied: the proof is valid, the replica is stale.
    metrics.validation_confirm_now(mark.applied_index() + 1);
    node.store.validation_reset_operation_counts();
    let read = view.settings(&[KEY]).await.expect("read back");
    assert_eq!(
        read.values.get(KEY).map(String::as_str),
        Some("new"),
        "the saving node never reads its stale replica"
    );
    assert_eq!(read.source, ViewSource::Authority);
    assert_eq!(view.observation().last_fallback, Some(ViewFallback::Floor));
    assert!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls
            >= 1
    );

    release.send(()).expect("release the learner's writer");
    node.await_replica(KEY, "new").await;
    metrics.validation_confirm_now(mark.applied_index());
    let read = view.settings(&[KEY]).await.expect("view read");
    assert_eq!(read.source, ViewSource::View);
    assert_eq!(read.values.get(KEY).map(String::as_str), Some("new"));
}

// Playback-start E1c: the route view (§4.4).

#[cfg(feature = "cluster-read-cost-validation")]
fn route_activation(user_id: i64, session: &str, incarnation: &str) -> MediaSessionActivation {
    MediaSessionActivation {
        expected_desired_revision: None,
        recovery_epoch: format!("epoch-{session}"),
        incarnation_id: incarnation.to_owned(),
        session_id: session.to_owned(),
        principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id },
        playback_id: format!("playback-{session}"),
        expected_predecessor_incarnation_id: None,
        fence_predecessor: false,
        request_id: None,
        request_fingerprint: "a".repeat(64),
        owner_node_id: "route-owner".to_owned(),
        recipe_json: "{}".to_owned(),
        response_json: "{}".to_owned(),
        publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: 0,
        now_ms: 1_000,
        lease_expires_at_ms: 900_000,
    }
}

/// Apply the frozen principal session rebuild to the replicated tables (rows
/// survive it) and record the layout as installed, so the route view's local
/// twin runs. Production installs it with the coordinated Source installer.
#[cfg(feature = "cluster-read-cost-validation")]
async fn install_principal_layout(client: &Client, store: &HiqliteAuthStore) {
    let statements: Vec<(String, hiqlite::Params)> =
        plurx_core::store::media_session_principal_rebuild_schema()
            .split("-- next statement\n")
            .map(|sql| {
                (
                    sql.trim().trim_end_matches(';').to_owned(),
                    hiqlite::params!(),
                )
            })
            .collect();
    for result in client
        .txn(statements)
        .await
        .expect("apply the principal rebuild")
    {
        result.expect("rebuild statement");
    }
    store.validation_assume_installed_session_layout();
}

/// End a session's row as a release does (`state`, `terminal_reason`).
#[cfg(feature = "cluster-read-cost-validation")]
async fn release(client: &Client, session: &str) {
    let sql = format!(
        "UPDATE media_sessions SET state = 'ended', terminal_reason = 'deleted' \
         WHERE session_id = '{session}'"
    );
    for result in client
        .txn([(sql, hiqlite::params!())])
        .await
        .expect("release")
    {
        assert_eq!(result.expect("release statement"), 1, "one row ended");
    }
}

/// On SQLite the route view is the store: every read is a store read, and no
/// answer carries a stamp a cache could keep.
#[tokio::test]
async fn applied_route_view_on_sqlite_is_the_store() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let view = AppliedView::authority(Arc::clone(&store));
    let absent = view
        .route("00000000-0000-4000-8000-0000000e1c01")
        .await
        .expect("standalone route");
    assert_eq!(absent.source, ViewSource::View);
    assert!(absent.route.is_none());
    assert!(absent.stamp.is_none(), "SQLite hands out no stamp");
    let observation = view.observation();
    assert_eq!(
        observation.view_total(AppliedTable::MediaSessions, ViewSource::View),
        1
    );
    assert_eq!(
        observation.view_total(AppliedTable::Settings, ViewSource::View),
        0
    );
    // Every per-row fact is met on the only copy.
    assert!(view.floor_met(view.observe()));

    let remote = AppliedView::remote_authority(Arc::clone(&store));
    let read = remote
        .route("00000000-0000-4000-8000-0000000e1c01")
        .await
        .expect("remote route");
    assert_eq!(read.source, ViewSource::Authority);
    assert!(!remote.floor_met(remote.observe()));
}

/// A release applied here moves the route slot: the entry made from the
/// earlier view answer is stale the moment it applies, with no clock, and the
/// next read re-reads the replica locally and sees the terminal row. A write
/// to another session moves the same slot (per-table invalidation, §12-7).
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test]
async fn applied_route_view_moves_with_a_release_applied_here() {
    const SESSION: &str = "00000000-0000-4000-8000-0000000e1c11";
    const OTHER: &str = "00000000-0000-4000-8000-0000000e1c12";
    let node = EmbeddedSingleton::start().await;
    let user = node
        .store
        .create_user("e1c-route", "hash", false)
        .await
        .expect("user")
        .id;
    for (session, incarnation) in [
        (SESSION, "00000000-0000-4000-8000-0000000e1c21"),
        (OTHER, "00000000-0000-4000-8000-0000000e1c22"),
    ] {
        node.store
            .activate_media_session(&route_activation(user, session, incarnation))
            .await
            .expect("activate")
            .expect("activation wins");
    }
    install_principal_layout(&node.client, &node.store).await;
    let metrics = PassiveRaftMetrics::validation_bounded_ready_applied(node.applied_index());
    let view = node.view(metrics.clone());
    metrics.validation_confirm_now(node.applied_index());

    node.store.validation_reset_operation_counts();
    let read = view.route(SESSION).await.expect("view route");
    assert_eq!(
        read.source,
        ViewSource::View,
        "{:?}",
        view.observation().last_fallback
    );
    assert_eq!(
        read.route,
        node.store
            .media_session_route(SESSION)
            .await
            .expect("Authority")
    );
    node.store.validation_reset_operation_counts();
    let read = view.route(SESSION).await.expect("view route");
    assert_eq!(read.source, ViewSource::View);
    assert_eq!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls,
        0,
        "a proven route view never asks the leader"
    );
    assert_eq!(
        read.route.as_ref().map(|route| route.state.as_str()),
        Some("active")
    );
    let stamp = read.stamp.expect("a view answer is stamped");
    assert_eq!(view.check(&stamp), StampCheck::Current);
    assert!(view.same_image(&stamp));

    release(&node.client, SESSION).await;
    assert_eq!(
        view.check(&stamp),
        StampCheck::Stale,
        "the release moved the route slot; no clock advanced"
    );
    assert!(!view.same_image(&stamp));
    node.store.validation_reset_operation_counts();
    let read = view.route(SESSION).await.expect("re-read");
    assert_eq!(read.source, ViewSource::View);
    assert_eq!(
        read.route.as_ref().map(|route| route.state.as_str()),
        Some("ended")
    );
    assert_eq!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls,
        0
    );

    let stamp = read.stamp.expect("stamped");
    release(&node.client, OTHER).await;
    assert_eq!(
        view.check(&stamp),
        StampCheck::Stale,
        "per-table: another session's row moves the slot too"
    );
}

/// The legacy session layout — the one a store has until the coordinated
/// Source installation rebuilds `media_sessions`, which this singleton never
/// runs: the route view answers every read from Authority and says why, and
/// an Authority read that re-verifies the layout does not open it. Once the
/// principal layout is installed, the same view answers locally.
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test]
async fn applied_route_view_answers_authority_in_the_legacy_layout() {
    const SESSION: &str = "00000000-0000-4000-8000-0000000e1c31";
    let node = EmbeddedSingleton::start().await;
    let user = node
        .store
        .create_user("e1c-legacy", "hash", false)
        .await
        .expect("user")
        .id;
    node.store
        .activate_media_session(&route_activation(
            user,
            SESSION,
            "00000000-0000-4000-8000-0000000e1c32",
        ))
        .await
        .expect("activate")
        .expect("activation wins");
    let metrics = PassiveRaftMetrics::validation_bounded_ready_applied(node.applied_index());
    let view = node.view(metrics.clone());
    metrics.validation_confirm_now(node.applied_index());

    for _ in 0..3 {
        node.store.validation_reset_operation_counts();
        let read = view.route(SESSION).await.expect("legacy route");
        assert_eq!(read.source, ViewSource::Authority);
        assert!(read.stamp.is_none());
        assert_eq!(
            read.route.as_ref().map(|route| route.session_id.as_str()),
            Some(SESSION)
        );
        assert_eq!(
            view.observation().last_fallback,
            Some(ViewFallback::LegacyLayout)
        );
        assert!(
            node.store
                .validation_operation_counts()
                .consistent_query_calls
                >= 1,
            "the legacy layout is answered by a quorum read"
        );
    }

    install_principal_layout(&node.client, &node.store).await;
    metrics.validation_confirm_now(node.applied_index());
    node.store.validation_reset_operation_counts();
    let read = view.route(SESSION).await.expect("installed route");
    assert_eq!(
        read.source,
        ViewSource::View,
        "{:?}",
        view.observation().last_fallback
    );
    assert_eq!(
        read.route.as_ref().map(|route| route.session_id.as_str()),
        Some(SESSION)
    );
    assert_eq!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls,
        0
    );
}

#[cfg(feature = "cluster-read-cost-validation")]
impl LaggingLearner {
    /// The learner's own replica, read directly.
    async fn replica_route_state(&self, session: &str) -> Option<String> {
        self.client
            .query_map::<RouteState, _>(
                "SELECT state FROM media_sessions WHERE session_id = $1",
                hiqlite::params!(session.to_owned()),
            )
            .await
            .expect("local read of the learner's replica")
            .into_iter()
            .next()
            .map(|row| row.state)
    }

    /// Until the learner has applied the principal rebuild with `session`.
    async fn await_principal_layout(&self, session: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let rebuilt = self
                .client
                .query_map::<RouteState, _>(
                    "SELECT state FROM media_sessions WHERE session_id = $1 \
                     AND owner_key IS NOT NULL",
                    hiqlite::params!(session.to_owned()),
                )
                .await
                .is_ok_and(|rows| !rows.is_empty());
            if rebuilt {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the learner applies the principal rebuild within 30 s"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn await_route_state(&self, session: &str, state: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while self.replica_route_state(session).await.as_deref() != Some(state) {
            assert!(
                std::time::Instant::now() < deadline,
                "the learner applies {session}={state} within 30 s"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[cfg(feature = "cluster-read-cost-validation")]
struct RouteState {
    state: String,
}

#[cfg(feature = "cluster-read-cost-validation")]
impl From<&mut hiqlite::Row<'_>> for RouteState {
    fn from(row: &mut hiqlite::Row<'_>) -> Self {
        Self {
            state: row.get("state"),
        }
    }
}

/// The review's B1 shape for routes, at the store layer: a learner whose own
/// apply of a release is held has an acknowledged terminal row its replica
/// does not hold, under a valid proof. The raw route view still answers the
/// pre-release row — which is why the daemon's cache keeps the release as an
/// `Observed` entry and never lets a view read replace it — and the per-row
/// floor taken after the acknowledgement ([`AppliedView::observe`]) stays
/// unmet until the release applies here, without closing the table. Dropping
/// that entry early closes the table instead ([`AppliedView::hold_floor`]).
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applied_route_view_never_meets_the_floor_of_a_release_its_replica_holds_back() {
    const SESSION: &str = "00000000-0000-4000-8000-0000000e1c41";
    const RELEASE: &str = "UPDATE media_sessions SET state = 'ended', \
                           terminal_reason = 'deleted' \
                           WHERE session_id = '00000000-0000-4000-8000-0000000e1c41'";
    let _case = HIQLITE_CASE.lock().await;
    let node = LaggingLearner::start().await;
    let user = node
        .store
        .create_user("e1c-learner", "hash", false)
        .await
        .expect("user")
        .id;
    node.store
        .activate_media_session(&route_activation(
            user,
            SESSION,
            "00000000-0000-4000-8000-0000000e1c42",
        ))
        .await
        .expect("activate")
        .expect("activation wins");
    install_principal_layout(&node.client, &node.store).await;
    node.await_principal_layout(SESSION).await;
    let mark = node.mark();
    let metrics = PassiveRaftMetrics::validation_bounded_ready_applied(mark.applied_index());
    let authority: Arc<dyn Store> = node.store.clone();
    let view = AppliedView::validation_replicated(
        authority,
        Arc::clone(&node.store),
        metrics.clone(),
        &node.client,
    );
    metrics.validation_confirm_now(mark.applied_index());
    let read = view.route(SESSION).await.expect("view route");
    assert_eq!(
        read.source,
        ViewSource::View,
        "{:?}",
        view.observation().last_fallback
    );
    let stamp = read.stamp.expect("stamped");

    let (entered, release) = node
        .client
        .validation_hold_transaction_before_commit(
            RELEASE.to_owned(),
            std::time::Instant::now() + Duration::from_secs(45),
        )
        .await
        .expect("hold the learner's apply of the release");
    node.client
        .txn([(RELEASE, hiqlite::params!())])
        .await
        .expect("the voters commit the release")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("the release applies on the voters");
    // The fact this process now holds, taken after the acknowledgement.
    let fact = view.observe();
    tokio::time::timeout(Duration::from_secs(30), entered)
        .await
        .expect("the learner's writer reaches the held release")
        .expect("hold entered");
    assert_eq!(
        node.replica_route_state(SESSION).await.as_deref(),
        Some("active"),
        "the learner's replica is behind its own acknowledged release"
    );
    // A confirmation after the release, at an index this replica has not
    // applied: the proof is valid and the replica is stale.
    metrics.validation_confirm_now(mark.applied_index() + 1);
    assert!(!view.floor_met(fact), "the release is not applied here");
    assert_eq!(
        view.check(&stamp),
        StampCheck::Current,
        "nothing applied here moved the slot"
    );
    let raw = view.route(SESSION).await.expect("raw view route");
    assert_eq!(
        (
            raw.source,
            raw.route.as_ref().map(|route| route.state.as_str())
        ),
        (ViewSource::View, Some("active")),
        "the raw view answers the pre-release row: only the cache's Observed entry stops it"
    );

    // An entry dropped before its floor is met closes the table.
    view.hold_floor(AppliedTable::MediaSessions, fact);
    let held = view.route(SESSION).await.expect("held route");
    assert_eq!(held.source, ViewSource::Authority);
    assert_eq!(
        held.route.as_ref().map(|route| route.state.as_str()),
        Some("ended")
    );
    assert_eq!(view.observation().last_fallback, Some(ViewFallback::Floor));

    release.send(()).expect("release the learner's writer");
    node.await_route_state(SESSION, "ended").await;
    metrics.validation_confirm_now(mark.applied_index());
    assert!(
        view.floor_met(fact),
        "applied and confirmed: the entry retires"
    );
    assert_eq!(view.check(&stamp), StampCheck::Stale);
    let read = view.route(SESSION).await.expect("view route");
    assert_eq!(read.source, ViewSource::View);
    assert_eq!(
        read.route.as_ref().map(|route| route.state.as_str()),
        Some("ended")
    );
}

/// A library, an item and one probed file, through the Store as production
/// writes them.
async fn planning_source(store: &dyn Store, path: &str) -> (i64, i64, i64) {
    let library = store
        .create_library(&NewLibrary {
            name: format!("planning {path}"),
            kind: LibraryKind::Movies,
            paths: vec![std::path::PathBuf::from(format!("/{path}"))],
            anime: false,
        })
        .await
        .expect("planning library");
    let item = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "planned".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("planning item");
    let probe = ProbeResult {
        video_codec: Some("hevc".into()),
        raw_json: Some(r#"{"revision":1}"#.into()),
        ..Default::default()
    };
    let file = store
        .upsert_file(item, &format!("/{path}/movie.mkv"), 100, 42, &probe)
        .await
        .expect("planning file");
    (library.id, item, file)
}

fn same_plan(left: &PlaybackPlanningSnapshot, right: &PlaybackPlanningSnapshot) -> bool {
    format!("{left:?}") == format!("{right:?}")
}

/// D3b, contract §4.2: the planning view on SQLite is the store itself, and a
/// process without a replica answers every plan from Authority.
#[tokio::test]
async fn applied_planning_view_on_sqlite_is_the_store() {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
    let (_, _, file) = planning_source(store.as_ref(), "sqlite").await;
    store
        .put_setting("playback.d3b", "on")
        .await
        .expect("setting");
    let keys = ["playback.d3b", "transcode.absent"];
    let expected = store
        .playback_planning_snapshot(file, &keys)
        .await
        .expect("store")
        .expect("source");
    let view = AppliedView::authority(Arc::clone(&store));
    let (plan, source) = view
        .planning(file, &keys)
        .await
        .expect("plan")
        .expect("source");
    assert_eq!(source, ViewSource::View);
    assert!(same_plan(&plan, &expected));
    assert!(view
        .planning(file + 1_000, &keys)
        .await
        .expect("plan")
        .is_none());
    assert_eq!(
        view.observation()
            .view_total(AppliedTable::Files, ViewSource::View),
        2
    );

    let remote = AppliedView::remote_authority(Arc::clone(&store));
    let (plan, source) = remote
        .planning(file, &keys)
        .await
        .expect("plan")
        .expect("source");
    assert_eq!(source, ViewSource::Authority);
    assert!(same_plan(&plan, &expected));
    assert_eq!(
        remote.observation().last_planning_fallback,
        Some(ViewFallback::NoProof)
    );
}

/// D3b, contract §4.2 and §8 ("files floor"): with a proof the plan is one
/// local statement equal to Authority's; a proof revoked while it runs, a
/// settings save, a direct `files` write and a cascading delete each answer
/// from Authority on the writing node until a later confirmation is applied.
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test]
async fn applied_planning_view_reads_the_replica_behind_the_settings_and_files_floors() {
    let node = EmbeddedSingleton::start().await;
    let (library, item, file) = planning_source(node.store.as_ref(), "replica").await;
    node.store
        .put_setting("playback.d3b", "before")
        .await
        .expect("seed setting");
    let keys = ["playback.d3b", "transcode.absent"];
    let expected = node
        .store
        .playback_planning_snapshot(file, &keys)
        .await
        .expect("Authority")
        .expect("source");

    let metrics = PassiveRaftMetrics::validation_bounded_ready_applied(node.applied_index());
    let view = node.view(metrics.clone());
    // The source and the setting were written by this process: both floors
    // are raised until a confirmation started after them is applied here.
    node.store.validation_reset_operation_counts();
    let (_, source) = view
        .planning(file, &keys)
        .await
        .expect("plan")
        .expect("source");
    assert_eq!(source, ViewSource::Authority);
    assert_eq!(
        view.observation().last_planning_fallback,
        Some(ViewFallback::Floor)
    );
    metrics.validation_confirm_now(node.applied_index());
    node.store.validation_reset_operation_counts();
    for _ in 0..4 {
        let (plan, source) = view
            .planning(file, &keys)
            .await
            .expect("plan")
            .expect("source");
        assert_eq!(source, ViewSource::View);
        assert!(same_plan(&plan, &expected));
    }
    let counts = node.store.validation_operation_counts();
    assert_eq!(
        counts.consistent_query_calls, 0,
        "a proven plan never asks the leader"
    );
    assert_eq!(counts.non_consistent_query_calls, 4);
    assert!(view
        .planning(file + 1_000, &keys)
        .await
        .expect("plan")
        .is_none());

    // Revocation while the statement runs: discarded, Authority once.
    view.validation_revoke_after_next_local();
    node.store.validation_reset_operation_counts();
    let (plan, source) = view
        .planning(file, &keys)
        .await
        .expect("plan")
        .expect("source");
    assert_eq!(source, ViewSource::Authority);
    assert!(same_plan(&plan, &expected));
    assert_eq!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls,
        1
    );
    assert_eq!(
        view.observation().last_planning_fallback,
        Some(ViewFallback::RevokedDuringRead)
    );
    // A fresh proof, then each writer kind in turn.
    let metrics = PassiveRaftMetrics::validation_bounded_ready_applied(node.applied_index());
    let view = node.view(metrics.clone());
    metrics.validation_confirm_now(node.applied_index());
    let reopened = |view: &AppliedView| {
        let view = view.clone();
        async move {
            view.planning(file, &keys)
                .await
                .expect("plan")
                .map(|(_, source)| source)
        }
    };
    assert_eq!(reopened(&view).await, Some(ViewSource::View));

    // A planning setting: the settings floor.
    node.store
        .put_setting("playback.d3b", "after")
        .await
        .expect("save");
    let (plan, source) = view
        .planning(file, &keys)
        .await
        .expect("plan")
        .expect("source");
    assert_eq!(
        (source, plan.settings["playback.d3b"].as_str()),
        (ViewSource::Authority, "after")
    );
    metrics.validation_confirm_now(node.applied_index());
    assert_eq!(reopened(&view).await, Some(ViewSource::View));

    // A direct `files` write: the files floor, with no settings change.
    let probe = ProbeResult {
        video_codec: Some("hevc".into()),
        raw_json: Some(r#"{"revision":2}"#.into()),
        ..Default::default()
    };
    node.store
        .upsert_file(item, "/replica/movie.mkv", 200, 43, &probe)
        .await
        .expect("rescan");
    let (plan, source) = view
        .planning(file, &keys)
        .await
        .expect("plan")
        .expect("source");
    assert_eq!((source, plan.file.size), (ViewSource::Authority, 200));
    assert_eq!(
        view.observation().last_planning_fallback,
        Some(ViewFallback::Floor)
    );
    // The settings view is not closed by a `files` write.
    assert_eq!(
        view.settings(&["playback.d3b"])
            .await
            .expect("settings")
            .source,
        ViewSource::View
    );
    metrics.validation_confirm_now(node.applied_index());
    assert_eq!(reopened(&view).await, Some(ViewSource::View));

    // A delete that reaches `files` only by cascade (libraries -> items ->
    // files): the files floor again, and the plan sees the source gone.
    assert!(node
        .store
        .delete_library(library)
        .await
        .expect("delete library"));
    node.store.validation_reset_operation_counts();
    assert!(view.planning(file, &keys).await.expect("plan").is_none());
    assert_eq!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls,
        1
    );
    assert_eq!(
        view.observation().last_planning_fallback,
        Some(ViewFallback::Floor)
    );
    metrics.validation_confirm_now(node.applied_index());
    node.store.validation_reset_operation_counts();
    assert!(view.planning(file, &keys).await.expect("plan").is_none());
    assert_eq!(
        node.store
            .validation_operation_counts()
            .consistent_query_calls,
        0
    );

    // An owner's authority plan is counted under its own reason.
    assert!(view
        .planning_from_authority(file, &keys, ViewFallback::BindingMismatch)
        .await
        .expect("plan")
        .is_none());
    assert_eq!(
        view.observation().last_planning_fallback,
        Some(ViewFallback::BindingMismatch)
    );
}

/// D3b, contract §8 ("files floor"), review S3: the dv-title bind command
/// (whose `dv_title_jobs_bind` trigger rewrites `files.probe_json`) and the
/// admitted probe result (`apply_probe_job`'s `UPDATE files`) each close the
/// planning view on the writing node until a later confirmation. A writer's
/// statements arm the floor by what they may write, whether or not this call
/// changes a row, so the probe result is driven with no admitted job.
#[cfg(feature = "cluster-read-cost-validation")]
#[tokio::test]
async fn applied_planning_view_closes_on_the_dv_title_and_probe_result_writers() {
    use plurx_core::store::BackgroundJobStore;
    use sha2::Digest;
    let node = EmbeddedSingleton::start().await;
    let (_, item, file) = planning_source(node.store.as_ref(), "writers").await;
    // A Dolby Vision probe: its publication seeds a title-scan demand.
    let dv = ProbeResult {
        video_codec: Some("hevc".into()),
        dolby_vision: plurx_core::domain::DolbyVisionFacts {
            profile: Some(7),
            rpu_present: Some(true),
            ..Default::default()
        },
        raw_json: Some(r#"{"streams":[{"index":0,"codec_type":"video"}]}"#.into()),
        ..Default::default()
    };
    node.store
        .upsert_file(item, "/writers/movie.mkv", 100, 42, &dv)
        .await
        .expect("DV probe");
    let keys = ["playback.d3b"];
    let metrics = PassiveRaftMetrics::validation_bounded_ready_applied(node.applied_index());
    let view = node.view(metrics.clone());
    metrics.validation_confirm_now(node.applied_index());
    let source = |view: &AppliedView| {
        let view = view.clone();
        async move {
            view.planning(file, &keys)
                .await
                .expect("plan")
                .map(|(_, source)| source)
        }
    };
    assert_eq!(source(&view).await, Some(ViewSource::View));

    // The dv-title bind: a command insert whose trigger writes `files`.
    let scanner = hex::encode(sha2::Sha256::digest(b"d3b-scanner"));
    let demand = node
        .store
        .dv_title_demands(&scanner, 0, 1_000)
        .await
        .expect("demands")
        .into_iter()
        .find(|demand| demand.file_id == file)
        .expect("the DV source demands a title scan");
    node.store
        .bind_dv_title_demand(demand.probe_revision, &scanner)
        .await
        .expect("bind the scanner");
    assert_eq!(source(&view).await, Some(ViewSource::Authority));
    assert_eq!(
        view.observation().last_planning_fallback,
        Some(ViewFallback::Floor)
    );
    metrics.validation_confirm_now(node.applied_index());
    assert_eq!(source(&view).await, Some(ViewSource::View));

    // The probe result through the background-jobs bridge.
    let applied = node
        .store
        .apply_probe_job(plurx_core::store::background_jobs_probe::ApplyProbeJob {
            job_id: uuid::Uuid::new_v4().to_string(),
            lease: plurx_core::cluster::coordination::Lease {
                resource: "repair:probe".into(),
                owner_node_id: "node-a".into(),
                fence: 1,
                revision: 1,
                expires_at_unix_ms: 4_102_444_800_000,
            },
            now_ms: 1_000,
        })
        .await
        .expect("apply");
    assert!(!applied, "no admitted job, so no row changes");
    assert_eq!(source(&view).await, Some(ViewSource::Authority));
    assert_eq!(
        view.observation().last_planning_fallback,
        Some(ViewFallback::Floor)
    );
    metrics.validation_confirm_now(node.applied_index());
    assert_eq!(source(&view).await, Some(ViewSource::View));
}
