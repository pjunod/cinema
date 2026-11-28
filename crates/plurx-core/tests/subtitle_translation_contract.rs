//! The real SQLite bridge, schema migration and common queue execute together.
use plurx_core::store::{
    background_jobs::{BackgroundJobStore, ClaimJob, ClaimOutcome, JobKind},
    subtitle_translation::{SourceRevision, TranslationProfile},
    subtitle_translation_mutation::{TranslationAction, TranslationMutation},
    SqliteStore,
};
fn profile() -> TranslationProfile {
    TranslationProfile {
        task: Default::default(),
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
    conn.execute_batch("INSERT INTO users(id,username,password_hash,is_admin) VALUES(1,'admin','hash',1),(2,'viewer','hash',0); INSERT INTO libraries(id,name,kind,paths) VALUES(1,'movies','movies','[\"/media\"]');INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(1,1,'movie','Movie','Movie'); INSERT INTO files(id,item_id,path,size,mtime,probe_json) VALUES(1,1,'/media/movie',100,20,'{}');INSERT INTO settings(key,value) VALUES('subtitles.translation.config.v1','{\"version\":1,\"enabled\":true}'); INSERT OR IGNORE INTO settings(key,value) VALUES('subtitles.ai.v1','{\"version\":1,\"download_match\":true,\"translate_text\":true,\"from_audio\":true,\"ahead_of_time\":false,\"who\":\"admins\",\"quality\":\"balanced\"}');").expect("catalog fixture");
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
            finish: false,
            continuity_id: None,
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
async fn automatic_language_is_resolved_by_fenced_preparation_before_asset_publication() {
    use plurx_core::store::{
        subtitle_translation_publication::BindTranslationSource,
        subtitle_translation_read::{TranslationQuery, TranslationRead},
    };
    let (_dir, store, _) = fixture();
    let mut request = create(1000);
    let TranslationAction::Create {
        finish,
        spoken_language,
        ..
    } = &mut request.action
    else {
        unreachable!()
    };
    *finish = true;
    *spoken_language = "auto".into();
    let receipt = store
        .mutate_subtitle_translation(request)
        .await
        .expect("auto start");
    let job = claim(
        &store,
        receipt["job_id"].as_str().expect("prepare job"),
        JobKind::SubtitleTranslationPrepare,
        1100,
    )
    .await;
    let proof = BindTranslationSource {
        token: job.token.clone().expect("attempt"),
        source: job.supported_payload().expect("auto prepare"),
        source_sha256: "c".repeat(64),
        local_identity: "held-model-and-source".into(),
        resolved_language: Some("en".into()),
        now_ms: 1200,
    };
    assert!(store
        .bind_translation_source(proof.clone())
        .await
        .expect("fenced source binding"));
    assert!(store
        .bind_translation_source(proof)
        .await
        .expect("exact retry"));
    let rows = store
        .read_subtitle_translation(TranslationRead {
            user_id: 1,
            now_ms: 1300,
            query: TranslationQuery::Operation {
                operation_id: receipt["operation_id"]
                    .as_str()
                    .expect("accepted operation ID")
                    .into(),
            },
        })
        .await
        .expect("read the English operation");
    assert_eq!(rows[0]["spoken_language"], "en");
    assert_eq!(rows[0]["output_language"], "en");
    assert_eq!(rows[0]["task"], "transcribe");
    assert!(rows[0]["asset_id"].is_string());
}

#[tokio::test]
async fn ai_intent_pins_the_route_and_viewer_release_is_scoped_to_its_playback() {
    use plurx_core::store::ai_subtitles::{Intent, IntentAccess, IntentRoute, Viewer};
    use plurx_core::store::subtitle_translation_read::{TranslationQuery, TranslationRead};
    let (_dir, store, _) = fixture();
    let mut request = create(1000);
    let TranslationAction::Create { finish, .. } = &mut request.action else {
        unreachable!()
    };
    *finish = true;
    let recipe = prepared_from(&store, request).await;
    let first = uuid::Uuid::new_v4().to_string();
    let second = uuid::Uuid::new_v4().to_string();
    let request_id = uuid::Uuid::new_v4().to_string();
    let initial = Intent {
        id: uuid::Uuid::new_v4().to_string(),
        digest: "f".repeat(64),
        source: recipe.source.clone(),
        route: IntentRoute::Audio,
        track_index: 0,
        language: None,
        speech_profile_id: None,
        playback_id: Some(first.clone()),
        position_ms: 0,
        issued_at_ms: 20_000,
        producer_id: None,
    };
    let reserved = store
        .ai_subtitle_intent(
            1,
            20_000,
            IntentAccess::Reserve {
                request_id: request_id.clone(),
                intent: initial.clone(),
            },
        )
        .await
        .expect("reserve subtitle intent")
        .expect("reserved intent");
    assert_eq!(reserved.route, IntentRoute::Audio);
    let replayed = store
        .ai_subtitle_intent(
            1,
            20_000,
            IntentAccess::Reserve {
                request_id: request_id.clone(),
                intent: Intent {
                    route: IntentRoute::Text,
                    id: uuid::Uuid::new_v4().to_string(),
                    ..initial.clone()
                },
            },
        )
        .await
        .expect("read the existing subtitle intent")
        .expect("original intent");
    assert_eq!(replayed.id, initial.id);
    assert_eq!(replayed.route, IntentRoute::Audio);
    assert!(store
        .ai_subtitle_intent(2, 20_000, IntentAccess::Operation { id: initial.id })
        .await
        .expect("check another account cannot read the intent")
        .is_none());
    let update = |playback_id: String, time, window| Viewer {
        operation_id: recipe.operation_id.clone(),
        playback_id,
        user_id: 1,
        now_ms: time,
        window,
    };
    for playback in [&first, &second] {
        assert!(store
            .ai_subtitle_viewer(update(playback.clone(), 20_000, Some((0, 1.0))))
            .await
            .expect("join viewer demand"));
    }
    assert!(store
        .ai_subtitle_viewer(update(first, 20_001, None))
        .await
        .expect("release the first viewer"));
    let demands = store
        .read_subtitle_translation(TranslationRead {
            user_id: 0,
            now_ms: 20_002,
            query: TranslationQuery::LiveDemands {
                operation_id: recipe.operation_id,
                operation_epoch: recipe.operation_epoch,
            },
        })
        .await
        .expect("read remaining active viewers");
    assert_eq!(demands.len(), 1);
    assert_eq!(demands[0]["playback_id"], second);
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
    // The queued preparation is observed as a JSON object. A bare scalar
    // subquery would hand the outer object a string, hiding the current job.
    use plurx_core::store::subtitle_translation_read::{TranslationQuery, TranslationRead};
    let observed = store
        .read_subtitle_translation(TranslationRead {
            user_id: 1,
            now_ms: 1050,
            query: TranslationQuery::Operation {
                operation_id: op.clone(),
            },
        })
        .await
        .expect("operation read");
    assert_eq!(observed[0]["active_job"]["job_id"], jobid.as_str());
    assert_eq!(
        observed[0]["active_job"]["kind"],
        "subtitle_translation_prepare"
    );
    // The worker reads only operations a viewer currently wants.
    let worker = |now_ms| TranslationRead {
        user_id: 0,
        now_ms,
        query: TranslationQuery::WorkerOperations {
            node_id: "worker".into(),
            after: None,
        },
    };
    let live = store
        .read_subtitle_translation(worker(1050))
        .await
        .expect("worker read");
    assert_eq!(live.len(), 1);
    let idle = store
        .read_subtitle_translation(worker(601_000))
        .await
        .expect("worker read");
    assert!(
        idle.is_empty(),
        "an expired demand leaves nothing to schedule"
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
                continuity_id: None,
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
async fn lapsed_demand_is_renewed_by_its_next_heartbeat_but_never_after_stop() {
    let (_dir, store, _conn) = fixture();
    let input = create(1000);
    let first = store
        .mutate_subtitle_translation(input.clone())
        .await
        .expect("create");
    let op = first["operation_id"].as_str().expect("root").to_owned();
    let demand = first["demand_id"].as_str().expect("demand").to_owned();
    let playback = match &input.action {
        TranslationAction::Create { playback_id, .. } => playback_id.clone(),
        _ => unreachable!(),
    };
    let beat = |generation: i64, now: i64| {
        mutate(
            TranslationAction::Demand {
                operation_id: op.clone(),
                expected_operation_epoch: 1,
                demand_id: demand.clone(),
                expected_generation: generation,
                playback_id: playback.clone(),
                position_ms: 1000,
                playback_rate: 1.0,
                paused: false,
                continuity_id: None,
            },
            now,
        )
    };
    // The 15 s demand lapsed while the viewer's tab was throttled.
    let renewed = store
        .mutate_subtitle_translation(beat(1, 40_000))
        .await
        .expect("renew");
    assert_eq!(renewed["outcome"], "accepted");
    assert_eq!(renewed["demand_generation"], "2");
    let stopped = store
        .mutate_subtitle_translation(mutate(
            TranslationAction::Stop {
                operation_id: op.clone(),
                expected_operation_epoch: 1,
            },
            41_000,
        ))
        .await
        .expect("stop");
    assert_eq!(stopped["state"], "stopped");
    let late = store
        .mutate_subtitle_translation(beat(2, 42_000))
        .await
        .expect("late heartbeat");
    assert_ne!(late["outcome"], "accepted", "heartbeats never undo Stop");
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
            continuity_id: None,
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
    claim_on_boot(store, id, kind, now, "00000000-0000-4000-8000-000000000002").await
}
async fn claim_on_boot(
    store: &SqliteStore,
    id: &str,
    kind: JobKind,
    now: i64,
    boot_id: &str,
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
            boot_id: boot_id.into(),
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
            resolved_language: None,
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
            resolved_language: None,
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
                continuity_id: None,
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
            resolved_language: None,
            local_identity: "fresh-held-A".into(),
            now_ms: 1700
        })
        .await
        .expect("explicit verified revival"));
    assert_eq!(conn.query_row("SELECT count(*) FROM subtitle_translation_assets WHERE id=?1 AND retired_at_ms IS NULL",[asset],|r|r.get::<_,i64>(0)).expect("revived"),1);
}

async fn prepared(store: &SqliteStore) -> plurx_core::store::subtitle_translation::RangeRecipe {
    prepared_from(store, create(1000)).await
}

#[tokio::test]
async fn vad_asset_identity_separates_producers_and_survives_retries() {
    let (_dir, store, _) = fixture();
    let mut recipe = prepared(&store).await;
    let old = recipe.expected_asset_id().expect("legacy asset identity");
    recipe.operation_id = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        recipe
            .expected_asset_id()
            .expect("same legacy asset identity"),
        old,
        "published legacy asset IDs stay readable"
    );
    for pipeline in ["translation-window-v2", "translation-window-v2-fast"] {
        recipe.profile.pipeline_revision = pipeline.into();
        let first = recipe
            .expected_asset_id()
            .expect("first VAD asset identity");
        recipe.operation_epoch += 1;
        assert_eq!(
            recipe
                .expected_asset_id()
                .expect("VAD identity after retry"),
            first
        );
        recipe.operation_id = uuid::Uuid::new_v4().to_string();
        assert_ne!(
            recipe
                .expected_asset_id()
                .expect("VAD identity for another producer"),
            first
        );
    }
}

