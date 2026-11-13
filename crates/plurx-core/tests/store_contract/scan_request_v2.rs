//! The pipeline contract v2 scan request sidecar on both backends: written
//! inside the admission, read back whole, updated in place, and one request
//! per `(API key, request_key)`. The SQLite lib test covers the same shape;
//! this runs it against three voters too, through the replicated extension
//! install (`install_scan_request_v2_schema`).
use super::*;
use plurx_core::store::background_jobs::EnqueueOutcome;
use plurx_core::store::background_jobs_library::{
    LibraryWorkInput, LibraryWorkQuery, NewLibraryWork,
};
use plurx_core::store::scan_request_v2::{
    ExpectedFile, PlacedReport, PresentReason, RetainedCode, ScanRequestV2Seed,
};

fn targeted(library_id: i64, path: &str) -> NewLibraryWork {
    NewLibraryWork {
        request_id: format!("sr-{}", uuid::Uuid::new_v4().simple()),
        library_id,
        input: LibraryWorkInput::Targeted {
            path: path.into(),
            ids: None,
            book: None,
            correlation_id: Some("t-1".into()),
            source: Some("monarr".into()),
        },
        now_ms: 1_000,
    }
}

fn seed(key_id: i64, request_key: &str) -> ScanRequestV2Seed {
    ScanRequestV2Seed {
        key_id,
        key_hash: format!("{key_id:064x}"),
        request_key: request_key.into(),
        expected_files: vec![ExpectedFile {
            path: "/tv/Show/Season 1/Show.S01E01.mkv".into(),
            bytes: 8_123_456_789,
            mtime_ns: 1_794_076_138_123_456_789,
        }],
        removed_paths: Vec::new(),
        probe_only: false,
    }
}

