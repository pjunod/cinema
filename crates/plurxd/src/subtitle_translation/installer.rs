//! Speech profile qualification through the one model operation.
//!
//! Translation no longer has its own install/verify path: the model receipt
//! that `subtitle_models` publishes after a multilingual model's German
//! translate self-test qualifies both speech profiles. What remains here is
//! the explicit local runtime qualification that drives that operation and
//! the progressive worker end to end.

/// Explicit local runtime qualification; excluded from every ordinary test run.
/// The production model-operation helper is also the HTTP setup handler's seam.
#[cfg(all(test, unix))]
mod qualification {
    use super::super::profile;
    use crate::{
        http::extract::{AdminUser, AuthUser},
        subtitle_translation::{api, config},
    };
    use crate::{state::AppState, subtitle_models::artifacts};
    use axum::{
        extract::{Path, Query, State},
        http::{HeaderMap, HeaderValue},
        Json,
    };
    use plurx_core::store::background_jobs_transcription::ModelAction;
    use plurx_core::store::{
        background_jobs::{JobPayload, JobState},
        keys, BackgroundJobStore, SqliteStore, Store,
    };
    use plurx_core::{
        domain::{ItemKind, LibraryKind, NewItem, NewLibrary},
        store::{
            background_jobs::JobQuery,
            subtitle_translation::{RangeRecipe, SegmentDraft, SourceRevision},
        },
    };
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::MetadataExt;
    use std::time::Duration;
    use std::{os::unix::fs::DirBuilderExt, path::PathBuf, sync::Arc};
    use tokio_util::sync::CancellationToken;

