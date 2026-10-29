use super::*;

#[test]
fn migration_step_commits_its_marker_with_its_shape_or_neither() {
    let conn = Connection::open_in_memory().expect("step fixture");
    let version = |conn: &Connection| -> i64 {
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user_version")
    };
    let table_exists = |conn: &Connection, name: &str| -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .expect("schema read")
            == 1
    };

    // A step that fails after its first DDL statement leaves neither the
    // shape nor the marker behind.
    let failed = SqliteStore::apply_migration_step(
        &conn,
        1,
        "CREATE TABLE torn (x INTEGER); CREATE TABLE torn (y INTEGER);",
        false,
    );
    assert!(failed.is_err(), "duplicate table must fail the step");
    assert!(
        conn.is_autocommit(),
        "failed step leaves no open transaction"
    );
    assert_eq!(version(&conn), 0);
    assert!(!table_exists(&conn, "torn"));

    // A step whose DDL leaves a dangling reference rolls back too.
    conn.pragma_update(None, "foreign_keys", "OFF")
        .expect("foreign keys off");
    let dangling = SqliteStore::apply_migration_step(
        &conn,
        1,
        "CREATE TABLE parent (id INTEGER PRIMARY KEY);
             CREATE TABLE child (parent_id INTEGER REFERENCES parent(id));
             INSERT INTO child VALUES (7);",
        false,
    );
    assert!(dangling.is_err(), "dangling reference must fail the step");
    assert_eq!(version(&conn), 0);
    assert!(!table_exists(&conn, "child"));

    // A successful step commits the shape and the marker together.
    SqliteStore::apply_migration_step(&conn, 1, "CREATE TABLE applied (x INTEGER);", false)
        .expect("successful step");
    assert_eq!(version(&conn), 1);
    assert!(table_exists(&conn, "applied"));

    // A shape a published build already committed only advances the marker.
    SqliteStore::apply_migration_step(&conn, 2, "CREATE TABLE applied (x INTEGER);", true)
        .expect("already-applied step");
    assert_eq!(version(&conn), 2);
}

#[test]
fn copy_output_source_guards_upgrade_active_rows_without_touching_unrelated_work() {
    let conn = Connection::open_in_memory().expect("source guard fixture");
    conn.execute_batch("CREATE TABLE files(id INTEGER PRIMARY KEY,size INTEGER,mtime INTEGER);
            CREATE TABLE background_jobs(id TEXT PRIMARY KEY,kind TEXT,state TEXT,last_error_code TEXT,revision INTEGER,payload_json TEXT,target_node_id TEXT);
            CREATE TABLE background_job_commands(id TEXT,operation TEXT,request_json TEXT,result_json TEXT);
            CREATE TABLE background_job_waiters(job_id TEXT,state TEXT,deadline_ms INTEGER,target_node_id TEXT,result_ref TEXT,updated_at_ms INTEGER);
            INSERT INTO files VALUES(1,100,1),(2,100,1);").expect("fixture tables");
    // Load the actual original two guards, not a simulated predecessor.
    for statement in super::super::background_jobs::SCHEMA.split("-- next statement\n") {
        if statement.contains("CREATE TRIGGER IF NOT EXISTS background_job_source_changed\n")
            || statement.contains("CREATE TRIGGER IF NOT EXISTS background_job_source_deleted\n")
        {
            conn.execute_batch(statement)
                .expect("original source guard");
        }
    }
    for (id, kind, state, file) in [
        ("copy", "copy_output_prepare", "queued", 1),
        ("copy-running", "copy_output_prepare", "running", 2),
        ("transcode", "transcode_prepare", "queued", 1),
        ("fragment", "fragment_index_build", "running", 2),
        ("unrelated", "provider_refresh", "queued", 1),
        ("complete", "copy_output_prepare", "succeeded", 1),
    ] {
        conn.execute(
                "INSERT INTO background_jobs(id,kind,state,last_error_code,revision,payload_json) VALUES(?1,?2,?3,NULL,7,?4)",
                params![
                    id,
                    kind,
                    state,
                    format!("{{\"file_id\":{file},\"source_size\":100,\"source_mtime\":1}}")
                ],
            )
            .expect("seed job");
    }
    conn.execute("UPDATE files SET size=101 WHERE id=1", [])
        .expect("old update");
    let state = |id: &str| {
        conn.query_row(
            "SELECT state,revision FROM background_jobs WHERE id=?1",
            [id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .expect("job state")
    };
    assert_eq!(state("copy"), ("queued".into(), 7));
    assert_eq!(state("transcode"), ("cancelled".into(), 8));
    conn.execute_batch(&format!(
        "BEGIN;\n{}\nCOMMIT;",
        super::super::background_jobs::COPY_OUTPUT_SCHEMA
    ))
    .expect("atomic source guard upgrade");
    conn.execute("UPDATE files SET size=102 WHERE id=1", [])
        .expect("new update");
    conn.execute("DELETE FROM files WHERE id=2", [])
        .expect("source deletion");
    assert_eq!(state("copy"), ("cancelled".into(), 8));
    assert_eq!(state("copy-running"), ("cancelling".into(), 8));
    assert_eq!(state("fragment"), ("cancelling".into(), 8));
    assert_eq!(state("unrelated"), ("queued".into(), 7));
    assert_eq!(state("complete"), ("succeeded".into(), 7));
    assert_eq!(state("transcode"), ("cancelled".into(), 8));
}

#[test]
fn sharing_sqlite_main103_upgrade_appends104_105_and_private92_93_refuse() {
    let conn = Connection::open_in_memory().expect("database");
    SqliteStore::apply_migrations_for_test(&conn, 103).expect("main migrations");
    SqliteStore::migrate(&conn).expect("composed sharing migration");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("marker"),
        SQLITE_SCHEMA_VERSION
    );
    assert!(conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='sharing_ingress_custody')",
            [],
            |row| row.get::<_, bool>(0)
        )
        .expect("custody"));
    for marker in [92, 93] {
        let old = Connection::open_in_memory().expect("old database");
        old.execute_batch(
            "CREATE TABLE sharing_identity(singleton INTEGER PRIMARY KEY,server_id TEXT);",
        )
        .expect("private shape");
        old.pragma_update(None, "user_version", marker)
            .expect("private marker");
        assert!(
            matches!(SqliteStore::migrate(&old),Err(StoreError::Migration(message)) if message.contains("private sharing SQLite lineage"))
        );
        assert_eq!(
            old.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("unchanged marker"),
            marker
        );
    }
}

#[test]
fn a05_v93_sqlite_prior_migration_preserves_unattributed_history() {
    let dir = tempfile::tempdir().expect("prior root");
    let db = dir.path().join("prior.db");
    {
        let conn = Connection::open(&db).expect("legacy database");
        // v93 adds the Link-attributed columns; everything before it is
        // the shape a pre-A-05 node carries.
        for sql in MIGRATIONS.iter().take(92) {
            conn.execute_batch(sql).expect("legacy migration");
        }
        conn.execute("INSERT INTO network_priors VALUES (42, 'test-gen', 'safari', 'network', 8000, 720, 100000, 1, 100000)", []).expect("seed legacy negative");
        conn.pragma_update(None, "user_version", 92)
            .expect("legacy version");
    }
    let store = SqliteStore::open(&db).expect("migrate database");
    drop(store);
    let conn = Connection::open(&db).expect("reopen database");
    let prior = crate::store::telemetry::get_prior(&conn, "test-gen", "safari", "network")
        .expect("read migrated prior")
        .expect("preserved legacy prior");
    assert_eq!(prior.active_starved_rung(100_000), Some(720));
    assert_eq!(prior.active_link_starved_rung(100_000), None);
    assert_eq!(prior.sustained_kbps, Some(8000));
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("schema version"),
        SQLITE_SCHEMA_VERSION
    );
}
use crate::domain::{MetadataPatch, ReadingStateWrite};
use crate::store::{MediaStore, PlaybackTelemetryStore, ReadingStore, SettingsStore, WatchStore};

/// The §3.2 pair: a write on the writer connection is visible to the
/// read pool at once (WAL read-your-writes across connections), and a
/// busy writer no longer makes readers queue — the read completes while
/// the writer connection is deliberately held hostage.
#[tokio::test]
async fn reads_see_writes_and_do_not_queue_behind_the_writer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(SqliteStore::open(&dir.path().join("plurx.db")).expect("open"));

    // Visibility across connections: put on the writer, get on a reader.
    store.put_setting("k", "v").await.expect("put");
    assert_eq!(
        store.get_setting("k").await.expect("get"),
        Some("v".to_owned()),
        "a committed write is visible to the read pool immediately"
    );

    // Park the writer connection: the closure holds its mutex until
    // released, which is exactly what a slow write or a scan batch does.
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = {
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            store
                .with_conn(move |_conn| {
                    held_tx.send(()).ok();
                    release_rx.recv().ok();
                    Ok(())
                })
                .await
        })
    };
    tokio::task::spawn_blocking(move || held_rx.recv())
        .await
        .expect("join")
        .expect("the holder took the writer connection");

    // The read must complete while the writer is held. Generous bound:
    // it passes in microseconds, and only a regression that routes reads
    // back through the writer mutex can spend it.
    tokio::time::timeout(std::time::Duration::from_secs(10), store.get_setting("k"))
        .await
        .expect("a read must not queue behind the writer")
        .expect("get");

    release_tx.send(()).expect("release");
    holder.await.expect("join").expect("holder");
}

