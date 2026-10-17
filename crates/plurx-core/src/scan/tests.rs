use std::path::PathBuf;

use super::*;
use crate::domain::{ItemEdit, ItemSort, NewLibrary};
use crate::store::{LibraryStore, MediaStore, SqliteStore, UserStore, WatchStore};

async fn write_fake_video(dir: &Path, rel: &str) -> PathBuf {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    // Content is irrelevant — probe will fail gracefully and the scanner
    // still records the file and builds the hierarchy.
    std::fs::write(&path, b"not really video").expect("write");
    // Return what the SCANNER will report, not what we happened to join.
    // scan_path canonicalizes on purpose (see the roots-escape test), and
    // on macOS TMPDIR is `/var/folders/...` — a symlink to
    // `/private/var/folders/...`. Without this, every path assertion here
    // passes on Linux and fails on a Mac.
    path.canonicalize().expect("canonicalize")
}

#[tokio::test]
async fn walker_pages_are_bounded_and_ordered() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut expected = Vec::new();
    for index in (0..1_100).rev() {
        let path = dir.path().join(format!("Movie {index:04} (2020).mkv"));
        std::fs::write(&path, b"").expect("write candidate");
        expected.push(path);
    }
    expected.sort();

    let (mut pages, walker) = spawn_walker(
        vec![dir.path().to_path_buf()],
        WalkFilter {
            kind: LibraryKind::Movies,
        },
    );
    let mut candidates = Vec::new();
    while let Some(page) = pages.recv().await {
        assert!(page.len() <= WALK_PAGE, "page exceeded its fixed bound");
        for event in page {
            if let WalkEvent::Candidate(path) = event {
                candidates.push(path);
            }
        }
    }
    walker.await.expect("walker");
    candidates.sort();

    assert_eq!(candidates, expected);
    assert_eq!(candidates.len(), 1_100);
}

#[tokio::test]
async fn dropping_the_scan_stops_the_walker() {
    let dir = tempfile::tempdir().expect("tmp");
    for index in 0..5_000 {
        std::fs::write(dir.path().join(format!("Document {index:04}.txt")), b"")
            .expect("write non-media entry");
    }
    let visits = std::sync::Arc::new(AtomicUsize::new(0));
    let hook_visits = std::sync::Arc::clone(&visits);
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let (pages, walker) = spawn_walker_with_entry_hook(
        vec![dir.path().to_path_buf()],
        WalkFilter {
            kind: LibraryKind::Movies,
        },
        move || {
            if hook_visits.fetch_add(1, Ordering::SeqCst) == 0 {
                started_tx.send(()).expect("signal first walked entry");
            }
            std::thread::sleep(Duration::from_millis(2));
        },
    );
    tokio::task::spawn_blocking(move || started_rx.recv())
        .await
        .expect("first-entry waiter")
        .expect("walker started");
    drop(pages);
    tokio::time::timeout(Duration::from_secs(1), walker)
        .await
        .expect("sparse orphaned walker should observe the dropped consumer")
        .expect("walker");
    assert!(
        visits.load(Ordering::SeqCst) < 64,
        "cancellation must not wait for a page of matching media"
    );
}

#[tokio::test]
async fn cooperative_scan_cancellation_joins_a_sparse_walker() {
    let dir = tempfile::tempdir().expect("tmp");
    for index in 0..100 {
        std::fs::write(dir.path().join(format!("sidecar-{index}.txt")), b"").expect("sidecar");
    }
    let cancellation = tokio_util::sync::CancellationToken::new();
    let signal = cancellation.clone();
    let visits = std::sync::Arc::new(AtomicUsize::new(0));
    let observed = visits.clone();
    crate::process::bounded::cancellable(cancellation, async {
        let (mut pages, walker) = spawn_walker_with_entry_hook(
            vec![dir.path().to_path_buf()],
            WalkFilter {
                kind: LibraryKind::Movies,
            },
            move || {
                observed.fetch_add(1, Ordering::SeqCst);
                signal.cancel();
            },
        );
        while pages.recv().await.is_some() {}
        walker.await.expect("joined cancelled walker");
        assert!(
            check_scan_cancellation().is_err(),
            "partial census cannot reconcile"
        );
    })
    .await;
    assert_eq!(visits.load(Ordering::SeqCst), 1);
}

#[test]
fn scan_phase_metrics_have_bounded_labels() {
    let metrics = prometheus_scan_walk();
    assert!(metrics.contains("phase=\"walk\""));
    assert!(metrics.contains("phase=\"process\""));
    assert!(metrics.contains("le=\"900\""));
    assert!(metrics.contains("le=\"+Inf\""));
}

// ---- targeted scan (integration plan P2) ---------------------------

