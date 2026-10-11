// Storage regressions use the same marker, reservation and maintenance paths
// as preparation construction; no test frees a counter in place of cleanup.
async fn private_storage_fixture(serve: &Arc<VodServe>, key: &str, cap: u64, budget: u64)
    -> Arc<super::copy_preparation::PreparationAllowance>
{
    let storage = super::retained::RetainedArtifactRegistry::reserve_preparation(&serve.shared, cap, budget)
        .await.expect("bounded private owner");
    storage.bind(key).expect("immutable key");
    storage.create_marker(&RenditionDir::new(serve.shared.base.join(key))).await.expect("owned marker");
    storage
}

#[tokio::test]
async fn private_footprint_release_and_inflight_commit_stay_in_one_owned_domain() {
    let base = crate::test_tempdir().expect("private footprint");
    let serve = bare_serve(base.path());
    let key = "a".repeat(64);
    let storage = private_storage_fixture(&serve, &key, 4096, 8192).await;
    let capacity = serve.shared.retained_artifacts.test_capacity_snapshot();
    assert_eq!(capacity.0 + capacity.1, 4096, "one bounded storage owner");
    serve.shared.working_set.store(37, Relaxed);
    let pending = storage.begin(200).expect("owned write before release");
    storage.release(); storage.release();
    assert!(storage.begin(1).is_none());
    serve.shared.preparation_storage.maintain(&serve.shared).await;
    assert_eq!(serve.shared.retained_artifacts.test_preparation_count(), 1);
    tokio::fs::write(base.path().join(&key).join(segment_name(0)), vec![0u8; 200]).await.expect("pending write");
    assert!(!pending.commit(true), "cancelled job cannot gain new liveness");
    assert_eq!(serve.shared.preparation_media.load(Relaxed), 200, "its already-owned write still has private storage");
    assert_eq!(serve.shared.working_set.load(Relaxed), 37);
    drain_private_cleanup(&serve).await;
    assert_eq!(serve.shared.preparation_media.load(Relaxed), 0);
    assert_eq!(serve.shared.working_set.load(Relaxed), 37);
    assert!(!base.path().join(key).exists());
}

#[tokio::test]
async fn private_unlink_failure_keeps_capacity_and_does_not_hold_an_in_budget_viewer() {
    let base = crate::test_tempdir().expect("private unlink");
    let (serve, file) = serve_on_file(base.path(), fixture_file()).await;
    let key = "b".repeat(64);
    let storage = private_storage_fixture(&serve, &key, 4096, 8192).await;
    let pending = storage.begin(100).expect("private media");
    tokio::fs::write(base.path().join(&key).join(segment_name(0)), vec![0u8; 100]).await.expect("private media");
    assert!(pending.commit(true));
    serve.shared.test_hooks().private_failures.lock().expect("fault").insert("unlink".into());
    storage.release();
    serve.shared.preparation_storage.maintain(&serve.shared).await;
    let diagnostics = serve.preparation_storage_diagnostics().await;
    assert_eq!(diagnostics.pending_cleanup_count, 1);
    assert_eq!(serve.shared.retained_artifacts.test_preparation_count(), 1);
    assert!(super::retained::RetainedArtifactRegistry::reserve_preparation(&serve.shared, 8192, 8192).await.is_none());
    create(&serve, &file, "ordinary-through-failure", "ordinary", &settings()).await;
    drop(fetch(&serve, "ordinary-through-failure", &segment_name(0)).await);
    serve.shared.test_hooks().private_failures.lock().expect("clear fault").clear();
    drain_private_cleanup(&serve).await;
    assert!(!base.path().join(&key).exists());
    assert_eq!(serve.shared.preparation_media.load(Relaxed), 0);
    serve.end("ordinary-through-failure", Terminal::Deleted).await;
}