/// K-05 M1: every Home and library-page read on the read pool (the 16
/// M1 moved there, plus `home_preview_pages`, which already was)
/// completes while the writer holds an open write transaction, and none
/// of them sees that transaction's uncommitted row.
///
/// The writer here is not just holding the mutex but has taken the WAL
/// write lock with `BEGIN IMMEDIATE` and inserted a library, which is what
/// a scan batch does. Each read gets two seconds; a method routed back
/// through `with_conn` waits for the release that only comes after all of
/// them returned, so it times out instead of passing slowly.
#[tokio::test]
async fn home_and_library_page_reads_do_not_queue_behind_a_held_writer_transaction() {
    use crate::domain::{ItemKind, ItemSort, LibraryKind, NewItem, NewLibrary};
    use crate::store::{LibraryStore, UserStore};

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(SqliteStore::open(&dir.path().join("plurx.db")).expect("open"));
    let user = store
        .create_user("reader", "hash", true)
        .await
        .expect("user");
    let library = store
        .create_library(&NewLibrary {
            name: "Shows".into(),
            kind: LibraryKind::Shows,
            paths: vec![],
            anime: false,
        })
        .await
        .expect("library");
    let item = |kind, parent_id, title: &str, season, episode| NewItem {
        library_id: library.id,
        kind,
        parent_id,
        title: title.to_owned(),
        year: None,
        season_number: season,
        episode_number: episode,
    };
    let show = store
        .insert_item(&item(ItemKind::Show, None, "Harbor Lights", None, None))
        .await
        .expect("show");
    let season = store
        .insert_item(&item(
            ItemKind::Season,
            Some(show),
            "Season 1",
            Some(1),
            None,
        ))
        .await
        .expect("season");
    let episode = store
        .insert_item(&item(
            ItemKind::Episode,
            Some(season),
            "Pilot",
            Some(1),
            Some(1),
        ))
        .await
        .expect("episode");
    store
        .put_progress(user.id, episode, 1_000, Some(10_000))
        .await
        .expect("progress");

    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = {
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            store
                .with_conn(move |conn| {
                    conn.execute_batch(
                        "BEGIN IMMEDIATE;
                             INSERT INTO libraries(name, kind, paths)
                             VALUES('uncommitted', 'movies', '[]');",
                    )?;
                    held_tx.send(()).ok();
                    release_rx.recv().ok();
                    conn.execute_batch("ROLLBACK")?;
                    Ok(())
                })
                .await
        })
    };
    tokio::task::spawn_blocking(move || held_rx.recv())
        .await
        .expect("join")
        .expect("the holder took the write lock");

    let bound = std::time::Duration::from_secs(2);
    macro_rules! off_the_writer {
        ($name:literal, $call:expr) => {
            tokio::time::timeout(bound, $call)
                .await
                .unwrap_or_else(|_| panic!("{} queued behind the writer", $name))
                .unwrap_or_else(|error| panic!("{}: {error}", $name))
        };
    }
    let libraries = off_the_writer!("list_libraries", store.list_libraries());
    assert_eq!(
        libraries.len(),
        1,
        "a read connection must not see the writer's uncommitted row"
    );
    off_the_writer!("get_library", store.get_library(library.id));
    let page = off_the_writer!(
        "list_top_items_in_genre",
        store.list_top_items_in_genre(library.id, ItemSort::Title, 0, 50, None)
    );
    assert_eq!((page.total, page.items.len()), (1, 1));
    off_the_writer!("home_preview_pages", store.home_preview_pages(24));
    off_the_writer!("recently_added", store.recently_added(None, 24));
    off_the_writer!("search_items", store.search_items("harbor", 24));
    off_the_writer!("get_item_children", store.get_item_children(show));
    off_the_writer!("episodes_for_show", store.episodes_for_show(show));
    off_the_writer!("files_for_item", store.files_for_item(episode));
    off_the_writer!("child_counts", store.child_counts(&[show, season]));
    off_the_writer!("item_max_heights", store.item_max_heights(&[episode]));
    off_the_writer!("item_media_facts", store.item_media_facts(&[episode]));
    off_the_writer!("watch_map", store.watch_map(user.id, &[episode]));
    off_the_writer!("watch_rollup", store.watch_rollup(user.id, show));
    off_the_writer!("watch_rollups", store.watch_rollups(user.id, &[show]));
    let in_progress = off_the_writer!("continue_watching", store.continue_watching(user.id, 24));
    assert_eq!(in_progress.len(), 1);
    off_the_writer!("next_up", store.next_up(user.id, 24));

    release_tx.send(()).expect("release");
    holder.await.expect("join").expect("holder");
}

/// `with_read_txn` gives a multi-statement closure one WAL snapshot: a
/// commit that lands between its two statements is invisible to the
/// second, where plain `with_read` shows it. That second half is the
/// control: it proves the interleaving really happened, so the first half
/// cannot pass merely because the insert was late.
#[tokio::test]
async fn read_transactions_see_one_snapshot_across_statements() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(SqliteStore::open(&dir.path().join("plurx.db")).expect("open"));
    store.put_setting("seed", "0").await.expect("seed");

    async fn two_counts(store: &Arc<SqliteStore>, snapshot: bool, key: &'static str) -> (i64, i64) {
        let (between_tx, between_rx) = std::sync::mpsc::channel::<()>();
        let (inserted_tx, inserted_rx) = std::sync::mpsc::channel::<()>();
        let writer = {
            let store = Arc::clone(store);
            tokio::spawn(async move {
                tokio::task::spawn_blocking(move || between_rx.recv())
                    .await
                    .expect("join")
                    .expect("reader reached the gap");
                store.put_setting(key, "1").await.expect("insert");
                inserted_tx.send(()).expect("signal");
            })
        };
        let read = move |conn: &Connection| -> Result<(i64, i64), StoreError> {
            let count = |conn: &Connection| -> Result<i64, StoreError> {
                Ok(conn.query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))?)
            };
            let first = count(conn)?;
            between_tx.send(()).ok();
            inserted_rx.recv().ok();
            Ok((first, count(conn)?))
        };
        let counts = if snapshot {
            store.with_read_txn(read).await
        } else {
            store.with_read(read).await
        }
        .expect("read");
        writer.await.expect("writer");
        counts
    }

    let (first, second) = two_counts(&store, true, "during-snapshot").await;
    assert_eq!(first, second, "one read transaction, one snapshot");
    let (first, second) = two_counts(&store, false, "during-plain-read").await;
    assert_eq!(
        second,
        first + 1,
        "control: without the transaction the second statement sees the commit"
    );
}

/// The revision advances on a change of ask and on nothing else.
///
/// That is the entire difference between this and the four counters §1
/// rules out. A capture revision moves on every snapshot publish, an
/// attachment generation on every reconnection, a generic intent
/// generation on a title change, a control sequence on every heartbeat —
/// so none of them can answer "has the viewer asked for something else".
/// A revision that moved when a client merely repeated itself would be the
/// same kind of number wearing a better name, and every admission point
/// comparing it would refuse work that nothing was wrong with.
///
/// Monotonicity is the other half: two asks have to be *orderable*, which
/// a content hash alone can never be. Going back to an earlier selection
/// is a new ask, not a return to an old one, so the revision goes forward
/// while the digest goes back — which is what makes ABA visible.
#[tokio::test]
async fn a_desired_revision_advances_on_a_new_ask_and_never_on_a_repeat() {
    use crate::store::MediaSessionStore;

    let store = SqliteStore::open_in_memory().expect("open");
    assert_eq!(
        store
            .desired_selection(
                &crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                "play-a"
            )
            .await
            .expect("read"),
        None,
        "a playback nobody has asked anything about says so, rather than \
             inventing a default that an admission point would compare against"
    );

    let auto = "a".repeat(64);
    let manual = "b".repeat(64);
    let record = |digest: &str, form: &str, now: i64| {
        let store = &store;
        let digest = digest.to_owned();
        let form = form.to_owned();
        async move {
            store
                .record_desired_selection(
                    &crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                    "play-a",
                    &digest,
                    &form,
                    now,
                )
                .await
                .expect("record")
        }
    };
    let first = record(&auto, "v1;quality=auto", 1_000).await;
    assert_eq!(first.revision, 1);
    assert_eq!(first.updated_at_ms, 1_000);

    // The same ask again, twice, from a client doing exactly what clients
    // do: repeating its selection on every exchange.
    let repeated = record(&auto, "v1;quality=auto", 2_000).await;
    assert_eq!(
        repeated, first,
        "repeating an ask is not changing it — not the revision, and not \
             even the timestamp, which would otherwise make an unchanged row \
             look freshly decided"
    );
    assert_eq!(record(&auto, "v1;quality=auto", 3_000).await, first);

    // A different ask.
    let second = record(&manual, "v1;quality=manual:720", 4_000).await;
    assert_eq!(second.revision, 2);
    assert_eq!(second.updated_at_ms, 4_000);
    assert_eq!(second.canonical_form, "v1;quality=manual:720");

    // And back to the first selection. The digest returns; the revision
    // does not. Without that, an ask that went A → B → A would be
    // indistinguishable from one that never left A, and a staged
    // successor for the first A could be committed against the second.
    let third = record(&auto, "v1;quality=auto", 5_000).await;
    assert_eq!(third.digest, first.digest);
    assert_eq!(
        third.revision, 3,
        "returning to an earlier selection is a new ask, not a return to an old one"
    );

    assert_eq!(
        store
            .desired_selection(
                &crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                "play-a"
            )
            .await
            .expect("read"),
        Some(third),
        "and the row that is read back is the one that was written"
    );
    assert_eq!(
        store
            .desired_selection(
                &crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 8 },
                "play-a"
            )
            .await
            .expect("read"),
        None,
        "asks are per viewer as well as per playback"
    );
    assert_eq!(
        store
            .desired_selection(
                &crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                "play-b"
            )
            .await
            .expect("read"),
        None
    );
}