async fn movie_library(store: &SqliteStore, dir: &Path) -> Library {
    store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![dir.to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib")
}

async fn show_library(store: &SqliteStore, dir: &Path) -> Library {
    store
        .create_library(&NewLibrary {
            name: "Shows".into(),
            kind: LibraryKind::Shows,
            paths: vec![dir.to_path_buf()],
            anime: false,
        })
        .await
        .expect("show library")
}

async fn only_show(store: &SqliteStore, library_id: i64) -> Item {
    let page = store
        .list_top_items(library_id, ItemSort::Title, 0, 10)
        .await
        .expect("shows");
    assert_eq!(page.total, 1, "expected one show: {:?}", page.items);
    page.items.into_iter().next().expect("show")
}

async fn only_episode(store: &SqliteStore, show_id: i64) -> Item {
    let seasons = store.get_item_children(show_id).await.expect("seasons");
    assert_eq!(seasons.len(), 1, "expected one season: {seasons:?}");
    let episodes = store
        .get_item_children(seasons[0].id)
        .await
        .expect("episodes");
    assert_eq!(episodes.len(), 1, "expected one episode: {episodes:?}");
    episodes.into_iter().next().expect("episode")
}

#[tokio::test]
async fn transient_probe_is_left_unprobed_and_retried_on_the_next_scan() {
    let store = SqliteStore::open_in_memory().expect("store");
    let directory = tempfile::tempdir().expect("media fixture");
    let path = write_fake_video(directory.path(), "Heat (1995).mkv").await;
    let library = movie_library(&store, directory.path()).await;
    let fixture = probe::tests::install_fixture(
        &path,
        probe::tests::FixtureMode::SleepThenJson,
        std::time::Duration::from_millis(200),
    );

    let first = scan_library(&store, &library).await.expect("initial scan");
    assert_eq!(first.added, 1);
    assert_eq!(first.degraded, 1);
    let item = store
        .list_top_items(library.id, ItemSort::Title, 0, 10)
        .await
        .expect("movie list")
        .items
        .into_iter()
        .next()
        .expect("movie item");
    assert!(
        !store
            .files_for_item(item.id)
            .await
            .expect("stored files")
            .into_iter()
            .next()
            .expect("file row")
            .probed
    );

    let second = scan_library(&store, &library).await.expect("repair scan");
    assert_eq!(second.unchanged, 1);
    assert_eq!(second.repaired, 1);
    assert!(
        store
            .files_for_item(item.id)
            .await
            .expect("stored files")
            .into_iter()
            .next()
            .expect("file row")
            .probed
    );
    assert_eq!(fixture.calls.load(Ordering::Relaxed), 2);
}

#[test]
fn probe_outcome_metrics_have_only_the_fixed_result_labels() {
    let metrics = prometheus_probe_outcomes();
    for outcome in ["ok", "failed", "transient", "parse"] {
        assert!(metrics.contains(&format!("outcome=\"{outcome}\"")));
    }
    assert_eq!(
        metrics.matches("plurx_scan_probe_outcomes_total{").count(),
        4
    );
}

#[tokio::test]
async fn books_library_keeps_text_and_multi_file_audio_as_first_class_items() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(dir.path(), "Ursula K. Le Guin/The Dispossessed (1974).epub").await;
    write_fake_video(dir.path(), "Andy Weir/Project Hail Mary/01.mp3").await;
    write_fake_video(
        dir.path(),
        "Andy Weir/Project Hail Mary/Disc 1/Chapter 02.m4a",
    )
    .await;
    write_fake_video(dir.path(), "Author One/Shared Title/01.mp3").await;
    write_fake_video(dir.path(), "Author Two/Shared Title/01.mp3").await;
    // A video in the same tree is not a book by accident.
    write_fake_video(dir.path(), "Andy Weir/interview.mp4").await;

    let lib = store
        .create_library(&NewLibrary {
            name: "Books".into(),
            kind: LibraryKind::Books,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");
    let report = scan_library(&store, &lib).await.expect("scan");
    assert_eq!(
        report.added, 5,
        "the EPUB and four audio parts were indexed"
    );

    let page = store
        .list_top_items(lib.id, ItemSort::Title, 0, 20)
        .await
        .expect("items");
    assert_eq!(
        page.total, 4,
        "parts of one work collapse, but two authors' same title stays separate"
    );
    assert_eq!(
        page.items
            .iter()
            .filter(|item| item.kind == ItemKind::Audiobook && item.title == "Shared Title")
            .count(),
        2,
        "the author directory is part of scanner identity"
    );
    let text = page
        .items
        .iter()
        .find(|item| item.kind == ItemKind::Book)
        .expect("text book");
    assert_eq!(text.title, "The Dispossessed");
    assert_eq!(text.year, Some(1974));
    let text_file = store.files_for_item(text.id).await.expect("text file");
    assert_eq!(text_file[0].container.as_deref(), Some("epub"));
    assert!(
        text_file[0].probed,
        "a valid text book must not enter the failed-ffprobe retry queue"
    );

    let audio = page
        .items
        .iter()
        .find(|item| item.kind == ItemKind::Audiobook)
        .expect("audiobook");
    assert_eq!(audio.title, "Project Hail Mary");
    assert_eq!(
        store
            .files_for_item(audio.id)
            .await
            .expect("audio files")
            .len(),
        2
    );
}

#[tokio::test]
async fn audiobook_runtime_reconciles_from_the_surviving_parts() {
    let store = SqliteStore::open_in_memory().expect("store");
    let library = store
        .create_library(&NewLibrary {
            name: "Books".into(),
            kind: LibraryKind::Books,
            paths: vec![PathBuf::from("/contract/books")],
            anime: false,
        })
        .await
        .expect("library");
    let item_id = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Audiobook,
            parent_id: None,
            title: "Two Parts".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("audiobook");
    let first = store
        .upsert_file(
            item_id,
            "/contract/books/01.m4b",
            10,
            1,
            &ProbeResult {
                duration_ms: Some(60_000),
                ..ProbeResult::default()
            },
        )
        .await
        .expect("first part");
    let second = store
        .upsert_file(
            item_id,
            "/contract/books/02.m4b",
            10,
            1,
            &ProbeResult {
                duration_ms: Some(90_000),
                ..ProbeResult::default()
            },
        )
        .await
        .expect("second part");
    let placed = [PlacedFile {
        item_id,
        file_id: first,
        path: "/contract/books/01.m4b".into(),
    }];

    refresh_audiobook_runtimes(&PublicationStore::unfenced(&store), &library, &placed)
        .await
        .expect("aggregate runtime");
    assert_eq!(
        store
            .get_item(item_id)
            .await
            .expect("item")
            .expect("audiobook item disappeared")
            .runtime_ms,
        Some(150_000)
    );

    store.delete_files(&[second]).await.expect("delete part");
    refresh_audiobook_runtimes(&PublicationStore::unfenced(&store), &library, &placed)
        .await
        .expect("reconcile runtime");
    assert_eq!(
        store
            .get_item(item_id)
            .await
            .expect("item")
            .expect("audiobook item disappeared")
            .runtime_ms,
        Some(60_000)
    );
}

#[tokio::test]
async fn audiobook_runtime_stays_unknown_when_no_part_was_timed() {
    let store = SqliteStore::open_in_memory().expect("store");
    let library = store
        .create_library(&NewLibrary {
            name: "Untimed Books".into(),
            kind: LibraryKind::Books,
            paths: vec![PathBuf::from("/contract/untimed-books")],
            anime: false,
        })
        .await
        .expect("library");
    let item_id = store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Audiobook,
            parent_id: None,
            title: "Untimed".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("audiobook");
    let file_id = store
        .upsert_file(
            item_id,
            "/contract/untimed-books/Untimed/Part 1.mp3",
            10,
            1,
            &ProbeResult::default(),
        )
        .await
        .expect("untimed part");

    refresh_audiobook_runtimes(
        &PublicationStore::unfenced(&store),
        &library,
        &[PlacedFile {
            item_id,
            file_id,
            path: "/contract/untimed-books/Untimed/Part 1.mp3".into(),
        }],
    )
    .await
    .expect("runtime refresh");

    assert_eq!(
        store
            .get_item(item_id)
            .await
            .expect("item")
            .expect("audiobook")
            .runtime_ms,
        None,
        "a failed probe is unknown, not a zero-length book"
    );
}

/// THE property. A targeted scan sees one subtree by construction, so if
/// it ever reconciled it would delete the rest of the library — every
/// item, on the first import after a folder landed. Asserted by row
/// count, because that is what would go to zero.
#[tokio::test]
async fn a_targeted_scan_never_prunes_what_it_did_not_look_at() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(dir.path(), "Heat (1995)/Heat (1995).mkv").await;
    write_fake_video(dir.path(), "The Matrix (1999)/The Matrix (1999).mkv").await;
    let lib = movie_library(&store, dir.path()).await;
    scan_library(&store, &lib).await.expect("full scan");

    let before = store
        .list_top_items(lib.id, ItemSort::Title, 0, 50)
        .await
        .expect("list")
        .items
        .len();
    assert_eq!(before, 2);

    // A new folder arrives and only IT is scanned.
    write_fake_video(dir.path(), "Alien (1979)/Alien (1979).mkv").await;
    let out = scan_path(&store, &lib, &dir.path().join("Alien (1979)"))
        .await
        .expect("targeted");
    assert_eq!(out.report.added, 1, "the new file was recorded");
    assert_eq!(
        out.report.removed_files, 0,
        "a targeted scan must never remove a file record"
    );
    assert_eq!(out.report.pruned_items, 0, "…or an item");

    let after = store
        .list_top_items(lib.id, ItemSort::Title, 0, 50)
        .await
        .expect("list")
        .items
        .len();
    assert_eq!(
        after, 3,
        "the two untouched movies must still be there — pruning against a \
             partial view would empty the library"
    );
}

/// The caller is owed an answer, not a shrug: "which item did my file
/// become" is the question the whole endpoint exists to answer.
#[tokio::test]
async fn a_targeted_scan_reports_what_it_placed() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let file = write_fake_video(dir.path(), "Heat (1995)/Heat (1995).mkv").await;
    let lib = movie_library(&store, dir.path()).await;

    let out = scan_path(&store, &lib, &dir.path().join("Heat (1995)"))
        .await
        .expect("targeted");
    assert_eq!(out.items.len(), 1, "one file, one placement");
    assert_eq!(out.items[0].path, file.to_string_lossy());
    assert!(out.items[0].item_id > 0 && out.items[0].file_id > 0);
    let item = store
        .get_item(out.items[0].item_id)
        .await
        .expect("get")
        .expect("some");
    assert_eq!(item.title, "Heat");
}

/// A single file is a legitimate target — Curator imports one episode at a
/// time as often as it imports a folder.
#[tokio::test]
async fn a_single_file_is_a_valid_target() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let file = write_fake_video(dir.path(), "Heat (1995)/Heat (1995).mkv").await;
    let lib = movie_library(&store, dir.path()).await;

    let out = scan_path(&store, &lib, &file).await.expect("targeted");
    assert_eq!(out.report.added, 1);
    assert_eq!(out.items.len(), 1);
}

/// Without canonicalization, `roots[0]/../../etc` passes a string prefix
/// test — which would make "scan this path" mean "read any directory on
/// the host" for anyone holding a scan key.
#[tokio::test]
async fn a_path_outside_the_roots_is_refused_and_says_which_roots() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let outside = tempfile::tempdir().expect("tmp2");
    write_fake_video(outside.path(), "Elsewhere (2020)/Elsewhere (2020).mkv").await;
    let lib = movie_library(&store, dir.path()).await;

    for bad in [
        outside.path().to_path_buf(),
        dir.path().join("..").join(".."),
        PathBuf::from("/etc"),
    ] {
        match scan_path(&store, &lib, &bad).await {
            Err(TargetError::OutsideRoots { roots, .. }) => {
                assert!(
                    roots.iter().any(|r| r == &dir.path().display().to_string()),
                    "the error must name the roots — a path-mapping slip between two \
                         applications is the likeliest cause and should diagnose itself"
                );
            }
            other => panic!("{} was not refused: {other:?}", bad.display()),
        }
    }
}

/// A symlink that escapes the root is the same attack wearing a hat.
#[cfg(unix)]
#[tokio::test]
async fn a_symlink_escaping_the_root_is_refused() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let outside = tempfile::tempdir().expect("tmp2");
    write_fake_video(outside.path(), "Secret (2020)/Secret (2020).mkv").await;
    let lib = movie_library(&store, dir.path()).await;

    let link = dir.path().join("escape");
    std::os::unix::fs::symlink(outside.path(), &link).expect("symlink");
    assert!(
        matches!(
            scan_path(&store, &lib, &link).await,
            Err(TargetError::OutsideRoots { .. })
        ),
        "a symlink pointing out of the library must not be scannable"
    );
}

/// A path that does not exist is refused rather than silently scanning
/// nothing — Curator sending a path plurx cannot see (the classic
/// container path-mapping mistake) must hear about it.
#[tokio::test]
async fn a_missing_path_is_refused() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let lib = movie_library(&store, dir.path()).await;
    assert!(matches!(
        scan_path(&store, &lib, &dir.path().join("nope")).await,
        Err(TargetError::OutsideRoots { .. })
    ));
}