    fn required_path(name: &str) -> PathBuf {
        let path = std::env::var_os(name)
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                panic!("{name} must name an explicit qualification-owned absolute path")
            });
        assert!(path.is_absolute(), "{name} must be absolute");
        path
    }

    async fn private_state() -> (PathBuf, AppState) {
        let root = required_path("PLURX_SPEECH_QUALIFICATION_ROOT");
        let temporary = required_path("TMPDIR");
        assert!(
            temporary.starts_with(&root),
            "TMPDIR must be inside the explicit qualification root"
        );
        let engine_path = required_path("PLURX_SUBTITLE_ENGINE");
        let decoder_path = required_path("PLURX_FFMPEG");
        assert!(
            engine_path.is_file(),
            "build the pinned packaged engine first"
        );
        assert!(decoder_path.is_file(), "provide the real decoder");
        std::fs::create_dir_all(&root).expect("qualification root");
        std::fs::create_dir_all(&temporary).expect("owned temporary root");
        let session = root.join(format!("speech-session-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&session)
            .expect("exclusive private qualification session");
        eprintln!(
            "private speech qualification session: {}",
            session.display()
        );
        let database = session.join("qualification.sqlite");
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open(&database).expect("private durable SQLite"));
        store
            .put_setting(keys::SW_POOL_THREADS, "2")
            .await
            .expect("bounded production CPU budget");
        let mut state = AppState::new_unhooked(
            "Local pinned speech qualification".into(),
            store,
            crate::state::Dirs {
                artwork: session.join("artwork"),
                transcode: session.join("transcode"),
                cache: session.join("cache"),
                subs: session.join("subs"),
                runtime_cache: session.join("runtime"),
                renditions: session.join("renditions"),
            },
            "speech-qualification".into(),
            Default::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(64)),
        );
        // Only model bytes persist between independent private job databases.
        state.subtitle_models = Arc::new(crate::subtitle_models::Runtime::new(&root));
        (session, state)
    }

    async fn qualify(
        state: &AppState,
        selected: &plurx_core::store::subtitle_translation::TranslationProfile,
        action: ModelAction,
        deadline: tokio::time::Instant,
    ) -> serde_json::Value {
        let profile_id = selected.id().expect("profile identity");
        let spec = profile::model_spec_for(selected).expect("registered qualification profile");
        let request = json!({
            "version":2, "request_id":uuid::Uuid::new_v4().to_string(),
            "intent_expires_at_ms":crate::state::clock_ms()+300000,
            "purpose":"translation", "profile_id":profile_id,
            "node_ids":[state.node_id],
        });
        let axum::Json(reply) =
            crate::subtitle_translation::model_operation(state, &spec.id, action, request)
                .await
                .expect("production setup request enqueue");
        let job_id = reply["operations"][0]["job_id"]
            .as_str()
            .expect("durable setup job")
            .to_owned();
        let queued = state
            .store
            .background_job(&job_id)
            .await
            .expect("queued job read")
            .expect("queued job");
        assert!(matches!(
            queued.state,
            JobState::Queued | JobState::Succeeded
        ));
        assert!(
            matches!(queued.supported_payload().expect("typed model payload"),
            JobPayload::SubtitleModelOperation { translation_profile:None, action:accepted_action, .. }
                if accepted_action == action)
        );
        if queued.state == JobState::Queued {
            let cancel = CancellationToken::new();
            let worker = crate::subtitle_models::worker_pass(state, &cancel);
            tokio::pin!(worker);
            tokio::select! {
                result = &mut worker => result.expect("real serial model worker"),
                () = tokio::time::sleep_until(deadline) => {
                    cancel.cancel();
                    let result = worker.await;
                    panic!("bounded qualification deadline reached; cancelled worker fully awaited: {result:?}");
                }
            };
        }
        let completed = state
            .store
            .background_job(&job_id)
            .await
            .expect("completed job read")
            .expect("completed job");
        assert_eq!(
            completed.state,
            JobState::Succeeded,
            "real task {:?} action {action:?}: error={:?}, checkpoint={:?}, attempt_errors={}",
            selected.task,
            completed.last_error_code,
            completed.checkpoint,
            completed.attempt_errors
        );
        assert!(completed.fence > 0, "actual durable claim required");
        assert_eq!(
            state.transcode.admissions().software_in_use(),
            0,
            "production resource guards released after publish"
        );
        let workers = crate::subtitle_translation::workers_public(state)
            .await
            .expect("live physical readiness inspection");
        assert!(
            workers.iter().any(
                |worker| worker["capabilities"].as_array().is_some_and(|caps| caps
                    .iter()
                    .any(|cap| cap["profile_id"] == profile_id && cap["status"] == "verified"))
            ),
            "the one published model receipt must qualify this task profile: {workers:?}"
        );
        json!({"job_id":job_id,"task":selected.task,"profile_id":profile_id,
            "action":action,"state":"succeeded","fence":completed.fence})
    }

    /// Downloads/reuses the pinned legacy Small model and exercises real decoding,
    /// Whisper inference, task-specific cue checks and durable receipt publish.
    /// It proves the compiled German fixture, not arbitrary media or languages.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "real pinned model download/inference; requires PLURX_SPEECH_QUALIFICATION_ROOT, PLURX_SUBTITLE_ENGINE, PLURX_FFMPEG and owned TMPDIR"]
    async fn pinned_whisper_install_and_verify_qualify_both_speech_tasks() {
        let (session, state) = private_state().await;
        let database = session.join("qualification.sqlite");
        let engine = artifacts::inspect_engine()
            .await
            .expect("verify real packaged engine and companions");
        let spec = profile::model_spec();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20 * 60);
        let mut evidence = Vec::new();
        for selected in profile::legacy_profiles() {
            for action in [ModelAction::Install, ModelAction::Verify] {
                evidence.push(qualify(&state, &selected, action, deadline).await);
            }
        }
        let reopened =
            SqliteStore::open(&database).expect("reopen committed qualification database");
        let receipts = reopened
            .subtitle_model_receipts(&state.node_id)
            .await
            .expect("durable model receipts");
        assert!(
            receipts
                .iter()
                .any(|receipt| receipt.model_sha256 == spec.sha256
                    && receipt.engine_sha256 == engine.binary_sha256
                    && receipt.boot_id == state.subtitle_models.boot_id),
            "the one model receipt must survive reopening private SQLite"
        );
        state.shutdown.cancel();
        let receipt = json!({"version":1,"scope":"local pinned German fixture through production queued Install/Verify for both speech tasks",
            "model_sha256":spec.sha256,"engine_sha256":engine.binary_sha256,"adapter_contract":engine.adapter_contract,
            "fixture_sha256":profile::SELF_TEST_SHA256,"operations":evidence});
        let receipt_path = session.join("speech-qualification.json");
        std::fs::write(
            &receipt_path,
            serde_json::to_vec_pretty(&receipt).expect("receipt JSON"),
        )
        .expect("persistent local evidence");
        eprintln!("speech qualification receipt: {}", receipt_path.display());
    }
    async fn reply(response: axum::response::Response) -> serde_json::Value {
        assert!(response.status().is_success(), "typed API success");
        let bytes = axum::body::to_bytes(response.into_body(), 65_536)
            .await
            .expect("bounded reply");
        serde_json::from_slice(&bytes).expect("typed API JSON")
    }

    fn playback_position(viewer: &serde_json::Value, clock: std::time::Instant) -> i64 {
        if viewer["moving"] != true {
            return 0;
        }
        i64::try_from(clock.elapsed().as_millis())
            .expect("bounded test clock")
            .min(viewer["duration_ms"].as_i64().expect("source duration"))
    }
    async fn observe_current(
        state: &AppState,
        admin: &plurx_core::domain::User,
        viewer: &mut serde_json::Value,
        clock: std::time::Instant,
    ) {
        use sha2::{Digest, Sha256};
        if viewer["moving"] != true {
            return;
        }
        let position = playback_position(viewer, clock);
        if position >= viewer["duration_ms"].as_i64().expect("duration") {
            return;
        }
        let operation = reply(
            super::super::api::operation(
                AuthUser(admin.clone()),
                State(state.clone()),
                Path(viewer["operation_id"].as_str().expect("operation").into()),
            )
            .await
            .expect("moving operation read"),
        )
        .await;
        let Some(asset) = operation["asset_id"].as_str() else {
            return;
        };
        let manifest = reply(
            super::super::api::manifest(
                AuthUser(admin.clone()),
                State(state.clone()),
                Path(asset.into()),
                axum::http::HeaderMap::new(),
                axum::extract::Query(
                    serde_json::from_value(json!({"position_ms":position}))
                        .expect("current-position query"),
                ),
            )
            .await
            .expect("moving manifest read"),
        )
        .await;
        let mut useful = false;
        let mut active_cue = false;
        for reference in manifest["segments"].as_array().expect("moving references") {
            if reference["start_ms"].as_i64().expect("start") > position
                || reference["end_ms"].as_i64().expect("end") <= position
            {
                continue;
            }
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                "x-translation-read-lease",
                axum::http::HeaderValue::from_str(
                    manifest["snapshot"]["lease_id"].as_str().expect("lease"),
                )
                .expect("header"),
            );
            let response = super::super::api::segment(
                AuthUser(admin.clone()),
                State(state.clone()),
                Path((
                    asset.into(),
                    reference["segment_id"].as_str().expect("segment").into(),
                )),
                headers,
            )
            .await
            .expect("current canonical read");
            let bytes = axum::body::to_bytes(response.into_body(), 16384)
                .await
                .expect("bounded current segment");
            assert_eq!(
                hex::encode(Sha256::digest(&bytes)),
                reference["digest"].as_str().expect("digest")
            );
            let body: serde_json::Value =
                serde_json::from_slice(&bytes).expect("canonical current body");
            assert_eq!(body["asset_id"], asset);
            assert_eq!(body["provenance"]["profile_id"], viewer["profile_id"]);
            assert_eq!(manifest["audio_index"], 2);
            active_cue = body["cues"].as_array().expect("cues").iter().any(|cue| {
                cue["start_ms"]
                    .as_i64()
                    .is_some_and(|start| start <= position)
                    && cue["end_ms"].as_i64().is_some_and(|end| end > position)
            });
            useful = body["cues"].as_array().expect("cues").iter().any(|cue| {
                !cue["text"].as_str().unwrap_or_default().is_empty()
                    && cue["start_ms"].as_i64().is_some_and(|start| {
                        start < reference["end_ms"].as_i64().expect("end")
                            && start <= position + 5000
                    })
                    && cue["end_ms"].as_i64().is_some_and(|end| {
                        end > reference["start_ms"].as_i64().expect("start")
                            && end >= position - 5000
                    })
            });
        }
        let observations = viewer["observations"]
            .as_array_mut()
            .expect("bounded observations");
        assert!(
            observations.len() < 128,
            "qualification read cadence remains bounded"
        );
        observations.push(json!({"position_ms":position,"useful_current_core":useful,"active_cue":active_cue,"cue_proximity_bound_ms":5000,
            "manifest_revision":manifest["manifest_revision"],"task":manifest["task"],"profile_id":manifest["profile_id"]}));
        if useful && viewer["first_useful_ms"].is_null() {
            viewer["first_useful_ms"] = position.into();
        }
    }
    async fn viewer_tick(
        state: &AppState,
        admin: &plurx_core::domain::User,
        viewer: &mut serde_json::Value,
        clock: std::time::Instant,
    ) -> Result<(), String> {
        let position = playback_position(viewer, clock);
        viewer["max_heartbeat_position_ms"] = position.into();
        let generation = viewer["demand_generation"]
            .as_str()
            .expect("generation")
            .parse::<u64>()
            .expect("counter")
            + 1;
        let request=serde_json::from_value(json!({"version":1,"request_id":uuid::Uuid::new_v4().to_string(),
            "intent_expires_at_ms":crate::state::clock_ms()+300000,"expected_operation_epoch":viewer["operation_epoch"],
            "demand_generation":generation.to_string(),"playback_id":viewer["playback_id"],"continuity_id":viewer["continuity_id"],
            "position_ms":position,"playback_rate":1.0,"paused":false})).expect("typed heartbeat");
        let response = super::super::api::demand(
            AuthUser(admin.clone()),
            State(state.clone()),
            Path((
                viewer["operation_id"].as_str().expect("operation").into(),
                viewer["demand_id"].as_str().expect("demand").into(),
            )),
            Json(request),
        )
        .await
        .map_err(|error| format!("heartbeat: {error:?}"))?;
        let receipt = reply(response).await;
        assert_eq!(receipt["demand_generation"], generation.to_string());
        assert_eq!(receipt["continuity_id"], viewer["continuity_id"]);
        viewer["demand_generation"] = receipt["demand_generation"].clone();
        observe_current(state, admin, viewer, clock).await;
        Ok(())
    }
    async fn viewer_pass(
        state: &AppState,
        admin: &plurx_core::domain::User,
        viewer: &mut serde_json::Value,
        deadline: tokio::time::Instant,
        clock: std::time::Instant,
    ) {
        let cancel = CancellationToken::new();
        let worker = crate::subtitle_translation::manager::worker_pass(state, &cancel);
        tokio::pin!(worker);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                result=&mut worker=>{assert!(result.expect("real progressive worker"),"queued/demanded work");break;}
                ()=tokio::time::sleep_until(deadline)=>{cancel.cancel();let result=worker.await;panic!("qualification deadline; cancelled/awaited: {result:?}");}
                _=heartbeat.tick()=>{if let Err(error)=viewer_tick(state,admin,viewer,clock).await {
                    cancel.cancel();let result=worker.await;panic!("{error}; worker awaited: {result:?}");
                }}
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "real legacy Small inference; explicit speech qualification root/cache/engine/ffmpeg/ffprobe/TMPDIR required"]
    async fn pinned_progressive_worker_commits_both_tasks_for_selected_audio_two() {
        progressive_case(35000, false, profile::legacy_profiles()).await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "real useful 1x Small playback qualification; explicit speech qualification root/cache/engine/ffmpeg/ffprobe/TMPDIR required"]
    async fn pinned_progressive_worker_covers_advancing_playback_for_both_tasks() {
        progressive_case(95000, true, profile::legacy_profiles()).await;
    }
    async fn progressive_case(
        duration_ms: i64,
        moving: bool,
        selected_profiles: [plurx_core::store::subtitle_translation::TranslationProfile; 2],
    ) {
        let (session, state) = private_state().await;
        assert!(
            required_path("PLURX_FFPROBE").is_file(),
            "real pinned probe required"
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20 * 60);
        let engine = artifacts::inspect_engine()
            .await
            .expect("actual packaged engine");
        let mut qualifications = Vec::new();
        for (index, selected) in selected_profiles.iter().enumerate() {
            let action = if moving && index == 0 {
                ModelAction::Install
            } else {
                ModelAction::Verify
            };
            qualifications.push(qualify(&state, selected, action, deadline).await);
        }
        let model_sha256 = selected_profiles[0].model_sha256.clone();
        let expected_cores = usize::try_from((duration_ms + 29999) / 30000).expect("core count");
        let source = session.join("selected-audio-two.mkv");
        let fixture = session.join("german.wav");
        std::fs::write(&fixture, profile::SELF_TEST_FIXTURE).expect("compiled publisher fixture");
        let mut command = tokio::process::Command::new(required_path("PLURX_FFMPEG"));
        command.args(format!("-v error -nostdin -threads 1 -f lavfi -i color=size=16x16:rate=1:duration={} -f lavfi -i anullsrc=r=16000:cl=mono -f lavfi -i sine=frequency=550:sample_rate=16000 -stream_loop -1 -i",duration_ms/1000).split_whitespace())
            .arg(&fixture).args(format!("-map 0:v -map 1:a -map 2:a -map 3:a -t {} -c:v ffv1 -threads 1 -c:a pcm_s16le -metadata:s:a:0 language=eng -metadata:s:a:1 language=fra -metadata:s:a:2 language=deu",duration_ms/1000).split_whitespace())
            .arg(&source).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
        let mut child = command.spawn().expect("finite fixture encoder");
        let status = match tokio::time::timeout(Duration::from_secs(60), child.wait()).await {
            Ok(result) => result.expect("fixture encoder exit"),
            Err(_) => {
                child.kill().await.expect("kill fixture encoder");
                panic!("finite fixture generation deadline");
            }
        };
        assert!(status.success(), "actual finite clip generation");
        let probe = plurx_core::scan::probe::probe_single_threaded(&source)
            .await
            .expect("actual source probe");
        assert_eq!(probe.audio_streams.len(), 3);
        assert_eq!(probe.audio_streams[2].index, 2);
        let metadata = std::fs::metadata(&source).expect("source identity");
        let store = state.store.as_ref();
        let library = store
            .create_library(&NewLibrary {
                name: "Private speech qualification".into(),
                kind: LibraryKind::Movies,
                paths: vec![session.clone()],
                anime: false,
            })
            .await
            .expect("real library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: format!("{}-second ordinal2 fixture", duration_ms / 1000),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("real catalogue item");
        let file_id = store
            .upsert_file(
                item,
                source.to_str().expect("source path"),
                i64::try_from(metadata.len()).expect("source size"),
                metadata.mtime(),
                &probe,
            )
            .await
            .expect("probed source registration");
        let file = store
            .get_file(file_id)
            .await
            .expect("source read")
            .expect("source");
        assert_eq!(file.duration_ms, Some(duration_ms));
        let source_sha = hex::encode(Sha256::digest(
            std::fs::read(&source).expect("private source bytes"),
        ));
        let admin = store
            .create_user("qualification-owner", "hash", true)
            .await
            .expect("actual admin row");
        let reader = store
            .create_user("qualification-reader", "hash", false)
            .await
            .expect("actual read-only user row");
        let st = State(state.clone());
        let read = || AuthUser(reader.clone());
        store
            .put_setting(
                "subtitles.ai.v1",
                crate::ai_subtitles::settings::TEST_ADMINS_ALL_ON,
            )
            .await
            .expect("AI subtitles on");
        let mut operations = Vec::new();
        for selected in selected_profiles {
            let settings = config::read(state.store.as_ref(), crate::state::clock_ms())
                .await
                .expect("normal configuration read");
            reply(api::update_settings(AdminUser(admin.clone()),State(state.clone()),Json(serde_json::from_value(json!({"version":1,"request_id":uuid::Uuid::new_v4().to_string(),
                "intent_expires_at_ms":crate::state::clock_ms()+300000,"expected_revision":settings.revision,"enabled":true,
                "model_id":selected.model_id,"profile_id":selected.id().expect("profile ID"),"preferred_spoken_language":"de"})).expect("typed CAS request"))).await.expect("normal settings CAS")).await;
            let started = std::time::Instant::now();
            let playback = uuid::Uuid::new_v4().to_string();
            let continuity = moving.then(|| uuid::Uuid::new_v4().to_string());
            let playback_started_at_ms = crate::state::clock_ms();
            let mut viewer=reply(api::create(AuthUser(admin.clone()),State(state.clone()),Path(file_id),Json(serde_json::from_value(json!({"version":1,"request_id":uuid::Uuid::new_v4().to_string(),
                "intent_expires_at_ms":crate::state::clock_ms()+300000,"source_size":file.size.to_string(),"source_mtime":file.mtime.to_string(),"audio_index":2,"spoken_language":"de",
                "output_language":selected.task.output_language("de"),"task":selected.task,"model_id":selected.model_id,"profile_id":selected.id().expect("profile ID"),
                "playback_id":playback,"continuity_id":continuity,"position_ms":0,"playback_rate":1.0,"paused":false})).expect("typed create"))).await.expect("real producer request")).await;
            assert_eq!(viewer["continuity_id"], json!(continuity));
            viewer["playback_id"] = playback.into();
            viewer["playback_started_at_ms"] = playback_started_at_ms.into();
            viewer["moving"] = moving.into();
            viewer["duration_ms"] = duration_ms.into();
            viewer["observations"] = json!([]);
            viewer["profile_id"] = selected.id().expect("profile").into();
            let operation_id = viewer["operation_id"]
                .as_str()
                .expect("operation ID")
                .to_owned();
            let mut manifest = json!(null);
            let mut first_ms = None;
            for _ in 0..expected_cores + 2 {
                viewer_pass(&state, &admin, &mut viewer, deadline, started).await;
                observe_current(&state, &admin, &mut viewer, started).await;
                let operation = reply(
                    api::operation(read(), st.clone(), Path(operation_id.clone()))
                        .await
                        .expect("typed operation read"),
                )
                .await;
                assert!(
                    operation["failed_ranges"]
                        .as_array()
                        .expect("typed range failures")
                        .is_empty(),
                    "actual progressive range failed before readback: {operation:?}"
                );
                let Some(asset) = operation["asset_id"].as_str() else {
                    continue;
                };
                let query = Query(
                    serde_json::from_value(json!({"position_ms":(duration_ms-90000).max(0)}))
                        .expect("manifest query"),
                );
                manifest = reply(
                    api::manifest(
                        read(),
                        st.clone(),
                        Path(asset.into()),
                        HeaderMap::new(),
                        query,
                    )
                    .await
                    .expect("typed manifest"),
                )
                .await;
                let count = manifest["segments"]
                    .as_array()
                    .expect("committed segment references")
                    .len();
                if count > 0 && first_ms.is_none() {
                    first_ms = Some(started.elapsed().as_millis());
                }
                if count == expected_cores {
                    break;
                }
            }
            assert_eq!(
                manifest["segments"].as_array().map(Vec::len),
                Some(expected_cores),
                "all real cores must commit: {manifest:?}"
            );
            assert_eq!(
                manifest["operation"]["state"], "complete",
                "all expected readable cores complete atomically"
            );
            assert_eq!(manifest["audio_index"], 2);
            assert_eq!(manifest["task"], selected.task.name());
            assert_eq!(manifest["profile_id"], selected.id().expect("profile"));
            assert_eq!(manifest["content_revision"], source_sha);
            assert_eq!(
                manifest["output_language"],
                selected.task.output_language("de")
            );
            let complete_ms = started.elapsed().as_millis();
            if moving {
                assert!(
                    complete_ms <= u128::try_from(duration_ms + 5000).expect("completion bound"),
                    "all cores must complete by EOF plus one observation cadence: {complete_ms}ms"
                );
                let mut observation = tokio::time::interval(Duration::from_secs(5));
                observation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                while started.elapsed() < Duration::from_secs(70) {
                    tokio::select! { _=observation.tick()=>viewer_tick(&state,&admin,&mut viewer,started).await.expect("continuing bounded viewer"),
                    ()=tokio::time::sleep_until(deadline)=>panic!("moving observation deadline") }
                }
                assert!(
                    viewer["first_useful_ms"]
                        .as_i64()
                        .is_some_and(|at| at < 30000),
                    "first currently useful coverage must arrive within first core: {viewer}"
                );
                for boundary in [30000, 60000] {
                    assert!(viewer["observations"].as_array().expect("observations").iter().filter(|o|
                        o["useful_current_core"]==true && o["position_ms"].as_i64().is_some_and(|p|p>boundary && p<boundary+30000)).count()>=2,
                        "must repeatedly read useful current-core canonical bytes after boundary {boundary}: {viewer}");
                }
            }
            let asset_id = manifest["asset_id"].as_str().expect("asset").to_owned();
            manifest = reply(
                api::manifest(
                    read(),
                    st.clone(),
                    Path(asset_id),
                    HeaderMap::new(),
                    Query(
                        serde_json::from_value(json!({"position_ms":(duration_ms-90000).max(0)}))
                            .expect("fresh final snapshot"),
                    ),
                )
                .await
                .expect("fresh canonical read lease"),
            )
            .await;
            let asset = manifest["asset_id"].as_str().expect("asset");
            let mut headers = HeaderMap::new();
            headers.insert(
                "x-translation-read-lease",
                HeaderValue::from_str(manifest["snapshot"]["lease_id"].as_str().expect("lease"))
                    .expect("lease header"),
            );
            let mut cues = Vec::new();
            let mut segments = Vec::new();
            for reference in manifest["segments"].as_array().expect("references") {
                let segment = reference["segment_id"]
                    .as_str()
                    .expect("segment")
                    .to_owned();
                let address = Path((asset.to_owned(), segment));
                let response = api::segment(read(), st.clone(), address, headers.clone())
                    .await
                    .expect("canonical immutable read");
                let bytes = axum::body::to_bytes(response.into_body(), 16384)
                    .await
                    .expect("bounded canonical segment");
                assert_eq!(
                    hex::encode(Sha256::digest(&bytes)),
                    reference["digest"].as_str().expect("digest")
                );
                let body: serde_json::Value =
                    serde_json::from_slice(&bytes).expect("canonical segment JSON");
                assert_eq!(body["provenance"]["engine_sha256"], engine.binary_sha256);
                let mut draft_json = body.clone();
                let object = draft_json.as_object_mut().expect("segment object");
                object.remove("version");
                object.remove("segment_id");
                for key in ["operation_epoch", "worker_fence"] {
                    let value = object["provenance"][key]
                        .as_str()
                        .expect("counter")
                        .parse::<i64>()
                        .expect("counter value");
                    object.get_mut("provenance").expect("provenance")[key] = value.into();
                }
                let draft: SegmentDraft =
                    serde_json::from_value(draft_json).expect("exact segment draft");
                let recipe = RangeRecipe {
                    asset_id: asset.into(),
                    operation_id: operation_id.clone(),
                    operation_epoch: viewer["operation_epoch"]
                        .as_str()
                        .expect("epoch")
                        .parse()
                        .expect("epoch value"),
                    source: SourceRevision {
                        file_id,
                        source_size: file.size,
                        source_mtime: file.mtime,
                    },
                    source_sha256: source_sha.clone(),
                    profile: selected.clone(),
                    audio_index: 2,
                    spoken_language: "de".into(),
                    duration_ms,
                    core_index: draft.core_index,
                };
                draft
                    .validate(&recipe)
                    .expect("source/profile/audio/core bounds/cue IDs validated");
                assert_eq!(
                    draft.canonical_body().expect("canonical wire"),
                    bytes.as_ref()
                );
                let owned = draft.cues;
                cues.extend(owned);
                segments.push(body);
            }
            assert!(
                if selected.task.is_translation() {
                    profile::english_self_test(&cues)
                } else {
                    profile::transcription_self_test(&cues)
                },
                "selected German audio must produce task-specific words"
            );
            // viewer_pass awaits each heartbeat reply before observing worker
            // completion. Release, like renewal, carries the next generation.
            let release_generation = viewer["demand_generation"]
                .as_str()
                .expect("last acknowledged generation")
                .parse::<i64>()
                .expect("canonical generation")
                .checked_add(1)
                .expect("next release generation");
            reply(api::release(AuthUser(admin.clone()),State(state.clone()),Path((operation_id.clone(),viewer["demand_id"].as_str().expect("demand").into())),Json(serde_json::from_value(json!({"version":1,"request_id":uuid::Uuid::new_v4().to_string(),
                "intent_expires_at_ms":crate::state::clock_ms()+300000,"expected_operation_epoch":viewer["operation_epoch"],"demand_generation":release_generation.to_string(),"playback_id":viewer["playback_id"]})).expect("typed release"))).await.expect("release owned viewer")).await;
            operations.push(json!({"operation_id":operation_id,"task":selected.task,"profile_id":selected.id().expect("profile"),"moving_playback":{"rate":1.0,"duration_ms":duration_ms,"max_heartbeat_position_ms":viewer["max_heartbeat_position_ms"],"continuity_id":viewer["continuity_id"]},"first_committed_ms":first_ms,"complete_ms":complete_ms,"processing_wall_per_source_second":complete_ms as f64/duration_ms as f64,"first_useful_ms":viewer["first_useful_ms"],"useful_coverage_observations":viewer["observations"],"segments":segments}));
        }
        let jobs = state
            .store
            .list_jobs(JobQuery {
                node_id: Some(state.node_id.clone()),
                state: None,
                kind: None,
                after_id: None,
                limit: 32,
            })
            .await
            .expect("bounded private job evidence");
        assert!(
            jobs.jobs.iter().all(|job| job.state == JobState::Succeeded),
            "all owned real jobs succeeded: {:?}",
            jobs.jobs
        );
        let jobs: Vec<_> = jobs
            .jobs
            .iter()
            .map(|job| json!({"job_id":job.id,"kind":job.supported_payload().expect("supported qualification job").kind(),"fence":job.fence,"state":job.state}))
            .collect();
        state.shutdown.cancel();
        let evidence = json!({"version":1,"scope":"local production progressive worker and leased canonical API reads; no real-time guarantee or video-player qualification",
            "duration_ms":duration_ms,"moving_playback":moving,"audio_ordinal":2,"source_index":3,"source_sha256":source_sha,"file_id":file_id.to_string(),"fresh_boot":state.subtitle_models.boot_id,"engine_sha256":engine.binary_sha256,
            "model_sha256":model_sha256,"qualifications":qualifications,"operations":operations,"jobs":jobs});
        let path = session.join(if moving {
            "moving-progressive-worker-qualification.json"
        } else {
            "progressive-worker-qualification.json"
        });
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&evidence).expect("evidence JSON"),
        )
        .expect("persistent evidence");
        eprintln!(
            "progressive worker qualification receipt: {}",
            path.display()
        );
    }
}
