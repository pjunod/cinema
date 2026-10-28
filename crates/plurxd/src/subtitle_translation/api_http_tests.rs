//! SQLite-backed public projections, lifecycle mutations and leased reads.
//! Worker publication fixtures use the real claim/bind/publish transactions;
//! they are synthetic protocol evidence and do not qualify a speech engine.
use super::*;
use axum::response::IntoResponse;
use plurx_core::{
    domain::{AudioStream, ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult, User},
    store::{
        background_jobs::{
            ClaimJob, ClaimOutcome, EnqueueJob, EnqueueOutcome, JobKind, JobRequest, JobSettlement,
            SettleJob,
        },
        subtitle_translation::{
            cue_id, ExecutionProvenance, RangeRecipe, SegmentDraft, TranslationCue,
        },
        subtitle_translation_publication::{BindTranslationSource, PublishTranslationSegment},
        subtitle_translation_settings::{TranslationSettings, SETTINGS_KEY},
    },
};
use std::{sync::Arc, time::Duration};

struct Fixture {
    state: AppState,
    admin: User,
    viewer: User,
    other: User,
    file: MediaFile,
    sha256: String,
    _root: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.state.shutdown.cancel();
    }
}

async fn fixture() -> Fixture {
    let root = crate::test_tempdir().expect("owned translation fixture");
    let directory = root.path().canonicalize().expect("canonical fixture root");
    let store = Arc::new(
        plurx_core::store::SqliteStore::open(&directory.join("store.db")).expect("SQLite schema"),
    );
    let state = AppState::new(
        "test".into(),
        store,
        crate::state::Dirs {
            artwork: directory.join("artwork"),
            transcode: directory.join("transcode"),
            cache: directory.join("cache"),
            subs: directory.join("subs"),
            runtime_cache: directory.join("runtime"),
            renditions: directory.join("renditions"),
        },
        "translation-http-worker".into(),
        Default::default(),
        Default::default(),
        Arc::new(crate::logbuf::LogBuffer::new(64)),
    );
    let admin = state
        .store
        .create_user("translation-admin", "unused", true)
        .await
        .expect("admin");
    let viewer = state
        .store
        .create_user("translation-viewer", "unused", false)
        .await
        .expect("viewer");
    let other = state
        .store
        .create_user("translation-other", "unused", true)
        .await
        .expect("second admin");
    state
        .store
        .put_setting(
            SETTINGS_KEY,
            &serde_json::to_string(&TranslationSettings {
                enabled: true,
                ..Default::default()
            })
            .expect("complete versioned settings fixture"),
        )
        .await
        .expect("translation on");
    let library = state
        .store
        .create_library(&NewLibrary {
            name: "Translation fixture".into(),
            kind: LibraryKind::Movies,
            paths: vec![directory.clone()],
            anime: false,
        })
        .await
        .expect("library");
    let item = state
        .store
        .insert_item(&NewItem {
            library_id: library.id,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Owned source".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let path = directory.join("source.mp4");
    let bytes =
        b"owned source bytes whose complete hash is checked before cached translation is served";
    std::fs::write(&path, bytes).expect("source bytes");
    let metadata = std::fs::metadata(&path).expect("source metadata");
    let mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("epoch")
        .as_secs() as i64;
    let probe = ProbeResult { duration_ms: Some(61_000), container: Some("mp4".into()), video_codec: Some("h264".into()), audio_streams: vec![AudioStream { index: 0, codec: "aac".into(), language: Some("deu".into()), default: true, ..Default::default() }], raw_json: Some(json!({"format":{"start_time":"0.000","duration":"61.000"},"streams":[{"index":0,"codec_type":"video","start_time":"0.000"},{"index":1,"codec_type":"audio","start_time":"0.000","duration":"61.000"}]}).to_string()), ..Default::default() };
    let file_id = state
        .store
        .upsert_file(
            item,
            &path.to_string_lossy(),
            bytes.len() as i64,
            mtime,
            &probe,
        )
        .await
        .expect("probed source");
    let file = state
        .store
        .get_file(file_id)
        .await
        .expect("source read")
        .expect("source");
    Fixture {
        state,
        admin,
        viewer,
        other,
        file,
        sha256: hex::encode(Sha256::digest(bytes)),
        _root: root,
    }
}
fn input<T: serde::de::DeserializeOwned>(body: Value) -> T {
    serde_json::from_value(body).expect("request shape")
}
fn envelope_value() -> Value {
    json!({"version":1,"request_id":uuid::Uuid::new_v4().to_string(),"intent_expires_at_ms":crate::state::clock_ms()+120_000})
}
fn create_input(fixture: &Fixture, language: &str) -> CreateRequest {
    let profile = super::super::profile::selected_profile();
    input(
        json!({"version":1,"request_id":uuid::Uuid::new_v4().to_string(),"intent_expires_at_ms":crate::state::clock_ms()+120_000,"source_size":fixture.file.size.to_string(),"source_mtime":fixture.file.mtime.to_string(),"audio_index":0,"spoken_language":language,"output_language":"en","task":"translate","model_id":profile.model_id,"profile_id":profile.id().expect("profile"),"playback_id":uuid::Uuid::new_v4().to_string(),"position_ms":0,"playback_rate":1.0,"paused":false}),
    )
}
async fn body(response: Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private,no-cache"
    );
    assert_eq!(response.headers()[header::VARY], "Authorization");
    assert!(response.headers().contains_key(CLOCK_HEADER));
    let bytes = axum::body::to_bytes(response.into_body(), PAGE_BYTES)
        .await
        .expect("bounded body");
    serde_json::from_slice(&bytes).expect("JSON")
}
async fn refused<T>(result: Result<T, ApiError>, status: StatusCode, code: &str) {
    let error = match result {
        Ok(_) => panic!("request unexpectedly succeeded"),
        Err(error) => error,
    };
    let response = error.into_response();
    assert_eq!(response.status(), status);
    let bytes = axum::body::to_bytes(response.into_body(), PAGE_BYTES)
        .await
        .expect("error body");
    let value: Value = serde_json::from_slice(&bytes).expect("error JSON");
    assert_eq!(value["code"], code, "{value}");
}
async fn seed_operation(f: &Fixture, language: &str) -> (CreateRequest, Value) {
    let request = create_input(f, language);
    let digest = config::request_digest(
        "POST",
        &format!("/files/{}/subtitle-translation", f.file.id),
        &request,
    )
    .expect("request digest");
    let receipt = f
        .state
        .store
        .mutate_subtitle_translation(TranslationMutation {
            user_id: f.admin.id,
            request_id: request.request_id.clone(),
            request_digest: digest,
            intent_expires_at_ms: request.intent_expires_at_ms,
            now_ms: crate::state::clock_ms(),
            action: TranslationAction::Create {
                source: SourceRevision {
                    file_id: f.file.id,
                    source_size: f.file.size,
                    source_mtime: f.file.mtime,
                },
                profile: super::super::profile::selected_profile(),
                audio_index: 0,
                spoken_language: language.into(),
                duration_ms: 61_000,
                target_node_id: f.state.node_id.clone(),
                playback_id: request.playback_id.clone(),
                position_ms: 0,
                playback_rate: 1.0,
                paused: false,
                continuity_id: None,
            },
        })
        .await
        .expect("admitted operation fixture");
    assert_eq!(receipt["outcome"], "accepted");
    (request, receipt)
}
async fn claim(f: &Fixture, id: &str, kind: JobKind) -> BackgroundJob {
    let job = f
        .state
        .store
        .background_job(id)
        .await
        .expect("job read")
        .expect("queued job");
    let now = crate::state::clock_ms();
    match f
        .state
        .store
        .claim_artifact_job(ClaimJob {
            job_id: id.into(),
            expected_revision: job.revision,
            node_id: f.state.node_id.clone(),
            boot_id: f.state.subtitle_models.boot_id.clone(),
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
        other => panic!("claim refused: {other:?}"),
    }
}
async fn bound(f: &Fixture, language: &str) -> RangeRecipe {
    let (_, receipt) = seed_operation(f, language).await;
    let job = claim(
        f,
        receipt["job_id"].as_str().expect("preparation job"),
        JobKind::SubtitleTranslationPrepare,
    )
    .await;
    let source = job.supported_payload().expect("preparation payload");
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
        panic!("preparation payload")
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
        source_sha256: f.sha256.clone(),
        profile,
        audio_index,
        spoken_language,
        duration_ms,
        core_index: 0,
    };
    recipe.asset_id = recipe.expected_asset_id().expect("asset identity");
    assert!(f
        .state
        .store
        .bind_translation_source(BindTranslationSource {
            token: job.token.expect("job token"),
            source,
            source_sha256: f.sha256.clone(),
            local_identity: "synthetic-claimed-source-publication".into(),
            now_ms: crate::state::clock_ms()
        })
        .await
        .expect("source binding"));
    recipe
}
async fn authorize(f: &Fixture, asset: &str) {
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match super::super::authorize_asset_source(&f.state, asset).await {
            Ok(()) => return,
            Err(error)
                if error.code() == Some("source_verifying")
                    && tokio::time::Instant::now() < until =>
            {
                tokio::time::sleep(Duration::from_millis(5)).await
            }
            Err(error) => panic!("held source verification failed: {error:?}"),
        }
    }
}
async fn range_job(f: &Fixture, recipe: &RangeRecipe) -> BackgroundJob {
    let operation = read_rows(
        &f.state,
        f.admin.id,
        TranslationQuery::Operation {
            operation_id: recipe.operation_id.clone(),
        },
    )
    .await
    .expect("operation")
    .remove(0);
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::state::clock_ms();
    let outcome = f
        .state
        .store
        .enqueue_job(EnqueueJob {
            id: id.clone(),
            payload: JobPayload::SubtitleTranslationRange {
                file_id: f.file.id,
                recipe: recipe.clone(),
                producer_preparation_job_id: operation["preparation_job_id"]
                    .as_str()
                    .expect("producer")
                    .into(),
                target_node_id: f.state.node_id.clone(),
            },
            dedupe_key: format!("translation-http-range:{id}"),
            priority: 1,
            not_before_ms: now,
            now_ms: now,
            request: JobRequest {
                scope: "translation-http-test".into(),
                request_id: uuid::Uuid::new_v4().to_string(),
                request_digest: "e".repeat(64),
                consumer_kind: "subtitle_translation".into(),
                consumer_ref: recipe.operation_id.clone(),
                target_node_id: Some(f.state.node_id.clone()),
                deadline_ms: None,
                retain_identity: false,
            },
        })
        .await
        .expect("enqueue range");
    assert!(matches!(outcome, EnqueueOutcome::Accepted { .. }));
    claim(f, &id, JobKind::SubtitleTranslationRange).await
}
async fn publish(f: &Fixture, recipe: &RangeRecipe, text: &str) {
    let job = range_job(f, recipe).await;
    let token = job.token.clone().expect("range token");
    let (start_ms, end_ms) = recipe.core_bounds().expect("core bounds");
    let draft = SegmentDraft {
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
                start_ms + 500,
                text,
            )
            .expect("cue identity"),
            start_ms,
            end_ms: start_ms + 500,
            text: text.into(),
        }],
        provenance: ExecutionProvenance {
            engine_sha256: "b".repeat(64),
            model_sha256: recipe.profile.model_sha256.clone(),
            operation_epoch: recipe.operation_epoch,
            platform: "linux-x86_64".into(),
            profile_id: recipe.profile.id().expect("profile"),
            source_sha256: recipe.source_sha256.clone(),
            worker_fence: token.fence,
        },
    };
    assert!(f
        .state
        .store
        .publish_translation_segment(PublishTranslationSegment {
            token,
            source: job.supported_payload().expect("range payload"),
            segment: draft,
            now_ms: crate::state::clock_ms()
        })
        .await
        .expect("atomic segment publication"));
}
fn lease_headers(lease: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        LEASE_HEADER,
        HeaderValue::from_str(lease).expect("lease header"),
    );
    headers
}
fn page_cursor(kind: &str, user: i64, asset: &str, lease: &Value) -> Cursor {
    let mut page = cursor(kind, user);
    page.asset_id = Some(asset.into());
    page.lease_id = Some(lease["lease_id"].as_str().expect("lease").into());
    page.snapshot_revision = Some(lease["revision"].as_str().expect("revision").into());
    page.after = "-1".into();
    page.window_start_ms = Some(0);
    page.window_end_ms = Some(61_000);
    page.through = Some(2);
    page
}