/// Re-scanning the same path is cheap and idempotent: unchanged files
/// short-circuit before probe, and nothing is added twice.
#[tokio::test]
async fn re_scanning_the_same_path_changes_nothing() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(dir.path(), "Heat (1995)/Heat (1995).mkv").await;
    let lib = movie_library(&store, dir.path()).await;
    let target = dir.path().join("Heat (1995)");

    let first = scan_path(&store, &lib, &target).await.expect("first");
    assert_eq!(first.report.added, 1);
    let second = scan_path(&store, &lib, &target).await.expect("second");
    assert_eq!(second.report.added, 0);
    assert_eq!(second.report.unchanged, 1);
    assert_eq!(
        store
            .list_top_items(lib.id, ItemSort::Title, 0, 50)
            .await
            .expect("list")
            .items
            .len(),
        1,
        "a second scan must not duplicate the item"
    );
}

#[tokio::test]
async fn scans_movies_incrementally_and_reconciles() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(dir.path(), "The Matrix (1999)/The Matrix (1999).mkv").await;
    write_fake_video(dir.path(), "Heat (1995).mkv").await;

    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");

    let r = scan_library(&store, &lib).await.expect("scan");
    assert_eq!(r.added, 2);
    let page = store
        .list_top_items(lib.id, ItemSort::Title, 0, 10)
        .await
        .expect("list");
    assert_eq!(page.total, 2);

    // Second scan: nothing changed.
    let r = scan_library(&store, &lib).await.expect("rescan");
    assert_eq!(r.added, 0);
    assert_eq!(r.unchanged, 2);

    // Delete one file, rescan: file removed and its movie pruned.
    std::fs::remove_file(dir.path().join("Heat (1995).mkv")).expect("rm");
    let r = scan_library_with_progress_and_prune_limit(&store, &lib, None, 1)
        .await
        .expect("rescan2");
    assert_eq!(r.removed_files, 1);
    assert_eq!(r.pruned_items, 1);
    let page = store
        .list_top_items(lib.id, ItemSort::Title, 0, 10)
        .await
        .expect("list");
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].title, "The Matrix");
}

#[tokio::test]
async fn root_that_is_not_a_directory_is_reported_by_the_consumer() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let root_file = write_fake_video(dir.path(), "Not A Root (2020).mkv").await;
    let mut lib = movie_library(&store, dir.path()).await;
    lib.paths = vec![root_file.clone()];

    let report = scan_library(&store, &lib).await.expect("scan");

    assert_eq!(report.errors, 1);
    assert_eq!(report.removed_files, 0);
    assert!(report.problems.iter().any(|problem| {
        problem
            == &format!(
                "library path `{}` does not exist on the server — if plurxd runs in a \
                     container, use the path as mounted inside the container (e.g. `/media/…`), \
                     and check the mount is present",
                root_file.display()
            )
    }));
}

#[tokio::test]
async fn default_prune_percent_removes_one_missing_file_from_a_small_library() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let mut paths = Vec::new();
    for index in 0..5 {
        paths.push(write_fake_video(dir.path(), &format!("Small Movie {index} (2020).mkv")).await);
    }
    let lib = movie_library(&store, dir.path()).await;
    scan_library(&store, &lib).await.expect("initial scan");

    std::fs::remove_file(&paths[0]).expect("remove one source");
    let report = scan_library_with_progress_and_prune_percent(
        &store,
        &lib,
        None,
        crate::config::DEFAULT_SCAN_PRUNE_PERCENT,
    )
    .await
    .expect("small-library rescan");

    assert_eq!(report.removed_files, 1);
    assert_eq!(
        store
            .library_file_paths(lib.id)
            .await
            .expect("remaining files")
            .len(),
        4
    );
}

#[tokio::test]
async fn zero_prune_percent_still_refuses_automatic_removal() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let path = write_fake_video(dir.path(), "Tiny Library (2020).mkv").await;
    let lib = movie_library(&store, dir.path()).await;
    scan_library(&store, &lib).await.expect("initial scan");

    std::fs::remove_file(path).expect("remove source");
    let report = scan_library_with_progress_and_prune_percent(&store, &lib, None, 0)
        .await
        .expect("disabled-prune rescan");

    assert_eq!(report.removed_files, 0);
    assert_eq!(report.errors, 1);
    assert!(report
        .problems
        .iter()
        .any(|problem| problem.contains("configured limit of 0")));
    assert_eq!(
        store
            .library_file_paths(lib.id)
            .await
            .expect("kept file")
            .len(),
        1
    );
}

#[tokio::test]
async fn a_scan_above_its_prune_bound_commits_no_delete() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let path = write_fake_video(dir.path(), "Heat (1995).mkv").await;
    let lib = movie_library(&store, dir.path()).await;
    scan_library(&store, &lib).await.expect("initial scan");
    let file_id = store.library_file_paths(lib.id).await.expect("file paths")[0].0;

    std::fs::remove_file(&path).expect("remove source");
    let report = scan_library_with_progress_and_prune_limit(&store, &lib, None, 0)
        .await
        .expect("bounded scan");
    assert_eq!(report.removed_files, 0);
    assert_eq!(report.pruned_items, 0);
    assert_eq!(report.errors, 1);
    assert!(report
        .problems
        .iter()
        .any(|problem| problem.contains("configured limit of 0")));
    assert!(store.get_file(file_id).await.expect("kept row").is_some());
}

async fn set_paths(store: &SqliteStore, lib: &Library, paths: Vec<PathBuf>) -> Library {
    store
        .update_library(
            lib.id,
            &NewLibrary {
                name: lib.name.clone(),
                kind: lib.kind,
                paths,
                anime: lib.anime,
            },
        )
        .await
        .expect("update paths")
        .expect("library")
}

/// Replacing a library's path with another is removing the old one: its
/// entries leave the catalog even though the new path is empty. (A mount
/// replaced *at the same path* is still the root identity's business.)
#[tokio::test]
async fn replacing_a_library_path_removes_the_old_paths_entries() {
    let store = SqliteStore::open_in_memory().expect("store");
    let original = tempfile::tempdir().expect("original root");
    let replacement = tempfile::tempdir().expect("replacement root");
    write_fake_video(original.path(), "Heat (1995).mkv").await;
    let lib = movie_library(&store, original.path()).await;
    scan_library(&store, &lib).await.expect("initial scan");
    let file_id = store.library_file_paths(lib.id).await.expect("file paths")[0].0;
    let replacement_library = set_paths(&store, &lib, vec![replacement.path().to_path_buf()]).await;

    let report = scan_library(&store, &replacement_library)
        .await
        .expect("replaced-root scan");
    assert_eq!(report.removed_files, 1);
    assert_eq!(report.pruned_items, 1);
    assert!(
        !report
            .problems
            .iter()
            .any(|problem| problem.contains("cleanup skipped")),
        "problems: {:?}",
        report.problems
    );
    assert!(store.get_file(file_id).await.expect("row read").is_none());
}

/// The reported bug: a removed path holding more files than the prune
/// bound used to leave every entry behind and the library with no root
/// identity, so no vanished-file cleanup ever ran for it again.
#[tokio::test]
async fn removing_a_library_path_removes_its_items_whatever_the_prune_bound() {
    let store = SqliteStore::open_in_memory().expect("store");
    let kept = tempfile::tempdir().expect("kept root");
    let removed = tempfile::tempdir().expect("removed root");
    let kept_file = write_fake_video(kept.path(), "Heat (1995).mkv").await;
    for index in 0..5 {
        write_fake_video(removed.path(), &format!("Removed {index} (2001).mkv")).await;
    }
    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![kept.path().to_path_buf(), removed.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");
    scan_library(&store, &lib).await.expect("initial scan");
    assert_eq!(
        store.library_file_paths(lib.id).await.expect("paths").len(),
        6
    );

    let lib = set_paths(&store, &lib, vec![kept.path().to_path_buf()]).await;
    let report = scan_library_with_progress_and_prune_percent(
        &store,
        &lib,
        None,
        crate::config::DEFAULT_SCAN_PRUNE_PERCENT,
    )
    .await
    .expect("rescan");
    assert_eq!(report.removed_files, 5);
    assert_eq!(report.pruned_items, 5);
    assert!(
        !report
            .problems
            .iter()
            .any(|problem| problem.contains("cleanup skipped")),
        "problems: {:?}",
        report.problems
    );
    let remaining = store.library_file_paths(lib.id).await.expect("paths");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].1.canonicalize().expect("kept file"), kept_file);
    assert_eq!(
        store
            .ensure_library_root_fingerprint(
                lib.id,
                &lib.paths,
                &library_root_fingerprint(&lib.paths).expect("fingerprint"),
                false,
            )
            .await
            .expect("root identity"),
        RootFingerprintStatus::Matched,
        "the scan of the remaining path re-established the library's identity"
    );
}