#[tokio::test]
async fn scan_request_sidecar_round_trips_and_key_is_unique_on_every_backend() {
    for_each_backend(|store, backend| async move {
        let library = store
            .create_library(&NewLibrary {
                name: format!("Sidecar {backend}"),
                kind: LibraryKind::Shows,
                paths: vec![format!("/tv-{backend}").into()],
                anime: false,
            })
            .await
            .expect("library");
        let first = targeted(library.id, "/tv/Show/Season 1");
        assert!(
            matches!(
                store
                    .enqueue_library_work_v2(first.clone(), seed(7, "t-1/0/abc/1"))
                    .await
                    .expect("first admission"),
                EnqueueOutcome::Accepted { .. }
            ),
            "{backend}"
        );
        let row = store
            .scan_request_v2(&first.request_id)
            .await
            .expect("read")
            .expect("written in the admission");
        assert_eq!(
            (row.key_id, row.request_key.as_str()),
            (7, "t-1/0/abc/1"),
            "{backend}"
        );
        assert_eq!(row.key_hash, seed(7, "").key_hash, "{backend}");
        assert_eq!(row.expected_files, seed(7, "").expected_files, "{backend}");
        assert_eq!(
            store
                .scan_request_by_key(&seed(7, "").key_hash, "t-1/0/abc/1")
                .await
                .expect("lookup"),
            Some(first.request_id.clone()),
            "{backend}"
        );
        assert_eq!(
            store
                .scan_request_by_key(&seed(8, "").key_hash, "t-1/0/abc/1")
                .await
                .expect("lookup"),
            None
        );

        assert!(store
            .set_scan_request_waiting_on(&first.request_id, "full_scan:TV", 2_000)
            .await
            .expect("waiting_on"));
        let jobs = store
            .library_work_requests(LibraryWorkQuery {
                request_id: Some(first.request_id.clone()),
                limit: 1,
                ..Default::default()
            })
            .await
            .expect("request");
        assert_eq!(
            store
                .set_library_job_waiting_on(&jobs[0].job_id, "root_unreadable:/tv", 2_500)
                .await
                .expect("job waiting_on"),
            1,
            "{backend}"
        );
        let files = vec![
            PlacedReport::Placed {
                path: "/tv/Show/Season 1/Show.S01E01.mkv".into(),
                item_id: 1201,
                file_id: 5531,
                bytes: 8_123_456_789,
                mtime_ns: 1_794_076_138_123_456_789,
            },
            PlacedReport::Missing {
                path: "/tv/Show/Season 1/Show.S01E02.mkv".into(),
            },
        ];
        assert!(store
            .set_scan_request_files(&first.request_id, &files, 3_000)
            .await
            .expect("files"));
        let row = store
            .scan_request_v2(&first.request_id)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(row.files, files, "{backend}");
        assert_eq!(row.waiting_on, "", "{backend}");

        // A concurrent identical request loses the whole admission.
        let duplicate = targeted(library.id, "/tv/Show/Season 1");
        assert!(
            store
                .enqueue_library_work_v2(duplicate.clone(), seed(7, "t-1/0/abc/1"))
                .await
                .is_err(),
            "{backend}"
        );
        assert!(
            store
                .library_work_requests(LibraryWorkQuery {
                    request_id: Some(duplicate.request_id.clone()),
                    limit: 1,
                    ..Default::default()
                })
                .await
                .expect("list")
                .is_empty(),
            "{backend}"
        );
        assert_eq!(
            store
                .scan_request_v2(&duplicate.request_id)
                .await
                .expect("read"),
            None
        );

        for (key_id, request_key) in [(7, "t-1/0/abc/2"), (8, "t-1/0/abc/1"), (7, ""), (7, "")] {
            let next = targeted(library.id, "/tv/Show/Season 1");
            store
                .enqueue_library_work_v2(next.clone(), seed(key_id, request_key))
                .await
                .expect("independent admission");
            assert_eq!(
                store
                    .scan_request_v2(&next.request_id)
                    .await
                    .expect("read")
                    .map(|row| (row.key_id, row.request_key)),
                Some((key_id, request_key.to_owned())),
                "{backend}"
            );
        }
        // Numeric API-key ids can be reused after deletion. Credential hashes
        // remain distinct on both backends, including the unique admission index.
        let replacement = targeted(library.id, "/tv/Show/Season 1");
        let mut replacement_seed = seed(7, "t-1/0/abc/1");
        replacement_seed.key_hash = "b".repeat(64);
        store
            .enqueue_library_work_v2(replacement.clone(), replacement_seed.clone())
            .await
            .expect("same numeric key id, different credential");
        assert_eq!(
            store
                .scan_request_by_key(&replacement_seed.key_hash, "t-1/0/abc/1")
                .await
                .expect("replacement lookup"),
            Some(replacement.request_id)
        );
        assert_eq!(
            store
                .scan_request_by_key(&seed(7, "").key_hash, "t-1/0/abc/1")
                .await
                .expect("original lookup"),
            Some(first.request_id)
        );
    })
    .await;
}

