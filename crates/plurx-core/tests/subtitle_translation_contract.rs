//! The real SQLite bridge, schema migration and common queue execute together.
use plurx_core::store::{
    background_jobs::{BackgroundJobStore, ClaimJob, ClaimOutcome, JobKind},
    subtitle_translation::{SourceRevision, TranslationProfile},
    subtitle_translation_mutation::{TranslationAction, TranslationMutation},
    SqliteStore,
};
fn profile() -> TranslationProfile {
    TranslationProfile {
        version: 1,
        model_id: "small".into(),
        model_sha256: "a".repeat(64),
        adapter_contract: "whisper.cpp-1.9.2-cpu-v1".into(),
        pipeline_revision: "translation-window-v1".into(),
        core_ms: 30000,
        left_context_ms: 5000,
        right_context_ms: 5000,
        timeline_contract: "source-audio-clock-v1".into(),
    }
}
fn fixture() -> (tempfile::TempDir, SqliteStore, rusqlite::Connection) {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("store.db");
    let store = SqliteStore::open(&path).expect("real migration");
    let conn = rusqlite::Connection::open(path).expect("fixture connection");
    conn.execute_batch("INSERT INTO users(id,username,password_hash,is_admin) VALUES(1,'admin','hash',1),(2,'viewer','hash',0); INSERT INTO libraries(id,name,kind,paths) VALUES(1,'movies','movies','[\"/media\"]');INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,1,'movie','Movie','Movie'); INSERT INTO files(id,item_id,path,size,mtime,probe_json) VALUES(1,1,'/media/movie',100,20,'{}');INSERT INTO settings(key,value) VALUES('subtitles.translation.config.v1','{\"version\":1,\"enabled\":true}');").expect("catalog fixture");
    (dir, store, conn)
}
fn create(now: i64) -> TranslationMutation {
    TranslationMutation {
        user_id: 1,
        request_id: uuid::Uuid::new_v4().to_string(),
        request_digest: uuid::Uuid::new_v4().simple().to_string().repeat(2),
        intent_expires_at_ms: now + 300000,
        now_ms: now,
        action: TranslationAction::Create {
            source: SourceRevision {
                file_id: 1,
                source_size: 100,
                source_mtime: 20,
            },
            profile: profile(),
            audio_index: 0,
            spoken_language: "de".into(),
            duration_ms: 61000,
            target_node_id: "worker".into(),
            playback_id: uuid::Uuid::new_v4().to_string(),
            position_ms: 0,
            playback_rate: 1.0,
            paused: false,
        },
    }
}
fn mutate(action: TranslationAction, now: i64) -> TranslationMutation {
    TranslationMutation {
        user_id: 1,
        request_id: uuid::Uuid::new_v4().to_string(),
        request_digest: uuid::Uuid::new_v4().simple().to_string().repeat(2),
        intent_expires_at_ms: now + 300000,
        now_ms: now,
        action,
    }
}
#[tokio::test]
async fn durable_preparation_stop_and_exact_replay_never_resurrect_work() {
    let (_dir, store, conn) = fixture();
    let input = create(1000);
    let first = store
        .mutate_subtitle_translation(input.clone())
        .await
        .expect("admitted");
    assert_eq!(first["outcome"], "accepted");
    let op = first["operation_id"].as_str().expect("root").to_owned();
    let jobid = first["job_id"].as_str().expect("real job").to_owned();
    let job = store
        .background_job(&jobid)
        .await
        .expect("lookup")
        .expect("durable queue job");
    assert_eq!(
        job.supported_payload().expect("typed payload").kind(),
        JobKind::SubtitleTranslationPrepare
    );
    let stopped = store
        .mutate_subtitle_translation(mutate(
            TranslationAction::Stop {
                operation_id: op.clone(),
                expected_operation_epoch: 1,
            },
            1100,
        ))
        .await
        .expect("stop");
    assert_eq!(stopped["operation_epoch"], "2");
    assert_eq!(stopped["state"], "stopped");
    let replay = store
        .mutate_subtitle_translation(input.clone())
        .await
        .expect("exact replay");
    assert_eq!(first, replay);
    assert_eq!(
        conn.query_row("SELECT count(*) FROM background_jobs", [], |r| r
            .get::<_, i64>(0))
            .expect("count"),
        1
    );
    let stale = store
        .mutate_subtitle_translation(mutate(
            TranslationAction::Demand {
                operation_id: op,
                expected_operation_epoch: 1,
                demand_id: first["demand_id"].as_str().expect("demand").into(),
                expected_generation: 1,
                playback_id: match input.action {
                    TranslationAction::Create { playback_id, .. } => playback_id,
                    _ => unreachable!(),
                },
                position_ms: 30000,
                playback_rate: 1.0,
                paused: false,
            },
            1200,
        ))
        .await
        .expect("stale heartbeat verdict");
    assert_eq!(stale["outcome"], "state_conflict");
    let current = store
        .background_job(&jobid)
        .await
        .expect("lookup")
        .expect("retained");
    assert_eq!(format!("{:?}", current.state), "Cancelled");
    assert!(matches!(
        store
            .claim_artifact_job(ClaimJob {
                job_id: jobid,
                expected_revision: current.revision,
                node_id: "worker".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::SubtitleTranslationPrepare,
                payload_version: 1,
                now_ms: 1300,
                dispatched_at_ms: 1300
            })
            .await
            .expect("claim"),
        ClaimOutcome::Cancelled | ClaimOutcome::Contended
    ));
}
#[tokio::test]
async fn expired_missing_heartbeat_receipt_and_quotas_do_not_create_work() {
    let (_dir, store, conn) = fixture();
    let input = create(1000);
    let first = store
        .mutate_subtitle_translation(input.clone())
        .await
        .expect("create");
    let op = first["operation_id"].as_str().expect("root").to_owned();
    let playback = match &input.action {
        TranslationAction::Create { playback_id, .. } => playback_id.clone(),
        _ => unreachable!(),
    };
    let mut heartbeat = mutate(
        TranslationAction::Demand {
            operation_id: op.clone(),
            expected_operation_epoch: 1,
            demand_id: first["demand_id"].as_str().expect("demand").into(),
            expected_generation: 1,
            playback_id: playback,
            position_ms: 1000,
            playback_rate: 1.0,
            paused: false,
        },
        1100,
    );
    heartbeat.intent_expires_at_ms = 1200;
    let receipt = store
        .mutate_subtitle_translation(heartbeat.clone())
        .await
        .expect("heartbeat");
    assert_eq!(receipt["demand_generation"], "2");
    conn.execute(
        "DELETE FROM subtitle_translation_requests WHERE request_id=?1",
        [&heartbeat.request_id],
    )
    .expect("retention beyond validity");
    heartbeat.now_ms = 62000;
    assert_eq!(
        store
            .mutate_subtitle_translation(heartbeat)
            .await
            .expect("expired first use")["outcome"],
        "intent_expired"
    );
    assert_eq!(
        conn.query_row(
            "SELECT generation FROM subtitle_translation_demands WHERE operation_id=?1",
            [op],
            |r| r.get::<_, i64>(0)
        )
        .expect("generation"),
        2
    );
    for i in 0..1020 {
        conn.execute(
            "INSERT INTO subtitle_translation_requests VALUES(1,?1,?2,'{}',0,999999999)",
            rusqlite::params![format!("retained-{i}"), "b".repeat(64)],
        )
        .expect("quota fixture");
    }
    assert_eq!(
        store
            .mutate_subtitle_translation(create(63000))
            .await
            .expect("quota verdict")["outcome"],
        "receipt_quota"
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM background_jobs", [], |r| r
            .get::<_, i64>(0))
            .expect("jobs"),
        1
    );
}