/// The store applies a removal only to the path set it was judged
/// against; once the library's paths have moved on it removes nothing.
#[tokio::test]
async fn the_store_removal_applies_only_to_the_current_paths() {
    let store = SqliteStore::open_in_memory().expect("store");
    let kept = tempfile::tempdir().expect("kept root");
    let removed = tempfile::tempdir().expect("removed root");
    write_fake_video(kept.path(), "Heat (1995).mkv").await;
    write_fake_video(removed.path(), "Ronin (1998).mkv").await;
    let both = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![kept.path().to_path_buf(), removed.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");
    scan_library(&store, &both).await.expect("initial scan");
    let removed_file = store
        .library_file_paths(both.id)
        .await
        .expect("paths")
        .into_iter()
        .find(|(_, path)| path.starts_with(removed.path()))
        .expect("removed-root file")
        .0;

    let one = set_paths(&store, &both, vec![kept.path().to_path_buf()]).await;
    assert_eq!(
        store
            .remove_files_outside_library_roots(both.id, &both.paths, &[removed_file])
            .await
            .expect("stale removal"),
        RootRemovalOutcome::ConfigurationChanged
    );
    assert_eq!(
        store
            .library_file_paths(both.id)
            .await
            .expect("paths")
            .len(),
        2
    );
    assert_eq!(
        store
            .remove_files_outside_library_roots(one.id, &one.paths, &[removed_file])
            .await
            .expect("removal"),
        RootRemovalOutcome::Applied {
            deleted_files: 1,
            pruned_items: 1
        }
    );
    let remaining = store.library_file_paths(one.id).await.expect("paths");
    assert_eq!(remaining.len(), 1);
    assert!(remaining[0].1.starts_with(kept.path()));
}

/// A replacement path the scan cannot read may be where the media moved:
/// the old entries stay until a scan has walked it.
#[tokio::test]
async fn an_unreadable_new_path_keeps_the_old_paths_entries() {
    let store = SqliteStore::open_in_memory().expect("store");
    let old = tempfile::tempdir().expect("old root");
    write_fake_video(old.path(), "Heat (1995).mkv").await;
    let lib = movie_library(&store, old.path()).await;
    scan_library(&store, &lib).await.expect("initial scan");
    let missing = old.path().join("not-mounted-yet");
    let lib = set_paths(&store, &lib, vec![missing]).await;

    let report = scan_library(&store, &lib).await.expect("rescan");
    assert_eq!(report.removed_files, 0);
    assert_eq!(report.pruned_items, 0);
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.contains("paths no longer in the library")),
        "problems: {:?}",
        report.problems
    );
    assert_eq!(
        store.library_file_paths(lib.id).await.expect("paths").len(),
        1
    );
}

/// Re-spelling a path (`dir//` → `dir`) re-records every file under the
/// new spelling; the old rows are that configuration change's leftovers,
/// not vanished files, so they go whatever the prune bound, the items
/// keep their identity, and cleanup stays healthy afterwards.
#[tokio::test]
async fn a_respelled_library_path_keeps_its_items_and_drops_the_old_rows() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("root");
    for index in 0..12 {
        write_fake_video(dir.path(), &format!("Respelled {index} (2002).mkv")).await;
    }
    let respelled = PathBuf::from(format!("{}//", dir.path().display()));
    let lib = movie_library(&store, &respelled).await;
    scan_library(&store, &lib).await.expect("initial scan");
    let mut items = Vec::new();
    for (file, _) in store.library_file_paths(lib.id).await.expect("paths") {
        items.push(
            store
                .get_file(file)
                .await
                .expect("file")
                .expect("row")
                .item_id,
        );
    }
    items.sort_unstable();

    let lib = set_paths(&store, &lib, vec![dir.path().to_path_buf()]).await;
    let report = scan_library_with_progress_and_prune_percent(
        &store,
        &lib,
        None,
        crate::config::DEFAULT_SCAN_PRUNE_PERCENT,
    )
    .await
    .expect("rescan");
    assert_eq!(report.removed_files, 12);
    assert_eq!(report.pruned_items, 0);
    assert!(
        !report
            .problems
            .iter()
            .any(|problem| problem.contains("cleanup skipped")),
        "problems: {:?}",
        report.problems
    );
    let mut survivors = Vec::new();
    for (file, path) in store.library_file_paths(lib.id).await.expect("paths") {
        assert!(!path.to_string_lossy().contains("//"), "{path:?}");
        survivors.push(
            store
                .get_file(file)
                .await
                .expect("file")
                .expect("row")
                .item_id,
        );
    }
    survivors.sort_unstable();
    assert_eq!(survivors, items);
}

/// A node whose mounted roots do not match the identity the cluster
/// recorded judges nothing, configuration leftovers included: its view of
/// which spelling is "outside" is exactly what is in doubt.
#[tokio::test]
async fn a_mismatched_root_identity_keeps_entries_outside_the_paths() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("root");
    write_fake_video(dir.path(), "Heat (1995).mkv").await;
    let lib = movie_library(&store, dir.path()).await;
    scan_library(&store, &lib).await.expect("initial scan");
    let (file, _) = store.library_file_paths(lib.id).await.expect("paths")[0].clone();
    let item = store
        .get_file(file)
        .await
        .expect("file")
        .expect("row")
        .item_id;
    let elsewhere = store
        .upsert_file(
            item,
            "/another-node/movies/Heat (1995).mkv",
            1,
            1,
            &ProbeResult::default(),
        )
        .await
        .expect("row recorded elsewhere");
    assert!(store
        .reset_library_root_fingerprint(lib.id)
        .await
        .expect("reset"));
    assert_eq!(
        store
            .ensure_library_root_fingerprint(lib.id, &lib.paths, "another-node", true)
            .await
            .expect("other identity"),
        RootFingerprintStatus::Established
    );

    let report = scan_library(&store, &lib).await.expect("rescan");
    assert_eq!(report.removed_files, 0);
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.contains("does not match")),
        "problems: {:?}",
        report.problems
    );
    assert!(store.get_file(elsewhere).await.expect("row").is_some());
}

/// Media moved to a path added in the same edit keeps its item: the
/// removal waits for the walk, which re-attaches the moved file first.
#[tokio::test]
async fn moving_media_to_a_new_library_path_keeps_its_item() {
    let store = SqliteStore::open_in_memory().expect("store");
    let old = tempfile::tempdir().expect("old root");
    let new = tempfile::tempdir().expect("new root");
    write_fake_video(old.path(), "Heat (1995).mkv").await;
    let lib = movie_library(&store, old.path()).await;
    scan_library(&store, &lib).await.expect("initial scan");
    let (old_file, _) = store.library_file_paths(lib.id).await.expect("paths")[0].clone();
    let item = store
        .get_file(old_file)
        .await
        .expect("file")
        .expect("row")
        .item_id;

    let moved = write_fake_video(new.path(), "Heat (1995).mkv").await;
    let lib = set_paths(&store, &lib, vec![new.path().to_path_buf()]).await;
    let report = scan_library(&store, &lib).await.expect("rescan");
    assert_eq!(report.removed_files, 1, "problems: {:?}", report.problems);
    assert_eq!(
        report.pruned_items, 0,
        "the item survives with its new file"
    );
    let remaining = store.library_file_paths(lib.id).await.expect("paths");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].1.canonicalize().expect("moved"), moved);
    let file = store
        .get_file(remaining[0].0)
        .await
        .expect("file")
        .expect("row");
    assert_eq!(file.item_id, item);
}

/// A scan that began before the library's paths were edited must not
/// record the old path set's identity: every scan of the new set would
/// then be refused as a mount mismatch, and cleanup would stop for good.
#[tokio::test]
async fn a_scan_of_outdated_paths_neither_removes_nor_establishes() {
    let store = SqliteStore::open_in_memory().expect("store");
    let first = tempfile::tempdir().expect("first root");
    let added = tempfile::tempdir().expect("added root");
    write_fake_video(first.path(), "Heat (1995).mkv").await;
    write_fake_video(added.path(), "Ronin (1998).mkv").await;
    let stale = movie_library(&store, first.path()).await;
    let current = set_paths(
        &store,
        &stale,
        vec![first.path().to_path_buf(), added.path().to_path_buf()],
    )
    .await;

    let report = scan_library(&store, &stale).await.expect("stale scan");
    assert_eq!(report.removed_files, 0);
    assert!(
        !report
            .problems
            .iter()
            .any(|problem| problem.contains("cleanup skipped")),
        "problems: {:?}",
        report.problems
    );
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.contains("paths changed while this scan ran")),
        "problems: {:?}",
        report.problems
    );
    assert_eq!(
        store
            .ensure_library_root_fingerprint(
                stale.id,
                &stale.paths,
                &library_root_fingerprint(&stale.paths).expect("fingerprint"),
                true,
            )
            .await
            .expect("stale identity"),
        RootFingerprintStatus::ConfigurationChanged
    );

    let report = scan_library(&store, &current).await.expect("current scan");
    assert!(
        !report
            .problems
            .iter()
            .any(|problem| problem.contains("cleanup skipped")),
        "problems: {:?}",
        report.problems
    );
    assert_eq!(
        store
            .library_file_paths(current.id)
            .await
            .expect("paths")
            .len(),
        2
    );
}

