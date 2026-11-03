//! Owner-loop lifecycle behavior against real SQLite rows and recording files.
use super::*;
use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
use plurx_core::dvr::{DvrMatchMode, DvrRule};
use plurx_core::store::{SqliteStore, Store};
use plurx_core::transcode::{EncoderCaps, Pipeline};

struct Fixture {
    root: tempfile::TempDir,
    manager: Arc<LiveTvManager>,
    owner: i64,
    other: i64,
}

impl Fixture {
    async fn new() -> Self {
        let root = crate::test_tempdir().expect("DVR lifecycle files");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("SQLite"));
        let owner = store
            .create_user("retention-owner", "unused", false)
            .await
            .expect("owner")
            .id;
        let other = store
            .create_user("other-viewer", "unused", false)
            .await
            .expect("other")
            .id;
        let transcode = Arc::new(crate::transcode::TranscodeManager::new(
            Arc::clone(&store),
            root.path().join("finite"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ));
        let manager = LiveTvManager::new(
            store,
            Arc::new(super::super::SystemInfo::default()),
            transcode,
            crate::serving_fence::ServingAuthority::always_ready(),
            "node-a".into(),
            root.path().join("live"),
            root.path().join("guide"),
        );
        Self {
            root,
            manager,
            owner,
            other,
        }
    }

    async fn rule(&self, mode: DvrKeepMode, value: i64) {
        assert!(self
            .manager
            .store
            .put_dvr_rule(&DvrRule {
                id: "retention".into(),
                owner_user_id: self.owner,
                priority: 0,
                name: "My recordings".into(),
                match_mode: DvrMatchMode::Title,
                match_value: "Programme".into(),
                channel_id: None,
                new_only: false,
                keep_mode: mode,
                keep_value: value,
                pad_start_s: 0,
                pad_end_s: 0,
                enabled: true,
                created_at_ms: 0,
                updated_at_ms: 0,
            })
            .await
            .expect("rule"));
    }