#[tokio::test]
async fn normal_rate_exhaustion_cannot_prevent_stop_or_release() {
    let (_dir, store, conn) = fixture();
    let input = create(1000);
    let first = store
        .mutate_subtitle_translation(input.clone())
        .await
        .expect("create");
    for i in 0..60 {
        conn.execute(
            "INSERT INTO subtitle_translation_requests VALUES(1,?1,?2,'{}',1000,999999999)",
            rusqlite::params![format!("rate-{i}"), "b".repeat(64)],
        )
        .expect("rate fixture");
    }
    assert_eq!(
        store
            .mutate_subtitle_translation(create(1200))
            .await
            .expect("rate verdict")["outcome"],
        "rate_limited"
    );
    let playback = match input.action {
        TranslationAction::Create { playback_id, .. } => playback_id,
        _ => unreachable!(),
    };
    let release = store
        .mutate_subtitle_translation(mutate(
            TranslationAction::Release {
                operation_id: first["operation_id"].as_str().expect("root").into(),
                expected_operation_epoch: 1,
                demand_id: first["demand_id"].as_str().expect("demand").into(),
                expected_generation: 1,
                playback_id: playback,
            },
            1300,
        ))
        .await
        .expect("release");
    assert_eq!(release["outcome"], "accepted");
    let stopped = store
        .mutate_subtitle_translation(mutate(
            TranslationAction::Stop {
                operation_id: first["operation_id"].as_str().expect("root").into(),
                expected_operation_epoch: 1,
            },
            1400,
        ))
        .await
        .expect("stop");
    assert_eq!(stopped["state"], "stopped");
}