#[test]
fn superseded_paths_compare_components_in_every_recorded_spelling() {
    let dir = tempfile::tempdir().expect("tmp");
    let real = dir.path().join("real-tv");
    std::fs::create_dir(&real).expect("real root");
    #[cfg(unix)]
    let root = {
        let link = dir.path().join("tv");
        std::os::unix::fs::symlink(&real, &link).expect("symlinked root");
        link
    };
    #[cfg(not(unix))]
    let root = real.clone();
    let spellings = root_spellings(std::slice::from_ref(&root)).expect("resolves");
    let canonical = real.canonicalize().expect("canonical");
    let known = vec![
        (1, root.join("Show/S01E01.mkv")),
        (
            2,
            PathBuf::from(format!("{}//Show/S01E02.mkv", root.display())),
        ),
        (3, canonical.join("Show/S01E03.mkv")),
        (
            4,
            PathBuf::from(format!("{}2/Show/S01E01.mkv", root.display())),
        ),
        (5, dir.path().join("elsewhere/x.mkv")),
        (
            6,
            PathBuf::from(format!("{}//Show/S01E04.mkv", root.display())),
        ),
    ];
    let seen: HashSet<String> = [root.join("Show/S01E01.mkv"), root.join("Show/S01E04.mkv")]
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        superseded_paths(&known, &seen, Some(spellings.as_slice())),
        vec![4, 5, 6]
    );
    assert_eq!(superseded_paths(&known, &seen, None), vec![6]);
    assert!(root_spellings(&[dir.path().join("missing")]).is_none());
}

#[tokio::test]
async fn missing_root_is_reported_and_preserves_files() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(dir.path(), "Heat (1995).mkv").await;

    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");
    let r = scan_library(&store, &lib).await.expect("scan");
    assert_eq!(r.added, 1);

    // The root vanishes (unmounted NAS / wrong container path). The scan
    // must say so loudly — and must NOT delete the known files.
    drop(dir);
    let r = scan_library_with_progress_and_prune_limit(&store, &lib, None, 1)
        .await
        .expect("rescan");
    assert_eq!(r.errors, 1);
    assert!(
        r.problems.iter().any(|p| p.contains("does not exist")),
        "problems: {:?}",
        r.problems
    );
    assert_eq!(r.removed_files, 0);
    assert_eq!(r.pruned_items, 0);
    let known = store.library_file_paths(lib.id).await.expect("paths");
    assert_eq!(known.len(), 1, "files must survive a missing root");
}

#[tokio::test]
async fn empty_root_reports_no_videos_found() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");

    // Path exists but holds no video files (the classic empty-volume /
    // wrong-subfolder misconfiguration): counts are all zero, so the
    // report must carry an explicit problem instead.
    let r = scan_library(&store, &lib).await.expect("scan");
    assert_eq!(r.added, 0);
    assert_eq!(r.errors, 0);
    assert!(
        r.problems
            .iter()
            .any(|p| p.contains("no video files found")),
        "problems: {:?}",
        r.problems
    );
}