/// The row refuses what its own constraints refuse, before the statement.
///
/// Both backends have to reject the same inputs or a replicated import
/// disagrees with the node it imported from — and the disagreement would
/// surface as an opaque constraint violation during a restore, which is
/// the worst possible moment to discover it.
#[tokio::test]
async fn a_desired_selection_is_bounded_before_it_reaches_the_table() {
    use crate::store::MediaSessionStore;

    let store = SqliteStore::open_in_memory().expect("open");
    const NOT_HEX: &str = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";
    let digest = "c".repeat(64);
    for (playback_id, digest, form, now, why) in [
        ("", digest.as_str(), "v1;x", 1, "an empty playback id"),
        (
            "play-a",
            "short",
            "v1;x",
            1,
            "a digest that is not a sha-256",
        ),
        ("play-a", NOT_HEX, "v1;x", 1, "a digest that is not hex"),
        ("play-a", digest.as_str(), "", 1, "an empty canonical form"),
        (
            "play-a",
            digest.as_str(),
            "v1;x",
            0,
            "a timestamp before the epoch",
        ),
    ] {
        assert!(
            store
                .record_desired_selection(
                    &crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                    playback_id,
                    digest,
                    form,
                    now
                )
                .await
                .is_err(),
            "{why} must be refused"
        );
    }
    assert_eq!(
        store
            .desired_selection(
                &crate::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
                "play-a"
            )
            .await
            .expect("read"),
        None,
        "and a refused write leaves nothing behind"
    );
}

#[tokio::test]
async fn settings_roundtrip_and_upsert() {
    let store = SqliteStore::open_in_memory().expect("open");
    assert_eq!(store.get_setting("k").await.expect("get"), None);
    store.put_setting("k", "v1").await.expect("put");
    store.put_setting("k", "v2").await.expect("upsert");
    assert_eq!(
        store.get_setting("k").await.expect("get"),
        Some("v2".to_owned())
    );
}

#[tokio::test]
async fn related_settings_publish_as_one_group() {
    let store = SqliteStore::open_in_memory().expect("open");
    store
        .put_settings(&[
            ("transcode.quality", "22"),
            ("transcode.rate_mode", "quality"),
        ])
        .await
        .expect("put group");
    assert_eq!(
        store.get_setting("transcode.quality").await.expect("get"),
        Some("22".to_owned())
    );
    assert_eq!(
        store.get_setting("transcode.rate_mode").await.expect("get"),
        Some("quality".to_owned())
    );
    assert_eq!(
        store
            .get_setting_pair("transcode.rate_mode", "transcode.quality")
            .await
            .expect("get pair"),
        (Some("quality".to_owned()), Some("22".to_owned()))
    );
}

#[tokio::test]
async fn generated_settings_compare_and_swap_the_complete_tuple() {
    let store = SqliteStore::open_in_memory().expect("open");
    let first = [
        ("live_tv.enabled", "0"),
        ("live_tv.device_ipv4", "10.42.4.20"),
        ("live_tv.config_generation", "1"),
    ];
    assert!(store
        .put_settings_if_generation("live_tv.config_generation", 0, &first)
        .await
        .expect("first CAS"));
    let stale = [
        ("live_tv.enabled", "1"),
        ("live_tv.device_ipv4", "10.42.4.21"),
        ("live_tv.config_generation", "1"),
    ];
    assert!(!store
        .put_settings_if_generation("live_tv.config_generation", 0, &stale)
        .await
        .expect("stale CAS"));
    let snapshot = store.settings_snapshot().await.expect("snapshot");
    assert_eq!(
        snapshot.get("live_tv.enabled").map(String::as_str),
        Some("0")
    );
    assert_eq!(
        snapshot.get("live_tv.device_ipv4").map(String::as_str),
        Some("10.42.4.20")
    );
    assert_eq!(
        snapshot
            .get("live_tv.config_generation")
            .map(String::as_str),
        Some("1")
    );

    store
        .put_setting("live_tv.config_generation", "garbage")
        .await
        .expect("inject corrupt generation");
    let next = [("live_tv.enabled", "1"), ("live_tv.config_generation", "1")];
    assert!(store
        .put_settings_if_generation("live_tv.config_generation", 0, &next)
        .await
        .is_err());
    assert_eq!(
        store
            .get_setting("live_tv.config_generation")
            .await
            .expect("corrupt generation remains")
            .as_deref(),
        Some("garbage")
    );
}

#[tokio::test]
async fn instance_id_is_a_uuid_and_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");

    let first = {
        let store = SqliteStore::open(&db).expect("open");
        store.instance_id().await.expect("instance id")
    };
    uuid::Uuid::parse_str(&first).expect("instance id is a uuid");

    let store = SqliteStore::open(&db).expect("reopen");
    let second = store.instance_id().await.expect("instance id");
    assert_eq!(
        first, second,
        "instance id must be immutable across restarts"
    );
}

/// Build a database at the pre-home-video schema (v5) and fill it with one
/// row of every shape v6's table rebuild has to carry across.
fn seed_v5(db: &Path) {
    let conn = Connection::open(db).expect("raw open");
    conn.pragma_update(None, "foreign_keys", "ON").expect("fk");
    for (index, sql) in MIGRATIONS.iter().enumerate().take(5) {
        conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
            .unwrap_or_else(|e| panic!("v{}: {e}", index + 1));
    }
    conn.pragma_update(None, "user_version", 5)
        .expect("version");
    conn.execute_batch(
        "INSERT INTO settings (key, value) VALUES ('instance.id', 'fixture-instance');
             INSERT INTO users (id, username, password_hash, is_admin)
                 VALUES (1, 'paul', 'hash', 1);
             INSERT INTO libraries (id, name, kind, paths, anime)
                 VALUES (1, 'Movies', 'movies', '[\"/media/movies\"]', 0),
                        (2, 'TV', 'shows', '[\"/media/tv\"]', 0);
             INSERT INTO items (id, library_id, kind, parent_id, title, sort_title, year, overview)
                 VALUES (10, 1, 'movie', NULL, 'Blade Runner', 'blade runner', 1982, 'Replicants.'),
                        (20, 2, 'show', NULL, 'Severance', 'severance', 2022, NULL),
                        (21, 2, 'season', 20, 'Season 1', 'season 1', NULL, NULL),
                        (22, 2, 'episode', 21, 'Good News', 'good news', NULL, NULL);
             INSERT INTO files (id, item_id, path, size, mtime, container, audio_offset_ms)
                 VALUES (100, 10, '/media/movies/blade-runner.mkv', 42, 7, 'mkv', -250);
             INSERT INTO watch_state (user_id, item_id, position_ms, watched)
                 VALUES (1, 10, 61000, 0);",
    )
    .expect("seed");
}

/// v9 turns "has an id" into "a provider has answered". The upgrade must
/// carry the old meaning across for everything already matched — the
/// failure mode is silent and expensive: every library in the world
/// re-fetching itself from TMDB the first time it starts on the new
/// version, one HTTP call per item, into a rate limit.
#[tokio::test]
async fn v9_backfills_already_matched_items_so_no_library_re_enriches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(8) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|e| panic!("v{}: {e}", index + 1));
        }
        conn.pragma_update(None, "user_version", 8)
            .expect("version");
        conn.execute_batch(
            "INSERT INTO libraries (id, name, kind, paths, anime)
                     VALUES (1, 'Movies', 'movies', '[\"/media/movies\"]', 0);
                 INSERT INTO items (id, library_id, kind, title, sort_title, tmdb_id, updated_at)
                     VALUES (10, 1, 'movie', 'Blade Runner', 'blade runner', 78, 1700000000),
                            (11, 1, 'movie', 'Unmatched', 'unmatched', NULL, 1700000000);",
        )
        .expect("seed v8");
    }

    let store = SqliteStore::open(&db).expect("migrate");
    let needing = store
        .items_needing_metadata(None, false, None)
        .await
        .expect("needing");
    assert_eq!(
        needing.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![11],
        "the matched item is left alone; only the unmatched one is queued"
    );

    let conn = Connection::open(&db).expect("raw reopen");
    let stamp: Option<i64> = conn
        .query_row("SELECT metadata_at FROM items WHERE id = 10", [], |r| {
            r.get(0)
        })
        .expect("stamp");
    assert_eq!(stamp, Some(1700000000), "backfilled from updated_at");
}