async fn claim(
    store: &SqliteStore,
    id: &str,
    kind: JobKind,
    now: i64,
) -> plurx_core::store::background_jobs::BackgroundJob {
    let job = store
        .background_job(id)
        .await
        .expect("job")
        .expect("present");
    match store
        .claim_artifact_job(ClaimJob {
            job_id: id.into(),
            expected_revision: job.revision,
            node_id: "worker".into(),
            boot_id: "00000000-0000-4000-8000-000000000002".into(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind,
            payload_version: 1,
            now_ms: now,
            dispatched_at_ms: now,
        })
        .await
        .expect("claim")
    {
        ClaimOutcome::Claimed { job } => *job,
        other => panic!("claim: {other:?}"),
    }
}
#[tokio::test]
async fn source_bind_cannot_replace_same_epoch_bytes_and_retry_can_revive_verified_retired_cache() {
    use plurx_core::store::subtitle_translation_publication::BindTranslationSource;
    let (_dir, store, conn) = fixture();
    let input = create(1000);
    let first = store
        .mutate_subtitle_translation(input.clone())
        .await
        .expect("create");
    let job = claim(
        &store,
        first["job_id"].as_str().expect("job"),
        JobKind::SubtitleTranslationPrepare,
        1100,
    )
    .await;
    let token = job.token.expect("claim token");
    let source: plurx_core::store::background_jobs::JobPayload =
        serde_json::from_value(job.payload).expect("payload");
    // Simulate an already accepted hash from this live attempt. A competing hash
    // with unchanged scanner metadata cannot change that epoch's asset binding.
    let a = "c".repeat(64);
    let b = "d".repeat(64);
    conn.execute("INSERT INTO subtitle_translation_source_bindings VALUES(?1,1,'worker',?2,'held-A',?3,1050)",rusqlite::params![first["operation_id"].as_str().expect("root"),token.boot_id,a]).expect("accepted binding");
    assert!(!store
        .bind_translation_source(BindTranslationSource {
            token: token.clone(),
            source: source.clone(),
            source_sha256: b,
            local_identity: "held-B".into(),
            now_ms: 1200
        })
        .await
        .expect("refused changed bytes"));
    assert!(store
        .bind_translation_source(BindTranslationSource {
            token,
            source,
            source_sha256: a,
            local_identity: "held-A".into(),
            now_ms: 1250
        })
        .await
        .expect("same accepted bytes"));
    let asset: String = conn
        .query_row(
            "SELECT asset_id FROM subtitle_translation_operations",
            [],
            |r| r.get(0),
        )
        .expect("asset");
    conn.execute(
        "UPDATE subtitle_translation_assets SET retired_at_ms=1300 WHERE id=?1",
        [&asset],
    )
    .expect("retired cache");
    let op = first["operation_id"].as_str().expect("root").to_owned();
    let stop = store
        .mutate_subtitle_translation(mutate(
            TranslationAction::Stop {
                operation_id: op.clone(),
                expected_operation_epoch: 1,
            },
            1400,
        ))
        .await
        .expect("stop");
    let retry = store
        .mutate_subtitle_translation(mutate(
            TranslationAction::RetryOperation {
                operation_id: op,
                expected_operation_epoch: 2,
                expected_view_revision: stop["view_revision"]
                    .as_str()
                    .expect("view")
                    .parse()
                    .expect("integer"),
                expected_state: "stopped".into(),
                playback_id: uuid::Uuid::new_v4().to_string(),
                position_ms: 0,
                playback_rate: 1.0,
                paused: false,
            },
            1500,
        ))
        .await
        .expect("retry");
    let job = claim(
        &store,
        retry["job_id"].as_str().expect("new prep"),
        JobKind::SubtitleTranslationPrepare,
        1600,
    )
    .await;
    assert!(store
        .bind_translation_source(BindTranslationSource {
            token: job.token.expect("token"),
            source: serde_json::from_value(job.payload).expect("payload"),
            source_sha256: "c".repeat(64),
            local_identity: "fresh-held-A".into(),
            now_ms: 1700
        })
        .await
        .expect("explicit verified revival"));
    assert_eq!(conn.query_row("SELECT count(*) FROM subtitle_translation_assets WHERE id=?1 AND retired_at_ms IS NULL",[asset],|r|r.get::<_,i64>(0)).expect("revived"),1);
}

async fn prepared(store: &SqliteStore) -> plurx_core::store::subtitle_translation::RangeRecipe {
    use plurx_core::store::{
        background_jobs::JobPayload, subtitle_translation::RangeRecipe,
        subtitle_translation_publication::BindTranslationSource,
    };
    let first = store
        .mutate_subtitle_translation(create(1000))
        .await
        .expect("create");
    let job = claim(
        store,
        first["job_id"].as_str().expect("job"),
        JobKind::SubtitleTranslationPrepare,
        1100,
    )
    .await;
    let source = job.supported_payload().expect("typed source");
    let JobPayload::SubtitleTranslationPrepare {
        file_id,
        source_size,
        source_mtime,
        operation_id,
        operation_epoch,
        profile,
        audio_index,
        spoken_language,
        duration_ms,
        ..
    } = source.clone()
    else {
        panic!("prepare")
    };
    let mut recipe = RangeRecipe {
        asset_id: String::new(),
        operation_id,
        operation_epoch,
        source: SourceRevision {
            file_id,
            source_size,
            source_mtime,
        },
        source_sha256: "c".repeat(64),
        profile,
        audio_index,
        spoken_language,
        duration_ms,
        core_index: 0,
    };
    recipe.asset_id = recipe.expected_asset_id().expect("asset identity");
    assert!(store
        .bind_translation_source(BindTranslationSource {
            token: job.token.expect("token"),
            source,
            source_sha256: recipe.source_sha256.clone(),
            local_identity: "verified-held-source".into(),
            now_ms: 1200
        })
        .await
        .expect("bind"));
    recipe
}
async fn range(
    store: &SqliteStore,
    recipe: &plurx_core::store::subtitle_translation::RangeRecipe,
    label: &str,
    now: i64,
) -> plurx_core::store::background_jobs::BackgroundJob {
    use plurx_core::store::background_jobs::{EnqueueJob, EnqueueOutcome, JobPayload, JobRequest};
    let id = uuid::Uuid::new_v4().to_string();
    let payload = JobPayload::SubtitleTranslationRange {
        file_id: 1,
        recipe: recipe.clone(),
        target_node_id: "worker".into(),
    };
    let outcome = store
        .enqueue_job(EnqueueJob {
            id: id.clone(),
            payload,
            dedupe_key: format!("translation-contract-{label}"),
            priority: 1,
            not_before_ms: 0,
            now_ms: now,
            request: JobRequest {
                scope: "translation-contract".into(),
                request_id: uuid::Uuid::new_v4().to_string(),
                request_digest: "e".repeat(64),
                consumer_kind: "subtitle_translation".into(),
                consumer_ref: recipe.operation_id.clone(),
                target_node_id: Some("worker".into()),
                deadline_ms: None,
                retain_identity: false,
            },
        })
        .await
        .expect("range enqueue");
    assert!(matches!(outcome, EnqueueOutcome::Accepted { .. }));
    claim(store, &id, JobKind::SubtitleTranslationRange, now + 1).await
}
fn draft(
    recipe: &plurx_core::store::subtitle_translation::RangeRecipe,
    fence: i64,
    text: &str,
) -> plurx_core::store::subtitle_translation::SegmentDraft {
    use plurx_core::store::subtitle_translation::{
        cue_id, ExecutionProvenance, SegmentDraft, TranslationCue,
    };
    let (start_ms, end_ms) = recipe.core_bounds().expect("core");
    SegmentDraft {
        asset_id: recipe.asset_id.clone(),
        core_index: recipe.core_index,
        start_ms,
        end_ms,
        cues: vec![TranslationCue {
            cue_id: cue_id(
                &recipe.asset_id,
                recipe.core_index,
                0,
                start_ms,
                start_ms + 1000,
                text,
            )
            .expect("cue ID"),
            start_ms,
            end_ms: start_ms + 1000,
            text: text.into(),
        }],
        provenance: ExecutionProvenance {
            engine_sha256: "b".repeat(64),
            model_sha256: recipe.profile.model_sha256.clone(),
            operation_epoch: recipe.operation_epoch,
            platform: "macos-arm64".into(),
            profile_id: recipe.profile.id().expect("profile ID"),
            source_sha256: recipe.source_sha256.clone(),
            worker_fence: fence,
        },
    }
}
#[tokio::test]
async fn immutable_winner_settles_loser_and_leases_expire_without_conditional_renewal() {
    use plurx_core::store::{
        subtitle_translation_publication::PublishTranslationSegment,
        subtitle_translation_read::{AcquireTranslationLease, TranslationQuery, TranslationRead},
    };
    let (_dir, store, conn) = fixture();
    let recipe = prepared(&store).await;
    let first = range(&store, &recipe, "first", 1300).await;
    let firstdraft = draft(&recipe, first.fence, "First valid English sentence");
    assert!(store
        .publish_translation_segment(PublishTranslationSegment {
            token: first.token.clone().expect("first token"),
            source: first.supported_payload().expect("first payload"),
            segment: firstdraft.clone(),
            now_ms: 1400
        })
        .await
        .expect("publish"));
    let body: String = conn
        .query_row("SELECT body FROM subtitle_translation_segments", [], |r| {
            r.get(0)
        })
        .expect("winner");
    let view: i64 = conn
        .query_row(
            "SELECT manifest_revision FROM subtitle_translation_assets",
            [],
            |r| r.get(0),
        )
        .expect("view");
    let second = range(&store, &recipe, "second", 1500).await;
    assert!(store
        .publish_translation_segment(PublishTranslationSegment {
            token: second.token.clone().expect("second token"),
            source: second.supported_payload().expect("second payload"),
            segment: draft(&recipe, second.fence, "Different valid inference"),
            now_ms: 1600
        })
        .await
        .expect("settle against winner"));
    assert_eq!(
        body,
        conn.query_row("SELECT body FROM subtitle_translation_segments", [], |r| {
            r.get::<_, String>(0)
        })
        .expect("immutable")
    );
    assert_eq!(
        view,
        conn.query_row(
            "SELECT manifest_revision FROM subtitle_translation_assets",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("no duplicate manifest revision")
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_snapshots",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("no unleased snapshots"),
        0
    );
    let lease = store
        .acquire_translation_lease(AcquireTranslationLease {
            user_id: 1,
            asset_id: recipe.asset_id.clone(),
            now_ms: 1700,
        })
        .await
        .expect("lease")
        .expect("admitted");
    let id = lease["lease_id"].as_str().expect("ID").to_owned();
    let same = store
        .acquire_translation_lease(AcquireTranslationLease {
            user_id: 1,
            asset_id: recipe.asset_id.clone(),
            now_ms: 1800,
        })
        .await
        .expect("reuse")
        .expect("live");
    assert_eq!(same["lease_id"], id);
    assert_eq!(same["expires_at_ms"], lease["expires_at_ms"]);
    let rows = store
        .read_subtitle_translation(TranslationRead {
            user_id: 1,
            now_ms: 2000,
            query: TranslationQuery::Segments {
                asset_id: recipe.asset_id.clone(),
                lease_id: id.clone(),
                after_core: -1,
                through_core: 3,
            },
        })
        .await
        .expect("bounded page");
    assert_eq!(rows.len(), 1);
    let expired = store
        .read_subtitle_translation(TranslationRead {
            user_id: 1,
            now_ms: 31700,
            query: TranslationQuery::Segment {
                asset_id: recipe.asset_id.clone(),
                lease_id: id,
                segment_id: firstdraft.segment_id(),
            },
        })
        .await
        .expect("expired read");
    assert!(expired.is_empty(), "same ETag cannot bypass lease expiry");
    let fresh = store
        .acquire_translation_lease(AcquireTranslationLease {
            user_id: 1,
            asset_id: recipe.asset_id,
            now_ms: 31800,
        })
        .await
        .expect("renew")
        .expect("fresh snapshot");
    assert_eq!(fresh["manifest_revision"], "1");
}

#[tokio::test]
async fn semantic_request_replay_precedes_changed_admission_and_requires_current_admin() {
    use plurx_core::store::subtitle_translation_read::{TranslationQuery, TranslationRead};
    let (_dir, store, conn) = fixture();
    let input = create(1000);
    let accepted = store
        .mutate_subtitle_translation(input.clone())
        .await
        .expect("create");
    conn.execute("UPDATE files SET size=101", [])
        .expect("changed authoritative source");
    let lookup = TranslationRead {
        user_id: 1,
        now_ms: 400000,
        query: TranslationQuery::RequestReceipt {
            request_id: input.request_id.clone(),
            request_digest: input.request_digest.clone(),
        },
    };
    assert_eq!(
        store
            .read_subtitle_translation(lookup.clone())
            .await
            .expect("replay"),
        vec![accepted]
    );
    assert_eq!(
        store
            .read_subtitle_translation(TranslationRead {
                query: TranslationQuery::RequestReceipt {
                    request_id: input.request_id.clone(),
                    request_digest: "f".repeat(64)
                },
                ..lookup.clone()
            })
            .await
            .expect("conflict")[0]["outcome"],
        "request_conflict"
    );
    conn.execute("UPDATE users SET is_admin=0 WHERE id=1", [])
        .expect("revoke");
    assert!(store
        .read_subtitle_translation(lookup)
        .await
        .expect("current authority")
        .is_empty());
}

#[tokio::test]
async fn expired_preparation_and_existing_asset_can_acquire_fresh_owned_preparation() {
    let (_dir, store, _conn) = fixture();
    let input = create(1000);
    let first = store
        .mutate_subtitle_translation(input.clone())
        .await
        .expect("create");
    let id = first["job_id"].as_str().expect("job");
    store
        .cancel_job(plurx_core::store::background_jobs::CancelJob {
            job_id: id.into(),
            now_ms: 17000,
        })
        .await
        .expect("expiry cleanup");
    let mut again = input.clone();
    again.request_id = uuid::Uuid::new_v4().to_string();
    again.request_digest = "b".repeat(64);
    again.now_ms = 18000;
    again.intent_expires_at_ms = 300000;
    let next = store
        .mutate_subtitle_translation(again)
        .await
        .expect("explicit new demand");
    assert_eq!(first["operation_id"], next["operation_id"]);
    assert_ne!(first["job_id"], next["job_id"]);
    claim(
        &store,
        next["job_id"].as_str().expect("new prep"),
        JobKind::SubtitleTranslationPrepare,
        18100,
    )
    .await;
    // A completed preparation on a bound asset is equally re-created on fresh
    // explicit Translate; this is the boot-time full-source verification seam.
    let (_dir2, store2, _conn2) = fixture();
    let recipe = prepared(&store2).await;
    let mut fresh = create(1500);
    if let TranslationAction::Create { playback_id, .. } = &mut fresh.action {
        *playback_id = uuid::Uuid::new_v4().to_string();
    }
    let receipt = store2
        .mutate_subtitle_translation(fresh)
        .await
        .expect("fresh Translate");
    assert_eq!(receipt["operation_id"], recipe.operation_id);
    let job = store2
        .background_job(receipt["job_id"].as_str().expect("boot prep"))
        .await
        .expect("lookup")
        .expect("job");
    assert_eq!(
        job.supported_payload().expect("payload").kind(),
        JobKind::SubtitleTranslationPrepare
    );
    assert_eq!(format!("{:?}", job.state), "Queued");
}

#[tokio::test]
async fn cancelled_range_rebinds_to_new_job_and_projects_the_new_failure() {
    use plurx_core::store::background_jobs::{JobSettlement, SettleJob};
    let (_dir, store, conn) = fixture();
    let recipe = prepared(&store).await;
    let old = range(&store, &recipe, "seek-away", 1300).await;
    let oldtoken = old.token.clone().expect("token");
    store
        .settle_job(SettleJob {
            token: oldtoken,
            settlement: JobSettlement::Stop {
                error_code: "demand_expired".into(),
            },
            now_ms: 1400,
        })
        .await
        .expect("stop");
    let new = range(&store, &recipe, "seek-back", 1500).await;
    assert_ne!(old.id, new.id);
    assert_eq!(
        conn.query_row("SELECT job_id FROM subtitle_translation_ranges", [], |r| {
            r.get::<_, String>(0)
        })
        .expect("rebound"),
        new.id
    );
    store
        .settle_job(SettleJob {
            token: new.token.expect("newtoken"),
            settlement: JobSettlement::Fail {
                error_code: "inference_failed".into(),
            },
            now_ms: 1600,
        })
        .await
        .expect("new failure");
    assert_eq!(
        conn.query_row("SELECT state FROM subtitle_translation_ranges", [], |r| {
            r.get::<_, String>(0)
        })
        .expect("projection"),
        "failed"
    );
}

#[tokio::test]
async fn idle_cache_collection_preserves_live_read_lease_and_stop_tombstone() {
    use plurx_core::store::subtitle_translation_read::AcquireTranslationLease;
    let (_dir, store, conn) = fixture();
    let recipe = prepared(&store).await;
    // Age only cache metadata, not the service clock or authoritative source.
    conn.execute(
        "UPDATE subtitle_translation_assets SET last_read_at_ms=0,created_at_ms=0",
        [],
    )
    .expect("age");
    let lease = store
        .acquire_translation_lease(AcquireTranslationLease {
            user_id: 1,
            asset_id: recipe.asset_id.clone(),
            now_ms: 100000000,
        })
        .await
        .expect("lease")
        .expect("live");
    conn.execute("UPDATE subtitle_translation_demands SET released=1", [])
        .expect("no active demand");
    conn.execute(
        "UPDATE subtitle_translation_assets SET last_read_at_ms=0",
        [],
    )
    .expect("old cache");
    store
        .maintain_translation_cache(100000001)
        .await
        .expect("collect protected");
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_assets WHERE retired_at_ms IS NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("held"),
        1
    );
    assert!(lease["expires_at_ms"]
        .as_i64()
        .is_some_and(|n| n > 100000001));
    store
        .maintain_translation_cache(100040000)
        .await
        .expect("collect after expiry");
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_assets WHERE retired_at_ms IS NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("retired"),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_operations",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("durable root"),
        1
    );
}