#[tokio::test]
async fn finish_demand_publishes_each_core_after_the_viewer_leaves() {
    use plurx_core::store::{
        subtitle_translation_publication::{BindTranslationSource, PublishTranslationSegment},
        subtitle_translation_read::{TranslationQuery, TranslationRead},
        MediaStore,
    };

    let (_dir, store, connection) = fixture();
    let mut request = create(1000);
    let TranslationAction::Create { finish, .. } = &mut request.action else {
        unreachable!()
    };
    *finish = true;
    let mut recipe = prepared_from(&store, request).await;
    let first_background_pass = 20_000;
    let query = |now_ms, query| TranslationRead {
        user_id: 0,
        now_ms,
        query,
    };
    assert!(store
        .read_subtitle_translation(query(
            first_background_pass,
            TranslationQuery::LiveDemands {
                operation_id: recipe.operation_id.clone(),
                operation_epoch: recipe.operation_epoch,
            }
        ))
        .await
        .expect("viewer demands")
        .is_empty());
    assert_eq!(
        store
            .ai_subtitle_active_work(1, None, first_background_pass)
            .await
            .expect("count background work before completion"),
        1
    );
    let finish_core = TranslationQuery::FinishCore {
        operation_id: recipe.operation_id.clone(),
        operation_epoch: recipe.operation_epoch,
    };
    for core_index in 0..recipe.core_count() {
        let now = first_background_pass + core_index * 10;
        let next = store
            .read_subtitle_translation(query(now, finish_core.clone()))
            .await
            .expect("next finish core");
        assert_eq!(next[0]["core_index"], core_index);
        recipe.core_index = core_index;
        let job = range_for_consumer(
            &store,
            &recipe,
            &format!("finish-{core_index}"),
            now,
            "subtitle_translation_range",
        )
        .await;
        assert!(range_interested(&store, &job.id, now + 2).await);
        let captured: i64 = connection
            .query_row(
                "SELECT count(*) FROM subtitle_translation_range_continuities WHERE job_id=?1",
                [&job.id],
                |row| row.get(0),
            )
            .expect("finish creates no viewer continuation");
        assert_eq!(captured, 0);
        assert!(store
            .publish_translation_segment(PublishTranslationSegment {
                token: job.token.clone().expect("token"),
                source: job.supported_payload().expect("range"),
                segment: draft(&recipe, job.fence, "Finished <caption>"),
                now_ms: now + 3,
            })
            .await
            .expect("durable finish publication"));
    }
    let operation = store
        .read_subtitle_translation(query(
            21_000,
            TranslationQuery::Operation {
                operation_id: recipe.operation_id.clone(),
            },
        ))
        .await
        .expect("completed operation");
    assert_eq!(operation[0]["state"], "complete");
    assert_eq!(
        store
            .ai_subtitle_active_work(1, None, 21_000)
            .await
            .expect("count background work after completion"),
        0
    );
    let file = store
        .get_file(1)
        .await
        .expect("read the completed file")
        .expect("current file");
    let caption = file.downloaded_subtitle(0).expect("completed stored track");
    assert!(caption.vtt.contains("Finished &lt;caption&gt;"));
    assert!(plurx_core::store::valid_downloaded_vtt(&caption.vtt));
    assert!(caption
        .transcription
        .as_ref()
        .expect("completed caption provenance")
        .progressive_asset_id
        .is_none());

    // Deleting the stored track neither discards the cached cues nor lets
    // maintenance immediately recreate the track. An explicit retry can
    // rebind the same asset and restore the track from its accepted cores.
    let track_id = caption.stable_id();
    store
        .delete_ai_subtitle_track(recipe.source.clone(), &track_id)
        .await
        .expect("delete AI track");
    assert!(store
        .ai_subtitle_excluded_from_schedule(recipe.source.clone())
        .await
        .expect("check automatic scheduling"));
    assert!(store
        .get_file(1)
        .await
        .expect("read the file after deletion")
        .expect("file survives caption deletion")
        .downloaded_subtitle(0)
        .is_none());
    let stopped = store
        .read_subtitle_translation(query(
            22_000,
            TranslationQuery::Operation {
                operation_id: recipe.operation_id.clone(),
            },
        ))
        .await
        .expect("deleted operation");
    assert_eq!(stopped[0]["state"], "stopped");
    assert_eq!(stopped[0]["reason"], "track_deleted");
    let remaining: i64 = connection
        .query_row(
            "SELECT count(*) FROM subtitle_translation_segments WHERE asset_id=?1",
            [&recipe.asset_id],
            |row| row.get(0),
        )
        .expect("retained segments");
    assert_eq!(remaining, recipe.core_count());

    let retry = store
        .mutate_subtitle_translation(mutate(
            TranslationAction::RetryOperation {
                operation_id: recipe.operation_id.clone(),
                expected_operation_epoch: recipe.operation_epoch,
                expected_view_revision: stopped[0]["view_revision"]
                    .as_str()
                    .expect("stopped operation revision")
                    .parse()
                    .expect("numeric stopped operation revision"),
                expected_state: "stopped".into(),
                playback_id: String::new(),
                position_ms: 0,
                playback_rate: 1.0,
                paused: false,
                continuity_id: None,
            },
            22_000,
        ))
        .await
        .expect("explicit retry");
    assert_eq!(retry["outcome"], "accepted");
    let preparation = claim(
        &store,
        retry["job_id"].as_str().expect("replacement prep"),
        JobKind::SubtitleTranslationPrepare,
        22_100,
    )
    .await;
    assert!(store
        .bind_translation_source(BindTranslationSource {
            source: preparation.supported_payload().expect("closed producer"),
            token: preparation.token.expect("fenced producer"),
            source_sha256: recipe.source_sha256.clone(),
            resolved_language: None,
            local_identity: "owned-source".into(),
            now_ms: 22_200,
        })
        .await
        .expect("cached result republishes atomically"));
    let refreshed = store
        .get_file(1)
        .await
        .expect("read regenerated file")
        .expect("current file after retry");
    let restored = refreshed.downloaded_subtitle(0).expect("restored captions");
    assert_eq!(restored.stable_id(), track_id);
    assert_eq!(restored.vtt, caption.vtt);
    store
        .maintain_translation_cache(100_000_000)
        .await
        .expect("retire unused inline backing");
    let metadata = store
        .read_subtitle_translation(query(
            100_000_001,
            TranslationQuery::Operation {
                operation_id: recipe.operation_id,
            },
        ))
        .await
        .expect("stored track survives cache retirement");
    assert_eq!(metadata[0]["state"], "complete");
    assert_eq!(metadata[0]["asset_id"], track_id);
}