#[tokio::test]
async fn empty_recordings_library_is_normal_but_unexpected_losses_stay_protected() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let lib = store
        .create_library(&NewLibrary {
            name: "Recordings".into(),
            kind: LibraryKind::Recordings,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("library");
    let empty = scan_library(&store, &lib).await.expect("empty scan");
    assert_eq!(empty.errors, 0);
    assert!(empty.problems.is_empty(), "{:?}", empty.problems);
    let mut paths = Vec::new();
    for n in 0..5 {
        paths.push(write_fake_video(dir.path(), &format!("Recording {n}.ts")).await);
    }
    scan_library(&store, &lib).await.expect("initial scan");
    for path in paths {
        std::fs::remove_file(path).expect("unexpected disappearance");
    }
    let report = scan_library_with_progress_and_prune_percent(
        &store,
        &lib,
        None,
        crate::config::DEFAULT_SCAN_PRUNE_PERCENT,
    )
    .await
    .expect("protected scan");
    assert_eq!(report.errors, 1);
    assert_eq!(report.removed_files, 0);
    assert_eq!(
        store
            .library_file_paths(lib.id)
            .await
            .expect("kept entries")
            .len(),
        5
    );
    assert!(report
        .problems
        .iter()
        .any(|p| p.contains("5 catalog file entries") && p.contains("limit of 1")));
}

/// The home arm end to end: folder mirroring, titles kept verbatim, the
/// date ladder, seed-once NFO handling, and a junk sidecar that complains
/// exactly once.
#[tokio::test]
async fn scans_home_tree_with_folders_dates_and_nfos() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(dir.path(), "2019/Beach Trip/Christmas 2019.mp4").await;
    write_fake_video(dir.path(), "2019/Beach Trip/2019-06-14 - Sandcastle.mp4").await;
    write_fake_video(dir.path(), "Loose clip.mov").await;
    // A photo — invisible in movie/show libraries, a first-class item here.
    std::fs::write(dir.path().join("2019/IMG_4021.jpg"), b"jpeg-ish").expect("photo");
    // One good sidecar, one that is just a URL someone saved.
    std::fs::write(
        dir.path().join("2019/Beach Trip/Christmas 2019.nfo"),
        "<movie><title>Christmas morning</title><premiered>2019-12-25</premiered>\
             <tag>kids</tag></movie>",
    )
    .expect("nfo");
    std::fs::write(
        dir.path().join("Loose clip.nfo"),
        "https://www.imdb.com/title/tt0133093/",
    )
    .expect("junk nfo");

    let lib = store
        .create_library(&NewLibrary {
            name: "Home".into(),
            kind: LibraryKind::Home,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");

    let r = scan_library(&store, &lib).await.expect("scan");
    assert_eq!(r.added, 4, "three videos and one photo");
    assert_eq!(r.seeded, 2, "both sidecars are consumed exactly once");
    assert!(
        r.problems.iter().any(|p| p.contains("Loose clip.nfo")),
        "a junk sidecar complains by name: {:?}",
        r.problems
    );

    // Top level: the mirrored "2019" folder and the loose clip.
    let top = store
        .list_top_items(lib.id, ItemSort::Title, 0, 10)
        .await
        .expect("list");
    assert_eq!(top.total, 2);
    let folder = top
        .items
        .iter()
        .find(|i| i.kind == ItemKind::Folder)
        .expect("2019 folder");
    assert_eq!(folder.title, "2019");
    let loose = top
        .items
        .iter()
        .find(|i| i.kind == ItemKind::Video)
        .expect("loose video");
    assert_eq!(loose.title, "Loose clip");

    // "2019" holds the photo and the nested "Beach Trip" folder —
    // subfolders sort first.
    let kids = store.get_item_children(folder.id).await.expect("children");
    assert_eq!(kids.len(), 2);
    assert_eq!(kids[0].kind, ItemKind::Folder);
    assert_eq!(kids[0].title, "Beach Trip");
    assert_eq!(kids[1].kind, ItemKind::Photo);
    assert_eq!(kids[1].title, "IMG_4021", "camera junk names stay honest");

    let clips = store.get_item_children(kids[0].id).await.expect("clips");
    assert_eq!(clips.len(), 2);
    let by_title = |t: &str| {
        clips
            .iter()
            .find(|i| i.title == t)
            .unwrap_or_else(|| panic!("missing {t}: {clips:?}"))
            .clone()
    };
    // NFO seeding renamed this one and set its date and tags.
    let christmas = by_title("Christmas morning");
    assert_eq!(christmas.recorded_at.as_deref(), Some("2019-12-25"));
    assert_eq!(christmas.tags, vec!["kids".to_owned()]);
    assert!(christmas.nfo_seeded_at.is_some());
    // No sidecar: the title survives the year (the movie parser would have
    // eaten it) and the date came off the filename.
    let sandcastle = by_title("Sandcastle");
    assert_eq!(sandcastle.recorded_at.as_deref(), Some("2019-06-14"));
    assert!(sandcastle.nfo_seeded_at.is_none());

    // Rescan: nothing changed, and nothing re-seeds. (These fixtures are
    // garbage bytes, so each one's probe is retried and fails again — the
    // repair path — which leaves them `unchanged` and `degraded`.)
    let r = scan_library(&store, &lib).await.expect("rescan");
    assert_eq!(r.added, 0);
    assert_eq!(r.unchanged, 4);
    assert_eq!(r.seeded, 0);
    assert_eq!(r.repaired, 0);
    // The repair retries the probe **in place**. Re-running placement here
    // would re-derive "Christmas 2019" from the filename, miss the item the
    // NFO renamed to "Christmas morning", create a second one, and prune the
    // first — losing every hand edit on it. This assert is that bug's tomb.
    assert_eq!(r.pruned_items, 0, "a repair must not re-place items");
    assert_eq!(r.updated, 0);

    // (a) A hand edit survives a rewritten NFO — the sidecar is dead to
    // plurx once consumed. This is the whole seed-once contract.
    store
        .update_item_fields(
            christmas.id,
            &ItemEdit {
                title: Some("Christmas at the beach".into()),
                ..Default::default()
            },
        )
        .await
        .expect("edit");
    std::fs::write(
        dir.path().join("2019/Beach Trip/Christmas 2019.nfo"),
        "<movie><title>OVERWRITTEN</title></movie>",
    )
    .expect("rewrite nfo");
    let r = scan_library(&store, &lib).await.expect("rescan2");
    assert_eq!(r.seeded, 0);
    assert_eq!(
        store
            .get_item(christmas.id)
            .await
            .expect("get")
            .expect("present")
            .title,
        "Christmas at the beach"
    );

    // (b) A sidecar written *after* the file was scanned seeds on the next
    // rescan — the file is unchanged, but the item is unseeded.
    std::fs::write(
        dir.path()
            .join("2019/Beach Trip/2019-06-14 - Sandcastle.nfo"),
        "<movie><title>Sandcastle contest</title></movie>",
    )
    .expect("late nfo");
    let r = scan_library(&store, &lib).await.expect("rescan3");
    assert_eq!(r.seeded, 1, "a late sidecar seeds exactly once");
    let seeded = store
        .get_item(sandcastle.id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(seeded.title, "Sandcastle contest");
    assert_eq!(
        seeded.recorded_at.as_deref(),
        Some("2019-06-14"),
        "an NFO without a date leaves the ladder's date alone"
    );
    let r = scan_library(&store, &lib).await.expect("rescan4");
    assert_eq!(r.seeded, 0, "and never again");
}

/// A minimal JPEG carrying one EXIF field: `DateTimeOriginal`. Hand-built
/// because the point is the metadata, not the pixels — a real camera file
/// would be megabytes of test fixture for the same assertion.
fn write_exif_jpeg(path: &Path, taken: &str) {
    fn le16(v: u16) -> [u8; 2] {
        v.to_le_bytes()
    }
    fn le32(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }
    let mut ascii = taken.as_bytes().to_vec();
    ascii.push(0);

    // TIFF: header → IFD0 (one pointer to the Exif IFD) → Exif IFD (one
    // DateTimeOriginal) → the string itself. Offsets are from the header.
    const IFD0: u32 = 8;
    const EXIF_IFD: u32 = 26;
    const ASCII_AT: u32 = 44;
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II*\0");
    tiff.extend_from_slice(&le32(IFD0));
    tiff.extend_from_slice(&le16(1));
    tiff.extend_from_slice(&le16(0x8769)); // ExifIFDPointer
    tiff.extend_from_slice(&le16(4)); // LONG
    tiff.extend_from_slice(&le32(1));
    tiff.extend_from_slice(&le32(EXIF_IFD));
    tiff.extend_from_slice(&le32(0));
    tiff.extend_from_slice(&le16(1));
    tiff.extend_from_slice(&le16(0x9003)); // DateTimeOriginal
    tiff.extend_from_slice(&le16(2)); // ASCII
    tiff.extend_from_slice(&le32(ascii.len() as u32));
    tiff.extend_from_slice(&le32(ASCII_AT));
    tiff.extend_from_slice(&le32(0));
    tiff.extend_from_slice(&ascii);

    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
    jpeg.extend_from_slice(&((2 + 6 + tiff.len()) as u16).to_be_bytes());
    jpeg.extend_from_slice(b"Exif\0\0");
    jpeg.extend_from_slice(&tiff);
    jpeg.extend_from_slice(&[0xFF, 0xD9]);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, jpeg).expect("write jpeg");
}

#[tokio::test]
async fn photos_take_their_date_from_exif() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_exif_jpeg(&dir.path().join("IMG_4021.jpg"), "2019:06:14 12:34:56");
    // No EXIF: the ladder falls through to the filename.
    std::fs::write(dir.path().join("2011-08-09 Picnic.jpg"), b"not a jpeg").expect("write");

    let lib = store
        .create_library(&NewLibrary {
            name: "Home".into(),
            kind: LibraryKind::Home,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");
    assert_eq!(scan_library(&store, &lib).await.expect("scan").added, 2);

    let page = store
        .list_top_items(lib.id, ItemSort::Recorded, 0, 10)
        .await
        .expect("list");
    let dates: Vec<(&str, Option<&str>)> = page
        .items
        .iter()
        .map(|i| (i.title.as_str(), i.recorded_at.as_deref()))
        .collect();
    assert_eq!(
        dates,
        vec![
            ("IMG_4021", Some("2019-06-14T12:34:56")),
            ("Picnic", Some("2011-08-09")),
        ],
        "EXIF beats the filename, and newest sorts first"
    );
    assert!(page.items.iter().all(|i| i.kind == ItemKind::Photo));
}

#[tokio::test]
async fn photos_stay_out_of_recently_added() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(dir.path(), "Trip/clip.mp4").await;
    for n in 0..5 {
        std::fs::write(dir.path().join(format!("Trip/IMG_{n}.jpg")), b"x").expect("photo");
    }
    let lib = store
        .create_library(&NewLibrary {
            name: "Home".into(),
            kind: LibraryKind::Home,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");
    scan_library(&store, &lib).await.expect("scan");

    // A photo import must not flood the home screen; its video and folder
    // still surface that something arrived.
    let recent = store
        .recently_added(Some(lib.id), 20)
        .await
        .expect("recent");
    let kinds: Vec<ItemKind> = recent.iter().map(|r| r.item.kind).collect();
    assert!(!kinds.contains(&ItemKind::Photo), "kinds: {kinds:?}");
    assert!(kinds.contains(&ItemKind::Video));
    assert!(kinds.contains(&ItemKind::Folder));
}

#[tokio::test]
async fn empty_home_root_names_photos_too() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let lib = store
        .create_library(&NewLibrary {
            name: "Home".into(),
            kind: LibraryKind::Home,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");
    let r = scan_library(&store, &lib).await.expect("scan");
    assert!(
        r.problems
            .iter()
            .any(|p| p.contains("no video or photo files found")),
        "problems: {:?}",
        r.problems
    );
}

#[tokio::test]
async fn photos_are_invisible_outside_home_libraries() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(dir.path(), "Heat (1995).mkv").await;
    std::fs::write(dir.path().join("poster.jpg"), b"jpeg-ish").expect("poster");

    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");
    let r = scan_library(&store, &lib).await.expect("scan");
    assert_eq!(r.added, 1, "the poster next to a movie stays invisible");
}

#[tokio::test]
async fn deleted_home_files_prune_their_folder_chain() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(dir.path(), "2019/Summer/Beach Trip/clip.mp4").await;
    let lib = store
        .create_library(&NewLibrary {
            name: "Home".into(),
            kind: LibraryKind::Home,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");
    assert_eq!(scan_library(&store, &lib).await.expect("scan").added, 1);

    std::fs::remove_file(dir.path().join("2019/Summer/Beach Trip/clip.mp4")).expect("rm");
    let r = scan_library_with_progress_and_prune_limit(&store, &lib, None, 1)
        .await
        .expect("rescan");
    assert_eq!(r.removed_files, 1);
    assert_eq!(
        r.pruned_items, 4,
        "the video and all three empty folders above it"
    );
    let top = store
        .list_top_items(lib.id, ItemSort::Title, 0, 10)
        .await
        .expect("list");
    assert_eq!(top.total, 0);
}

#[tokio::test]
async fn scans_show_hierarchy() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(
        dir.path(),
        "Severance (2022)/Season 01/Severance - S01E01 - Good News.mkv",
    )
    .await;
    write_fake_video(
        dir.path(),
        "Severance (2022)/Season 01/Severance - S01E02 - Half Loop.mkv",
    )
    .await;
    // A stray non-episode file is skipped, not errored.
    write_fake_video(dir.path(), "Severance (2022)/trailer.mkv").await;

    let lib = store
        .create_library(&NewLibrary {
            name: "TV".into(),
            kind: LibraryKind::Shows,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");

    let r = scan_library(&store, &lib).await.expect("scan");
    assert_eq!(r.added, 2);
    assert_eq!(r.skipped, 1);

    // One show → one season → two episodes.
    let page = store
        .list_top_items(lib.id, ItemSort::Title, 0, 10)
        .await
        .expect("list");
    assert_eq!(page.total, 1);
    let show = &page.items[0];
    assert_eq!(show.title, "Severance");
    let seasons = store.get_item_children(show.id).await.expect("seasons");
    assert_eq!(seasons.len(), 1);
    let eps = store.get_item_children(seasons[0].id).await.expect("eps");
    assert_eq!(eps.len(), 2);
    assert_eq!(eps[0].episode_number, Some(1));

    // A skip is not an error, but it must still say which file and why —
    // "1 skipped" with nothing to look at is a dead end.
    // Two probe failures (the fake episodes); the skipped stray never
    // reaches probe, so it is not an error.
    assert_eq!(r.errors, 2);
    assert_eq!(r.skip_groups.len(), 1, "groups: {:?}", r.skip_groups);
    let g = &r.skip_groups[0];
    assert_eq!(g.count, 1);
    assert!(
        g.folder.ends_with("Severance (2022)"),
        "a skip is reported under its SHOW folder, not its season or \
             release folder; got {:?}",
        g.folder
    );
    assert!(
        g.reason.contains("S01E02"),
        "the reason must still say what was expected, got {:?}",
        g.reason
    );
    assert_eq!(
        g.samples,
        vec!["trailer.mkv".to_owned()],
        "a group names some of its files, or there is nothing to act on"
    );
}

#[tokio::test]
async fn scan_identity_new_season_keeps_directory_owner_after_show_rename() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(
        dir.path(),
        "Harbor Lights/Season 1/Harbor Lights S01E01.mkv",
    )
    .await;
    let library = show_library(&store, dir.path()).await;
    scan_library(&store, &library).await.expect("initial scan");
    let original = only_show(&store, library.id).await;
    store
        .update_item_fields(
            original.id,
            &ItemEdit {
                title: Some("Harbor Lights (US)".into()),
                ..ItemEdit::default()
            },
        )
        .await
        .expect("rename");

    let new_path = write_fake_video(
        dir.path(),
        "Harbor Lights/Season 5/Harbor Lights S05E01.mkv",
    )
    .await;
    let targeted = scan_path(&store, &library, &new_path)
        .await
        .expect("targeted scan");

    assert_eq!(targeted.report.added, 1);
    assert_eq!(targeted.items.len(), 1);
    let survivor = only_show(&store, library.id).await;
    assert_eq!(survivor.id, original.id);
    let seasons = store.get_item_children(survivor.id).await.expect("seasons");
    assert_eq!(
        seasons
            .iter()
            .map(|season| season.season_number)
            .collect::<Vec<_>>(),
        vec![Some(1), Some(5)]
    );
}

#[tokio::test]
async fn scan_identity_second_version_keeps_episode_after_punctuation_rename() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(
        dir.path(),
        "City Watch/Season 2/City Watch S02E03 source.mkv",
    )
    .await;
    let library = show_library(&store, dir.path()).await;
    scan_library(&store, &library).await.expect("initial scan");
    let show = only_show(&store, library.id).await;
    let episode = only_episode(&store, show.id).await;
    store
        .update_item_fields(
            show.id,
            &ItemEdit {
                title: Some("City-Watch".into()),
                ..ItemEdit::default()
            },
        )
        .await
        .expect("rename");

    let alternate = write_fake_video(
        dir.path(),
        "City Watch/Season 2/City Watch S02E03 alternate.mkv",
    )
    .await;
    scan_path(&store, &library, &alternate)
        .await
        .expect("targeted scan");

    assert_eq!(only_show(&store, library.id).await.id, show.id);
    let files = store.files_for_item(episode.id).await.expect("versions");
    assert_eq!(files.len(), 2, "both versions remain attached");
    assert!(files.iter().all(|file| file.item_id == episode.id));
}

#[tokio::test]
async fn scan_identity_changed_file_preserves_ids_and_complete_watch_state() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let path = write_fake_video(dir.path(), "Night Shift/Season 1/Night Shift S01E04.mkv").await;
    let library = show_library(&store, dir.path()).await;
    scan_library(&store, &library).await.expect("initial scan");
    let show = only_show(&store, library.id).await;
    let episode = only_episode(&store, show.id).await;
    let (file_id, stored_path) = store
        .library_file_paths(library.id)
        .await
        .expect("file inventory")[0]
        .clone();
    let file = store
        .get_file(file_id)
        .await
        .expect("file lookup")
        .expect("file");
    let source = std::fs::metadata(&stored_path).expect("source metadata");
    store
        .upsert_file(
            episode.id,
            &stored_path.to_string_lossy(),
            source.len() as i64,
            file.mtime,
            &ProbeResult {
                raw_json: Some("{}".into()),
                ..ProbeResult::default()
            },
        )
        .await
        .expect("mark initial probe successful");
    let user = store
        .create_user("scan-identity-viewer", "hash", false)
        .await
        .expect("user");
    let before = store
        .put_progress_at(user.id, episode.id, 42_000, Some(100_000), Some(1))
        .await
        .expect("watch state");
    store
        .update_item_fields(
            show.id,
            &ItemEdit {
                title: Some("Night Shift: After Dark".into()),
                ..ItemEdit::default()
            },
        )
        .await
        .expect("rename");
    std::fs::write(&path, b"not really video, but repacked").expect("rewrite");

    let report = scan_library(&store, &library).await.expect("full rescan");
    assert_eq!(report.pruned_items, 0);
    let after_file = store
        .get_file(file_id)
        .await
        .expect("file lookup")
        .expect("file");
    assert_eq!(after_file.id, file.id);
    assert_eq!(after_file.item_id, episode.id);
    assert_eq!(only_show(&store, library.id).await.id, show.id);
    assert_eq!(
        store
            .watch_state(user.id, episode.id)
            .await
            .expect("watch lookup"),
        Some(before)
    );
}

#[tokio::test]
async fn scan_identity_changed_file_control_keeps_ids_without_metadata_rename() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let path = write_fake_video(dir.path(), "Quiet Harbor/Season 1/Quiet Harbor S01E01.mkv").await;
    let library = show_library(&store, dir.path()).await;
    scan_library(&store, &library).await.expect("initial scan");
    let show = only_show(&store, library.id).await;
    let episode = only_episode(&store, show.id).await;
    let file_id = store
        .library_file_paths(library.id)
        .await
        .expect("file inventory")[0]
        .0;
    let file = store
        .get_file(file_id)
        .await
        .expect("file lookup")
        .expect("file");
    std::fs::write(&path, b"not really video, but changed").expect("rewrite");

    scan_library(&store, &library).await.expect("full rescan");
    let after = store
        .get_file(file_id)
        .await
        .expect("file lookup")
        .expect("file");
    assert_eq!((after.id, after.item_id), (file.id, episode.id));
    assert_eq!(only_show(&store, library.id).await.id, show.id);
}

#[tokio::test]
async fn scan_identity_mtime_only_change_keeps_movie_and_failed_probe_owner() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let path = write_fake_video(dir.path(), "Copper Road (2024)/Copper Road (2024).mkv").await;
    let library = movie_library(&store, dir.path()).await;
    scan_library(&store, &library).await.expect("initial scan");
    let original = store
        .list_top_items(library.id, ItemSort::Title, 0, 10)
        .await
        .expect("movies")
        .items
        .into_iter()
        .next()
        .expect("movie");
    let file_id = store.library_file_paths(library.id).await.expect("files")[0].0;
    store
        .update_item_fields(
            original.id,
            &ItemEdit {
                title: Some("Copper Road: Restored".into()),
                ..ItemEdit::default()
            },
        )
        .await
        .expect("rename");
    let file = std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open source");
    file.set_times(
        std::fs::FileTimes::new()
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5)),
    )
    .expect("change mtime only");

    let report = scan_library(&store, &library).await.expect("rescan");
    assert_eq!(report.updated, 1);
    assert_eq!(report.pruned_items, 0);
    let after = store.get_file(file_id).await.expect("file").expect("file");
    assert_eq!(after.item_id, original.id);
    assert_eq!(
        store
            .list_top_items(library.id, ItemSort::Title, 0, 10)
            .await
            .expect("movies")
            .items
            .len(),
        1
    );
}