#[tokio::test]
async fn translation_qualification_never_mints_unrun_generation_readiness() {
    use plurx_core::store::{
        background_jobs::{EnqueueJob, JobPayload, JobRequest},
        background_jobs_transcription::{
            ModelAction, PublishSubtitleModel, SubtitleModelReceipt, TranslationQualification,
        },
    };
    let (_dir, store, conn) = fixture();
    for (label, action, qualified) in [
        ("translation", ModelAction::TranslationVerify, true),
        ("ordinary", ModelAction::Verify, false),
    ] {
        let id = uuid::Uuid::new_v4().to_string();
        let selected = profile();
        let source = JobPayload::SubtitleModelOperation {
            translation_profile: qualified.then_some(selected.clone()),
            action,
            model_id: "small".into(),
            catalog_revision: "catalog".into(),
            model_sha256: selected.model_sha256.clone(),
            adapter_contract: selected.adapter_contract.clone(),
            target_node_id: "worker".into(),
        };
        store
            .enqueue_job(EnqueueJob {
                id: id.clone(),
                payload: source.clone(),
                dedupe_key: format!("qualification-{label}"),
                priority: 1,
                not_before_ms: 1000,
                now_ms: 1000,
                request: JobRequest {
                    scope: "qualification".into(),
                    request_id: uuid::Uuid::new_v4().to_string(),
                    request_digest: "e".repeat(64),
                    consumer_kind: "subtitle_model_operation".into(),
                    consumer_ref: label.into(),
                    target_node_id: Some("worker".into()),
                    deadline_ms: None,
                    retain_identity: false,
                },
            })
            .await
            .expect("enqueue");
        let job = claim(&store, &id, JobKind::SubtitleModelOperation, 1100).await;
        let token = job.token.expect("token");
        let receipt = SubtitleModelReceipt {
            node_id: "worker".into(),
            boot_id: token.boot_id.clone(),
            model_id: "small".into(),
            model_sha256: selected.model_sha256.clone(),
            adapter_contract: selected.adapter_contract.clone(),
            engine_sha256: "d".repeat(64),
            model_file_identity: "model-identity".into(),
            engine_file_identity: "engine-identity".into(),
            job_id: id,
            fence: token.fence,
            verified_at_ms: 1200,
        };
        assert!(store
            .publish_subtitle_model(PublishSubtitleModel {
                token,
                source,
                receipt,
                translation: qualified.then_some(TranslationQualification {
                    profile: selected,
                    self_test_sha256: "f".repeat(64)
                }),
                now_ms: 1200
            })
            .await
            .expect("publish"));
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM subtitle_translation_capabilities",
                [],
                |r| r.get::<_, i64>(0)
            )
            .expect("translation receipt"),
            1
        );
        assert_eq!(
            store
                .subtitle_model_receipts("worker")
                .await
                .expect("normal receipt")
                .len(),
            if qualified { 0 } else { 1 }
        );
    }
}