/// The single most important test in the home-video feature: v6 rebuilds
/// both `libraries` and `items` (SQLite can't alter a CHECK), and a
/// mistake there eats a real library.
#[tokio::test]
async fn v6_rebuild_preserves_everything() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    seed_v5(&db);

    // Open through the real binary path — this applies v6.
    let store = SqliteStore::open(&db).expect("migrate");
    assert_eq!(
        store.instance_id().await.expect("instance id"),
        "fixture-instance",
        "the instance id must survive the rebuild"
    );

    let conn = Connection::open(&db).expect("raw reopen");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert_eq!(version, MIGRATIONS.len() as i64);
    // 63 -> 64 for v64, `FILES_FIELD_ORDER_COLUMN`: one additive
    // `ALTER TABLE files ADD COLUMN field_order` appended by the interlace
    // work, covered by `v64_adds_field_order_to_the_existing_files_table`
    // below. 64 -> 65 for v65, `FILES_LUMINANCE_COLUMNS_BATCH`: the four
    // additive `files` luminance columns the tone-map work drafted as v64
    // and which append after the field-order column now that it reached
    // main first. 65 -> 66 for v66, `FRAGMENT_INDEXES_VALIDATION_COLUMN`:
    // the node-local publication proof C-05 drafted as v64, appended after
    // both of those for the same reason they reached main first. No
    // earlier entry moved; the list stays append-only. v67 adds durable
    // downloaded captions to files. v68 adds external-reader file grants;
    // v69 adds the cluster subtitle-source queue and publication metadata;
    // v70 adds K-05 M5's catalogue read indexes; v71 adds the common queue.
    // v72–v76 add library work, domain leases, source-I/O reservations,
    // provider dispatch budgets and the subtitle adapter; v77 adds artwork holders,
    // and v78 retains portable transcode source/manifest provenance.
    // v79–v85 add predictions, embeddings, probe/integrity work, Live TV
    // resource claims, subtitle reconciliation and bounded job history;
    // v86 compacts settled receipts under waiter pressure; v87 adds
    // expiring viewer interests through analysis and artifacts; v88 adds the
    // unconditional result-key/target/force index for bounded cleanup.
    // v89 indexes preparation history; v90 records explicit DV requests;
    // v91 records transactional planning generation.
    // v92–v97 append offline audio, independent Link columns/table,
    // copy preparation, candidate recovery and encoded preparation.

    // v98–v101 add Jellyfin compatibility: permanent wire identities,
    // compatibility-only logins, bounded negotiations with exact native
    // references, and manual-edit watch revisions; v102 adds quality
    // cancellation and v103 continuous dependencies; v104–v105 add sharing
    // authority and outer-ingress custody; v106-v110 append Cinema receiver
    // credentials, consent, revision fencing, login rebind and cleanup guards.
    // v111 appends managed subtitles; v112 appends progressive translation;
    // v113 adds ratings/lists and v114 adds source-revision title acquisition;
    // v115 adds one-time playback handoff codes and file-scoped tokens;
    // v116 appends text translation, v117 Shared caption leases,
    // v118 producer retargeting and v119 claim-bound playback continuity;
    // v120 upgrades managed source-reader admission while retaining its exact predecessor.
    assert_eq!(
        version, 120,
        "a new migration must be a deliberate bump, not a surprise — \
             the list is append-only and every entry is one somebody shipped"
    );
    let files: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'files'",
            [],
            |r| r.get(0),
        )
        .expect("files table");
    // The two spellings of the same migration must not drift. The
    // replicated backend runs one statement per transaction entry and
    // rusqlite refuses a multi-statement `execute`, so the DDL exists as a
    // list and as a batch; the single-node list is append-only, which
    // makes a divergent edit unfixable once shipped.
    assert_eq!(
        super::super::FILES_DOLBY_VISION_COLUMNS
            .iter()
            .map(|statement| format!("{statement};"))
            .collect::<Vec<_>>()
            .join("\n"),
        super::super::FILES_DOLBY_VISION_COLUMNS_BATCH.trim(),
        "the per-statement and batch spellings of the Dolby Vision \
             migration have drifted"
    );
    for column in [
        "dv_profile",
        "dv_level",
        "dv_bl_compat_id",
        "dv_el_present",
        "dv_rpu_present",
    ] {
        assert!(
            files.contains(column),
            "v38 carries the Dolby Vision configuration record as columns \
                 so nothing has to read a profile number back out of a display \
                 label: {files}"
        );
    }
    assert_eq!(
        super::super::FILES_LUMINANCE_COLUMNS
            .iter()
            .map(|statement| format!("{statement};"))
            .collect::<Vec<_>>()
            .join("\n"),
        super::super::FILES_LUMINANCE_COLUMNS_BATCH.trim(),
        "the per-statement and batch spellings of the luminance migration have drifted"
    );
    for column in [
        "max_cll",
        "max_fall",
        "mastering_max_luminance",
        "luminance_source",
    ] {
        assert!(files.contains(column), "v64 carries {column}: {files}");
    }
    let fragment_indexes: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master
                  WHERE type = 'table' AND name = 'fragment_indexes'",
            [],
            |r| r.get(0),
        )
        .expect("fragment index table");
    assert!(
        fragment_indexes.contains("PRIMARY KEY (file_id, argv_fingerprint)"),
        "v37 re-keys the table on the copy pipeline as well as the file, \
             or a Dolby Vision title's second identity silently overwrites its \
             first: {fragment_indexes}"
    );

    let preparations: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master
                  WHERE type = 'table' AND name = 'media_session_preparations'",
            [],
            |r| r.get(0),
        )
        .expect("preparation ledger");
    assert!(
        preparations.contains("PRIMARY KEY (user_id, playback_id)"),
        "v42 enforces one staged successor per playback in the schema \
             rather than in a read-then-write, which is the only place a \
             concurrent second prepare cannot slip past: {preparations}"
    );
    assert!(
        preparations.contains("staged_incarnation_id              TEXT NOT NULL UNIQUE"),
        "an incarnation may be staged by at most one playback, for the \
             same reason a pointer's current_incarnation_id is unique: \
             {preparations}"
    );
    assert!(
        preparations.contains("expected_predecessor_incarnation_id TEXT NOT NULL"),
        "the predecessor is recorded at prepare time and never re-derived \
             at commit — a pointer that moved means a newer generation exists \
             and the staged successor must abort rather than reap it: \
             {preparations}"
    );

    // Every row survives, values identical.
    let dangling: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .expect("fk check");
    assert_eq!(dangling, 0, "migration left dangling foreign keys");

    let (name, kind, anime): (String, String, i64) = conn
        .query_row(
            "SELECT name, kind, anime FROM libraries WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("library");
    assert_eq!(
        (name.as_str(), kind.as_str(), anime),
        ("Movies", "movies", 0)
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM libraries", [], |r| r.get::<_, i64>(0))
            .expect("count"),
        2
    );

    let (title, year, overview, parent): (String, i64, String, Option<i64>) = conn
        .query_row(
            "SELECT title, year, overview, parent_id FROM items WHERE id = 10",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("item");
    assert_eq!(
        (title.as_str(), year, overview.as_str(), parent),
        ("Blade Runner", 1982, "Replicants.", None)
    );
    assert_eq!(
        conn.query_row("SELECT parent_id FROM items WHERE id = 22", [], |r| r
            .get::<_, i64>(0))
            .expect("episode parent"),
        21,
        "the show hierarchy must survive"
    );
    // New columns get their defaults, not NULL-vs-missing surprises.
    let (recorded, tags, seeded): (Option<String>, String, Option<i64>) = conn
        .query_row(
            "SELECT recorded_at, tags, nfo_seeded_at FROM items WHERE id = 10",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("new columns");
    assert_eq!((recorded, tags.as_str(), seeded), (None, "[]", None));

    // Files and watch state are untouched (audio_offset_ms included).
    let (path, offset): (String, i64) = conn
        .query_row(
            "SELECT path, audio_offset_ms FROM files WHERE id = 100",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("file");
    assert_eq!(
        (path.as_str(), offset),
        ("/media/movies/blade-runner.mkv", -250)
    );
    assert_eq!(
        conn.query_row(
            "SELECT position_ms FROM watch_state WHERE user_id = 1 AND item_id = 10",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("watch state"),
        61000
    );

    // The FTS index was recreated and rebuilt: a pre-migration title is
    // still findable, and searching it doesn't error on the new column.
    let hits = store.search_items("blade", 10).await.expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].item.title, "Blade Runner");

    // And the whole point: the new kinds insert cleanly now that the
    // CHECK constraints are gone.
    conn.execute(
        "INSERT INTO libraries (id, name, kind, paths, anime)
             VALUES (3, 'Home', 'home', '[\"/media/home\"]', 0)",
        [],
    )
    .expect("home library");
    conn.execute(
        "INSERT INTO items (id, library_id, kind, parent_id, title, sort_title)
             VALUES (30, 3, 'folder', NULL, '2019', '2019'),
                    (31, 3, 'video', 30, 'Beach', 'beach'),
                    (32, 3, 'photo', 30, 'IMG_4021', 'img_4021')",
        [],
    )
    .expect("home items");
}

#[tokio::test]
async fn v17_adds_playback_telemetry_without_touching_v16_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(16) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|e| panic!("v{}: {e}", index + 1));
        }
        conn.pragma_update(None, "user_version", 16)
            .expect("version");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives')",
            [],
        )
        .expect("seed v16 row");
    }

    let store = SqliteStore::open(&db).expect("migrate v16 to v17");
    assert_eq!(
        store
            .get_setting("migration.proof")
            .await
            .expect("read proof")
            .as_deref(),
        Some("survives")
    );
    let event_id = store
        .record_playback_event(&crate::domain::PlaybackEvent {
            at_unix_ms: 1_700_000_000_000,
            event: "migration_proof".to_owned(),
            ..crate::domain::PlaybackEvent::default()
        })
        .await
        .expect("write v17 row");
    assert!(event_id > 0);

    let conn = Connection::open(&db).expect("raw reopen");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version");
    assert_eq!(version, SQLITE_SCHEMA_VERSION);
    for index in ["playback_events_by_event", "playback_events_by_file"] {
        let present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                [index],
                |row| row.get(0),
            )
            .expect("index lookup");
        assert_eq!(present, 1, "missing {index}");
    }
}

#[test]
fn v18_backfills_offline_rate_control_without_retargeting_packages() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(17) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.pragma_update(None, "user_version", 17)
            .expect("version");
        conn.execute(
            "INSERT INTO users (id, username, password_hash) VALUES (1, 'paul', 'hash')",
            [],
        )
        .expect("seed user");
        conn.execute(
            "INSERT INTO offline_packages
                    (id, request_id, user_id, file_id, node_id, source_path, source_size,
                     source_mtime, target_height, subtitle_mode, state, phase, expires_at)
                 VALUES ('package', 'request', 1, 9, 'node', '/media/movie.mkv', 1000,
                         7, 720, 'none', 'queued', 'waiting_for_encoder', 10000)",
            [],
        )
        .expect("seed v17 package");
    }

    SqliteStore::open(&db).expect("migrate v17 to v18");
    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row(
            "SELECT effective_rate_control FROM offline_packages WHERE id = 'package'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("backfilled snapshot"),
        "vbr"
    );
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    assert!(conn
        .execute(
            "UPDATE offline_packages SET effective_rate_control = 'cbr' WHERE id = 'package'",
            [],
        )
        .is_err());
    for invalid in ["qvbr:", "qvbr:21junk", "qvbr:256", "qvbr:021"] {
        assert!(
            conn.execute(
                "UPDATE offline_packages SET effective_rate_control = ?1 WHERE id = 'package'",
                [invalid],
            )
            .is_err(),
            "constraint accepted {invalid}"
        );
    }
}