#[tokio::test]
async fn four_hour_caption_over_the_inline_cap_retains_a_stored_track() {
    use plurx_core::store::{
        subtitle_translation_publication::PublishTranslationSegment,
        subtitle_translation_read::{TranslationQuery, TranslationRead},
        MediaStore, MAX_DOWNLOADED_SUBTITLE_BYTES,
    };

    let (dir, store, mut conn) = fixture();
    let mut request = create(1000);
    let TranslationAction::Create {
        finish,
        duration_ms,
        ..
    } = &mut request.action
    else {
        unreachable!()
    };
    *finish = true;
    *duration_ms = 4 * 60 * 60 * 1000;
    let mut recipe = prepared_from(&store, request).await;
    let text = "Spoken words. ".repeat(50);
    let preceding_cores = recipe.core_count() - 1;
    let tx = conn.transaction().expect("earlier published cores");
    let mut accepted_bytes = 0;
    for core_index in 0..preceding_cores {
        recipe.core_index = core_index;
        let segment = draft(&recipe, 1, &text);
        let body = String::from_utf8(segment.canonical_body().expect("canonical segment body"))
            .expect("decode canonical segment JSON");
        accepted_bytes += i64::try_from(body.len()).expect("segment byte count fits storage");
        tx.execute(
            "INSERT INTO subtitle_translation_segments
             (asset_id,core_index,segment_id,publication_id,digest,body,start_ms,end_ms,accepted_revision,created_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,3000)",
            rusqlite::params![recipe.asset_id,core_index,segment.segment_id(),uuid::Uuid::new_v4().to_string(),
                "d".repeat(64),body,segment.start_ms,segment.end_ms,core_index+1],
        ).expect("accepted core");
    }
    tx.execute(
        "UPDATE subtitle_translation_assets SET manifest_revision=?1,accepted_bytes=?2 WHERE id=?3",
        rusqlite::params![preceding_cores, accepted_bytes, recipe.asset_id],
    )
    .expect("accepted manifest");
    tx.commit().expect("fixture committed");

    recipe.core_index = preceding_cores;
    let job = range(&store, &recipe, "long-finish", 20_000).await;
    assert!(store
        .publish_translation_segment(PublishTranslationSegment {
            token: job.token.clone().expect("token"),
            source: job.supported_payload().expect("range"),
            segment: draft(&recipe, job.fence, &text),
            now_ms: 20_003,
        })
        .await
        .expect("atomic completion"));
    let inline: String = conn
        .query_row(
            "SELECT downloaded_subtitles FROM files WHERE id=1",
            [],
            |r| r.get(0),
        )
        .expect("read inline subtitle storage");
    assert_eq!(inline, "[]");

    let reopened = SqliteStore::open(&dir.path().join("store.db")).expect("reopened catalog");
    reopened
        .maintain_translation_cache(90_000_000)
        .await
        .expect("idle cache sweep");
    let file = reopened
        .get_file(1)
        .await
        .expect("read the file after reopening")
        .expect("file");
    let caption = file
        .downloaded_subtitle(0)
        .expect("asset-backed stored track");
    assert_eq!(caption.stable_id(), recipe.asset_id);
    assert_eq!(
        caption
            .transcription
            .as_ref()
            .expect("stored progressive caption provenance")
            .progressive_asset_id
            .as_deref(),
        Some(recipe.asset_id.as_str())
    );
    let rows = reopened
        .read_subtitle_translation(TranslationRead {
            user_id: 0,
            now_ms: 90_001_000,
            query: TranslationQuery::CompletedVtt {
                asset_id: recipe.asset_id,
                source: recipe.source,
            },
        })
        .await
        .expect("stored track VTT");
    let vtt = rows[0].as_str().expect("WebVTT");
    assert!(vtt.len() > MAX_DOWNLOADED_SUBTITLE_BYTES);
    assert!(vtt.contains("03:59:30.000 -->"));
}

