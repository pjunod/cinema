//! Producer reassignment uses the real writer, preserving root Stop and cached read authority.
use plurx_core::store::{
    background_jobs::BackgroundJobStore,
    subtitle_translation::{SourceRevision, TranslationProfile},
    subtitle_translation_mutation::{TranslationAction, TranslationMutation},
    subtitle_translation_read::{TranslationQuery, TranslationRead},
    SqliteStore,
};
use serde_json::Value;
fn fixture() -> (tempfile::TempDir, SqliteStore, rusqlite::Connection) {
    let dir = tempfile::tempdir().expect("owned fixture");
    let path = dir.path().join("store.db");
    let store = SqliteStore::open(&path).expect("migrate");
    let conn = rusqlite::Connection::open(path).expect("connection");
    conn.execute_batch("INSERT INTO users(id,username,password_hash,is_admin) VALUES(1,'admin','hash',1); INSERT INTO libraries(id,name,kind,paths) VALUES(1,'movies','movies','[\"/media\"]'); INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,1,'movie','Movie','Movie'); INSERT INTO files(id,item_id,path,size,mtime,probe_json) VALUES(1,1,'/media/movie',100,20,'{}'); INSERT INTO settings(key,value) VALUES('subtitles.translation.config.v1','{\"version\":1,\"enabled\":true}');").expect("catalog");
    (dir, store, conn)
}
fn mutation(action: TranslationAction, now: i64) -> TranslationMutation {
    TranslationMutation {
        user_id: 1,
        request_id: uuid::Uuid::new_v4().to_string(),
        request_digest: uuid::Uuid::new_v4().simple().to_string().repeat(2),
        now_ms: now,
        intent_expires_at_ms: now + 60_000,
        action,
    }
}
fn create() -> TranslationMutation {
    mutation(
        TranslationAction::Create {
            source: SourceRevision {
                file_id: 1,
                source_size: 100,
                source_mtime: 20,
            },
            profile: TranslationProfile {
                task: Default::default(),
                version: 1,
                model_id: "small".into(),
                model_sha256: "a".repeat(64),
                adapter_contract: "whisper.cpp-1.9.2-cpu-v1".into(),
                pipeline_revision: "translation-window-v1".into(),
                core_ms: 30_000,
                left_context_ms: 5_000,
                right_context_ms: 5_000,
                timeline_contract: "source-audio-clock-v1".into(),
            },
            audio_index: 0,
            spoken_language: "de".into(),
            duration_ms: 61_000,
            target_node_id: "worker-a".into(),
            playback_id: uuid::Uuid::new_v4().to_string(),
            position_ms: 0,
            playback_rate: 1.0,
            paused: false,
            continuity_id: None,
        },
        1_000,
    )
}
async fn operation(store: &SqliteStore, id: &str, now: i64) -> Value {
    store
        .read_subtitle_translation(TranslationRead {
            user_id: 0,
            now_ms: now,
            query: TranslationQuery::Operation {
                operation_id: id.into(),
            },
        })
        .await
        .expect("read")
        .remove(0)
}
fn retarget(op: &Value, node: &str, now: i64) -> TranslationMutation {
    mutation(
        TranslationAction::Retarget {
            operation_id: op["operation_id"].as_str().expect("id").into(),
            expected_operation_epoch: op["operation_epoch"]
                .as_str()
                .expect("epoch")
                .parse()
                .expect("number"),
            expected_view_revision: op["view_revision"]
                .as_str()
                .expect("view")
                .parse()
                .expect("number"),
            expected_preparation_job_id: op["preparation_job_id"]
                .as_str()
                .expect("producer")
                .into(),
            expected_target_node_id: op["target_node_id"].as_str().expect("target").into(),
            target_node_id: node.into(),
        },
        now,
    )
}
#[tokio::test]
async fn retarget_preserves_root_epoch_and_demand_but_never_reuses_a_producer_incarnation() {
    let (_dir, store, conn) = fixture();
    let created = store
        .mutate_subtitle_translation(create())
        .await
        .expect("create");
    let id = created["operation_id"].as_str().expect("id");
    let original = operation(&store, id, 1_001).await;
    let request = retarget(&original, "worker-b", 1_002);
    let moved = store
        .mutate_subtitle_translation(request.clone())
        .await
        .expect("retarget");
    assert_eq!(moved["outcome"], "accepted");
    let on_b = operation(&store, id, 1_003).await;
    assert_eq!(on_b["operation_epoch"], original["operation_epoch"]);
    assert_eq!(on_b["target_node_id"], "worker-b");
    assert_ne!(on_b["preparation_job_id"], original["preparation_job_id"]);
    assert_eq!(
        conn.query_row(
            "SELECT state FROM background_jobs WHERE id=?1",
            [original["preparation_job_id"].as_str().expect("old")],
            |r| r.get::<_, String>(0)
        )
        .expect("cancelled"),
        "cancelled"
    );
    assert_eq!(conn.query_row("SELECT count(*) FROM subtitle_translation_demands WHERE released=0 AND generation=1 AND operation_epoch=1",[],|r|r.get::<_,i64>(0)).expect("demand"),1);
    let back = store
        .mutate_subtitle_translation(retarget(&on_b, "worker-a", 1_004))
        .await
        .expect("back");
    assert_eq!(back["outcome"], "accepted");
    let on_a = operation(&store, id, 1_005).await;
    assert_ne!(on_a["preparation_job_id"], original["preparation_job_id"]);
    let stale = store
        .mutate_subtitle_translation(retarget(&original, "worker-b", 1_006))
        .await
        .expect("stale");
    assert_eq!(stale["outcome"], "state_conflict");
    let stop = mutation(
        TranslationAction::Stop {
            operation_id: id.into(),
            expected_operation_epoch: 1,
        },
        1_007,
    );
    store.mutate_subtitle_translation(stop).await.expect("stop");
    assert_eq!(
        store
            .mutate_subtitle_translation(request)
            .await
            .expect("replay"),
        moved
    );
    assert_eq!(operation(&store, id, 1_008).await["state"], "stopped");
}
#[tokio::test]
async fn retarget_refuses_expired_or_revoked_demand_disable_and_newer_view() {
    for change in ["UPDATE subtitle_translation_demands SET expires_at_ms=1001", "UPDATE users SET is_admin=0 WHERE id=1",
        "UPDATE settings SET value='{\"version\":1,\"enabled\":false}' WHERE key='subtitles.translation.config.v1'",
        "UPDATE subtitle_translation_operations SET view_revision=view_revision+1"] {
        let (_dir,store,conn)=fixture();
        let created=store.mutate_subtitle_translation(create()).await.expect("create");
        let id=created["operation_id"].as_str().expect("id");
        let before=operation(&store,id,1_001).await;
        let request=retarget(&before,"worker-b",1_002);
        conn.execute_batch(change).expect("changed authority");
        let result=store.mutate_subtitle_translation(request).await.expect("refused");
        assert_ne!(result["outcome"],"accepted");
        assert_eq!(operation(&store,id,1_003).await["preparation_job_id"],before["preparation_job_id"]);
    }
}
#[tokio::test]
async fn sqlite_recognized_legacy_trigger_upgrade_preserves_operation_and_receipt_rows() {
    let (dir, store, conn) = fixture();
    let request = create();
    let created = store
        .mutate_subtitle_translation(request.clone())
        .await
        .expect("create");
    let id = created["operation_id"].as_str().expect("id").to_owned();
    let legacy = include_str!("../src/store/subtitle_translation.sql")
        .split("-- next statement")
        .find(|sql| {
            sql.trim_start()
                .starts_with("CREATE TRIGGER subtitle_translation_mutate ")
        })
        .expect("known legacy trigger");
    conn.execute(
        "INSERT INTO subtitle_model_receipts(node_id,model_id,receipt_json) VALUES('worker-a','small','{\"fixture\":true}')",
        [],
    ).expect("owned managed model receipt");
    // v120 changed the managed marker and resource view. A v117 fixture must
    // restore their exact published v1 definitions before replaying v120;
    // retained operation, request, model-receipt and queue rows stay owned.
    let managed_v1 = include_str!("../src/store/subtitle_managed_schema.sql");
    let managed_marker = managed_v1
        .split("-- next statement")
        .find(|sql| sql.contains("CREATE TABLE subtitle_managed_schema("))
        .expect("published managed v1 marker DDL");
    let managed_view = managed_v1
        .split("-- next statement")
        .find(|sql| {
            sql.trim_start()
                .starts_with("CREATE VIEW IF NOT EXISTS background_job_required_resources")
        })
        .expect("published managed v1 resource view");
    conn.execute_batch(&format!(
        "SAVEPOINT historical_retarget_fixture;
         DROP TRIGGER subtitle_translation_mutate;
         DROP TRIGGER subtitle_translation_capture_continuity;
         DROP TRIGGER subtitle_translation_retire_continuity;
         DROP TRIGGER subtitle_translation_revoke_continuity;
         DROP TABLE subtitle_translation_range_continuities;
         ALTER TABLE subtitle_translation_demands DROP COLUMN continuity_id;
         {legacy}
         DROP VIEW background_job_required_resources;
         DROP TABLE subtitle_managed_schema;
         {managed_marker}
         INSERT INTO subtitle_managed_schema VALUES(1,1);
         {managed_view}
         PRAGMA user_version=117;
         RELEASE historical_retarget_fixture;"
    ))
    .expect("atomic exact historical fixture without discarding owned rows");
    drop(conn);
    drop(store);
    let reopened = SqliteStore::open(&dir.path().join("store.db")).expect("explicit upgrade");
    assert_eq!(store_epoch(&reopened, &id).await, "1");
    let upgraded = rusqlite::Connection::open(dir.path().join("store.db"))
        .expect("upgraded fixture inspection");
    assert_eq!(
        upgraded
            .query_row(
                "SELECT version FROM subtitle_managed_schema WHERE singleton=1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .expect("upgraded managed marker"),
        2
    );
    assert_eq!(
        upgraded.query_row("SELECT receipt_json FROM subtitle_model_receipts WHERE node_id='worker-a' AND model_id='small'", [], |row| row.get::<_, String>(0))
            .expect("retained managed model receipt"),
        r#"{"fixture":true}"#
    );
    assert_eq!(
        reopened
            .mutate_subtitle_translation(request)
            .await
            .expect("retained exact receipt"),
        created
    );
}
async fn store_epoch(store: &SqliteStore, id: &str) -> String {
    operation(store, id, 1_002).await["operation_epoch"]
        .as_str()
        .expect("epoch")
        .into()
}