#[tokio::test]
async fn scan_identity_movie_version_uses_directory_after_display_rename() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(dir.path(), "Copper Road (2024)/Copper.Road.2024.source.mkv").await;
    let library = movie_library(&store, dir.path()).await;
    scan_library(&store, &library).await.expect("initial scan");
    let movie = store
        .list_top_items(library.id, ItemSort::Title, 0, 10)
        .await
        .expect("movies")
        .items[0]
        .clone();
    store
        .update_item_fields(
            movie.id,
            &ItemEdit {
                title: Some("Copper Road: Restored".into()),
                ..ItemEdit::default()
            },
        )
        .await
        .expect("rename");
    let alternate = write_fake_video(
        dir.path(),
        "Copper Road (2024)/Copper.Road.2024.alternate.mkv",
    )
    .await;
    scan_path(&store, &library, &alternate)
        .await
        .expect("targeted scan");

    assert_eq!(
        store
            .list_top_items(library.id, ItemSort::Title, 0, 10)
            .await
            .expect("movies")
            .items
            .len(),
        1
    );
    assert_eq!(
        store
            .files_for_item(movie.id)
            .await
            .expect("versions")
            .len(),
        2
    );
}

#[tokio::test]
async fn scan_identity_library_root_is_not_a_show_directory_identity() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let library = show_library(&store, dir.path()).await;
    assert_eq!(
        usable_source_directory(&library, Some(dir.path())),
        None,
        "a root/Season N/file layout cannot turn the root into one show"
    );

    let nested = dir.path().join("Nested Show");
    std::fs::create_dir_all(&nested).expect("nested root");
    let overlapping = store
        .create_library(&NewLibrary {
            name: "Overlapping roots".into(),
            kind: LibraryKind::Shows,
            paths: vec![dir.path().to_path_buf(), nested.clone()],
            anime: false,
        })
        .await
        .expect("overlapping library");
    assert_eq!(
        usable_source_directory(&overlapping, Some(&nested)),
        None,
        "a path equal to any configured root is never show identity"
    );
}

#[tokio::test]
async fn scan_identity_anime_uses_directory_when_layout_is_recognized() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(
        dir.path(),
        "Lantern Archive/Season 1/Lantern Archive S01E01.mkv",
    )
    .await;
    let library = store
        .create_library(&NewLibrary {
            name: "Anime".into(),
            kind: LibraryKind::Shows,
            paths: vec![dir.path().to_path_buf()],
            anime: true,
        })
        .await
        .expect("library");
    scan_library(&store, &library).await.expect("initial scan");
    let original = only_show(&store, library.id).await;
    store
        .update_item_fields(
            original.id,
            &ItemEdit {
                title: Some("Lantern Archive: Remastered".into()),
                ..ItemEdit::default()
            },
        )
        .await
        .expect("rename");
    let next = write_fake_video(
        dir.path(),
        "Lantern Archive/Season 2/Lantern Archive S02E01.mkv",
    )
    .await;
    scan_path(&store, &library, &next)
        .await
        .expect("targeted scan");

    assert_eq!(
        store
            .list_top_items(library.id, ItemSort::Title, 0, 10)
            .await
            .expect("shows")
            .items
            .len(),
        1,
        "recognized anime layouts keep directory ownership after a display rename"
    );
}