#[tokio::test]
async fn scheduled_finish_needs_no_viewer_and_respects_the_saved_choice() {
    let (_dir, store, conn) = fixture();
    let mut request = create(1000);
    request.user_id = 0;
    let TranslationAction::Create {
        finish,
        playback_id,
        ..
    } = &mut request.action
    else {
        unreachable!()
    };
    *finish = true;
    playback_id.clear();
    assert_eq!(
        store
            .mutate_subtitle_translation(request.clone())
            .await
            .expect("request disabled scheduled work")["outcome"],
        "disabled"
    );
    conn.execute(
        "UPDATE settings SET value=json_set(value,'$.ahead_of_time',json('true')) WHERE key='subtitles.ai.v1'",
        [],
    ).expect("enable ahead-of-time subtitles");
    request.request_id = uuid::Uuid::new_v4().to_string();
    let result = store
        .mutate_subtitle_translation(request)
        .await
        .expect("scheduled start");
    assert_eq!(result["outcome"], "accepted");
    assert!(result["demand_id"].is_null());
    let id = result["job_id"].as_str().expect("preparation job");
    let job = store
        .background_job(id)
        .await
        .expect("read scheduled preparation job")
        .expect("scheduled preparation job exists");
    assert_eq!(job.priority, 0);
    assert_eq!(
        claim(&store, id, JobKind::SubtitleTranslationPrepare, 1100)
            .await
            .priority,
        0
    );
}
async fn prepared_from(
    store: &SqliteStore,
    initial: TranslationMutation,
) -> plurx_core::store::subtitle_translation::RangeRecipe {
    use plurx_core::store::{
        background_jobs::JobPayload, subtitle_translation::RangeRecipe,
        subtitle_translation_publication::BindTranslationSource,
    };
    let first = store
        .mutate_subtitle_translation(initial)
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
            resolved_language: None,
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
    range_for_consumer(store, recipe, label, now, "subtitle_translation").await
}
async fn range_for_consumer(
    store: &SqliteStore,
    recipe: &plurx_core::store::subtitle_translation::RangeRecipe,
    label: &str,
    now: i64,
    consumer_kind: &str,
) -> plurx_core::store::background_jobs::BackgroundJob {
    use plurx_core::store::background_jobs::{EnqueueJob, EnqueueOutcome, JobPayload, JobRequest};
    let id = uuid::Uuid::new_v4().to_string();
    let operations = store.read_subtitle_translation(plurx_core::store::subtitle_translation_read::TranslationRead {
        user_id:0,now_ms:now,query:plurx_core::store::subtitle_translation_read::TranslationQuery::WorkerOperations {node_id:"worker".into(),after:None}
    }).await.expect("current producer operations");
    let producer = operations
        .iter()
        .find(|op| op["operation_id"] == recipe.operation_id)
        .and_then(|op| op["preparation_job_id"].as_str())
        .expect("current producer preparation")
        .to_owned();
    let payload = JobPayload::SubtitleTranslationRange {
        file_id: 1,
        recipe: recipe.clone(),
        target_node_id: "worker".into(),
        producer_preparation_job_id: producer,
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
                consumer_kind: consumer_kind.into(),
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
    // A second viewer joins the incumbent producer; boot reconciliation must
    // explicitly replace its incarnation before making a fresh source proof.
    use plurx_core::store::{
        subtitle_translation_publication::BindTranslationSource,
        subtitle_translation_read::{TranslationQuery, TranslationRead},
    };
    let (_dir2, store2, _conn2) = fixture();
    let recipe = prepared(&store2).await;
    let observation = |now_ms| TranslationRead {
        user_id: 1,
        now_ms,
        query: TranslationQuery::Operation {
            operation_id: recipe.operation_id.clone(),
        },
    };
    let incumbent = store2
        .read_subtitle_translation(observation(1400))
        .await
        .expect("incumbent")
        .remove(0);
    let mut fresh = create(1500);
    if let TranslationAction::Create { playback_id, .. } = &mut fresh.action {
        *playback_id = uuid::Uuid::new_v4().to_string();
    }
    let receipt = store2
        .mutate_subtitle_translation(fresh)
        .await
        .expect("fresh Translate");
    assert_eq!(receipt["operation_id"], recipe.operation_id);
    assert_eq!(receipt["state"], "partial");
    let joined = store2
        .read_subtitle_translation(observation(1550))
        .await
        .expect("joined producer")
        .remove(0);
    assert_eq!(
        joined["preparation_job_id"], incumbent["preparation_job_id"],
        "joining must not fence active ranges"
    );
    let replacement = store2
        .mutate_subtitle_translation(mutate(
            TranslationAction::Retarget {
                operation_id: recipe.operation_id.clone(),
                expected_operation_epoch: 1,
                expected_view_revision: joined["view_revision"]
                    .as_str()
                    .expect("view")
                    .parse()
                    .expect("number"),
                expected_preparation_job_id: joined["preparation_job_id"]
                    .as_str()
                    .expect("producer")
                    .into(),
                expected_target_node_id: "worker".into(),
                target_node_id: "worker".into(),
            },
            1600,
        ))
        .await
        .expect("boot reconciliation");
    assert_eq!(replacement["outcome"], "accepted");
    assert_eq!(replacement["operation_epoch"], "1");
    assert_ne!(replacement["job_id"], joined["preparation_job_id"]);
    let job = store2
        .background_job(replacement["job_id"].as_str().expect("boot prep"))
        .await
        .expect("lookup")
        .expect("job");
    assert_eq!(
        job.supported_payload().expect("payload").kind(),
        JobKind::SubtitleTranslationPrepare
    );
    assert_eq!(format!("{:?}", job.state), "Queued");
    let new_boot = uuid::Uuid::new_v4().to_string();
    let old_binding = store2
        .read_subtitle_translation(TranslationRead {
            user_id: 0,
            now_ms: 1650,
            query: TranslationQuery::SourceBinding {
                asset_id: recipe.asset_id.clone(),
                node_id: "worker".into(),
            },
        })
        .await
        .expect("unready old binding")
        .remove(0);
    assert_ne!(old_binding["boot_id"], new_boot);
    assert_eq!(
        old_binding["preparation_ready"], false,
        "new incarnation cannot borrow the old boot's source verification"
    );
    let owned = claim_on_boot(
        &store2,
        &job.id,
        JobKind::SubtitleTranslationPrepare,
        1700,
        &new_boot,
    )
    .await;
    assert!(store2
        .bind_translation_source(BindTranslationSource {
            token: owned.token.clone().expect("new boot claim"),
            source: owned.supported_payload().expect("owned payload"),
            source_sha256: recipe.source_sha256.clone(),
            resolved_language: None,
            local_identity: "new-boot-held-source".into(),
            now_ms: 1800,
        })
        .await
        .expect("fresh owned verification"));
    let binding = store2
        .read_subtitle_translation(TranslationRead {
            user_id: 0,
            now_ms: 1850,
            query: TranslationQuery::SourceBinding {
                asset_id: recipe.asset_id.clone(),
                node_id: "worker".into(),
            },
        })
        .await
        .expect("new binding")
        .remove(0);
    assert_eq!(binding["boot_id"], new_boot);
    assert_eq!(binding["preparation_job_id"], job.id);
    assert_eq!(binding["preparation_ready"], true);
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
async fn one_model_receipt_qualifies_and_legacy_translation_actions_publish_nothing() {
    use plurx_core::store::{
        background_jobs::{EnqueueJob, JobPayload, JobRequest},
        background_jobs_transcription::{ModelAction, PublishSubtitleModel, SubtitleModelReceipt},
    };
    for (label, action, legacy) in [
        ("translation", ModelAction::TranslationVerify, true),
        ("ordinary", ModelAction::Verify, false),
    ] {
        // A rejected publication leaves the claim active. Give each action its
        // own worker slot so both cases exercise publication.
        let (_dir, store, conn) = fixture();
        let id = uuid::Uuid::new_v4().to_string();
        let selected = profile();
        let source = JobPayload::SubtitleModelOperation {
            translation_profile: legacy.then_some(selected.clone()),
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
        let published = store
            .publish_subtitle_model(PublishSubtitleModel {
                token,
                source,
                receipt,
                now_ms: 1200,
            })
            .await;
        if legacy {
            assert!(
                published.is_err(),
                "a legacy translation action publishes nothing"
            );
        } else {
            assert!(published.expect("publish"));
        }
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM subtitle_translation_capabilities",
                [],
                |r| r.get::<_, i64>(0)
            )
            .expect("capability rows"),
            0,
            "no per-profile capability rows are written any more"
        );
        assert_eq!(
            store
                .subtitle_model_receipts("worker")
                .await
                .expect("model receipts")
                .len(),
            if legacy { 0 } else { 1 }
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
                continuity_id: None,
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

#[tokio::test]
async fn shared_grant_reads_reuse_fixed_lease_and_never_create_compute_authority() {
    use plurx_core::store::{
        shared_caption_read::{GrantCaptionPrincipal, GrantCaptionQuery as Q, GrantCaptionRead},
        subtitle_translation_publication::PublishTranslationSegment,
    };
    let (_dir, store, conn) = fixture();
    let recipe = prepared(&store).await;
    let job = range(&store, &recipe, "shared-read", 1300).await;
    assert!(store
        .publish_translation_segment(PublishTranslationSegment {
            token: job.token.clone().expect("token"),
            source: job.supported_payload().expect("source"),
            segment: draft(&recipe, job.fence, "Published caption"),
            now_ms: 1400
        })
        .await
        .expect("publish"));
    let principal = GrantCaptionPrincipal {
        grant_id: uuid::Uuid::new_v4().to_string(),
        binding_sha256: "f".repeat(64),
        file_id: 1,
    };
    let counts = || {
        conn.query_row("SELECT (SELECT count(*) FROM users),(SELECT count(*) FROM background_jobs),(SELECT count(*) FROM subtitle_translation_demands), (SELECT sum(generation) FROM subtitle_translation_demands)",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?))).expect("authority counts")
    };
    let before = counts();
    let read = |principal: GrantCaptionPrincipal, query, now_ms| GrantCaptionRead {
        principal,
        query,
        now_ms,
    };
    let lease = store
        .read_shared_captions(read(
            principal.clone(),
            Q::AcquireLease {
                asset_id: recipe.asset_id.clone(),
            },
            1700,
        ))
        .await
        .expect("lease")
        .remove(0);
    let id = lease["lease_id"].as_str().expect("ID").to_owned();
    let other_asset = "a".repeat(64);
    for query in [
        Q::Lease {
            asset_id: other_asset.clone(),
            lease_id: id.clone(),
        },
        Q::Window {
            asset_id: other_asset,
            lease_id: id.clone(),
            position_ms: 0,
        },
    ] {
        assert!(store
            .read_shared_captions(read(principal.clone(), query, 1800))
            .await
            .expect("asset-scoped shared read")
            .is_empty());
    }
    for seek in 0..300_i64 {
        let reused = store
            .read_shared_captions(read(
                principal.clone(),
                Q::AcquireLease {
                    asset_id: recipe.asset_id.clone(),
                },
                2000 + seek,
            ))
            .await
            .expect("rapid shared seek lease")
            .remove(0);
        assert_eq!(
            reused, lease,
            "seeks do not allocate grant leases for a stable revision"
        );
        store
            .read_shared_captions(read(
                principal.clone(),
                Q::Window {
                    asset_id: recipe.asset_id.clone(),
                    lease_id: id.clone(),
                    position_ms: (seek % 3) * 15000,
                },
                2000 + seek,
            ))
            .await
            .expect("rapid shared moving window");
    }
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM shared_caption_read_leases",
            [],
            |row| row.get::<_, i64>(0)
        )
        .expect("bounded grant lease count"),
        1
    );
    let repeated = store
        .read_shared_captions(read(
            principal.clone(),
            Q::AcquireLease {
                asset_id: recipe.asset_id.clone(),
            },
            4700,
        ))
        .await
        .expect("repeat")
        .remove(0);
    assert_eq!(
        lease, repeated,
        "polling reuses fixed expiry, not a sliding TTL"
    );
    let window = store
        .read_shared_captions(read(
            principal.clone(),
            Q::Window {
                asset_id: recipe.asset_id.clone(),
                lease_id: id.clone(),
                position_ms: 0,
            },
            4800,
        ))
        .await
        .expect("window");
    assert_eq!(window.len(), 1);
    let segment = window[0]["segment_id"]
        .as_str()
        .expect("segment")
        .to_owned();
    assert_eq!(
        store
            .read_shared_captions(read(
                principal.clone(),
                Q::Segment {
                    asset_id: recipe.asset_id.clone(),
                    lease_id: id.clone(),
                    segment_id: segment
                },
                4900
            ))
            .await
            .expect("bytes")
            .len(),
        1
    );
    conn.execute("UPDATE files SET size=101 WHERE id=1", [])
        .expect("source replacement");
    assert!(store
        .read_shared_captions(read(
            principal.clone(),
            Q::Lease {
                asset_id: recipe.asset_id.clone(),
                lease_id: id.clone()
            },
            4950
        ))
        .await
        .expect("changed source cannot retain read authority")
        .is_empty());
    conn.execute("UPDATE files SET size=100 WHERE id=1", [])
        .expect("restore fixture");
    let mut wrong = principal.clone();
    wrong.grant_id = uuid::Uuid::new_v4().to_string();
    let mut wrong_binding = principal.clone();
    wrong_binding.binding_sha256 = "e".repeat(64);
    let mut wrong_file = principal.clone();
    wrong_file.file_id = 2;
    for other in [wrong, wrong_binding, wrong_file] {
        assert!(store
            .read_shared_captions(read(
                other,
                Q::Lease {
                    asset_id: recipe.asset_id.clone(),
                    lease_id: id.clone()
                },
                5000
            ))
            .await
            .expect("refuse mismatched authority")
            .is_empty());
    }
    assert!(store
        .read_shared_captions(read(
            principal.clone(),
            Q::Lease {
                asset_id: recipe.asset_id.clone(),
                lease_id: id
            },
            31700
        ))
        .await
        .expect("expiry")
        .is_empty());
    assert_eq!(
        counts(),
        before,
        "read grants cannot mint users/jobs/demands or heartbeat computation"
    );
    let held = store
        .read_shared_captions(read(
            principal,
            Q::AcquireLease {
                asset_id: recipe.asset_id.clone(),
            },
            100000000,
        ))
        .await
        .expect("new read lease");
    assert_eq!(held.len(), 1);
    conn.execute(
        "UPDATE subtitle_translation_snapshots SET created_at_ms=0",
        [],
    )
    .expect("age snapshot");
    assert!(store
        .acquire_translation_lease(
            plurx_core::store::subtitle_translation_read::AcquireTranslationLease {
                user_id: 1,
                asset_id: "0".repeat(64),
                now_ms: 100000001
            }
        )
        .await
        .expect("account lease cleanup")
        .is_none());
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_snapshots",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("grant protected snapshot"),
        1
    );

    conn.execute("UPDATE subtitle_translation_demands SET released=1", [])
        .expect("release computation");
    conn.execute(
        "UPDATE subtitle_translation_assets SET last_read_at_ms=0,created_at_ms=0",
        [],
    )
    .expect("age cache");
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
    store
        .maintain_translation_cache(100040000)
        .await
        .expect("collect expired");
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_assets WHERE retired_at_ms IS NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("retired"),
        0
    );
}