#[tokio::test]
async fn v19_adds_node_local_network_priors_without_touching_v18_rows() {
    use crate::domain::{CredentialGeneration, NetworkPriorObservation};
    use crate::store::NetworkPriorStore;

    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(18) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.pragma_update(None, "user_version", 18)
            .expect("version");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives')",
            [],
        )
        .expect("seed v18 row");
    }

    let store = SqliteStore::open(&db).expect("migrate v18 to v19");
    assert_eq!(
        store
            .get_setting("migration.proof")
            .await
            .expect("read proof")
            .as_deref(),
        Some("survives")
    );
    let prior = store
        .observe_network_prior(&NetworkPriorObservation {
            credential_generation: CredentialGeneration::from("v19-test-gen".to_owned()),
            client_class: "chrome".to_owned(),
            network_fingerprint: "192.0.2.0/24".to_owned(),
            throughput_kbps: Some(6_000),
            observed_at_ms: 1_700_000_000_000,
            ..NetworkPriorObservation::default()
        })
        .await
        .expect("write v19 prior");
    assert_eq!(prior.sustained_kbps, Some(6_000));

    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    let index: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'network_priors_by_updated'",
            [],
            |row| row.get(0),
        )
        .expect("index lookup");
    assert_eq!(index, 1);
}

#[tokio::test]
async fn v20_adds_revision_bound_reading_state_without_touching_v19_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(19) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.pragma_update(None, "user_version", 19)
            .expect("version");
        conn.execute_batch(
            "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives');
                 INSERT INTO users (id, username, password_hash, is_admin)
                     VALUES (1, 'reader', 'hash', 0);
                 INSERT INTO libraries (id, name, kind, paths, anime)
                     VALUES (1, 'Books', 'books', '[\"/books\"]', 0);
                 INSERT INTO items (id, library_id, kind, title, sort_title)
                     VALUES (10, 1, 'book', 'Migration Book', 'migration book');
                 INSERT INTO files (id, item_id, path, size, mtime)
                     VALUES (100, 10, '/books/migration.epub', 4096, 77);",
        )
        .expect("seed v19 rows");
    }

    let store = SqliteStore::open(&db).expect("migrate v19 to v20");
    assert_eq!(
        store
            .get_setting("migration.proof")
            .await
            .expect("read proof")
            .as_deref(),
        Some("survives")
    );
    let state = store
        .put_reading_state(
            1,
            10,
            &ReadingStateWrite {
                file_id: 100,
                file_size: 4096,
                file_mtime: 77,
                locator_json: r#"{"version":1,"href":"chapter.xhtml"}"#.into(),
                progression_millis: 500_000,
                completed: false,
                recorded_at: Some(123),
            },
        )
        .await
        .expect("write v20 reading state");
    assert_eq!(state.progression_millis, 500_000);
    assert_eq!(
        store
            .current_reading_state(1, 10)
            .await
            .expect("read v20 state")
            .expect("state"),
        state
    );

    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    assert!(conn
        .execute(
            "UPDATE reading_state SET progression_millis = 1000001 \
                 WHERE user_id = 1 AND item_id = 10 AND file_id = 100",
            [],
        )
        .is_err());
    assert!(conn
        .execute(
            "UPDATE reading_state SET completed = 2 \
                 WHERE user_id = 1 AND item_id = 10 AND file_id = 100",
            [],
        )
        .is_err());
}

#[tokio::test]
async fn v21_adds_book_facts_without_touching_v20_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(20) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.pragma_update(None, "user_version", 20)
            .expect("version");
        conn.execute_batch(
            "INSERT INTO libraries (id, name, kind, paths, anime)
                     VALUES (1, 'Books', 'books', '[\"/books\"]', 0);
                 INSERT INTO items (id, library_id, kind, title, sort_title)
                     VALUES (10, 1, 'book', 'Migration Book', 'migration book');
                 INSERT INTO files (id, item_id, path, size, mtime)
                     VALUES (100, 10, '/books/migration.epub', 4096, 77);",
        )
        .expect("seed v20 rows");
    }

    let store = SqliteStore::open(&db).expect("migrate v20 to current");
    let item = store
        .get_item(10)
        .await
        .expect("read migrated item")
        .expect("migrated item");
    assert_eq!(item.title, "Migration Book");
    assert_eq!(item.author, None);
    assert_eq!(item.book_work_id, None);
    assert_eq!(item.book_edition_id, None);
    assert_eq!(item.book_metadata_source, None);
    assert_eq!(
        store.files_for_item(10).await.expect("read migrated file")[0].path,
        std::path::PathBuf::from("/books/migration.epub")
    );

    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    let index: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_items_book_work'",
            [],
            |row| row.get(0),
        )
        .expect("book work index");
    assert_eq!(index, 1);
}

#[tokio::test]
async fn v22_replaces_user_id_key_with_credential_generation() {
    use crate::domain::{CredentialGeneration, NetworkPriorObservation};
    use crate::store::NetworkPriorStore;

    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(20) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.execute_batch(
            "INSERT INTO network_priors
                     (user_id, client_class, network_fingerprint, sample_count, updated_at_ms)
                 VALUES (1, 'chrome', '192.0.2.0/24', 5, 1000);
                 INSERT INTO settings (key, value)
                 VALUES ('migration.proof', 'survives-v21');",
        )
        .expect("seed v20 rows");
        conn.pragma_update(None, "user_version", 21)
            .expect("version");
    }

    let store = SqliteStore::open(&db).expect("migrate v21 to current");
    assert_eq!(
        store
            .get_setting("migration.proof")
            .await
            .expect("read proof")
            .as_deref(),
        Some("survives-v21")
    );
    let prior = store
        .observe_network_prior(&NetworkPriorObservation {
            credential_generation: CredentialGeneration::from("v22-test-gen".to_owned()),
            client_class: "safari".to_owned(),
            network_fingerprint: "10.0.0.0/24".to_owned(),
            throughput_kbps: Some(8_000),
            observed_at_ms: 2_000_000_000_000,
            ..NetworkPriorObservation::default()
        })
        .await
        .expect("write v22 prior");
    assert_eq!(prior.sustained_kbps, Some(8_000));
    assert_eq!(prior.credential_generation.as_str(), "v22-test-gen");

    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    let old_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM network_priors", [], |row| row.get(0))
        .expect("count new-format rows");
    assert_eq!(old_rows, 1, "legacy numeric-key rows must be dropped");
}

#[tokio::test]
async fn v23_adds_monotone_job_leases_without_losing_v22_state() {
    use crate::cluster::coordination::LeaseClaim;
    use crate::store::{CoordinationStore, SettingsStore};

    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(22) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives-v22')",
            [],
        )
        .expect("seed v22 row");
        conn.pragma_update(None, "user_version", 22)
            .expect("version");
    }

    let store = SqliteStore::open(&db).expect("migrate v22 to v23");
    assert_eq!(
        store
            .get_setting("migration.proof")
            .await
            .expect("read v22 proof")
            .as_deref(),
        Some("survives-v22")
    );
    let lease = store
        .acquire_lease("migration-proof", "node-a", 100, 200)
        .await
        .expect("acquire migrated lease");
    assert!(matches!(lease, LeaseClaim::Acquired(lease) if lease.fence == 1));

    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('job_leases')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("job lease columns"),
        6
    );
}

#[tokio::test]
async fn v24_adds_the_pretranscode_queue_without_losing_v23_state() {
    use crate::store::SettingsStore;

    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(23) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives-v23')",
            [],
        )
        .expect("seed v23 row");
        conn.pragma_update(None, "user_version", 23)
            .expect("version");
    }

    let store = SqliteStore::open(&db).expect("migrate v23 to v24");
    assert_eq!(
        store
            .get_setting("migration.proof")
            .await
            .expect("read v23 proof")
            .as_deref(),
        Some("survives-v23")
    );
    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('pretranscode_jobs')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("queue columns"),
        24
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('transcode_cache_locations')
                  WHERE name = 'manifest_digest'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("cache manifest column"),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('transcode_cache_locations')
                  WHERE name = 'scrub_object_index'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("cache scrub cursor column"),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master
                  WHERE type = 'trigger' AND name = 'pretranscode_jobs_cancel_source'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("source trigger"),
        1
    );
}

#[tokio::test]
async fn v25_adds_media_session_routing_without_losing_v24_state() {
    use crate::store::SettingsStore;

    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(24) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('migration.session.proof', 'survives-v24')",
            [],
        )
        .expect("seed v24 row");
        conn.pragma_update(None, "user_version", 24)
            .expect("version");
    }

    let store = SqliteStore::open(&db).expect("migrate v24 to v25");
    assert_eq!(
        store
            .get_setting("migration.session.proof")
            .await
            .expect("read v24 proof")
            .as_deref(),
        Some("survives-v24")
    );
    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    for (table, columns) in [
        ("media_session_requests", 10),
        // 5 since v49 added `desired_revision`, the ask a pointer write
        // was decided against.
        ("media_playback_pointers", 5),
        // 21 after v51's drain deadline and v53's recovery epoch.
        ("media_sessions", 21),
    ] {
        assert_eq!(
            conn.query_row(
                &format!("SELECT COUNT(*) FROM pragma_table_info('{table}')"),
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or_else(|error| panic!("inspect {table}: {error}")),
            columns,
            "{table} schema"
        );
    }
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index'
                   AND name IN ('media_session_requests_expiry', 'media_sessions_owner',
                                'media_sessions_user', 'media_sessions_expiry',
                                'media_sessions_retention')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("inspect media-session indexes"),
        5
    );
}