/// Contract amendment `removed_paths`: the removed paths a request names are
/// stored with it in the admission and read back apart from its expected
/// files, and the report's removal states round-trip, on every backend.
#[tokio::test]
async fn scan_request_removed_paths_round_trip_on_every_backend() {
    for_each_backend(|store, backend| async move {
        let library = store
            .create_library(&NewLibrary {
                name: format!("Removed {backend}"),
                kind: LibraryKind::Movies,
                paths: vec![format!("/movies-{backend}").into()],
                anime: false,
            })
            .await
            .expect("library");
        let request = targeted(library.id, "/movies");
        let mut moved = seed(9, "op/41/abc/1");
        moved.removed_paths = vec![
            "/movies/Dune (1984)/Dune.1984.mkv".into(),
            "/movies/Dune (1984)".into(),
        ];
        store
            .enqueue_library_work_v2(request.clone(), moved.clone())
            .await
            .expect("admission");
        let row = store
            .scan_request_v2(&request.request_id)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(row.expected_files, moved.expected_files, "{backend}");
        assert_eq!(row.removed_paths, moved.removed_paths, "{backend}");
        let files = vec![
            PlacedReport::Gone {
                path: "/movies/Dune (1984)/Dune.1984.mkv".into(),
                item_id: 77,
                item_ids: vec![77, 78],
                item_removed: true,
            },
            PlacedReport::Unknown {
                path: "/movies/Dune (1984)".into(),
            },
            PlacedReport::Present {
                path: "/movies/Other".into(),
                reason: PresentReason::SymlinkInChain,
            },
            PlacedReport::Retained {
                path: "/movies/Kept".into(),
                item_id: 79,
                code: RetainedCode::RootIdentityUnset,
                retryable: true,
                reason: "no root identity".into(),
            },
        ];
        assert!(store
            .set_scan_request_files(&request.request_id, &files, 3_000)
            .await
            .expect("files"));
        let row = store
            .scan_request_v2(&request.request_id)
            .await
            .expect("read")
            .expect("row");
        assert_eq!(row.files, files, "{backend}");
        assert_eq!(row.removed_paths, moved.removed_paths, "{backend}");
        assert!(row.probe_only, "removed paths imply probe-only, {backend}");

        // The same key again is a replay: the first row, removed paths and
        // all, is what the key names, and the second admission wrote nothing.
        let replay = targeted(library.id, "/movies");
        let mut again = seed(9, "op/41/abc/1");
        again.removed_paths = vec!["/movies/Other".into()];
        assert!(
            store
                .enqueue_library_work_v2(replay.clone(), again)
                .await
                .is_err(),
            "{backend}"
        );
        assert_eq!(
            store
                .scan_request_by_key(&moved.key_hash, "op/41/abc/1")
                .await
                .expect("by key"),
            Some(request.request_id.clone()),
            "{backend}"
        );
        assert!(store
            .scan_request_v2(&replay.request_id)
            .await
            .expect("read")
            .is_none());
    })
    .await;
}

/// A probe-only request never shares a job with one that is not, whatever the
/// path; probe-only requests share one with each other. Neither becomes
/// whole-library work.
#[tokio::test]
async fn probe_only_requests_are_their_own_job_on_every_backend() {
    for_each_backend(|store, backend| async move {
        let library = store
            .create_library(&NewLibrary {
                name: format!("Probe only {backend}"),
                kind: LibraryKind::Movies,
                paths: vec![format!("/movies-{backend}").into()],
                anime: false,
            })
            .await
            .expect("library");
        let job_of = |id: String| {
            let store = store.clone();
            async move {
                let record = store
                    .library_work_requests(LibraryWorkQuery {
                        request_id: Some(id),
                        limit: 1,
                        ..Default::default()
                    })
                    .await
                    .expect("record")
                    .remove(0);
                assert!(
                    matches!(record.input, LibraryWorkInput::Targeted { .. }),
                    "never whole-library work"
                );
                record.job_id
            }
        };
        let mut removal = seed(11, "op/1/a/1");
        removal.removed_paths = vec!["/movies/Dune/Dune.mkv".into()];
        let plain = seed(11, "op/2/b/1");
        let mut flagged = seed(11, "op/3/c/1");
        flagged.probe_only = true;
        let mut ids = Vec::new();
        for (key, seed) in [("a", removal), ("b", plain), ("c", flagged)] {
            let request = targeted(library.id, "/movies");
            store
                .enqueue_library_work_v2(request.clone(), seed)
                .await
                .unwrap_or_else(|error| panic!("{key}: {error}"));
            ids.push(job_of(request.request_id).await);
        }
        assert_ne!(
            ids[0], ids[1],
            "a removal never joins a walking request, {backend}"
        );
        assert_eq!(
            ids[0], ids[2],
            "probe-only requests share a pass, {backend}"
        );
    })
    .await;
}