#[tokio::test]
async fn final_readable_core_completes_without_gaps_or_loser_downgrade_and_cached_retry_completes()
{
    use plurx_core::store::{
        subtitle_translation_publication::{BindTranslationSource, PublishTranslationSegment},
        subtitle_translation_read::{AcquireTranslationLease, TranslationQuery, TranslationRead},
    };
    let (_dir, store, conn) = fixture();
    let recipe = prepared(&store).await;
    let observation = |now| TranslationRead {
        user_id: 1,
        now_ms: now,
        query: TranslationQuery::Operation {
            operation_id: recipe.operation_id.clone(),
        },
    };
    for (index, now) in [(0, 1300), (2, 1500)] {
        let mut core = recipe.clone();
        core.core_index = index;
        let job = range(&store, &core, &format!("gap-{index}"), now).await;
        assert!(store
            .publish_translation_segment(PublishTranslationSegment {
                token: job.token.clone().expect("token"),
                source: job.supported_payload().expect("payload"),
                segment: draft(&core, job.fence, "Readable owned cue"),
                now_ms: now + 10
            })
            .await
            .expect("core publication"));
        assert_eq!(
            store
                .read_subtitle_translation(observation(now + 20))
                .await
                .expect("partial")[0]["state"],
            "partial",
            "count or tail alone cannot conceal a missing core"
        );
    }
    let old = store
        .acquire_translation_lease(AcquireTranslationLease {
            user_id: 1,
            asset_id: recipe.asset_id.clone(),
            now_ms: 1600,
        })
        .await
        .expect("partial snapshot")
        .expect("lease");
    let mut last = recipe.clone();
    last.core_index = 1;
    // The common queue reserves one source lane for a viewer. Give this
    // fixture an explicit foreground interest so both attempts acquire real,
    // counted reservations; two background readers must remain contended.
    let winner = range_for_consumer(&store, &last, "last-winner", 1700, "playback_fragment").await;
    let loser = range(&store, &last, "last-loser", 1750).await;
    assert_eq!(conn.query_row(
        "SELECT count(*) FROM background_job_reservations WHERE resource_key='source_io' AND expires_at_ms>1751",
        [], |r| r.get::<_, i64>(0)
    ).expect("two counted live source reservations"), 2);
    let publication = |job: &plurx_core::store::background_jobs::BackgroundJob,
                       text: &str,
                       now_ms| PublishTranslationSegment {
        token: job.token.clone().expect("owned claim"),
        source: job.supported_payload().expect("typed payload"),
        segment: draft(&last, job.fence, text),
        now_ms,
    };
    let winning = publication(&winner, "Immutable winner", 1800);
    assert!(store
        .publish_translation_segment(winning.clone())
        .await
        .expect("last core"));
    assert_eq!(
        store
            .read_subtitle_translation(observation(1810))
            .await
            .expect("completed")[0]["state"],
        "complete"
    );
    assert!(store
        .publish_translation_segment(publication(&loser, "Different loser bytes", 1820))
        .await
        .expect("loser cleanup"));
    assert_eq!(
        store
            .background_job(&loser.id)
            .await
            .expect("job")
            .expect("loser")
            .state,
        plurx_core::store::background_jobs::JobState::Succeeded
    );
    assert_eq!(
        store
            .read_subtitle_translation(observation(1830))
            .await
            .expect("complete after loser")[0]["state"],
        "complete"
    );
    assert_eq!(
        conn.query_row(
            "SELECT manifest_revision FROM subtitle_translation_assets",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("immutable revision"),
        3
    );
    let current = store
        .acquire_translation_lease(AcquireTranslationLease {
            user_id: 1,
            asset_id: recipe.asset_id.clone(),
            now_ms: 1900,
        })
        .await
        .expect("complete snapshot")
        .expect("lease");
    assert_ne!(
        current["lease_id"], old["lease_id"],
        "a new revision receives a separate bounded snapshot"
    );
    for seek in 0..300_i64 {
        let lease = store
            .acquire_translation_lease(AcquireTranslationLease {
                user_id: 1,
                asset_id: recipe.asset_id.clone(),
                now_ms: 2000 + seek,
            })
            .await
            .expect("rapid seek lease")
            .expect("lease");
        for field in ["lease_id", "manifest_revision", "expires_at_ms"] {
            assert_eq!(
                lease[field], current[field],
                "same revision reuses one fixed-expiry lease"
            );
        }
        let core = seek % 3;
        let rows = store
            .read_subtitle_translation(TranslationRead {
                user_id: 1,
                now_ms: 2000 + seek,
                query: TranslationQuery::Segments {
                    asset_id: recipe.asset_id.clone(),
                    lease_id: current["lease_id"].as_str().expect("lease ID").into(),
                    after_core: core - 1,
                    through_core: core,
                },
            })
            .await
            .expect("moving window");
        assert_eq!(rows.len(), 1);
    }
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_read_leases",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("bounded leases"),
        2
    );
    let old_rows = store
        .read_subtitle_translation(TranslationRead {
            user_id: 1,
            now_ms: 2400,
            query: TranslationQuery::Segments {
                asset_id: recipe.asset_id.clone(),
                lease_id: old["lease_id"].as_str().expect("old lease").into(),
                after_core: -1,
                through_core: 2,
            },
        })
        .await
        .expect("old snapshot remains readable");
    assert_eq!(
        old_rows.len(),
        2,
        "completion must not widen an old leased snapshot"
    );
    let stop = store
        .mutate_subtitle_translation(mutate(
            TranslationAction::Stop {
                operation_id: recipe.operation_id.clone(),
                expected_operation_epoch: 1,
            },
            2500,
        ))
        .await
        .expect("Stop completed producer");
    assert_eq!(stop["state"], "stopped");
    assert!(store
        .publish_translation_segment(winning)
        .await
        .expect("exact receipt replay"));
    assert_eq!(
        store
            .read_subtitle_translation(observation(2550))
            .await
            .expect("Stop retained")[0]["state"],
        "stopped"
    );
    let retried = store
        .mutate_subtitle_translation(mutate(
            TranslationAction::RetryOperation {
                operation_id: recipe.operation_id.clone(),
                expected_operation_epoch: stop["operation_epoch"]
                    .as_str()
                    .expect("epoch")
                    .parse()
                    .expect("counter"),
                expected_view_revision: stop["view_revision"]
                    .as_str()
                    .expect("view")
                    .parse()
                    .expect("counter"),
                expected_state: "stopped".into(),
                playback_id: uuid::Uuid::new_v4().to_string(),
                position_ms: 0,
                playback_rate: 1.0,
                paused: false,
                continuity_id: None,
            },
            2600,
        ))
        .await
        .expect("explicit retry");
    let prep = claim(
        &store,
        retried["job_id"].as_str().expect("fresh preparation"),
        JobKind::SubtitleTranslationPrepare,
        2700,
    )
    .await;
    assert!(store
        .bind_translation_source(BindTranslationSource {
            token: prep.token.clone().expect("new claim"),
            source: prep.supported_payload().expect("new preparation"),
            source_sha256: recipe.source_sha256.clone(),
            resolved_language: None,
            local_identity: "fresh owned source attestation".into(),
            now_ms: 2800
        })
        .await
        .expect("all-cached binding"));
    assert_eq!(
        store
            .read_subtitle_translation(observation(2850))
            .await
            .expect("cached completion")[0]["state"],
        "complete",
        "all readable cores complete at binding without further range publication"
    );
    assert_eq!(
        conn.query_row(
            "SELECT manifest_revision FROM subtitle_translation_assets",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("same immutable revision"),
        3
    );
}

#[tokio::test]
async fn legacy_range_incarnation_decodes_without_poll_poison_but_cannot_claim_or_publish() {
    use plurx_core::store::background_jobs::{
        CandidateQuery, JobPayload, JobSettlement, SettleJob,
    };
    use plurx_core::store::subtitle_translation_publication::PublishTranslationSegment;
    let (_dir, store, conn) = fixture();
    let recipe = prepared(&store).await;
    let job = range(&store, &recipe, "pre-upgrade", 1300).await;
    assert!(store
        .settle_job(SettleJob {
            token: job.token.clone().expect("claim"),
            settlement: JobSettlement::Yield {
                error_code: None,
                checkpoint: None,
                not_before_ms: 1400
            },
            now_ms: 1350
        })
        .await
        .expect("yield owned job"));
    let mut legacy = job.payload.clone();
    legacy
        .as_object_mut()
        .expect("payload")
        .remove("producer_preparation_job_id");
    let parsed: JobPayload =
        serde_json::from_value(legacy.clone()).expect("old durable payload remains typed");
    parsed.validate().expect("legacy shape remains inspectable");
    assert_eq!(
        serde_json::to_value(&parsed).expect("legacy canonical encoding"),
        legacy
    );
    conn.execute(
        "UPDATE background_jobs SET payload_json=json(?1) WHERE id=?2",
        rusqlite::params![legacy.to_string(), job.id],
    )
    .expect("pre-upgrade persisted payload fixture");
    let queued = store
        .background_job(&job.id)
        .await
        .expect("legacy row")
        .expect("job");
    queued
        .supported_payload()
        .expect("worker polling does not fail deserialization");
    let candidates = store
        .job_candidates(CandidateQuery {
            node_id: "worker".into(),
            kinds: vec![JobKind::SubtitleTranslationRange],
            after: None,
            now_ms: 1400,
            limit: 16,
            admissible_only: false,
        })
        .await
        .expect("legacy candidates");
    assert!(
        candidates
            .jobs
            .iter()
            .any(|candidate| candidate.id == job.id),
        "existing daemon reconciliation can retire the obsolete row"
    );
    let outcome = store
        .claim_artifact_job(ClaimJob {
            job_id: job.id.clone(),
            expected_revision: queued.revision,
            node_id: "worker".into(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            claim_id: uuid::Uuid::new_v4().to_string(),
            kind: JobKind::SubtitleTranslationRange,
            payload_version: 1,
            now_ms: 1400,
            dispatched_at_ms: 1400,
        })
        .await
        .expect("closed legacy claim");
    assert!(!matches!(outcome, ClaimOutcome::Claimed { .. }));
    assert!(!store
        .publish_translation_segment(PublishTranslationSegment {
            token: job.token.expect("old token"),
            source: parsed,
            segment: draft(&recipe, job.fence, "Obsolete output"),
            now_ms: 1450
        })
        .await
        .expect("closed legacy publication"));
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_segments",
            [],
            |row| row.get::<_, i64>(0)
        )
        .expect("no publication"),
        0
    );
}