#[test]
fn v26_backfills_storage_identity_and_repairs_rolling_legacy_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        conn.pragma_update(None, "foreign_keys", "ON").expect("fk");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(25) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.execute_batch(
            "INSERT INTO settings (key, value) VALUES ('instance.id', 'migration-v26');
                 INSERT INTO users (id, username, password_hash, is_admin)
                    VALUES (1, 'migration', 'hash', 1);
                 INSERT INTO libraries (id, name, kind, paths, anime)
                    VALUES (1, 'Migration', 'movies', '[]', 0);
                 INSERT INTO items (id, library_id, kind, title, sort_title)
                    VALUES (1, 1, 'movie', 'Migration', 'migration');
                 INSERT INTO files (id, item_id, path, size, mtime)
                    VALUES (1, 1, '/migration.mkv', 10, 20);
                 INSERT INTO transcode_cache_recipes
                    (recipe_hash, file_id, recipe_version)
                    VALUES ('legacy-recipe', 1, 1);
                 INSERT INTO transcode_cache_locations
                    (recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
                     manifest_digest, scrub_object_index, last_used_at, last_seen_at)
                    VALUES ('legacy-recipe', 'node-a', 'local', 'legacy-generation',
                            100, 1, NULL, 0, 30, 30);",
        )
        .expect("seed exact v25 location");
        conn.pragma_update(None, "user_version", 25)
            .expect("version");
    }

    drop(SqliteStore::open(&db).expect("migrate v25 to v26"));
    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row(
            "SELECT storage_id, generation_id FROM transcode_cache_locations
                  WHERE recipe_hash = 'legacy-recipe'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("backfilled identity"),
        (
            "node:node-a:cache".to_owned(),
            "legacy-generation".to_owned()
        )
    );
    conn.execute(
        "INSERT INTO transcode_cache_recipes (recipe_hash, file_id, recipe_version)
             VALUES ('rolling-recipe', 1, 1)",
        [],
    )
    .expect("seed rolling recipe");
    conn.execute(
        "INSERT INTO transcode_cache_locations
                (recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
                 manifest_digest, scrub_object_index, last_used_at, last_seen_at)
             VALUES ('rolling-recipe', 'node-b', 'local', 'rolling-generation',
                     0, 0, NULL, 0, 40, 40)",
        [],
    )
    .expect("legacy incomplete write on v26 schema");
    assert_eq!(
        conn.query_row(
            "SELECT storage_id, generation_id FROM transcode_cache_locations
                  WHERE recipe_hash = 'rolling-recipe'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("trigger repaired identity"),
        (
            "node:node-b:cache".to_owned(),
            "rolling-generation".to_owned()
        )
    );
    conn.execute(
        "INSERT INTO transcode_cache_locations
                (recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
                 manifest_digest, scrub_object_index, last_used_at, last_seen_at)
             VALUES ('rolling-recipe', 'node-b', 'local', 'rolling-generation-2',
                     0, 0, NULL, 0, 50, 50)
             ON CONFLICT(recipe_hash, node_id, storage_class) DO UPDATE SET
                relative_dir = excluded.relative_dir,
                bytes = excluded.bytes,
                complete = excluded.complete,
                last_used_at = excluded.last_used_at,
                last_seen_at = excluded.last_seen_at",
        [],
    )
    .expect("legacy binary upsert on v26 schema");
    assert_eq!(
        conn.query_row(
            "SELECT storage_id, generation_id FROM transcode_cache_locations
                  WHERE recipe_hash = 'rolling-recipe'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .expect("update trigger repaired generation identity"),
        (
            "node:node-b:cache".to_owned(),
            "rolling-generation-2".to_owned(),
        )
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'
                   AND name IN ('cache_storage_members', 'cache_consumer_pins')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("shared cache tables"),
        2
    );
    assert!(conn
        .query_row(
            "SELECT sql FROM sqlite_master
                  WHERE type = 'index' AND name = 'transcode_cache_storage_generation'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("storage generation index")
        .contains("WHERE storage_id <> '' AND generation_id <> ''"));
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger'
                   AND name IN ('transcode_cache_location_identity_ai',
                                'transcode_cache_location_identity_au')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("rolling identity triggers"),
        2
    );
}

/// v13 adds a column to `items`, which is the migration shape with a
/// silent failure mode: `ITEM_COLS` and `ITEM_COL_COUNT` are positional,
/// and four queries select `ITEM_COLS` and then read their own trailing
/// columns at `ITEM_COL_COUNT + n`. Get the count wrong and none of them
/// errors — `search_items` simply starts reporting a JSON array of genres
/// as the show title, because both are TEXT.
///
/// So this checks the upgrade twice over: the row survives with the new
/// column defaulted, and every consumer of the positional offsets still
/// reads what it meant to.
#[tokio::test]
async fn v13_adds_genres_without_shifting_the_positional_readers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(12) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|e| panic!("v{}: {e}", index + 1));
        }
        conn.pragma_update(None, "user_version", 12)
            .expect("version");
        conn.execute_batch(
            "INSERT INTO users (id, username, password_hash, is_admin)
                     VALUES (1, 'paul', 'hash', 1);
                 INSERT INTO libraries (id, name, kind, paths, anime)
                     VALUES (1, 'TV', 'shows', '[\"/media/tv\"]', 0);
                 INSERT INTO items (id, library_id, kind, parent_id, title, sort_title,
                                    tags, metadata_at, poster_path)
                     VALUES (10, 1, 'show', NULL, 'Severance', 'severance', '[\"x\"]', 1, NULL),
                            (11, 1, 'season', 10, 'Season 1', 'season 1', '[]', 1, 'season.jpg'),
                            (12, 1, 'episode', 11, 'Good News', 'good news', '[]', 1, NULL);
                 INSERT INTO files (id, item_id, path, size, mtime)
                     VALUES (100, 12, '/media/tv/s01e01.mkv', 42, 7);
                 INSERT INTO watch_state (user_id, item_id, position_ms, watched)
                     VALUES (1, 12, 61000, 0);",
        )
        .expect("seed v12");
    }

    // Open through the real binary path — this applies v13.
    let store = SqliteStore::open(&db).expect("migrate");
    let conn = Connection::open(&db).expect("raw reopen");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert_eq!(version, MIGRATIONS.len() as i64, "all migrations applied");

    // The new column defaults, and the neighbouring JSON list is untouched.
    let (genres, tags): (String, String) = conn
        .query_row("SELECT genres, tags FROM items WHERE id = 10", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("columns");
    assert_eq!((genres.as_str(), tags.as_str()), ("[]", "[\"x\"]"));

    let item = store.get_item(10).await.expect("get").expect("show");
    assert!(item.genres.is_empty(), "an upgraded row has no genres yet");
    assert_eq!(item.tags, vec!["x".to_owned()]);

    // The four positional readers. Each appends its own columns after
    // `ITEM_COLS`; a stale `ITEM_COL_COUNT` makes them read the genres
    // column instead, with no error to notice.
    store
        .apply_metadata(
            10,
            &MetadataPatch {
                genres: Some(vec!["Drama".into(), "Mystery".into()]),
                ..Default::default()
            },
        )
        .await
        .expect("patch genres");

    let recent = store.recently_added(Some(1), 10).await.expect("recent");
    let episode = recent
        .iter()
        .find(|r| r.item.id == 12)
        .expect("the episode is on the rail");
    assert_eq!(
        episode.show_title.as_deref(),
        Some("Severance"),
        "recently_added must read the show title, not the genres column"
    );
    assert_eq!(episode.season_poster.as_deref(), Some("season.jpg"));

    let hits = store.search_items("severance", 10).await.expect("search");
    assert!(!hits.is_empty(), "the show is still findable");
    assert!(
        hits.iter()
            .all(|h| h.show_title.is_none() || h.show_title.as_deref() == Some("Severance")),
        "search_items must read the show title, not the genres column: {:?}",
        hits.iter()
            .map(|h| h.show_title.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        hits.iter()
            .find(|h| h.item.id == 10)
            .map(|h| h.item.genres.clone()),
        Some(vec!["Drama".to_owned(), "Mystery".to_owned()]),
        "and it must read the genres themselves as genres"
    );

    let in_progress = store.continue_watching(1, 10).await.expect("continue");
    let entry = in_progress
        .iter()
        .find(|r| r.item.id == 12)
        .expect("the part-watched episode");
    assert_eq!(
        entry.show_title.as_deref(),
        Some("Severance"),
        "continue_watching must read the show title, not the genres column"
    );
    assert_eq!(
        entry.state.position_ms, 61000,
        "and its watch state after that"
    );
    assert_eq!(entry.season_poster.as_deref(), Some("season.jpg"));

    let next = store.next_up(1, 10).await.expect("next up");
    for r in &next {
        assert!(
            r.show_title.is_none() || r.show_title.as_deref() == Some("Severance"),
            "next_up must read the show title, not the genres column: {:?}",
            r.show_title
        );
    }
}

#[tokio::test]
async fn v14_offline_packages_do_not_cascade_with_rescanned_files() {
    let store = SqliteStore::open_in_memory().expect("store");
    let conn = store.conn.lock().expect("connection");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("version");
    assert_eq!(version, MIGRATIONS.len() as i64);
    assert!(
        version >= 14,
        "the offline-package migration must be present"
    );

    let file_foreign_keys: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_foreign_key_list('offline_packages') \
                 WHERE \"table\" = 'files'",
            [],
            |row| row.get(0),
        )
        .expect("foreign keys");
    assert_eq!(
        file_foreign_keys, 0,
        "a rescan must not cascade-delete package state"
    );
    let package_id_unique: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_index_list('offline_package_leases') \
                 WHERE \"unique\" = 1",
            [],
            |row| row.get(0),
        )
        .expect("lease indexes");
    assert!(package_id_unique > 0, "one stable lease per package");
}