#[tokio::test]
async fn settings_models_and_options_project_saved_preferences_without_qualifying_a_worker() {
    let f = fixture().await;
    let saved = body(
        settings(AdminUser(f.admin.clone()), State(f.state.clone()))
            .await
            .expect("settings"),
    )
    .await;
    assert_eq!(saved["enabled"], true);
    let mut update = envelope_value();
    update["expected_revision"] = saved["revision"].clone();
    update["enabled"] = json!(false);
    update["model_id"] = Value::Null;
    update["profile_id"] = Value::Null;
    update["preferred_spoken_language"] = json!("de");
    let changed = body(
        update_settings(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            Json(input(update.clone())),
        )
        .await
        .expect("save"),
    )
    .await;
    assert_eq!(changed["enabled"], false);
    let replay = body(
        update_settings(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            Json(input(update)),
        )
        .await
        .expect("save replay"),
    )
    .await;
    assert_eq!(changed["revision"], replay["revision"]);
    let catalog = body(
        models(AdminUser(f.admin.clone()), State(f.state.clone()))
            .await
            .expect("models"),
    )
    .await;
    assert_eq!(catalog["profiles"].as_array().expect("profiles").len(), 2);
    assert!(catalog["workers"]
        .as_array()
        .expect("worker observations")
        .iter()
        .all(|worker| worker["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .all(|cap| cap["status"] != "verified")));
    let options_document = body(
        options(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
        )
        .await
        .expect("options"),
    )
    .await;
    assert_eq!(options_document["file_id"], f.file.id.to_string());
    assert_eq!(options_document["capability"]["supported"], true);
    assert_eq!(
        options_document["audio_tracks"][0]["normalized_language"],
        "de"
    );
    assert_eq!(options_document["audio_tracks"][0]["source_index"], 1);
    assert_eq!(options_document["permissions"]["demand"], false);
    assert_eq!(options_document["permissions"]["read"], true);
    assert!(options_document["assets"]
        .as_array()
        .expect("assets")
        .is_empty());
    refused(
        options(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(0),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    )
    .await;
}

#[tokio::test]
async fn create_replays_before_source_or_settings_changes_but_rechecks_current_admin_authority() {
    let f = fixture().await;
    let (request, receipt) = seed_operation(&f, "de").await;
    let expected = mutation_receipt(&receipt, "accepted").expect("safe receipt");
    let first = body(
        create(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
            Json(input(serde_json::to_value(&request).expect("input"))),
        )
        .await
        .expect("lost response replay"),
    )
    .await;
    assert_eq!(first, expected);
    f.state
        .store
        .put_setting(
            SETTINGS_KEY,
            &serde_json::to_string(&TranslationSettings::default())
                .expect("complete disabled settings fixture"),
        )
        .await
        .expect("disable");
    let changed_probe = ProbeResult {
        video_codec: Some("h264".into()),
        ..Default::default()
    };
    f.state
        .store
        .upsert_file(
            f.file.item_id,
            &f.file.path.to_string_lossy(),
            f.file.size + 1,
            f.file.mtime + 1,
            &changed_probe,
        )
        .await
        .expect("scanner changed source");
    let replay = body(
        create(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
            Json(input(serde_json::to_value(&request).expect("input"))),
        )
        .await
        .expect("exact replay after changed admission"),
    )
    .await;
    assert_eq!(replay, expected);
    let mut changed = serde_json::to_value(&request).expect("input");
    changed["paused"] = json!(true);
    refused(
        create(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
            Json(input(changed)),
        )
        .await,
        StatusCode::CONFLICT,
        "request_conflict",
    )
    .await;
    let db = rusqlite::Connection::open(f._root.path().join("store.db"))
        .expect("owned fixture connection");
    db.execute("UPDATE users SET is_admin=0 WHERE id=?1", [f.admin.id])
        .expect("revoke admin");
    refused(
        create(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
            Json(request),
        )
        .await,
        StatusCode::FORBIDDEN,
        "permission_denied",
    )
    .await;
}

#[tokio::test]
async fn create_reuses_an_existing_recipe_and_demand_release_stop_retry_keep_epoch_fences() {
    let f = fixture().await;
    let (_, original) = seed_operation(&f, "de").await;
    let request = create_input(&f, "de");
    let playback = request.playback_id.clone();
    let created = body(
        create(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
            Json(request),
        )
        .await
        .expect("join existing recipe"),
    )
    .await;
    assert_eq!(created["operation_id"], original["operation_id"]);
    let op = created["operation_id"]
        .as_str()
        .expect("operation")
        .to_owned();
    let demand_id = created["demand_id"].as_str().expect("demand").to_owned();
    let mut demand_body = envelope_value();
    demand_body["expected_operation_epoch"] = created["operation_epoch"].clone();
    demand_body["demand_generation"] = json!("2");
    demand_body["playback_id"] = json!(playback);
    demand_body["position_ms"] = json!(30_000);
    demand_body["playback_rate"] = json!(2.0);
    demand_body["paused"] = json!(false);
    let renewed = body(
        demand(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath((op.clone(), demand_id.clone())),
            Json(input(demand_body.clone())),
        )
        .await
        .expect("demand renewal"),
    )
    .await;
    assert_eq!(renewed["outcome"], "updated");
    assert_eq!(renewed["demand_generation"], "2");
    let replay = body(
        demand(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath((op.clone(), demand_id.clone())),
            Json(input(demand_body)),
        )
        .await
        .expect("renewal replay"),
    )
    .await;
    assert_eq!(replay, renewed);
    let mut release_body = envelope_value();
    release_body["expected_operation_epoch"] = created["operation_epoch"].clone();
    release_body["demand_generation"] = json!("3");
    release_body["playback_id"] = json!(playback);
    let released = body(
        release(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath((op.clone(), demand_id)),
            Json(input(release_body)),
        )
        .await
        .expect("release viewer demand"),
    )
    .await;
    assert_eq!(released["outcome"], "released");
    let mut stop_body = envelope_value();
    stop_body["expected_operation_epoch"] = created["operation_epoch"].clone();
    let stopped = body(
        stop(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath(op.clone()),
            Json(input(stop_body.clone())),
        )
        .await
        .expect("stop root"),
    )
    .await;
    assert_eq!(stopped["state"], "stopped");
    assert_eq!(stopped["operation_epoch"], "2");
    let stopped_replay = body(
        stop(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath(op.clone()),
            Json(input(stop_body)),
        )
        .await
        .expect("stop replay"),
    )
    .await;
    assert_eq!(stopped_replay, stopped);
    let observed = body(
        operation(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(op.clone()),
        )
        .await
        .expect("viewer operation"),
    )
    .await;
    assert_eq!(observed["state"], "stopped");
    assert!(observed.get("target_node_id").is_none());
    let mut retry_body = envelope_value();
    retry_body["scope"] = json!("operation");
    retry_body["expected_operation_epoch"] = stopped["operation_epoch"].clone();
    retry_body["expected_view_revision"] = stopped["view_revision"].clone();
    retry_body["expected_state"] = json!("stopped");
    retry_body["playback_id"] = json!(uuid::Uuid::new_v4().to_string());
    retry_body["position_ms"] = json!(0);
    retry_body["playback_rate"] = json!(1.0);
    retry_body["paused"] = json!(false);
    let resumed = body(
        retry(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath(op.clone()),
            Json(input(retry_body.clone())),
        )
        .await
        .expect("explicit retry"),
    )
    .await;
    assert_eq!(resumed["outcome"], "resumed");
    assert_eq!(resumed["operation_epoch"], "3");
    retry_body["request_id"] = json!(uuid::Uuid::new_v4().to_string());
    refused(
        retry(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath(op),
            Json(input(retry_body)),
        )
        .await,
        StatusCode::CONFLICT,
        "epoch_conflict",
    )
    .await;
}

#[tokio::test]
async fn invalid_create_and_demands_never_leave_partial_jobs_or_authority() {
    let f = fixture().await;
    let base = serde_json::to_value(create_input(&f, "de")).expect("create input");
    for (field, value) in [
        ("version", json!(2)),
        ("request_id", json!("invalid")),
        ("source_size", json!("01")),
        ("source_mtime", json!("+1")),
        ("spoken_language", json!("en")),
        ("output_language", json!("fr")),
        ("audio_index", json!(256)),
        ("position_ms", json!(-1)),
        ("playback_rate", json!(0.0)),
        ("profile_id", json!("bad")),
    ] {
        let mut bad = base.clone();
        bad[field] = value;
        refused(
            create(
                AdminUser(f.admin.clone()),
                State(f.state.clone()),
                ApiPath(f.file.id),
                Json(input(bad)),
            )
            .await,
            StatusCode::BAD_REQUEST,
            "invalid_request",
        )
        .await;
    }
    let mut expired = base.clone();
    expired["intent_expires_at_ms"] = json!(0);
    refused(
        create(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
            Json(input(expired)),
        )
        .await,
        StatusCode::GONE,
        "intent_expired",
    )
    .await;
    let mut stale = base.clone();
    stale["source_size"] = json!((f.file.size + 1).to_string());
    refused(
        create(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
            Json(input(stale)),
        )
        .await,
        StatusCode::CONFLICT,
        "source_changed",
    )
    .await;
    refused(
        create(
            AdminUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
            Json(input(base)),
        )
        .await,
        StatusCode::FORBIDDEN,
        "permission_denied",
    )
    .await;
    let jobs = f
        .state
        .store
        .job_candidates(plurx_core::store::background_jobs::CandidateQuery {
            node_id: f.state.node_id.clone(),
            kinds: vec![
                JobKind::SubtitleTranslationPrepare,
                JobKind::SubtitleTranslationRange,
            ],
            after: None,
            now_ms: crate::state::clock_ms(),
            limit: 32,
        })
        .await
        .expect("no accepted jobs");
    assert!(jobs.jobs.is_empty());
}

#[tokio::test]
async fn manifest_leases_reuse_etags_but_a_new_stop_view_cannot_be_hidden_by_not_modified() {
    let f = fixture().await;
    let recipe = bound(&f, "de").await;
    authorize(&f, &recipe.asset_id).await;
    publish(&f, &recipe, "A & B <literal>").await;
    let first = manifest(
        AuthUser(f.viewer.clone()),
        State(f.state.clone()),
        ApiPath(recipe.asset_id.clone()),
        HeaderMap::new(),
        Query(ManifestQuery {
            position_ms: 0,
            renew_lease: None,
        }),
    )
    .await
    .expect("manifest");
    let tag = first.headers()[header::ETAG].clone();
    assert!(first.headers().contains_key(EXPIRY_HEADER));
    let first = body(first).await;
    assert_eq!(first["segments"].as_array().expect("segment refs").len(), 1);
    assert_eq!(first["coverage"][0]["start_ms"], 0);
    assert_eq!(first["coverage"][0]["end_ms"], 30_000);
    let lease = first["snapshot"]["lease_id"].as_str().expect("lease");
    let mut conditional = lease_headers(lease);
    conditional.insert(header::IF_NONE_MATCH, tag);
    let unchanged = manifest(
        AuthUser(f.viewer.clone()),
        State(f.state.clone()),
        ApiPath(recipe.asset_id.clone()),
        conditional.clone(),
        Query(ManifestQuery {
            position_ms: 0,
            renew_lease: None,
        }),
    )
    .await
    .expect("conditional manifest");
    assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
    assert!(axum::body::to_bytes(unchanged.into_body(), 16)
        .await
        .expect("empty body")
        .is_empty());
    let mut stop_input = envelope_value();
    stop_input["expected_operation_epoch"] = json!("1");
    body(
        stop(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath(recipe.operation_id.clone()),
            Json(input(stop_input)),
        )
        .await
        .expect("stop"),
    )
    .await;
    let changed = body(
        manifest(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(recipe.asset_id.clone()),
            conditional,
            Query(ManifestQuery {
                position_ms: 0,
                renew_lease: None,
            }),
        )
        .await
        .expect("live Stop is visible"),
    )
    .await;
    assert_eq!(changed["operation"]["state"], "stopped");
    assert_eq!(changed["snapshot"]["lease_id"], lease);
    assert_eq!(
        changed["segments"], first["segments"],
        "accepted immutable bytes survive Stop"
    );
    let renewed = body(
        manifest(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(recipe.asset_id),
            lease_headers(lease),
            Query(ManifestQuery {
                position_ms: 0,
                renew_lease: Some(1),
            }),
        )
        .await
        .expect("explicit lease read"),
    )
    .await;
    assert_eq!(
        renewed["snapshot"]["lease_id"], lease,
        "a sufficiently fresh lease is reused without extending expiry"
    );
    assert!(
        renewed["snapshot"]["expires_in_ms"]
            .as_i64()
            .expect("remaining lease")
            <= first["snapshot"]["expires_in_ms"]
                .as_i64()
                .expect("initial remaining lease")
    );
}

#[tokio::test]
async fn leased_segments_and_pages_are_bound_to_account_asset_revision_and_actual_bytes() {
    let f = fixture().await;
    let recipe = bound(&f, "de").await;
    authorize(&f, &recipe.asset_id).await;
    publish(&f, &recipe, "Raw <b> text & punctuation").await;
    let document = body(
        manifest(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(recipe.asset_id.clone()),
            HeaderMap::new(),
            Query(ManifestQuery {
                position_ms: 0,
                renew_lease: None,
            }),
        )
        .await
        .expect("manifest"),
    )
    .await;
    let snapshot = &document["snapshot"];
    let headers = lease_headers(snapshot["lease_id"].as_str().expect("lease"));
    let segment_id = document["segments"][0]["segment_id"]
        .as_str()
        .expect("segment")
        .to_owned();
    let wire = segment(
        AuthUser(f.viewer.clone()),
        State(f.state.clone()),
        ApiPath((recipe.asset_id.clone(), segment_id.clone())),
        headers.clone(),
    )
    .await
    .expect("immutable segment");
    let bytes = axum::body::to_bytes(wire.into_body(), PAGE_BYTES)
        .await
        .expect("raw bytes");
    assert_eq!(
        hex::encode(Sha256::digest(&bytes)),
        document["segments"][0]["digest"]
    );
    let decoded: Value = serde_json::from_slice(&bytes).expect("segment JSON");
    assert_eq!(decoded["cues"][0]["text"], "Raw <b> text & punctuation");
    let coverage_cursor = page_cursor("coverage", f.viewer.id, &recipe.asset_id, snapshot);
    let covered = body(
        coverage(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(recipe.asset_id.clone()),
            headers.clone(),
            Query(PageQuery {
                cursor: encode_cursor(&coverage_cursor).expect("cursor"),
            }),
        )
        .await
        .expect("coverage page"),
    )
    .await;
    assert_eq!(covered["entries"][0]["end_ms"], 30_000);
    let segment_cursor = page_cursor("segments", f.viewer.id, &recipe.asset_id, snapshot);
    let segments = body(
        segments(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(recipe.asset_id.clone()),
            headers.clone(),
            Query(PageQuery {
                cursor: encode_cursor(&segment_cursor).expect("cursor"),
            }),
        )
        .await
        .expect("segments page"),
    )
    .await;
    assert_eq!(segments["entries"][0]["segment_id"], segment_id);
    refused(
        segment(
            AuthUser(f.other.clone()),
            State(f.state.clone()),
            ApiPath((recipe.asset_id.clone(), segment_id.clone())),
            headers.clone(),
        )
        .await,
        StatusCode::GONE,
        "translation_snapshot_expired",
    )
    .await;
    refused(
        segment(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath((recipe.asset_id.clone(), segment_id.clone())),
            HeaderMap::new(),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    )
    .await;
    refused(
        segment(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath((recipe.asset_id.clone(), "f".repeat(64))),
            headers.clone(),
        )
        .await,
        StatusCode::NOT_FOUND,
        "segment_unavailable",
    )
    .await;
    let mut stale = coverage_cursor.clone();
    stale.snapshot_revision = Some("999".into());
    refused(
        coverage(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(recipe.asset_id.clone()),
            headers.clone(),
            Query(PageQuery {
                cursor: encode_cursor(&stale).expect("cursor"),
            }),
        )
        .await,
        StatusCode::GONE,
        "translation_snapshot_expired",
    )
    .await;
    refused(
        coverage(
            AuthUser(f.other.clone()),
            State(f.state.clone()),
            ApiPath(recipe.asset_id.clone()),
            headers.clone(),
            Query(PageQuery {
                cursor: encode_cursor(&coverage_cursor).expect("cursor"),
            }),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    )
    .await;
    let connection = rusqlite::Connection::open(f._root.path().join("store.db"))
        .expect("owned fixture connection");
    connection
        .execute(
            "UPDATE subtitle_translation_segments SET body='{}' WHERE segment_id=?1",
            [&segment_id],
        )
        .expect("corrupted stored bytes");
    refused(
        segment(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath((recipe.asset_id, segment_id)),
            headers,
        )
        .await,
        StatusCode::SERVICE_UNAVAILABLE,
        "segment_unavailable",
    )
    .await;
}

#[tokio::test]
async fn failed_range_projection_retry_and_stale_failure_pages_use_the_live_view_fence() {
    let f = fixture().await;
    let recipe = bound(&f, "de").await;
    authorize(&f, &recipe.asset_id).await;
    let job = range_job(&f, &recipe).await;
    f.state
        .store
        .settle_job(SettleJob {
            token: job.token.expect("claim token"),
            settlement: JobSettlement::Fail {
                error_code: "inference_failed".into(),
            },
            now_ms: crate::state::clock_ms(),
        })
        .await
        .expect("range failure");
    let operation = body(
        operation(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(recipe.operation_id.clone()),
        )
        .await
        .expect("failure projection"),
    )
    .await;
    assert_eq!(operation["state"], "partial");
    assert_eq!(operation["failed_ranges"][0]["reason"], "inference_failed");
    let document = body(
        manifest(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(recipe.asset_id.clone()),
            HeaderMap::new(),
            Query(ManifestQuery {
                position_ms: 0,
                renew_lease: None,
            }),
        )
        .await
        .expect("partial manifest"),
    )
    .await;
    let snapshot = &document["snapshot"];
    let headers = lease_headers(snapshot["lease_id"].as_str().expect("lease"));
    let mut page = page_cursor("failures", f.viewer.id, &recipe.asset_id, snapshot);
    page.operation_id = Some(recipe.operation_id.clone());
    page.epoch = Some(operation["operation_epoch"].as_str().expect("epoch").into());
    page.view_revision = Some(operation["view_revision"].as_str().expect("view").into());
    let pages = body(
        failed_ranges(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(recipe.operation_id.clone()),
            headers.clone(),
            Query(PageQuery {
                cursor: encode_cursor(&page).expect("cursor"),
            }),
        )
        .await
        .expect("failure page"),
    )
    .await;
    assert_eq!(pages["entries"].as_array().expect("failures").len(), 1);
    let mut retry_input = envelope_value();
    retry_input["scope"] = json!("range");
    retry_input["expected_operation_epoch"] = operation["operation_epoch"].clone();
    retry_input["expected_operation_state"] = json!("partial");
    retry_input["core_index"] = json!(0);
    retry_input["expected_range_revision"] =
        operation["failed_ranges"][0]["range_revision"].clone();
    retry_input["expected_range_state"] = json!("failed");
    retry_input["playback_id"] = json!(uuid::Uuid::new_v4().to_string());
    retry_input["position_ms"] = json!(0);
    retry_input["playback_rate"] = json!(1.0);
    retry_input["paused"] = json!(false);
    let retried = body(
        retry(
            AdminUser(f.admin.clone()),
            State(f.state.clone()),
            ApiPath(recipe.operation_id.clone()),
            Json(input(retry_input)),
        )
        .await
        .expect("retry selected failed range"),
    )
    .await;
    assert_eq!(retried["outcome"], "range_retry_queued");
    assert_eq!(retried["core_index"], 0);
    assert!(retried["job_id"].is_string());
    refused(
        failed_ranges(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(recipe.operation_id.clone()),
            headers,
            Query(PageQuery {
                cursor: encode_cursor(&page).expect("old cursor"),
            }),
        )
        .await,
        StatusCode::GONE,
        "translation_snapshot_expired",
    )
    .await;
}

#[tokio::test]
async fn asset_pages_are_bound_to_the_file_and_viewer_and_source_changes_revoke_cached_reads() {
    let f = fixture().await;
    let recipe = bound(&f, "de").await;
    authorize(&f, &recipe.asset_id).await;
    let options = body(
        options(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
        )
        .await
        .expect("asset options"),
    )
    .await;
    assert_eq!(options["assets"].as_array().expect("assets").len(), 1);
    let mut page = cursor("assets", f.viewer.id);
    page.file_id = Some(f.file.id.to_string());
    page.after = recipe.asset_id.clone();
    let encoded = encode_cursor(&page).expect("asset page cursor");
    let last = body(
        assets(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
            Query(PageQuery {
                cursor: encoded.clone(),
            }),
        )
        .await
        .expect("end of assets"),
    )
    .await;
    assert!(last["entries"].as_array().expect("entries").is_empty());
    assert!(last["next_cursor"].is_null());
    refused(
        assets(
            AuthUser(f.other.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
            Query(PageQuery { cursor: encoded }),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    )
    .await;
    page.file_id = Some((f.file.id + 1).to_string());
    refused(
        assets(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
            Query(PageQuery {
                cursor: encode_cursor(&page).expect("cursor"),
            }),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    )
    .await;
    f.state
        .store
        .upsert_file(
            f.file.item_id,
            &f.file.path.to_string_lossy(),
            f.file.size + 1,
            f.file.mtime + 1,
            &ProbeResult {
                video_codec: Some("h264".into()),
                ..Default::default()
            },
        )
        .await
        .expect("authoritative source replacement");
    // LIVE_ASSET's canonical size/mtime predicate hides this old asset
    // before authorize_asset_source can obtain a source to recheck. Preserve
    // that ordinary absence response rather than exposing obsolete metadata.
    assert!(read_rows(
        &f.state,
        f.viewer.id,
        TranslationQuery::Asset {
            asset_id: recipe.asset_id.clone()
        },
    )
    .await
    .expect("canonical asset visibility")
    .is_empty());
    let absent = match manifest(
        AuthUser(f.viewer.clone()),
        State(f.state.clone()),
        ApiPath(recipe.asset_id),
        HeaderMap::new(),
        Query(ManifestQuery {
            position_ms: 0,
            renew_lease: None,
        }),
    )
    .await
    {
        Ok(_) => panic!("replaced source must not expose its stale cached asset"),
        Err(error) => error.into_response(),
    };
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
    let bytes = axum::body::to_bytes(absent.into_body(), PAGE_BYTES)
        .await
        .expect("ordinary absence body");
    let absent: Value = serde_json::from_slice(&bytes).expect("absence JSON");
    assert_eq!(absent, json!({"error":"translation asset not found"}));
    refused(
        operation(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(recipe.operation_id),
        )
        .await,
        StatusCode::CONFLICT,
        "source_changed",
    )
    .await;
}

#[tokio::test]
async fn asset_listing_continues_after_sixteen_entries_without_duplicates_or_account_reuse() {
    let f = fixture().await;
    let mut expected = std::collections::BTreeSet::new();
    for language in [
        "de", "fr", "es", "it", "pt", "nl", "da", "sv", "no", "fi", "pl", "cs", "sk", "ro", "hu",
        "tr", "uk",
    ] {
        let recipe = bound(&f, language).await;
        expected.insert(recipe.asset_id);
        // End each owned demand so this pagination fixture respects the real
        // four-live-demands-per-user limit rather than changing that limit.
        let now = crate::state::clock_ms();
        let stopped = f
            .state
            .store
            .mutate_subtitle_translation(TranslationMutation {
                user_id: f.admin.id,
                request_id: uuid::Uuid::new_v4().to_string(),
                request_digest: uuid::Uuid::new_v4().simple().to_string().repeat(2),
                intent_expires_at_ms: now + 120_000,
                now_ms: now,
                action: TranslationAction::Stop {
                    operation_id: recipe.operation_id,
                    expected_operation_epoch: 1,
                },
            })
            .await
            .expect("release fixture demand through Stop");
        assert_eq!(stopped["outcome"], "accepted");
    }
    let first = body(
        options(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
        )
        .await
        .expect("first asset page"),
    )
    .await;
    assert_eq!(first["assets"].as_array().expect("first page").len(), 16);
    let cursor = first["assets_next_cursor"]
        .as_str()
        .expect("seventeenth asset has a cursor")
        .to_owned();
    let second = body(
        assets(
            AuthUser(f.viewer.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
            Query(PageQuery {
                cursor: cursor.clone(),
            }),
        )
        .await
        .expect("second asset page"),
    )
    .await;
    assert_eq!(second["entries"].as_array().expect("last page").len(), 1);
    assert!(second["next_cursor"].is_null());
    let all = first["assets"]
        .as_array()
        .expect("first")
        .iter()
        .chain(second["entries"].as_array().expect("second"))
        .map(|asset| {
            asset["asset_id"]
                .as_str()
                .expect("asset identity")
                .to_owned()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(all, expected);
    refused(
        assets(
            AuthUser(f.other.clone()),
            State(f.state.clone()),
            ApiPath(f.file.id),
            Query(PageQuery { cursor }),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    )
    .await;
}