async fn continuity_heartbeat(
    store: &SqliteStore,
    recipe: &plurx_core::store::subtitle_translation::RangeRecipe,
    playback: &str,
    position: i64,
    now: i64,
    continuity: Option<&str>,
) -> serde_json::Value {
    use plurx_core::store::subtitle_translation_read::{TranslationQuery, TranslationRead};
    let demand = store
        .read_subtitle_translation(TranslationRead {
            user_id: 0,
            now_ms: now,
            query: TranslationQuery::LiveDemands {
                operation_id: recipe.operation_id.clone(),
                operation_epoch: 1,
            },
        })
        .await
        .expect("live demand")
        .into_iter()
        .find(|d| d["playback_id"] == playback)
        .expect("this playback demand");
    let receipt = store
        .mutate_subtitle_translation(mutate(
            TranslationAction::Demand {
                operation_id: recipe.operation_id.clone(),
                expected_operation_epoch: 1,
                demand_id: demand["demand_id"].as_str().expect("demand").into(),
                expected_generation: demand["generation"].as_i64().expect("generation"),
                playback_id: playback.into(),
                position_ms: position,
                playback_rate: 1.0,
                paused: false,
                continuity_id: continuity.map(str::to_owned),
            },
            now,
        ))
        .await
        .expect("heartbeat");
    assert_eq!(receipt["outcome"], "accepted");
    assert_eq!(
        receipt["continuity_id"],
        serde_json::to_value(continuity).expect("echo")
    );
    receipt
}
async fn range_interested(store: &SqliteStore, id: &str, now: i64) -> bool {
    use plurx_core::store::subtitle_translation_read::{TranslationQuery, TranslationRead};
    store
        .read_subtitle_translation(TranslationRead {
            user_id: 0,
            now_ms: now,
            query: TranslationQuery::RangeInterest { job_id: id.into() },
        })
        .await
        .expect("running interest")
        .first()
        .is_some_and(|row| row["interested"] == true)
}
#[tokio::test]
async fn moving_clock_continuity_finishes_claimed_core_and_cleans_exact_attempt_grants() {
    use plurx_core::store::{
        background_jobs::{RenewJobs, RenewOutcome},
        subtitle_translation_publication::PublishTranslationSegment,
    };
    let (_dir, store, conn) = fixture();
    let continuity = uuid::Uuid::new_v4().to_string();
    let mut initial = create(1000);
    let playback = if let TranslationAction::Create {
        continuity_id,
        playback_id,
        ..
    } = &mut initial.action
    {
        *continuity_id = Some(continuity.clone());
        playback_id.clone()
    } else {
        unreachable!()
    };
    if let TranslationAction::Create { duration_ms, .. } = &mut initial.action {
        *duration_ms = 180000;
    }
    let recipe = prepared_from(&store, initial).await;
    let job = range(&store, &recipe, "moving-clock", 1300).await;
    let mut token = job.token.clone().expect("owned");
    for position in (5000..=120000).step_by(5000) {
        let now = 1300 + position;
        continuity_heartbeat(&store, &recipe, &playback, position, now, Some(&continuity)).await;
        token = match store
            .renew_jobs(RenewJobs {
                tokens: vec![token],
                now_ms: now,
            })
            .await
            .expect("renew")
            .remove(0)
        {
            RenewOutcome::Renewed { token } => token,
            other => panic!("renewal: {other:?}"),
        };
        assert!(
            range_interested(&store, &job.id, now).await,
            "ordinary 1x progression must not cancel at a core boundary"
        );
    }
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_range_continuities",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("grants"),
        1
    );
    assert!(store
        .publish_translation_segment(PublishTranslationSegment {
            token,
            source: job.supported_payload().expect("payload"),
            segment: draft(&recipe, job.fence, "Completed continuously owned range"),
            now_ms: 121400,
        })
        .await
        .expect("publication after four core boundaries"));
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_range_continuities",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("terminal cleanup"),
        0
    );
}
#[tokio::test]
async fn seek_or_legacy_heartbeat_revokes_continuation_but_same_window_stays_useful() {
    use plurx_core::store::subtitle_translation_publication::PublishTranslationSegment;
    for legacy in [false, true] {
        let (_dir, store, conn) = fixture();
        let continuity = uuid::Uuid::new_v4().to_string();
        let mut initial = create(1000);
        let playback = if let TranslationAction::Create {
            continuity_id,
            playback_id,
            ..
        } = &mut initial.action
        {
            *continuity_id = Some(continuity.clone());
            playback_id.clone()
        } else {
            unreachable!()
        };
        let recipe = prepared_from(&store, initial).await;
        let job = range(&store, &recipe, "seek-revocation", 1300).await;
        let seek = uuid::Uuid::new_v4().to_string();
        let next = if legacy { None } else { Some(seek.as_str()) };
        continuity_heartbeat(&store, &recipe, &playback, 30000, 1400, next).await;
        assert!(!range_interested(&store, &job.id, 1450).await);
        let publication = PublishTranslationSegment {
            token: job.token.clone().expect("token"),
            source: job.supported_payload().expect("payload"),
            segment: draft(&recipe, job.fence, "Obsolete seek output"),
            now_ms: 1500,
        };
        assert!(!store
            .publish_translation_segment(publication.clone())
            .await
            .expect("withdrawn continuity"));
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM subtitle_translation_segments",
                [],
                |r| r.get::<_, i64>(0)
            )
            .expect("no obsolete bytes"),
            0
        );
        continuity_heartbeat(&store, &recipe, &playback, 1000, 1600, next).await;
        assert!(
            range_interested(&store, &job.id, 1650).await,
            "same-window seek can still use the exact core"
        );
        assert!(store
            .publish_translation_segment(PublishTranslationSegment {
                now_ms: 1700,
                ..publication
            })
            .await
            .expect("useful core"));
    }
}
#[tokio::test]
async fn continuity_expiry_stop_and_claim_replacement_never_reuse_old_grants() {
    use plurx_core::store::background_jobs::{JobSettlement, SettleJob};
    let (_dir, store, conn) = fixture();
    let continuity = uuid::Uuid::new_v4().to_string();
    let mut initial = create(1000);
    if let TranslationAction::Create { continuity_id, .. } = &mut initial.action {
        *continuity_id = Some(continuity);
    }
    let recipe = prepared_from(&store, initial).await;
    let first = range(&store, &recipe, "grant-replacement", 1300).await;
    assert!(store
        .settle_job(SettleJob {
            token: first.token.expect("token"),
            settlement: JobSettlement::Yield {
                error_code: None,
                checkpoint: None,
                not_before_ms: 1500
            },
            now_ms: 1400
        })
        .await
        .expect("yield"));
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_range_continuities",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("yield cleans grant"),
        0
    );
    let next = claim(&store, &first.id, JobKind::SubtitleTranslationRange, 1600).await;
    assert!(next.fence > first.fence);
    assert_eq!(
        conn.query_row(
            "SELECT fence FROM subtitle_translation_range_continuities",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("new attempt grant"),
        next.fence
    );
    assert!(
        !range_interested(&store, &next.id, 16001).await,
        "expired demand cannot retain work"
    );
    store
        .mutate_subtitle_translation(mutate(
            TranslationAction::Stop {
                operation_id: recipe.operation_id,
                expected_operation_epoch: 1,
            },
            17000,
        ))
        .await
        .expect("Stop");
    assert!(!range_interested(&store, &next.id, 17500).await);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_range_continuities",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("Stop clears grant"),
        0
    );
}