#[test]
fn v54_migration_rejects_legacy_offline_claim_and_publication_sql() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw v53 open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(53) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.pragma_update(None, "user_version", 53)
            .expect("v53 marker");
        conn.execute(
            "INSERT INTO users (id, username, password_hash, is_admin)
                 VALUES (1, 'legacy-offline', 'hash', 1)",
            [],
        )
        .expect("seed user");
        conn.execute_batch(
            "INSERT INTO libraries (id, name, kind, paths, anime)
                    VALUES (1, 'Legacy offline', 'movies', '[]', 0);
                 INSERT INTO items (id, library_id, kind, title, sort_title)
                    VALUES (42, 1, 'movie', 'Legacy offline', 'legacy offline');
                 INSERT INTO files (id, item_id, path, size, mtime)
                    VALUES (42, 42, '/media/legacy.mkv', 100, 1);",
        )
        .expect("seed cache recipe parents");
        conn.execute(
            "INSERT INTO offline_packages
                    (id, request_id, user_id, file_id, node_id, source_path,
                     source_size, source_mtime, target_height, subtitle_mode,
                     state, phase, estimated_bytes, reserved_bytes, expires_at)
                 VALUES ('legacy-package', 'legacy-request', 1, 42, 'legacy-node',
                         '/media/legacy.mkv', 100, 1, 720, 'none', 'queued',
                         'waiting_for_encoder', 50, 50, 999999)",
            [],
        )
        .expect("seed v53 package");
        conn.execute(
            "INSERT INTO transcode_cache_recipes (recipe_hash, file_id, recipe_version)
                 VALUES ('legacy-recipe', 42, 1)",
            [],
        )
        .expect("seed recipe");
        conn.execute(
            "INSERT INTO transcode_cache_locations
                    (recipe_hash, node_id, storage_class, relative_dir, bytes, complete)
                 VALUES ('legacy-recipe', 'legacy-node', 'local', 'legacy-recipe', 0, 0)",
            [],
        )
        .expect("seed incomplete cache location");
    }

    SqliteStore::open(&db).expect("migrate v53 through offline fences");
    let conn = Connection::open(&db).expect("raw current reopen");
    assert!(conn
        .execute(
            "UPDATE offline_packages SET state = 'preparing'
                 WHERE id = 'legacy-package' AND state = 'queued'",
            [],
        )
        .is_err());
    conn.execute(
        "UPDATE offline_packages
             SET state = 'preparing', claim_generation = claim_generation + 1
             WHERE id = 'legacy-package' AND state = 'queued'",
        [],
    )
    .expect("current claim shape");
    assert!(conn
        .execute(
            "UPDATE offline_packages SET state = 'queued'
                 WHERE id = 'legacy-package' AND state = 'preparing'",
            [],
        )
        .is_err());
    assert!(conn
        .execute(
            "UPDATE offline_packages SET state = 'ready', recipe_hash = 'legacy-recipe'
                 WHERE id = 'legacy-package'",
            [],
        )
        .is_err());
    assert!(conn
        .execute(
            "UPDATE offline_packages SET state = 'failed'
                 WHERE id = 'legacy-package'",
            [],
        )
        .is_err());
    assert!(conn
        .execute(
            "UPDATE transcode_cache_locations
                 SET complete = 1, bytes = 100
                 WHERE recipe_hash = 'legacy-recipe' AND node_id = 'legacy-node'",
            [],
        )
        .is_err());
    conn.execute(
        "UPDATE transcode_cache_locations
             SET complete = 1, bytes = 100,
                 publication_generation = publication_generation + 1
             WHERE recipe_hash = 'legacy-recipe' AND node_id = 'legacy-node'",
        [],
    )
    .expect("current cache publication shape");
    assert!(conn
        .execute(
            "UPDATE transcode_cache_locations SET bytes = 101
                 WHERE recipe_hash = 'legacy-recipe' AND node_id = 'legacy-node'",
            [],
        )
        .is_err());
}

#[tokio::test]
async fn refuses_databases_from_the_future() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    SqliteStore::open(&db).expect("create");

    {
        let conn = Connection::open(&db).expect("raw open");
        conn.pragma_update(None, "user_version", 9999)
            .expect("bump");
    }

    match SqliteStore::open(&db).map(|_| ()) {
        Err(StoreError::Migration(msg)) => assert!(msg.contains("newer")),
        other => panic!("expected migration error, got {other:?}"),
    }
}

#[test]
fn v31_adds_analysis_requests_to_a_real_v30_shape() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(30) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.pragma_update(None, "user_version", 30)
            .expect("v30 marker");
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master
                      WHERE type = 'table' AND name = 'analysis_requests'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("v30 shape"),
            0,
            "the historical v30 fixture must not contain the v31 table"
        );
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives')",
            [],
        )
        .expect("seed v30 state");
    }

    SqliteStore::open(&db).expect("migrate v30 to v31");
    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    assert_eq!(
        conn.query_row(
            "SELECT value FROM settings WHERE key = 'migration.proof'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("migration proof"),
        "survives"
    );
    for object in [
        "analysis_requests",
        "analysis_requests_due",
        "analysis_requests_status",
        "analysis_requests_one_active_source",
        "analysis_requests_cancel_source",
        "analysis_requests_supersede_source",
        "analysis_requests_bound_terminal_history",
    ] {
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                [object],
                |row| row.get::<_, i64>(0),
            )
            .expect("schema object"),
            1,
            "missing v31 schema object {object}"
        );
    }
}

#[test]
fn v32_adds_terminal_control_acknowledgements_without_losing_v31_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(31) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.pragma_update(None, "user_version", 31)
            .expect("v31 marker");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives-v31')",
            [],
        )
        .expect("seed v31 state");
    }

    SqliteStore::open(&db).expect("migrate v31 to v32");
    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    assert_eq!(
        conn.query_row(
            "SELECT value FROM settings WHERE key = 'migration.proof'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("migration proof"),
        "survives-v31"
    );
    for object in [
        "media_session_terminal_acks",
        "media_session_terminal_acks_expiry",
    ] {
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                [object],
                |row| row.get::<_, i64>(0),
            )
            .expect("schema object"),
            1,
            "missing v32 schema object {object}"
        );
    }
}

#[test]
fn v39_to_v41_add_annotations_and_widen_analysis_without_losing_requests() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(38) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.pragma_update(None, "user_version", 38)
            .expect("v38 marker");
        conn.execute(
            "INSERT INTO analysis_requests
                    (request_id, file_id, source_size, source_mtime, component,
                     force_rebuild, target_node_id, state, not_before_ms,
                     created_at_ms, updated_at_ms)
                 VALUES ('preserved-v38', 7, 100, 10, 'fragment_index', 0,
                         'node-a', 'queued', 20, 20, 20)",
            [],
        )
        .expect("seed v38 analysis request");
    }

    SqliteStore::open(&db).expect("migrate v38 through annotations and component widening");
    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    assert_eq!(
        conn.query_row(
            "SELECT component FROM analysis_requests WHERE request_id = 'preserved-v38'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("preserved request"),
        "fragment_index"
    );
    conn.execute(
        "INSERT INTO analysis_requests
                (request_id, file_id, source_size, source_mtime, component,
                 force_rebuild, target_node_id, state, not_before_ms,
                 created_at_ms, updated_at_ms)
             VALUES ('semantic-v41', 8, 100, 10, 'skip_markers', 0,
                     'node-a', 'queued', 21, 21, 21)",
        [],
    )
    .expect("widened component accepts semantic work");
    for table in [
        "timeline_annotation_sets",
        "timeline_manual_overrides",
        "analysis_lifecycle_counters",
    ] {
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get::<_, i64>(0),
            )
            .expect("timeline table"),
            1,
            "missing {table}"
        );
    }
}

#[test]
fn v41_schema_commit_with_a_stale_marker_recovers_without_replaying_ddl() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(41) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.pragma_update(None, "user_version", 40)
            .expect("simulate interruption after the v41 schema commit");
    }

    SqliteStore::open(&db).expect("settle the committed v41 migration");
    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    assert!(SqliteStore::analysis_component_schema_is_current(&conn)
        .expect("inspect current analysis schema"));
}