/// THE BUG THIS GUARDS: a show whose files carry no `S01E02` skips EVERY
/// episode, and the report printed one line per file. Twenty-four lines
/// that differ only in the episode number are not twenty-four problems —
/// they are one mis-named show — and a real library produced hundreds,
/// which the ten-line cap then truncated into uselessness.
#[tokio::test]
async fn skips_are_grouped_by_show_not_listed_per_file() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    // A show with no S/E marker anywhere reachable: not in the filename,
    // and not in the parent either — the parent is a "Season NN" folder,
    // which names a season and so refuses to lend an episode number.
    // NOT the crammed `101` form: that IS a marker, and `SEE_CRAMMED`
    // reads it now, so a fixture built on it would be placed, not skipped,
    // and this test would guard nothing.
    for ep in 1..=6 {
        write_fake_video(
            dir.path(),
            &format!("Paper Moons/Season 1/paper.moons-med-{ep}.avi"),
        )
        .await;
    }
    // A second broken show, so grouping is doing more than one bucket.
    for ep in 1..=2 {
        write_fake_video(dir.path(), &format!("Other Show/Season 1/clip-{ep}.mkv")).await;
    }
    let lib = store
        .create_library(&NewLibrary {
            name: "TV".into(),
            kind: LibraryKind::Shows,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");

    let r = scan_library(&store, &lib).await.expect("scan");

    assert_eq!(r.skipped, 8, "every file still counts");
    assert_eq!(
        r.skip_groups.len(),
        2,
        "eight skipped files across two shows is TWO rows, got: {:?}",
        r.skip_groups
    );
    // Worst first: the folder costing the most files is the one to rename.
    assert_eq!(r.skip_groups[0].count, 6);
    assert!(r.skip_groups[0].folder.ends_with("Paper Moons"));
    assert_eq!(r.skip_groups[1].count, 2);
    assert!(r.skip_groups[1].folder.ends_with("Other Show"));
    // Samples are a taste, not the list this replaces.
    assert_eq!(r.skip_groups[0].samples.len(), SKIP_SAMPLES);
    // And the counts still add up to the partition.
    assert_eq!(
        r.added + r.updated + r.unchanged + r.skipped + r.unreadable,
        8,
        "the buckets must still partition every candidate file"
    );
}

/// THE BUG THIS GUARDS: `errors` used to be incremented in four places and
/// only two of them recorded a `problems` line, so the settings page showed
/// a bare red "2 errors" with nothing underneath and no way to find out
/// which files were involved. Every counted error must name its file.
#[tokio::test]
async fn every_counted_error_names_its_file() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    // Not real video → ffprobe exits nonzero → the probe-failure branch.
    write_fake_video(dir.path(), "Heat (1995).mkv").await;
    write_fake_video(dir.path(), "The Matrix (1999).mkv").await;

    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");

    let r = scan_library(&store, &lib).await.expect("scan");
    assert_eq!(r.added, 2, "unprobeable files are still added");
    assert_eq!(r.errors, 2, "one probe failure each");
    assert_eq!(
        r.problems.len(),
        r.errors,
        "every error needs a line the operator can read: {:?}",
        r.problems
    );
    for name in ["Heat (1995).mkv", "The Matrix (1999).mkv"] {
        assert!(
            r.problems.iter().any(|p| p.contains(name)),
            "no problem line mentions {name}: {:?}",
            r.problems
        );
    }
    assert!(
        r.problems.iter().all(|p| p.contains("media details")),
        "probe failures should say what was lost: {:?}",
        r.problems
    );
}

/// An unreadable file (listed by the walk, then unstattable) is the one
/// error path that used to be *completely* silent — no problem line and no
/// log line either. It must report, and must not delete the file's record.
#[cfg(unix)]
#[tokio::test]
async fn unstattable_file_is_reported_and_record_kept() {
    use std::os::unix::fs::symlink;

    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let real = write_fake_video(dir.path(), "Heat (1995).mkv").await;

    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");
    assert_eq!(scan_library(&store, &lib).await.expect("scan").added, 1);

    // A symlink to the file, then the target is replaced by a dangling
    // one: WalkDir(follow_links) lists it, `metadata()` then fails.
    let link = dir.path().join("Heat (1995) copy.mkv");
    symlink(&real, &link).expect("symlink");
    std::fs::remove_file(&real).expect("rm target");

    let r = scan_library(&store, &lib).await.expect("rescan");
    assert!(r.errors >= 1, "a dangling entry must count as an error");
    assert!(
        r.problems.iter().any(|p| p.contains("Heat (1995)")),
        "expected a problem naming the unreadable file, got: {:?}",
        r.problems
    );
}

/// The record buckets must be a true partition of the files the walk found.
/// `errors` deliberately is NOT one of them — it overlaps `added` via
/// `degraded` — so this asserts the sum that actually has to hold, and that
/// the overlap is exactly as advertised.
#[tokio::test]
async fn record_buckets_partition_the_files_found() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    // 2 unprobeable movies + 1 file the movie parser still places.
    write_fake_video(dir.path(), "Heat (1995).mkv").await;
    write_fake_video(dir.path(), "The Matrix (1999).mkv").await;
    write_fake_video(dir.path(), "Sicario (2015).mkv").await;

    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");

    let r = scan_library(&store, &lib).await.expect("scan");
    let found = 3;
    assert_eq!(
        r.added + r.updated + r.unchanged + r.skipped + r.unreadable,
        found,
        "record buckets must sum to the files found: {r:?}"
    );
    // Every file was added AND degraded — the overlap the doc comment
    // promises, and the reason `errors` can exceed `found`.
    assert_eq!(r.added, found);
    assert_eq!(r.degraded, found);
    assert_eq!(r.unreadable, 0);
    assert_eq!(r.errors, r.degraded + r.unreadable);

    // Rescanning changes nothing on disk, so every file is unchanged — but
    // these fixtures are garbage bytes, so their probe failed, and a file
    // with no media details IS re-probed on every scan (that is the whole
    // repair path: a chmod doesn't move size or mtime). It stays in the
    // `unchanged` bucket while failing again, which is what keeps the
    // partition true.
    let r = scan_library(&store, &lib).await.expect("rescan");
    assert_eq!(
        r.added + r.updated + r.unchanged + r.skipped + r.unreadable,
        found
    );
    assert_eq!(r.unchanged, found);
    assert_eq!(r.updated, 0, "a repair attempt is not an edit");
    assert_eq!(r.degraded, found, "still no media details");
    assert_eq!(r.repaired, 0, "and nothing was recovered");
    assert_eq!(r.errors, r.degraded);
}

/// The retry path itself: a file whose probe failed is picked up by
/// `files_missing_probe` and re-probed on demand, without a scan.
#[tokio::test]
async fn failed_probes_are_retryable_without_a_rescan() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    write_fake_video(dir.path(), "Movie (2024).mkv").await;
    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");
    scan_library(&store, &lib).await.expect("scan");

    let broken = store
        .files_missing_probe(Some(lib.id))
        .await
        .expect("missing");
    assert_eq!(broken.len(), 1, "the garbage fixture never probed");
    assert!(!broken[0].probed);

    // ffprobe still refuses it, so the pass reports it as still failing —
    // with the file named, per the report's contract.
    let report = reprobe_files(&store, &broken).await.expect("reprobe");
    assert_eq!(report.attempted, 1);
    assert_eq!(report.repaired, 0);
    assert_eq!(report.still_failing, 1);
    assert_eq!(report.problems.len(), 1);
    assert!(report.problems[0].contains("Movie (2024).mkv"));

    // A file that vanished between scan and retry is `gone`, not an error.
    std::fs::remove_file(dir.path().join("Movie (2024).mkv")).expect("rm");
    let report = reprobe_files(&store, &broken).await.expect("reprobe");
    assert_eq!(report.gone, 1);
    assert_eq!(report.still_failing, 0);
}

#[tokio::test]
async fn problem_list_is_capped_with_a_trailing_count() {
    let store = SqliteStore::open_in_memory().expect("store");
    let dir = tempfile::tempdir().expect("tmp");
    let n = MAX_PROBLEMS + 7;
    for i in 0..n {
        write_fake_video(dir.path(), &format!("Movie {i} (2024).mkv")).await;
    }

    let lib = store
        .create_library(&NewLibrary {
            name: "Movies".into(),
            kind: LibraryKind::Movies,
            paths: vec![dir.path().to_path_buf()],
            anime: false,
        })
        .await
        .expect("lib");

    let r = scan_library(&store, &lib).await.expect("scan");
    assert_eq!(r.errors, n, "every file still counts");
    assert_eq!(
        r.problems.len(),
        MAX_PROBLEMS + 1,
        "capped list plus the summary line"
    );
    let last = r.problems.last().expect("summary line");
    assert!(
        last.contains("and 7 more problems"),
        "summary must say how many were hidden, got: {last}"
    );
}