#[tokio::test]
async fn another_continuous_viewer_keeps_shared_range_alive_after_first_viewer_seeks() {
    let (_dir, store, conn) = fixture();
    let continuity = uuid::Uuid::new_v4().to_string();
    let mut initial = create(1000);
    let playback = if let TranslationAction::Create {
        continuity_id,
        playback_id,
        ..
    } = &mut initial.action
    {
        *continuity_id = Some(continuity.clone());
        playback_id.clone()
    } else {
        unreachable!()
    };
    let recipe = prepared_from(&store, initial).await;
    let peer_continuity = uuid::Uuid::new_v4().to_string();
    let mut peer = create(1250);
    let peer_playback = if let TranslationAction::Create {
        continuity_id,
        playback_id,
        ..
    } = &mut peer.action
    {
        *continuity_id = Some(peer_continuity.clone());
        playback_id.clone()
    } else {
        unreachable!()
    };
    store
        .mutate_subtitle_translation(peer)
        .await
        .expect("peer joins");
    let job = range(&store, &recipe, "shared-continuity", 1300).await;
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_range_continuities",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("two supporters"),
        2
    );
    continuity_heartbeat(
        &store,
        &recipe,
        &playback,
        30000,
        1400,
        Some(&uuid::Uuid::new_v4().to_string()),
    )
    .await;
    continuity_heartbeat(
        &store,
        &recipe,
        &peer_playback,
        30000,
        1500,
        Some(&peer_continuity),
    )
    .await;
    assert!(
        range_interested(&store, &job.id, 1600).await,
        "one viewer's seek cannot cancel the other captured traversal"
    );
    continuity_heartbeat(
        &store,
        &recipe,
        &peer_playback,
        30000,
        1700,
        Some(&uuid::Uuid::new_v4().to_string()),
    )
    .await;
    assert!(
        !range_interested(&store, &job.id, 1800).await,
        "all withdrawn continuities cancel obsolete range"
    );
}