    fn row(&self, id: &str, start: i64, state: DvrState) -> DvrRecording {
        DvrRecording {
            id: id.into(),
            origin: DvrOrigin::Rule,
            rule_id: Some("retention".into()),
            requested_by_user_id: None,
            channel_id: id.into(),
            guide_number: "7.1".into(),
            channel_name: "Channel".into(),
            airing_start: start,
            airing_end: start + 60,
            capture_start: start,
            capture_end: start + 60,
            title: "Programme".into(),
            episode_title: None,
            episode: None,
            synopsis: None,
            image_url: None,
            original_air_date: None,
            series_id: None,
            programme_id: None,
            state,
            state_reason: None,
            attempt: 1,
            gap_s: 0,
            late_start_s: 0,
            tuner_owner_node_id: Some("node-a".into()),
            path: None,
            bytes: 0,
            last_progress_ms: None,
            stop_requested_at_ms: None,
            stop_requested_by_user_id: None,
            item_id: None,
            file_id: None,
            started_at_ms: None,
            finished_at_ms: None,
            stopped_by_user_id: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    async fn insert(&self, row: &DvrRecording) {
        assert_eq!(
            self.manager
                .store
                .insert_dvr_airing_if_absent(row)
                .await
                .expect("insert"),
            DvrInsertOutcome::Inserted
        );
    }

    async fn get(&self, id: &str) -> DvrRecording {
        self.manager
            .store
            .get_dvr_recording(id)
            .await
            .expect("read")
            .expect("recording")
    }

    async fn item(&self, title: &str) -> i64 {
        let library = self
            .manager
            .store
            .create_library(&NewLibrary {
                name: title.into(),
                kind: LibraryKind::Recordings,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("library");
        self.manager
            .store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: title.into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("catalog item")
    }
}

#[tokio::test]
async fn retention_orders_airings_and_applies_exact_age_boundaries_without_touching_live_rows() {
    let now = 10 * 86_400;
    for (mode, value, deleted) in [
        (DvrKeepMode::All, 0, vec![]),
        (DvrKeepMode::LastN, 2, vec!["old"]),
        (DvrKeepMode::LastN, 0, vec!["old", "boundary", "new"]),
        (DvrKeepMode::Days, 2, vec!["old"]),
        (DvrKeepMode::Days, 0, vec!["old", "boundary", "new"]),
    ] {
        let f = Fixture::new().await;
        f.rule(mode, value).await;
        for (id, start, state) in [
            ("old", now - 2 * 86_400 - 1, DvrState::Done),
            ("boundary", now - 2 * 86_400, DvrState::Partial),
            ("new", now - 1, DvrState::Done),
            ("live", now - 4 * 86_400, DvrState::Recording),
        ] {
            f.insert(&f.row(id, start, state)).await;
        }
        f.manager.dvr_retain(now).await.expect("retention");
        for id in ["old", "boundary", "new"] {
            let row = f.get(id).await;
            assert_eq!(
                row.state == DvrState::Deleted,
                deleted.contains(&id),
                "{mode:?} {id}"
            );
            let history = f
                .manager
                .store
                .list_dvr_events(id, None, None, 20)
                .await
                .expect("history");
            assert_eq!(
                history
                    .rows
                    .iter()
                    .filter(|event| event.kind == "deleted")
                    .count(),
                usize::from(deleted.contains(&id))
            );
        }
        assert_eq!(f.get("live").await.state, DvrState::Recording);
        f.manager.dvr_retain(now).await.expect("idempotent sweep");
        for id in deleted {
            let history = f
                .manager
                .store
                .list_dvr_events(id, None, None, 20)
                .await
                .expect("history");
            assert_eq!(
                history
                    .rows
                    .iter()
                    .filter(|event| event.kind == "deleted")
                    .count(),
                1
            );
        }
    }
}

#[tokio::test]
async fn until_watched_requires_the_rule_owner_and_preserves_unlinked_recordings() {
    let f = Fixture::new().await;
    f.rule(DvrKeepMode::UntilWatched, 0).await;
    let owned = f.item("owner watched").await;
    let other = f.item("other watched").await;
    f.manager
        .store
        .set_watched(f.owner, owned, true)
        .await
        .expect("owner watch");
    f.manager
        .store
        .set_watched(f.other, other, true)
        .await
        .expect("other watch");
    for (id, item) in [
        ("owner", Some(owned)),
        ("other", Some(other)),
        ("unlinked", None),
    ] {
        let mut row = f.row(id, 100, DvrState::Done);
        row.item_id = item;
        f.insert(&row).await;
    }
    f.manager
        .dvr_retain(1_000)
        .await
        .expect("owner watch retention");
    assert_eq!(f.get("owner").await.state, DvrState::Deleted);
    assert_eq!(f.get("other").await.state, DvrState::Done);
    assert_eq!(f.get("unlinked").await.state, DvrState::Done);
    assert!(
        f.manager
            .store
            .get_item(owned)
            .await
            .expect("catalog read")
            .is_some(),
        "retention marks intent before physical purge"
    );
}

#[tokio::test]
async fn purge_removes_dotted_channel_siblings_and_repairs_already_purged_catalog_links() {
    let f = Fixture::new().await;
    f.rule(DvrKeepMode::All, 0).await;
    let path = f.root.path().join("Programme_7.1.ts");
    let sidecar = f.root.path().join("Programme_7.1.json");
    let unrelated = f.root.path().join("Programme_7.json");
    tokio::fs::write(&path, b"recording")
        .await
        .expect("recording");
    tokio::fs::write(&sidecar, b"sidecar")
        .await
        .expect("sidecar");
    tokio::fs::write(&unrelated, b"unrelated")
        .await
        .expect("unrelated");
    let mut row = f.row("deleted", 100, DvrState::Deleted);
    row.path = Some(path.to_string_lossy().into_owned());
    row.item_id = Some(f.item("deleted item").await);
    row.file_id = Some(
        f.manager
            .store
            .upsert_file(
                row.item_id.expect("item"),
                &path.to_string_lossy(),
                9,
                1,
                &ProbeResult::default(),
            )
            .await
            .expect("catalog file"),
    );
    f.insert(&row).await;
    let mut legacy = f.row("legacy", 200, DvrState::Deleted);
    legacy.item_id = Some(f.item("legacy item").await);
    f.insert(&legacy).await;
    f.manager.dvr_purge_deleted().await.expect("physical purge");
    assert!(!path.exists());
    assert!(!sidecar.exists());
    assert_eq!(
        tokio::fs::read(&unrelated)
            .await
            .expect("unrelated survives"),
        b"unrelated"
    );
    for old in [&row, &legacy] {
        let current = f.get(&old.id).await;
        assert_eq!(current.state, DvrState::Deleted);
        assert!(current.path.is_none() && current.item_id.is_none() && current.file_id.is_none());
        assert!(f
            .manager
            .store
            .get_item(old.item_id.expect("old link"))
            .await
            .expect("catalog")
            .is_none());
    }
    f.manager.dvr_purge_deleted().await.expect("repeat purge");
    let events = f
        .manager
        .store
        .list_dvr_events("deleted", None, None, 20)
        .await
        .expect("events");
    assert_eq!(
        events
            .rows
            .iter()
            .filter(|event| event.kind == "deleted")
            .count(),
        1
    );
}

#[tokio::test]
async fn purge_refuses_foreign_or_unowned_paths_and_retries_failed_physical_deletion() {
    let f = Fixture::new().await;
    f.rule(DvrKeepMode::All, 0).await;
    for (id, owner) in [("foreign", Some("node-b")), ("unowned", None)] {
        let path = f.root.path().join(format!("{id}.ts"));
        tokio::fs::write(&path, b"keep")
            .await
            .expect("foreign file");
        let mut row = f.row(id, 100, DvrState::Deleted);
        row.tuner_owner_node_id = owner.map(str::to_owned);
        row.path = Some(path.to_string_lossy().into_owned());
        f.insert(&row).await;
        assert!(!f.manager.delete_recording_files(&row).await);
        assert_eq!(tokio::fs::read(path).await.expect("kept bytes"), b"keep");
    }
    let path = f.root.path().join("blocked_7.1.ts");
    tokio::fs::create_dir(&path)
        .await
        .expect("deletion refusal");
    let mut blocked = f.row("blocked", 200, DvrState::Deleted);
    blocked.path = Some(path.to_string_lossy().into_owned());
    blocked.item_id = Some(f.item("kept until disk deletion").await);
    blocked.file_id = Some(
        f.manager
            .store
            .upsert_file(
                blocked.item_id.expect("item"),
                &path.to_string_lossy(),
                0,
                1,
                &ProbeResult::default(),
            )
            .await
            .expect("catalog file"),
    );
    f.insert(&blocked).await;
    f.manager.dvr_purge_deleted().await.expect("deferred purge");
    assert_eq!(f.get("blocked").await.path, blocked.path);
    assert!(f
        .manager
        .store
        .get_item(blocked.item_id.expect("catalog link"))
        .await
        .expect("item")
        .is_some());
    tokio::fs::remove_dir(&path)
        .await
        .expect("remove obstruction");
    f.manager
        .dvr_purge_deleted()
        .await
        .expect("missing file counts as removed");
    assert!(f.get("blocked").await.path.is_none());
    assert!(f.get("foreign").await.path.is_some());
    assert!(f.get("unowned").await.path.is_some());
}

#[tokio::test]
async fn recovery_records_worker_loss_once_and_keeps_attempts_when_storage_is_unavailable() {
    let f = Fixture::new().await;
    f.rule(DvrKeepMode::All, 0).await;
    let invalid_root = f.root.path().join("not-a-directory");
    tokio::fs::write(&invalid_root, b"keep root occupant")
        .await
        .expect("blocked root");
    f.manager
        .store
        .put_setting(
            plurx_core::store::keys::DVR_ROOT,
            &invalid_root.to_string_lossy(),
        )
        .await
        .expect("root setting");
    let now = 1_800_000_000;
    let base = f.root.path().join("orphan_7.1");
    let part = attempt_path(&base, 1);
    tokio::fs::write(&part, b"captured before restart")
        .await
        .expect("sealed attempt");
    let mut row = f.row("lost", now - 100, DvrState::Recording);
    row.path = Some(final_path(&base).to_string_lossy().into_owned());
    row.attempt = DVR_ATTEMPTS_MAX;
    row.started_at_ms = Some((now - 20) * 1_000);
    row.created_at_ms = (now - 14 * 86_400) * 1_000;
    f.insert(&row).await;
    let (live, dvr) = f.manager.dvr_configs().await.expect("configuration");
    let (events, _queue) = super::super::webhook::channel();
    for _ in 0..2 {
        f.manager
            .dvr_recover(&live, &dvr, &events, live.generation, now)
            .await
            .expect("deferred recovery");
    }
    let current = f.get("lost").await;
    assert_eq!(current.state, DvrState::Recording);
    assert_eq!(current.attempt, DVR_ATTEMPTS_MAX);
    assert_eq!(current.path, row.path);
    assert_eq!(
        tokio::fs::read(&part).await.expect("attempt retained"),
        b"captured before restart"
    );
    assert!(!final_path(&base).exists());
    let history = f
        .manager
        .store
        .list_dvr_events("lost", None, None, 50)
        .await
        .expect("history");
    let losses = history
        .rows
        .iter()
        .filter(|event| event.kind == "capture_interrupted")
        .collect::<Vec<_>>();
    assert_eq!(losses.len(), 1);
    assert_eq!(losses[0].reason_code.as_deref(), Some("worker_lost"));
    assert_eq!(losses[0].attempt, Some(DVR_ATTEMPTS_MAX));
    assert!(history.head.expect("durable event head").history_has_gap);
    assert!(f
        .manager
        .resource_capture("lost")
        .await
        .expect("authority lookup")
        .is_none());
}