#[tokio::test]
async fn explicit_failed_range_retry_queues_its_exact_revision_and_source_library() {
    use plurx_core::store::background_jobs::{JobSettlement, SettleJob};
    let (_dir, store, conn) = fixture();
    let recipe = prepared(&store).await;
    conn.execute(
        "INSERT INTO background_storage_domains VALUES(1,'/media','translation-fixture-domain')",
        [],
    )
    .expect("physical domain mapping");
    let failed = range(&store, &recipe, "failed-core", 1300).await;
    store
        .settle_job(SettleJob {
            token: failed.token.expect("token"),
            settlement: JobSettlement::Fail {
                error_code: "inference_failed".into(),
            },
            now_ms: 1400,
        })
        .await
        .expect("failure");
    let revision: i64 = conn
        .query_row(
            "SELECT range_revision FROM subtitle_translation_ranges",
            [],
            |r| r.get(0),
        )
        .expect("revision");
    let result = store
        .mutate_subtitle_translation(mutate(
            TranslationAction::RetryRange {
                operation_id: recipe.operation_id.clone(),
                expected_operation_epoch: 1,
                core_index: 0,
                expected_range_revision: revision,
                playback_id: uuid::Uuid::new_v4().to_string(),
                position_ms: 0,
                playback_rate: 1.0,
                paused: false,
            },
            1500,
        ))
        .await
        .expect("retry");
    assert_eq!(result["outcome"], "accepted");
    assert_eq!(result["core_index"], 0);
    assert_eq!(result["range_revision"], (revision + 1).to_string());
    let id = result["job_id"].as_str().expect("atomic durable job");
    let job = claim(&store, id, JobKind::SubtitleTranslationRange, 1600).await;
    assert_eq!(
        conn.query_row("SELECT job_id FROM subtitle_translation_ranges", [], |r| {
            r.get::<_, String>(0)
        })
        .expect("same claim"),
        job.id
    );
    let keys = conn
        .prepare("SELECT resource_key FROM background_job_required_resources WHERE job_id=?1")
        .expect("resources")
        .query_map([id], |r| r.get::<_, String>(0))
        .expect("rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("keys");
    assert!(
        keys.iter()
            .any(|v| v == "source_io:translation-fixture-domain"),
        "actual file's library reservation: {keys:?}"
    );
}

#[tokio::test]
async fn publication_global_byte_cap_is_atomic_and_revision_max_minus_one_settles() {
    use plurx_core::store::subtitle_translation_publication::PublishTranslationSegment;
    let (_dir, store, conn) = fixture();
    let recipe = prepared(&store).await;
    let job = range(&store, &recipe, "global-cap", 1300).await;
    // A competing retained cache already consumes the entire global allowance.
    // Per-asset accounting is individually valid; the writer must check the sum.
    for i in 0..8 {
        conn.execute("INSERT INTO subtitle_translation_assets(id,operation_id,file_id,source_json,source_sha256,profile_json,profile_id,audio_index,spoken_language,duration_ms,core_count,accepted_bytes,created_at_ms,last_read_at_ms) SELECT printf('%064x',?1+10),operation_id,file_id,source_json,source_sha256,profile_json,profile_id,audio_index,spoken_language,duration_ms,core_count,16777216,created_at_ms,last_read_at_ms FROM subtitle_translation_assets WHERE id=?2",rusqlite::params![i,&recipe.asset_id]).expect("concurrent cache accounting");
    }
    let request = PublishTranslationSegment {
        token: job.token.clone().expect("token"),
        source: job.supported_payload().expect("payload"),
        segment: draft(&recipe, job.fence, "bounded cue"),
        now_ms: 1400,
    };
    assert!(!store
        .publish_translation_segment(request.clone())
        .await
        .expect("atomic quota refusal"));
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_segments",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("no partial insert"),
        0
    );
    assert_eq!(
        format!(
            "{:?}",
            store
                .background_job(&job.id)
                .await
                .expect("lookup")
                .expect("job")
                .state
        ),
        "Running"
    );
    conn.execute(
        "DELETE FROM subtitle_translation_assets WHERE id!=?1",
        [&recipe.asset_id],
    )
    .expect("capacity available");
    conn.execute(
        "UPDATE subtitle_translation_operations SET view_revision=9223372036854775806",
        [],
    )
    .expect("boundary");
    assert!(store
        .publish_translation_segment(request)
        .await
        .expect("settled successful transaction at boundary"));
    assert_eq!(
        format!(
            "{:?}",
            store
                .background_job(&job.id)
                .await
                .expect("lookup")
                .expect("job")
                .state
        ),
        "Succeeded"
    );
    assert_eq!(
        conn.query_row(
            "SELECT view_revision FROM subtitle_translation_operations",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("bounded"),
        i64::MAX
    );
}