/// A live session written before the drain column survives gaining it, and
/// gains it as null rather than as a deadline in 1970.
///
/// The second half of the test is the guard, not the column. A migration
/// commits its own transaction and only then bumps `user_version`, so a
/// crash in that window leaves the shape applied and the marker behind —
/// and `ADD COLUMN` replayed onto a column that is already there fails
/// permanently, which is a database nobody can open again. v41, v45, v46
/// and v47 each carry this guard for the same reason; a step that adds a
/// column and does not is one power cut away from that.
#[test]
fn v51_adds_a_drain_deadline_to_a_real_v50_shape_and_survives_a_stale_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(50) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.pragma_update(None, "user_version", 50)
            .expect("v50 marker");
        conn.execute(
            "INSERT INTO media_sessions (
                     incarnation_id, session_id, user_id, playback_id,
                     request_fingerprint, owner_node_id, owner_epoch,
                     lease_expires_at_ms, state, recipe_json, response_json,
                     updated_at_ms)
                 VALUES ('inc-v50', 'sess-v50', 7, 'playback-v50', 'fingerprint',
                         'node-a', 1, 5000, 'active', '{}', '{}', 4000)",
            [],
        )
        .expect("seed a v50 session");
    }

    {
        let conn = Connection::open(&db).expect("raw reopen");
        conn.execute_batch(&format!("BEGIN;\n{}\nCOMMIT;", MIGRATIONS[50]))
            .expect("commit the v51 shape without advancing its marker");
    }

    SqliteStore::open(&db).expect("settle the committed v51 migration");
    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    assert!(SqliteStore::drain_deadline_column_exists(&conn)
        .expect("inspect the drain deadline column"));
    let (state, deadline) = conn
        .query_row(
            "SELECT state, drain_deadline_ms FROM media_sessions
                  WHERE incarnation_id = 'inc-v50'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .expect("the v50 session survives");
    assert_eq!(state, "active", "the migration is additive");
    assert_eq!(
            deadline, None,
            "a session that existed before the column is not draining, and null is how this schema says so"
        );
}

/// A binary that predates the drain cannot take a draining session over.
///
/// The application SQL in both backends excludes a non-null deadline from
/// takeover, and that fences this binary only. Compatibility is checked
/// when a database is opened and never again, so every node still running
/// the previous binary during a rolling upgrade keeps issuing takeover
/// SQL from before the column existed. This runs exactly that statement —
/// the v49-era CAS, with no drain predicate in it — against a draining
/// row, and the trigger is the only thing in its path.
///
/// `RAISE(IGNORE)`, so the statement reports zero rows changed rather
/// than raising. That is the answer the old binary already knows how to
/// read: its takeover checks for exactly one row and rolls back when it
/// does not get it, which is how it loses a race to another survivor.
#[test]
fn a_draining_session_refuses_the_ownership_move_an_older_binary_would_make() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    SqliteStore::open(&db).expect("install current schema");
    let conn = Connection::open(&db).expect("raw open");
    conn.execute(
        "INSERT INTO media_sessions (
                 incarnation_id, session_id, user_id, playback_id,
                 request_fingerprint, owner_node_id, owner_epoch,
                 lease_expires_at_ms, state, recipe_json, response_json,
                 updated_at_ms, drain_deadline_ms)
             VALUES ('inc-drain', 'sess-drain', 3, 'playback-drain', 'fingerprint',
                     'node-a', 4, 5000, 'active', '{}', '{}', 4000, 9000)",
        [],
    )
    .expect("seed a draining session");

    // The pre-v51 takeover CAS, verbatim in the shape that matters: it
    // names the owner and epoch it expects and says nothing about a drain.
    let moved = conn
        .execute(
            "UPDATE media_sessions
                    SET owner_node_id = 'node-b', owner_epoch = 5,
                        lease_expires_at_ms = 20000,
                        discontinuity_sequence = discontinuity_sequence + 1,
                        updated_at_ms = 10000
                  WHERE incarnation_id = 'inc-drain' AND owner_node_id = 'node-a'
                    AND owner_epoch = 4 AND state = 'active'",
            [],
        )
        .expect("the old statement runs; it simply changes nothing");
    assert_eq!(
        moved, 0,
        "an ownership move on a draining session must affect no rows"
    );
    let (owner, epoch, state): (String, i64, String) = conn
        .query_row(
            "SELECT owner_node_id, owner_epoch, state FROM media_sessions
                  WHERE incarnation_id = 'inc-drain'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read the row back");
    assert_eq!(
        (owner.as_str(), epoch),
        ("node-a", 4),
        "ownership is intact"
    );
    assert_eq!(state, "active", "and nothing else about the row moved");

    // The owner keeps renewing this row for the whole drain, and the
    // sweep and the owner's own end both write `state`. Fencing any of
    // those would break the drain rather than protect it.
    assert_eq!(
        conn.execute(
            "UPDATE media_sessions SET lease_expires_at_ms = 30000, updated_at_ms = 11000
                  WHERE incarnation_id = 'inc-drain'",
            [],
        )
        .expect("renew a draining session"),
        1,
        "renewal is untouched"
    );
    assert_eq!(
        conn.execute(
            "UPDATE media_sessions SET state = 'ended', terminal_reason = 'superseded',
                        updated_at_ms = 12000
                  WHERE incarnation_id = 'inc-drain'",
            [],
        )
        .expect("end a draining session"),
        1,
        "ending it is untouched"
    );
}

#[test]
fn v32_upgrades_through_analysis_indexes_terminal_reason_and_publication_fence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(32) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.pragma_update(None, "user_version", 32)
            .expect("v32 marker");
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('migration.proof', 'survives-v32')",
            [],
        )
        .expect("seed v32 state");
    }

    SqliteStore::open(&db).expect("migrate v32 to current");
    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    assert_eq!(
        conn.query_row(
            "SELECT value FROM settings WHERE key = 'migration.proof'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("migration proof"),
        "survives-v32"
    );
    for object in [
        "analysis_requests_result_history",
        "cluster_fragment_index_jobs_status_history",
    ] {
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                [object],
                |row| row.get::<_, i64>(0),
            )
            .expect("schema object"),
            1,
            "missing v33 schema object {object}"
        );
    }
    let mut plan = conn
        .prepare(
            "EXPLAIN QUERY PLAN
                 SELECT cache_key FROM (
                   SELECT cache_key, updated_at_ms FROM (
                     SELECT cache_key, updated_at_ms FROM cluster_fragment_index_jobs
                      WHERE state = 'ready'
                      ORDER BY updated_at_ms DESC, cache_key LIMIT 8192)
                   UNION ALL
                   SELECT cache_key, updated_at_ms FROM (
                     SELECT cache_key, updated_at_ms FROM cluster_fragment_index_jobs
                      WHERE state = 'failed'
                      ORDER BY updated_at_ms DESC, cache_key LIMIT 8192)
                   UNION ALL
                   SELECT cache_key, updated_at_ms FROM (
                     SELECT cache_key, updated_at_ms FROM cluster_fragment_index_jobs
                      WHERE state = 'cancelled'
                      ORDER BY updated_at_ms DESC, cache_key LIMIT 8192)
                   ORDER BY updated_at_ms DESC, cache_key LIMIT 8192)",
        )
        .expect("prepare summary terminal-window plan");
    let details = plan
        .query_map([], |row| row.get::<_, String>(3))
        .expect("query summary terminal-window plan")
        .collect::<Result<Vec<_>, _>>()
        .expect("summary terminal-window plan rows");
    assert_eq!(
        details
            .iter()
            .filter(|detail| { detail.contains("cluster_fragment_index_jobs_status_history") })
            .count(),
        3,
        "each terminal state must have its own bounded newest-first index walk: {details:?}"
    );
    let columns = conn
        .prepare("PRAGMA table_info(media_sessions)")
        .expect("prepare media-session columns")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("read media-session columns")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect media-session columns");
    assert!(columns.iter().any(|column| column == "terminal_reason"));
    assert!(columns
        .iter()
        .any(|column| column == "publication_ready_at_ms"));
}

#[test]
fn v49_adds_the_terminal_analysis_identity_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(49) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.pragma_update(None, "user_version", 49)
            .expect("v49 marker");
    }

    SqliteStore::open(&db).expect("migrate v49 to current");
    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master
                  WHERE type = 'index' AND name = 'analysis_requests_terminal_identity'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("terminal identity index"),
        1
    );
}

#[test]
fn v64_adds_field_order_to_the_existing_files_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        for (index, sql) in MIGRATIONS.iter().enumerate().take(63) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.pragma_update(None, "user_version", 63)
            .expect("v63 marker");
    }

    SqliteStore::open(&db).expect("migrate v63 to current");
    let conn = Connection::open(&db).expect("raw reopen");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("version"),
        SQLITE_SCHEMA_VERSION
    );
    let columns = conn
        .prepare("PRAGMA table_info(files)")
        .expect("prepare files columns")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("read files columns")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect files columns");
    assert!(columns.iter().any(|column| column == "field_order"));
}

#[test]
fn v92_offline_audio_snapshot_upgrade_preserves_legacy_package() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("plurx.db");
    {
        let conn = Connection::open(&db).expect("raw open");
        // v92 adds `offline_packages.audio_recipe`; a v91 database is the
        // last shape without it.
        for (index, sql) in MIGRATIONS.iter().enumerate().take(91) {
            conn.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
                .unwrap_or_else(|error| panic!("v{}: {error}", index + 1));
        }
        conn.execute("INSERT INTO users (id, username, password_hash) VALUES (1, 'synthetic-audio', 'unused-test-hash')", []).expect("owned test user");
        conn.execute("INSERT INTO offline_packages (id, request_id, user_id, file_id, node_id, source_path, source_size, source_mtime, target_height, subtitle_mode, state, phase, expires_at) VALUES ('legacy-audio', 'legacy-request', 1, 1, 'owned-node', '/synthetic/source.mkv', 4096, 1, 720, 'none', 'queued', 'queued', 10000)", []).expect("legacy package");
        conn.pragma_update(None, "user_version", 91)
            .expect("v91 marker");
    }
    SqliteStore::open(&db).expect("migrate legacy snapshot");
    let conn = Connection::open(&db).expect("raw reopen");
    let (path, audio) = conn
        .query_row(
            "SELECT source_path, audio_recipe FROM offline_packages WHERE id = 'legacy-audio'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .expect("retained package");
    assert_eq!(path, "/synthetic/source.mkv");
    assert_eq!(audio, None, "upgrade must not invent a new audio recipe");
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("upgraded version"),
        SQLITE_SCHEMA_VERSION
    );
}