// Renewing a demand is permitted after expiry; renewing an already-withdrawn
// captured attempt grant is not. Exercise both mutation paths and UUID reuse.
async fn withdrawn_continuity_never_revives_case(late_create: bool, aba: bool) {
    use plurx_core::store::{
        subtitle_translation_publication::PublishTranslationSegment,
        subtitle_translation_read::{TranslationQuery, TranslationRead},
    };
    let (_dir, store, conn) = fixture();
    let continuity = uuid::Uuid::new_v4().to_string();
    let mut initial = create(1000);
    let playback = if let TranslationAction::Create {
        continuity_id,
        playback_id,
        ..
    } = &mut initial.action
    {
        *continuity_id = Some(continuity.clone());
        playback_id.clone()
    } else {
        unreachable!()
    };
    let recipe = prepared_from(&store, initial.clone()).await;
    let job = range(&store, &recipe, "withdrawn-continuity", 1300).await;
    let demand = store
        .read_subtitle_translation(TranslationRead {
            user_id: 0,
            now_ms: 1350,
            query: TranslationQuery::LiveDemands {
                operation_id: recipe.operation_id.clone(),
                operation_epoch: 1,
            },
        })
        .await
        .expect("original demand")
        .remove(0);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_range_continuities",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("original grant"),
        1
    );
    let now = if aba {
        let next = uuid::Uuid::new_v4().to_string();
        continuity_heartbeat(&store, &recipe, &playback, 30000, 1400, Some(&next)).await;
        continuity_heartbeat(&store, &recipe, &playback, 30000, 1500, Some(&continuity)).await;
        1600
    } else {
        assert!(
            !range_interested(&store, &job.id, 16001).await,
            "expiry withdraws running authority"
        );
        let receipt = if late_create {
            initial.request_id = uuid::Uuid::new_v4().to_string();
            initial.request_digest = "9".repeat(64);
            initial.now_ms = 20000;
            initial.intent_expires_at_ms = 300000;
            if let TranslationAction::Create { position_ms, .. } = &mut initial.action {
                *position_ms = 30000;
            }
            store
                .mutate_subtitle_translation(initial)
                .await
                .expect("late Create upsert")
        } else {
            store
                .mutate_subtitle_translation(mutate(
                    TranslationAction::Demand {
                        operation_id: recipe.operation_id.clone(),
                        expected_operation_epoch: 1,
                        demand_id: demand["demand_id"].as_str().expect("id").into(),
                        expected_generation: 1,
                        playback_id: playback.clone(),
                        position_ms: 30000,
                        playback_rate: 1.0,
                        paused: false,
                        continuity_id: Some(continuity.clone()),
                    },
                    20000,
                ))
                .await
                .expect("late heartbeat renews demand")
        };
        assert_eq!(receipt["outcome"], "accepted");
        assert_eq!(receipt["demand_generation"], "2");
        assert_eq!(receipt["continuity_id"], continuity);
        20100
    };
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_range_continuities",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("irreversible grant withdrawal"),
        0
    );
    assert!(
        !range_interested(&store, &job.id, now).await,
        "late renewal or UUID ABA cannot revive out-of-window attempt"
    );
    let publication = PublishTranslationSegment {
        token: job.token.clone().expect("token"),
        source: job.supported_payload().expect("payload"),
        segment: draft(&recipe, job.fence, "Withdrawn traversal output"),
        now_ms: now + 10,
    };
    assert!(!store
        .publish_translation_segment(publication.clone())
        .await
        .expect("atomic publication refusal"));
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM subtitle_translation_segments",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("no withdrawn bytes"),
        0
    );
    continuity_heartbeat(
        &store,
        &recipe,
        &playback,
        1000,
        now + 100,
        Some(&continuity),
    )
    .await;
    assert!(
        range_interested(&store, &job.id, now + 110).await,
        "current useful window recovers without reviving the captured grant"
    );
    assert!(store
        .publish_translation_segment(PublishTranslationSegment {
            now_ms: now + 120,
            ..publication
        })
        .await
        .expect("useful window publication"));
}
#[tokio::test]
async fn expired_demand_renewal_never_revives_old_out_of_window_continuity_grant() {
    withdrawn_continuity_never_revives_case(false, false).await;
}
#[tokio::test]
async fn late_create_upsert_never_revives_old_out_of_window_continuity_grant() {
    withdrawn_continuity_never_revives_case(true, false).await;
}
#[tokio::test]
async fn continuity_uuid_aba_never_revives_an_already_withdrawn_attempt_grant() {
    withdrawn_continuity_never_revives_case(false, true).await;
}

fn set_ai_who(conn: &rusqlite::Connection, who: &str) {
    conn.execute(
        "UPDATE settings SET value=json_set(value,'$.who',?1) WHERE key='subtitles.ai.v1'",
        [who],
    )
    .expect("AI subtitles who");
}

/// AI subtitles T2: `who` replaces `is_admin` in translation admission. A
/// viewer (user 2) can start, prepare and claim a range only while
/// `subtitles.ai.v1` says everyone; with admins-only the same range is not
/// claimable and a new start is forbidden.
#[tokio::test]
async fn non_admin_translation_range_is_claimable_only_when_ai_subtitles_who_is_everyone() {
    use plurx_core::store::background_jobs::{EnqueueJob, EnqueueOutcome, JobPayload, JobRequest};
    let (_dir, store, conn) = fixture();
    let viewer = |now| {
        let mut input = create(now);
        input.user_id = 2;
        input
    };
    let refused = store
        .mutate_subtitle_translation(viewer(1000))
        .await
        .expect("admins-only verdict");
    assert_eq!(refused["outcome"], "forbidden", "{refused}");
    set_ai_who(&conn, "everyone");
    let recipe = prepared_from(&store, viewer(1000)).await;
    let claimed = range(&store, &recipe, "viewer-core-0", 1300).await;
    assert!(!claimed.id.is_empty());

    let mut next = recipe.clone();
    next.core_index = 1;
    let operations = store
        .read_subtitle_translation(plurx_core::store::subtitle_translation_read::TranslationRead {
            user_id: 0,
            now_ms: 1400,
            query: plurx_core::store::subtitle_translation_read::TranslationQuery::WorkerOperations {
                node_id: "worker".into(),
                after: None,
            },
        })
        .await
        .expect("worker operations");
    let producer = operations
        .iter()
        .find(|op| op["operation_id"] == recipe.operation_id)
        .and_then(|op| op["preparation_job_id"].as_str())
        .expect("producer")
        .to_owned();
    let id = uuid::Uuid::new_v4().to_string();
    let outcome = store
        .enqueue_job(EnqueueJob {
            id: id.clone(),
            payload: JobPayload::SubtitleTranslationRange {
                file_id: 1,
                recipe: next,
                target_node_id: "worker".into(),
                producer_preparation_job_id: producer,
            },
            dedupe_key: "translation-contract-viewer-core-1".into(),
            priority: 1,
            not_before_ms: 0,
            now_ms: 1400,
            request: JobRequest {
                scope: "translation-contract".into(),
                request_id: uuid::Uuid::new_v4().to_string(),
                request_digest: "e".repeat(64),
                consumer_kind: "subtitle_translation_range".into(),
                consumer_ref: recipe.operation_id.clone(),
                target_node_id: Some("worker".into()),
                deadline_ms: None,
                retain_identity: false,
            },
        })
        .await
        .expect("range enqueue");
    assert!(
        matches!(outcome, EnqueueOutcome::Accepted { .. }),
        "{outcome:?}"
    );
    set_ai_who(&conn, "admins");
    let queued = store
        .background_job(&id)
        .await
        .expect("job")
        .expect("queued");
    let attempt = |revision| ClaimJob {
        job_id: id.clone(),
        expected_revision: revision,
        node_id: "worker".into(),
        boot_id: "00000000-0000-4000-8000-000000000002".into(),
        claim_id: uuid::Uuid::new_v4().to_string(),
        kind: JobKind::SubtitleTranslationRange,
        payload_version: 1,
        now_ms: 1401,
        dispatched_at_ms: 1401,
    };
    let refused = store
        .claim_artifact_job(attempt(queued.revision))
        .await
        .expect("admins-only claim");
    assert!(
        !matches!(refused, ClaimOutcome::Claimed { .. }),
        "a non-admin's range is not claimable while AI subtitles are admins-only: {refused:?}"
    );
    set_ai_who(&conn, "everyone");
    let queued = store
        .background_job(&id)
        .await
        .expect("job")
        .expect("still queued");
    let claimed = store
        .claim_artifact_job(attempt(queued.revision))
        .await
        .expect("everyone claim");
    assert!(
        matches!(claimed, ClaimOutcome::Claimed { .. }),
        "{claimed:?}"
    );
}