#[tokio::test]
async fn stale_private_cleanup_cannot_touch_a_replacement_incarnation() {
    let base = crate::test_tempdir().expect("stale cleanup");
    let serve = bare_serve(base.path());
    let key = "c".repeat(64);
    let storage = private_storage_fixture(&serve, &key, 4096, 8192).await;
    let mut replacement = synthetic_rendition(base.path()).await;
    Arc::get_mut(&mut replacement).expect("unpublished replacement").key = key.clone();
    serve.shared.renditions.lock().await.insert(key.clone(), replacement);
    serve.shared.working_set.store(73, Relaxed);
    storage.release();
    serve.shared.preparation_storage.maintain(&serve.shared).await;
    assert!(base.path().join(&key).join(super::preparation_storage::MARKER).exists());
    assert_eq!(serve.shared.working_set.load(Relaxed), 73);
    assert_eq!(serve.shared.retained_artifacts.test_preparation_count(), 1);
    serve.shared.renditions.lock().await.remove(&key);
    drain_private_cleanup(&serve).await;
    assert_eq!(serve.shared.working_set.load(Relaxed), 73);

    // No current map entry: the on-disk replacement alone must protect its
    // durable plan from an old owner's retry.
    let key = "8".repeat(64);
    let storage = private_storage_fixture(&serve, &key, 4096, 8192).await;
    let directory = base.path().join(&key);
    let marker = directory.join(super::preparation_storage::MARKER);
    let original_marker = tokio::fs::read(&marker).await.expect("old marker");
    let mut replacement_marker: serde_json::Value = serde_json::from_slice(&original_marker).expect("marker");
    replacement_marker["nonce"] = serde_json::json!(uuid::Uuid::new_v4());
    tokio::fs::write(&marker, serde_json::to_vec(&replacement_marker).expect("replacement marker")).await.expect("replace owner");
    tokio::fs::write(directory.join(segment_name(0)), b"replacement media").await.expect("replacement bytes");
    let replacement = synthetic_rendition(base.path()).await;
    let identity = SourceIdentity::new(1, 1, "replacement-generation");
    assert!(serve.shared.store.put_rendition_plan(&key, 7, &replacement.plan, &identity).await.expect("replacement plan"));
    storage.release();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            serve.shared.preparation_storage.maintain(&serve.shared).await;
            if serve.preparation_storage_diagnostics().await.last_failure_class == "incarnation" { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("stale owner observes replacement");
    assert!(serve.shared.store.rendition_plan(&key, &identity).await.expect("surviving plan").is_some());
    assert_eq!(tokio::fs::read(directory.join(segment_name(0))).await.expect("surviving file"), b"replacement media");
    assert_eq!(tokio::fs::read(&marker).await.expect("surviving marker"), serde_json::to_vec(&replacement_marker).expect("same replacement"));
    tokio::fs::write(marker, original_marker).await.expect("restore fixture ownership");
    drain_private_cleanup(&serve).await;
    assert!(serve.shared.store.rendition_plan(&key, &identity).await.expect("retired old plan").is_none());
}

#[tokio::test]
async fn restart_private_marker_cleanup_preserves_surviving_hard_links() {
    let base = crate::test_tempdir().expect("restart storage");
    let serve = bare_serve(base.path());
    let key = "d".repeat(64);
    let storage = private_storage_fixture(&serve, &key, 4096, 8192).await;
    let path = base.path().join(&key).join(segment_name(0));
    tokio::fs::write(&path, b"retained media").await.expect("source scratch");
    let retained = base.path().join("retained-surviving-link");
    tokio::fs::hard_link(&path, &retained).await.expect("surviving inode owner");
    drop(storage); drop(serve); // Model lost process memory, not a cleanup.
    let fresh = bare_serve(base.path());
    fresh.shared.preparation_storage.reconcile(&fresh.shared).await;
    drain_private_cleanup(&fresh).await;
    assert!(!base.path().join(key).exists());
    assert_eq!(tokio::fs::read(retained).await.expect("hard link survives"), b"retained media");
    assert_eq!(fresh.shared.working_set.load(Relaxed), 0);
}

#[tokio::test]
async fn legacy_inventory_reserves_unmarked_preparation_and_ordinary_cache_until_proved_removed() {
    let base = crate::test_tempdir().expect("legacy cache");
    let prep = base.path().join("e".repeat(64));
    let ordinary = base.path().join("f".repeat(64));
    tokio::fs::create_dir(&prep).await.expect("old unmarked preparation");
    tokio::fs::create_dir(&ordinary).await.expect("old ordinary cache");
    tokio::fs::write(prep.join(segment_name(0)), vec![0u8; 7]).await.expect("old preparation bytes");
    tokio::fs::write(ordinary.join(segment_name(0)), vec![0u8; 11]).await.expect("old ordinary bytes");
    let serve = bare_serve(base.path());
    serve.shared.preparation_storage.reconcile(&serve.shared).await;
    assert!(serve.shared.retained_artifacts.cold_ready());
    assert_eq!(serve.shared.retained_artifacts.cold_capacity(), 18);
    assert_eq!(serve.shared.working_set.load(Relaxed), 0);
    assert!(super::retained::RetainedArtifactRegistry::reserve_preparation(&serve.shared, 1, 18).await.is_none());
    assert!(prep.exists() && ordinary.exists(), "a namespace hash is never deletion authority");
    tokio::fs::remove_file(prep.join(segment_name(0))).await.expect("safe exact removal");
    serve.shared.preparation_storage.reconcile(&serve.shared).await;
    assert_eq!(serve.shared.retained_artifacts.cold_capacity(), 11);
}

#[tokio::test]
async fn cancelled_inventory_waiter_cannot_drop_legacy_or_marked_directory_ownership() {
    for marked in [false, true] {
        let base = crate::test_tempdir().expect("cancel inventory");
        let key = "1".repeat(64);
        let path = base.path().join(&key);
        tokio::fs::create_dir(&path).await.expect("legacy dir");
        tokio::fs::write(path.join(segment_name(0)), vec![0u8; 19]).await.expect("legacy bytes");
        if marked {
            tokio::fs::write(path.join(super::preparation_storage::MARKER), serde_json::to_vec(&serde_json::json!({"version":1,"key":key,"nonce":uuid::Uuid::new_v4(),"cap":4096})).expect("marker JSON")).await.expect("crash marker");
        }
        let serve = bare_serve(base.path());
        let hooks = serve.shared.test_hooks();
        let pause = if marked { hooks.inventory_marker.arm("marked inventory") } else { hooks.inventory_entry.arm("legacy entry") };
        let waiter = { let serve = Arc::clone(&serve); tokio::spawn(async move { serve.shared.preparation_storage.reconcile(&serve.shared).await; }) };
        let held = pause.reached().await;
        waiter.abort(); let _ = waiter.await;
        assert!(!serve.shared.retained_artifacts.cold_ready());
        held.release();
        // The cancelled waiter leaves its owner completing filesystem I/O;
        // retry reconciliation may return while that owner holds the mutex.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                serve.shared.preparation_storage.reconcile(&serve.shared).await;
                if serve.shared.retained_artifacts.cold_ready() { break; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.expect("detached inventory owner settles after waiter cancellation");
        assert!(serve.shared.retained_artifacts.cold_ready());
        if marked { drain_private_cleanup(&serve).await; assert!(!path.exists()); }
        else { assert_eq!(serve.shared.retained_artifacts.cold_capacity(), 19); assert!(path.exists()); }
        assert_eq!(serve.shared.working_set.load(Relaxed), 0);
    }
}

#[tokio::test]
async fn malformed_private_marker_blocks_new_preparation_without_synthetic_viewer_pressure() {
    let base = crate::test_tempdir().expect("bad marker");
    let key = "2".repeat(64);
    tokio::fs::create_dir(base.path().join(&key)).await.expect("dir");
    tokio::fs::write(base.path().join(&key).join(super::preparation_storage::MARKER), b"malformed").await.expect("marker");
    let serve = bare_serve(base.path());
    assert!(super::retained::RetainedArtifactRegistry::reserve_preparation(&serve.shared, 1, 8192).await.is_none());
    assert!(!serve.shared.retained_artifacts.cold_ready());
    assert!(base.path().join(key).exists());
    assert_eq!(serve.shared.working_set.load(Relaxed), 0);
}

#[tokio::test]
async fn marked_restart_inventory_defers_count_saturation_and_resumes_after_cleanup() {
    let base = crate::test_tempdir().expect("saturated restart");
    let key = "3".repeat(64);
    let path = base.path().join(&key);
    tokio::fs::create_dir(&path).await.expect("dir");
    tokio::fs::write(path.join(super::preparation_storage::MARKER), serde_json::to_vec(&serde_json::json!({"version":1,"key":key,"nonce":uuid::Uuid::new_v4(),"cap":4096})).expect("marker JSON")).await.expect("marker");
    let serve = bare_serve(base.path());
    let mut reservations = Vec::new();
    loop {
        let nonce = uuid::Uuid::new_v4();
        if !serve.shared.retained_artifacts.restore_preparation(nonce, 1).expect("unique owner") { break; }
        reservations.push(nonce);
    }
    serve.shared.preparation_storage.reconcile(&serve.shared).await;
    assert!(!serve.shared.retained_artifacts.cold_ready());
    serve.shared.retained_artifacts.release_preparation(reservations.pop().expect("saturated owner"));
    serve.shared.preparation_storage.reconcile(&serve.shared).await;
    serve.shared.preparation_storage.reconcile(&serve.shared).await;
    drain_private_cleanup(&serve).await;
    assert!(serve.shared.retained_artifacts.cold_ready(), "count saturation is resumable, not corrupt ownership");
    assert!(!path.exists());
}

#[tokio::test]
async fn restored_private_cleanup_preserves_reserved_and_unreadable_durable_dependencies() {
    use plurx_core::playback::continuous_quality::{QualityAttachment, QualityLedger, QualityInterval, QualityOperation, QualityTransitionRequest};
    let base = crate::test_tempdir().expect("restored dependencies");
    let database = base.path().join("store.sqlite");
    let store = Arc::new(SqliteStore::open(&database).expect("store"));
    let key = "4".repeat(64);
    let path = base.path().join(&key);
    tokio::fs::create_dir(&path).await.expect("private crash directory");
    tokio::fs::write(path.join(super::preparation_storage::MARKER), serde_json::to_vec(&serde_json::json!({"version":1,"key":key,"nonce":uuid::Uuid::new_v4(),"cap":4096})).expect("marker JSON")).await.expect("marker");
    tokio::fs::write(path.join(segment_name(0)), b"reserved").await.expect("private bytes");
    let generation = uuid::Uuid::new_v4().to_string();
    let session = uuid::Uuid::new_v4().to_string();
    activate_control_route(&store, &session, &generation).await;
    let interval = QualityInterval { artifact_id: "a".repeat(64), rendition_id: key.clone(), timescale: 1000, from_tick: 0, through_tick: 1000, byte_length: 8 };
    let attachment = QualityAttachment { client_instance_id: uuid::Uuid::new_v4().to_string(), lifetime_id: session, attachment_id: uuid::Uuid::new_v4().to_string(), family_id: "b".repeat(64) };
    let mut ledger = QualityLedger::new(generation.clone(), 1, attachment.clone()).expect("ledger");
    let transaction = uuid::Uuid::new_v4().to_string();
    let mut request = QualityTransitionRequest { version: 1, generation, control_epoch: 1, sequence: 1, attachment, transaction_id: transaction.clone(), operation: QualityOperation::Prepare { intent_revision: 1, target_rendition_id: key.clone() } };
    ledger.apply(&request, now_ms()).expect("prepare");
    ledger.ready(&transaction, vec![interval.clone()]).expect("ready");
    request.sequence = 2; request.operation = QualityOperation::Scheduled { intervals: vec![interval] };
    ledger.apply(&request, now_ms()).expect("scheduled");
    assert!(store.write_quality_ledger(&ledger, "node-a", 0, now_ms()).await.expect("reserve"));
    let serve = local_serve(base.path().to_owned(), store.clone());
    serve.shared.preparation_storage.reconcile(&serve.shared).await;
    serve.shared.preparation_storage.maintain(&serve.shared).await;
    assert!(path.join(segment_name(0)).exists());
    assert_eq!(serve.shared.retained_artifacts.test_preparation_count(), 1);
    let connection = rusqlite::Connection::open(database).expect("fault connection");
    connection.execute("ALTER TABLE continuous_quality_ledgers RENAME TO unavailable_quality_ledgers", []).expect("actual lookup failure");
    assert!(store.quality_reserved_intervals(&key).await.is_err());
    serve.shared.preparation_storage.maintain(&serve.shared).await;
    assert!(path.join(super::preparation_storage::MARKER).exists());
    assert_eq!(serve.shared.retained_artifacts.test_preparation_count(), 1);
    connection.execute("ALTER TABLE unavailable_quality_ledgers RENAME TO continuous_quality_ledgers", []).expect("restore lookup");
    connection.execute("DELETE FROM continuous_quality_ledgers", []).expect("clear durable obligation");
    assert!(store.quality_reserved_intervals(&key).await.expect("verified unreserved").is_empty());
    drain_private_cleanup(&serve).await;
    assert!(!path.exists());
    assert_eq!(serve.shared.working_set.load(Relaxed), 0);
}
