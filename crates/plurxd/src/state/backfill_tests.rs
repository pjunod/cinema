use super::*;
use plurx_core::{
    domain::{NewItem, NewLibrary, ProbeResult, FIELD_ORDER_UNKNOWN},
    store::{LibraryStore, MediaStore, SettingsStore, SqliteStore},
};
use serde_json::json;

struct Fixture {
    store: Arc<SqliteStore>,
    jobs: Arc<JobManager>,
    root: tempfile::TempDir,
    item: i64,
}

impl Fixture {
    async fn new() -> Self {
        let root = crate::test_tempdir().expect("backfill fixture");
        let store =
            Arc::new(SqliteStore::open(&root.path().join("catalogue.sqlite")).expect("SQLite"));
        let library = store
            .create_library(&NewLibrary {
                name: "Legacy stored probes".into(),
                kind: LibraryKind::Movies,
                paths: vec![root.path().join("unmounted-library")],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Legacy movie".into(),
                year: Some(2026),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("movie");
        let jobs = JobManager::test_new(store.clone(), root.path().join("artwork"));
        Self {
            store,
            jobs,
            root,
            item,
        }
    }

    async fn legacy_file(&self, name: &str, raw: &str, dv: bool) -> i64 {
        let id = self.scanner_file(name, raw, dv).await;
        assert_eq!(
            self.file(id).await.field_order.as_deref(),
            Some(FIELD_ORDER_UNKNOWN),
            "the current scanner records an explicit unknown token"
        );
        // The supported scanner API cannot manufacture a pre-migration NULL.
        // Reproduce only that historical absence in this owned SQLite file.
        let connection = rusqlite::Connection::open(self.root.path().join("catalogue.sqlite"))
            .expect("owned legacy fixture connection");
        assert_eq!(
            connection
                .execute(
                    "UPDATE files SET field_order = NULL WHERE id = ?1 AND field_order = ?2",
                    rusqlite::params![id, FIELD_ORDER_UNKNOWN],
                )
                .expect("legacy field-order absence"),
            1
        );
        id
    }

    async fn scanner_file(&self, name: &str, raw: &str, dv: bool) -> i64 {
        let path = self.root.path().join("unmounted-library").join(name);
        assert!(!path.exists(), "backfills must work without opening media");
        self.store
            .upsert_file(
                self.item,
                path.to_str().expect("path"),
                1234,
                7,
                &ProbeResult {
                    container: Some("mkv".into()),
                    video_codec: Some("hevc".into()),
                    hdr: dv.then(|| "dolby_vision".into()),
                    hdr_format: dv.then(|| "Dolby Vision".into()),
                    raw_json: Some(raw.into()),
                    ..Default::default()
                },
            )
            .await
            .expect("legacy row")
    }

    async fn file(&self, id: i64) -> MediaFile {
        self.store
            .get_file(id)
            .await
            .expect("file read")
            .expect("file row")
    }

    async fn setting(&self, key: &str) -> Option<String> {
        self.store.get_setting(key).await.expect("setting read")
    }
}

fn dv_probe() -> String {
    json!({"streams":[{"index":0,"codec_type":"video","codec_name":"hevc",
        "side_data_list":[{"side_data_type":"DOVI configuration record","dv_profile":8,
            "dv_level":6,"rpu_present_flag":1,"el_present_flag":0,"bl_present_flag":1,
            "dv_bl_signal_compatibility_id":1}]}]})
    .to_string()
}

#[tokio::test]
async fn legacy_dv_backfill_advances_past_unrepairable_probes_and_derives_a_consistent_label() {
    let fixture = Fixture::new().await;
    let malformed = fixture.legacy_file("malformed.mkv", "not JSON", true).await;
    let absent = fixture
        .legacy_file("without-record.mkv", r#"{"streams":[]}"#, true)
        .await;
    let repairable = fixture
        .legacy_file("profile-eight.mkv", &dv_probe(), true)
        .await;
    fixture.jobs.clone().backfill_dolby_vision_facts().await;
    let file = fixture.file(repairable).await;
    assert_eq!(file.dolby_vision.profile, Some(8));
    assert_eq!(file.dolby_vision.level, Some(6));
    assert_eq!(file.dolby_vision.bl_compat_id, Some(1));
    let expected = plurx_core::scan::probe::parse_probe_json(
        &serde_json::from_str(&dv_probe()).expect("probe"),
    );
    assert_eq!(
        file.hdr_format, expected.hdr_format,
        "the label and authoritative DV facts move together"
    );
    for id in [malformed, absent] {
        assert!(
            fixture.file(id).await.dolby_vision.is_empty(),
            "no facts may be invented from an unparseable or absent record"
        );
    }
    let cursor = fixture.jobs.local_job_key(keys::JOB_DV_BACKFILL_CURSOR);
    assert_eq!(fixture.setting(&cursor).await, Some(repairable.to_string()));
    assert_eq!(
        fixture.setting(keys::JOB_DV_BACKFILL_DONE).await,
        None,
        "only an empty next page proves completion"
    );
    let before = fixture.file(repairable).await;
    fixture.jobs.clone().backfill_dolby_vision_facts().await;
    let after = fixture.file(repairable).await;
    assert_eq!(before.dolby_vision, after.dolby_vision);
    assert_eq!(
        before.hdr_format, after.hdr_format,
        "a still-held singleton cannot rewrite a completed row"
    );
}

#[tokio::test]
async fn legacy_dv_backfill_is_bounded_to_one_page_of_256_rows() {
    let fixture = Fixture::new().await;
    let raw = dv_probe();
    let mut ids = Vec::new();
    for index in 0..257 {
        ids.push(
            fixture
                .legacy_file(&format!("page-{index}.mkv"), &raw, true)
                .await,
        );
    }
    fixture.jobs.clone().backfill_dolby_vision_facts().await;
    assert_eq!(fixture.file(ids[255]).await.dolby_vision.profile, Some(8));
    assert!(
        fixture.file(ids[256]).await.dolby_vision.is_empty(),
        "the next page remains due"
    );
    let cursor = fixture.jobs.local_job_key(keys::JOB_DV_BACKFILL_CURSOR);
    assert_eq!(fixture.setting(&cursor).await, Some(ids[255].to_string()));
    assert_eq!(fixture.setting(keys::JOB_DV_BACKFILL_DONE).await, None);
}

#[tokio::test]
async fn legacy_backfills_honor_their_node_cursor_and_global_completion_stamp() {
    let fixture = Fixture::new().await;
    let behind = fixture.legacy_file("behind.mkv", &dv_probe(), true).await;
    let ahead = fixture.legacy_file("ahead.mkv", &dv_probe(), true).await;
    let cursor = fixture.jobs.local_job_key(keys::JOB_DV_BACKFILL_CURSOR);
    fixture
        .store
        .put_setting(&cursor, &behind.to_string())
        .await
        .expect("prior cursor");
    fixture.jobs.clone().backfill_dolby_vision_facts().await;
    assert!(
        fixture.file(behind).await.dolby_vision.is_empty(),
        "a forward-only pass must not rescan behind its durable cursor"
    );
    assert_eq!(fixture.file(ahead).await.dolby_vision.profile, Some(8));

    let empty = Fixture::new().await;
    empty.jobs.clone().backfill_dolby_vision_facts().await;
    empty.jobs.clone().backfill_video_codec_tags().await;
    empty.jobs.clone().backfill_field_order().await;
    for key in [
        keys::JOB_DV_BACKFILL_DONE,
        keys::JOB_VIDEO_CODEC_TAG_BACKFILL_DONE,
        keys::JOB_FIELD_ORDER_BACKFILL_DONE,
    ] {
        assert_eq!(
            empty.setting(key).await,
            Some("1".into()),
            "empty inventory completes {key}"
        );
    }
    let late = empty
        .scanner_file("after-completion.mkv", &dv_probe(), true)
        .await;
    empty.jobs.clone().backfill_dolby_vision_facts().await;
    empty.jobs.clone().backfill_video_codec_tags().await;
    empty.jobs.clone().backfill_field_order().await;
    let late = empty.file(late).await;
    assert!(late.dolby_vision.is_empty());
    assert!(late.video_codec_tag.is_none());
    assert_eq!(
        late.field_order.as_deref(),
        Some(FIELD_ORDER_UNKNOWN),
        "a completed migration preserves the current scanner's explicit unknown token"
    );
}

#[tokio::test]
async fn legacy_codec_tag_and_field_order_backfills_select_playable_video_and_keep_unknowns_honest()
{
    let fixture = Fixture::new().await;
    let selected=fixture.legacy_file("attached-art-first.mkv",&json!({"streams":[
        {"index":0,"codec_type":"video","codec_name":"mjpeg","codec_tag_string":"jpeg","field_order":"progressive","disposition":{"attached_pic":1}},
        {"index":3,"codec_type":"video","codec_name":"hevc","codec_tag_string":"hev1","field_order":"tt","disposition":{"attached_pic":0}},
        {"index":4,"codec_type":"audio","codec_name":"aac","codec_tag_string":"mp4a"}
    ]}).to_string(),false).await;
    let progressive=fixture.legacy_file("progressive.mkv",&json!({"streams":[
        {"codec_type":"video","codec_name":"hevc","codec_tag_string":"hvc1","field_order":"progressive"}
    ]}).to_string(),false).await;
    let unknown = fixture
        .legacy_file("unknown.mkv", "broken stored probe", false)
        .await;
    fixture.jobs.clone().backfill_video_codec_tags().await;
    fixture.jobs.clone().backfill_field_order().await;
    let selected = fixture.file(selected).await;
    assert_eq!(selected.video_codec_tag.as_deref(), Some("hev1"));
    assert_eq!(
        selected.field_order.as_deref(),
        Some("tt"),
        "cover art may not choose the playback field order"
    );
    let progressive = fixture.file(progressive).await;
    assert_eq!(progressive.video_codec_tag.as_deref(), Some("hvc1"));
    assert_eq!(progressive.field_order.as_deref(), Some("progressive"));
    let unknown_row = fixture.file(unknown).await;
    assert!(
        unknown_row.video_codec_tag.is_none(),
        "a malformed probe cannot invent a sample entry"
    );
    assert_eq!(
        unknown_row.field_order.as_deref(),
        Some(FIELD_ORDER_UNKNOWN)
    );
    for key in [
        keys::JOB_VIDEO_CODEC_TAG_BACKFILL_CURSOR,
        keys::JOB_FIELD_ORDER_BACKFILL_CURSOR,
    ] {
        assert_eq!(
            fixture.setting(&fixture.jobs.local_job_key(key)).await,
            Some(unknown.to_string()),
            "unrepairable rows must not starve the rest of {key}"
        );
    }
}

struct Learner;

#[plurx_core::cluster::coordination::cluster_job_async_trait]
impl ClusterJobAuthority for Learner {
    async fn may_run_cluster_jobs(&self) -> bool {
        false
    }
}

#[tokio::test]
async fn legacy_backfills_acquire_no_singleton_and_write_nothing_on_a_learner() {
    let fixture = Fixture::new().await;
    let file = fixture.legacy_file("learner.mkv", &dv_probe(), true).await;
    let jobs = Arc::new(JobManager::new_with_scan_prune_percent(
        fixture.store.clone(),
        fixture.root.path().join("learner-artwork"),
        plurx_core::config::DEFAULT_SCAN_PRUNE_PERCENT,
        "learner".into(),
        Arc::new(Learner),
    ));
    jobs.clone().backfill_dolby_vision_facts().await;
    jobs.clone().backfill_video_codec_tags().await;
    jobs.clone().backfill_field_order().await;
    let row = fixture.file(file).await;
    assert!(row.dolby_vision.is_empty());
    assert!(row.video_codec_tag.is_none());
    assert!(row.field_order.is_none());
    for key in [
        keys::JOB_DV_BACKFILL_CURSOR,
        keys::JOB_VIDEO_CODEC_TAG_BACKFILL_CURSOR,
        keys::JOB_FIELD_ORDER_BACKFILL_CURSOR,
    ] {
        assert_eq!(fixture.setting(&jobs.local_job_key(key)).await, None);
    }
    for resource in [
        "catalogue:dv-facts",
        "catalogue:video-codec-tag",
        "catalogue:field-order",
    ] {
        let lease = fixture
            .jobs
            .acquire_job(resource.into())
            .await
            .expect("writable store")
            .expect("learner took no lease");
        lease.release().await.expect("release check lease");
    }
}
