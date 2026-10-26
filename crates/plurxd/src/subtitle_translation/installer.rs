//! Execute an already claimed translation-model operation. The manager owns
//! claim/Stop/shutdown cancellation, settlement and qualified publication; this
//! leaf returns evidence only after the real foreign-to-English self-test.
use super::{profile, runtime};
use crate::{
    background_jobs::JobFence,
    state::AppState,
    subtitle_models::artifacts::{self, Engine, ManagedModel},
    transcode::FragmentAdmission,
};
use plurx_core::{
    error::StoreError,
    store::{
        background_jobs::JobPayload,
        background_jobs_transcription::{
            ModelAction, SubtitleModelReceipt, SubtitlePhase, TranslationQualification,
        },
    },
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub(crate) const CATALOG_REVISION: &str = "plurx-managed-subtitle-models-v2";

pub(crate) struct Qualification {
    pub(crate) receipt: SubtitleModelReceipt,
    pub(crate) translation: TranslationQualification,
}

fn failure(code: &str) -> StoreError {
    StoreError::Task(code.into())
}

fn interrupted(cancel: &CancellationToken) -> Result<(), StoreError> {
    if cancel.is_cancelled() {
        Err(failure("worker_interrupted"))
    } else {
        Ok(())
    }
}

fn action(source: &JobPayload, node_id: &str) -> Result<ModelAction, StoreError> {
    source.validate()?;
    let spec = profile::model_spec();
    let JobPayload::SubtitleModelOperation {
        translation_profile: Some(accepted),
        action,
        model_id,
        catalog_revision,
        model_sha256,
        adapter_contract,
        target_node_id,
    } = source
    else {
        return Err(failure("translation_unverified"));
    };
    if !action.is_translation()
        || !profile::registered(accepted)
        || model_id != &spec.id
        || model_sha256 != &spec.sha256
        || adapter_contract != &accepted.adapter_contract
        || catalog_revision != CATALOG_REVISION
        || target_node_id != node_id
    {
        return Err(failure("translation_unverified"));
    }
    Ok(*action)
}

async fn phase(
    fence: &JobFence,
    value: SubtitlePhase,
    cancel: &CancellationToken,
) -> Result<(), StoreError> {
    interrupted(cancel)?;
    let current = tokio::select! {
        () = cancel.cancelled() => return Err(failure("worker_interrupted")),
        result = tokio::time::timeout(Duration::from_secs(2), fence.observe_subtitle(value)) => {
            result.map_err(|_| failure("worker_interrupted"))??
        }
    };
    if current {
        Ok(())
    } else {
        Err(failure("worker_interrupted"))
    }
}

async fn admit(
    state: &AppState,
    fence: &JobFence,
    cancel: &CancellationToken,
) -> Result<FragmentAdmission, StoreError> {
    interrupted(cancel)?;
    phase(fence, SubtitlePhase::WaitingForCapacity, cancel).await?;
    let admitted = state
        .transcode
        .admit_speech_queued(cancel)
        .await
        .filter(|admission| state.transcode.speech_capacity_available(admission))
        .ok_or_else(|| failure("waiting_for_playback_capacity"))?;
    interrupted(cancel)?;
    phase(fence, SubtitlePhase::Verifying, cancel).await?;
    Ok(admitted)
}

async fn hash_admitted(
    state: &AppState,
    cancel: &CancellationToken,
    admission: &FragmentAdmission,
) -> Result<ManagedModel, StoreError> {
    interrupted(cancel)?;
    if !state.transcode.speech_capacity_available(admission) {
        return Err(failure("waiting_for_playback_capacity"));
    }
    let spec = profile::model_spec();
    let hash = artifacts::open_model_cancellable(&state.subtitle_models.root, &spec, cancel);
    tokio::pin!(hash);
    tokio::select! {
        result = &mut hash => result.map_err(StoreError::Task),
        () = crate::subtitle_transcription::watch_speech_capacity(&state.transcode, admission) => {
            cancel.cancel();
            // This await is mandatory: the blocking hash checks its token at
            // each 128KiB chunk and must retire before admission is released.
            let _ = hash.await;
            Err(failure("waiting_for_playback_capacity"))
        }
    }
}

/// No media/CPU reservation is held during Install's network transfer. Verify
/// admits before hashing; both actions admit before the production self-test.
/// The caller must await this operation after cancelling manager_cancel and
/// immediately use the qualified JobFence publication seam on success.
pub(crate) async fn execute(
    state: &AppState,
    source: &JobPayload,
    engine: &Engine,
    fence: &JobFence,
    manager_cancel: &CancellationToken,
) -> Result<Qualification, StoreError> {
    let cancel = manager_cancel.child_token();
    interrupted(&cancel)?;
    let action = action(source, &state.node_id)?;
    let selected = match source {
        JobPayload::SubtitleModelOperation {
            translation_profile: Some(profile),
            ..
        } => profile.clone(),
        _ => return Err(failure("translation_unverified")),
    };
    let spec = profile::model_spec();
    if engine.adapter_contract != selected.adapter_contract {
        return Err(failure("packaged_engine_unqualified"));
    }
    let (model, admission) = match action {
        ModelAction::TranslationInstall => {
            phase(fence, SubtitlePhase::Downloading, &cancel).await?;
            let model = artifacts::install_model(&state.subtitle_models.root, &spec, &cancel)
                .await
                .map_err(StoreError::Task)?;
            interrupted(&cancel)?;
            let admission = admit(state, fence, &cancel).await?;
            (model, admission)
        }
        ModelAction::TranslationVerify => {
            let admission = admit(state, fence, &cancel).await?;
            phase(fence, SubtitlePhase::Verifying, &cancel).await?;
            let model = hash_admitted(state, &cancel, &admission).await?;
            (model, admission)
        }
        _ => return Err(failure("translation_unverified")),
    };
    interrupted(&cancel)?;
    if !state.transcode.speech_capacity_available(&admission) {
        return Err(failure("waiting_for_playback_capacity"));
    }
    crate::subtitle_transcription::managed_artifacts_unchanged(engine, &model)?;
    phase(fence, SubtitlePhase::Verifying, &cancel).await?;
    runtime::self_test_translation(state, engine, &model, &selected, fence, &admission, &cancel)
        .await?;
    interrupted(&cancel)?;
    crate::subtitle_transcription::managed_artifacts_unchanged(engine, &model)?;
    let named_model = artifacts::model_identity(&state.subtitle_models.root, &spec)
        .await
        .map_err(StoreError::Task)?;
    if named_model != model.identity || model.sha256 != spec.sha256 {
        return Err(failure("artifact_digest_mismatch"));
    }
    // Re-inspect the current trusted package, including its companions. A held
    // executable from a replaced bundle cannot qualify the new named package.
    // Await the bounded engine verifier rather than timing out/detaching it.
    let named_engine = artifacts::inspect_engine()
        .await
        .map_err(StoreError::Task)?;
    if named_engine.identity != engine.identity
        || named_engine.binary_sha256 != engine.binary_sha256
        || named_engine.adapter_contract != engine.adapter_contract
        || named_engine.platform != engine.platform
    {
        return Err(failure("packaged_engine_changed"));
    }
    crate::subtitle_transcription::managed_artifacts_unchanged(engine, &model)?;
    interrupted(&cancel)?;
    phase(fence, SubtitlePhase::Validating, &cancel).await?;
    Ok(Qualification {
        receipt: SubtitleModelReceipt {
            // JobFence binds node/boot/job/fence/time atomically at publication.
            node_id: String::new(),
            boot_id: String::new(),
            model_id: spec.id,
            model_sha256: spec.sha256,
            adapter_contract: engine.adapter_contract.clone(),
            engine_sha256: engine.binary_sha256.clone(),
            model_file_identity: artifacts::identity_key(model.identity),
            engine_file_identity: artifacts::identity_key(engine.identity),
            job_id: String::new(),
            fence: 0,
            verified_at_ms: 0,
        },
        translation: TranslationQualification {
            profile: selected,
            self_test_sha256: profile::SELF_TEST_SHA256.into(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(action: ModelAction) -> JobPayload {
        let selected = profile::selected_profile();
        JobPayload::SubtitleModelOperation {
            translation_profile: Some(selected.clone()),
            action,
            model_id: selected.model_id,
            catalog_revision: CATALOG_REVISION.into(),
            model_sha256: selected.model_sha256,
            adapter_contract: selected.adapter_contract,
            target_node_id: "translation-worker".into(),
        }
    }

    #[test]
    fn translation_install_and_verify_require_the_exact_server_owned_recipe() {
        for expected in [
            ModelAction::TranslationInstall,
            ModelAction::TranslationVerify,
        ] {
            let source = payload(expected);
            assert_eq!(
                action(&source, "translation-worker").expect("test setup"),
                expected
            );
            assert!(action(&source, "another-worker").is_err());
            let mut changed = source.clone();
            if let JobPayload::SubtitleModelOperation {
                catalog_revision, ..
            } = &mut changed
            {
                *catalog_revision = "old-catalog".into();
            }
            assert!(action(&changed, "translation-worker").is_err());
            let mut changed = source.clone();
            if let JobPayload::SubtitleModelOperation {
                translation_profile: Some(profile),
                ..
            } = &mut changed
            {
                profile.left_context_ms = 2_000;
            }
            assert!(action(&changed, "translation-worker").is_err());
            let mut changed = source;
            if let JobPayload::SubtitleModelOperation { model_sha256, .. } = &mut changed {
                *model_sha256 = "0".repeat(64);
            }
            assert!(action(&changed, "translation-worker").is_err());
        }
    }

    #[test]
    fn generation_model_action_cannot_qualify_translation_or_ignore_cancellation() {
        let mut source = payload(ModelAction::TranslationVerify);
        if let JobPayload::SubtitleModelOperation {
            action,
            translation_profile,
            ..
        } = &mut source
        {
            *action = ModelAction::Verify;
            *translation_profile = None;
        }
        assert!(source.validate().is_ok());
        assert!(action(&source, "translation-worker").is_err());
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            matches!(interrupted(&cancel),Err(StoreError::Task(code)) if code=="worker_interrupted")
        );
    }
}

/// Explicit local runtime qualification; excluded from every ordinary test run.
/// The production model-operation helper is also the HTTP setup handler's seam.
#[cfg(all(test, unix))]
mod qualification {
    use super::*;
    use crate::{
        http::extract::{AdminUser, AuthUser},
        subtitle_translation::{api, config},
    };
    use axum::{
        extract::{Path, Query, State},
        http::{HeaderMap, HeaderValue},
        Json,
    };
    use plurx_core::store::{
        background_jobs::{JobPayload, JobState},
        keys,
        subtitle_translation_read::{TranslationQuery, TranslationRead},
        BackgroundJobStore, SqliteStore, Store,
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
    use std::{os::unix::fs::DirBuilderExt, path::PathBuf, sync::Arc};

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
        let spec = profile::model_spec();
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
        assert_eq!(queued.state, JobState::Queued);
        assert!(
            matches!(queued.supported_payload().expect("typed model payload"),
            JobPayload::SubtitleModelOperation { translation_profile:Some(ref accepted), action:accepted_action, .. }
                if accepted == selected && accepted_action == action)
        );
        let cancel = CancellationToken::new();
        let worker = crate::subtitle_translation::manager::worker_pass(state, &cancel);
        tokio::pin!(worker);
        let progressed = tokio::select! {
            result = &mut worker => result.expect("real serial setup worker"),
            () = tokio::time::sleep_until(deadline) => {
                cancel.cancel();
                let result = worker.await;
                panic!("bounded qualification deadline reached; cancelled worker fully awaited: {result:?}");
            }
        };
        assert!(
            progressed,
            "the setup job must run in the real serial worker"
        );
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
            "actual published receipt must qualify its own task profile: {workers:?}"
        );
        json!({"job_id":job_id,"task":selected.task,"profile_id":profile_id,
            "action":action,"state":"succeeded","fence":completed.fence})
    }

    /// Downloads/reuses the pinned small model and exercises real decoding,
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
        for selected in profile::profiles() {
            for action in [
                ModelAction::TranslationInstall,
                ModelAction::TranslationVerify,
            ] {
                evidence.push(qualify(&state, &selected, action, deadline).await);
            }
        }
        let reopened =
            SqliteStore::open(&database).expect("reopen committed qualification database");
        let receipts = reopened
            .read_subtitle_translation(TranslationRead {
                user_id: 0,
                now_ms: crate::state::clock_ms(),
                query: TranslationQuery::Capabilities {
                    node_id: state.node_id.clone(),
                },
            })
            .await
            .expect("durable qualified capabilities");
        for selected in profile::profiles() {
            let id = selected.id().expect("profile ID");
            assert!(
                receipts.iter().any(|receipt| receipt["profile_id"] == id
                    && receipt["model"]["model_sha256"] == spec.sha256
                    && receipt["model"]["engine_sha256"] == engine.binary_sha256
                    && receipt["model"]["boot_id"] == state.subtitle_models.boot_id
                    && receipt["self_test_sha256"] == profile::SELF_TEST_SHA256),
                "both exact task receipts must survive reopening private SQLite"
            );
        }
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

    async fn viewer_pass(
        state: &AppState,
        admin: &plurx_core::domain::User,
        viewer: &mut serde_json::Value,
        deadline: tokio::time::Instant,
    ) {
        let cancel = CancellationToken::new();
        let worker = crate::subtitle_translation::manager::worker_pass(state, &cancel);
        tokio::pin!(worker);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                result = &mut worker => { assert!(result.expect("real progressive worker"), "queued/demanded work"); break; }
                () = tokio::time::sleep_until(deadline) => {
                    cancel.cancel(); let result = worker.await;
                    panic!("qualification deadline; actual worker cancelled and awaited: {result:?}");
                }
                _ = heartbeat.tick() => {
                    let generation = viewer["demand_generation"].as_str().expect("generation").parse::<u64>().expect("counter") + 1;
                    let request = serde_json::from_value(json!({"version":1,"request_id":uuid::Uuid::new_v4().to_string(),
                        "intent_expires_at_ms":crate::state::clock_ms()+300000,"expected_operation_epoch":viewer["operation_epoch"],
                        "demand_generation":generation.to_string(),"playback_id":viewer["playback_id"],"position_ms":0,"playback_rate":1.0,"paused":false})).expect("typed heartbeat");
                    let result = crate::subtitle_translation::api::demand(AdminUser(admin.clone()),State(state.clone()),
                        Path((viewer["operation_id"].as_str().expect("operation").into(),viewer["demand_id"].as_str().expect("demand").into())),Json(request)).await;
                    let response = match result { Ok(response)=>response, Err(error)=>{cancel.cancel();let result=worker.await;panic!("heartbeat failed: {error:?}; worker awaited: {result:?}");} };
                    let receipt=reply(response).await;
                    assert_eq!(receipt["demand_generation"],generation.to_string());
                    viewer["demand_generation"]=receipt["demand_generation"].clone();
                }
            }
        }
    }

    /// Finite private source -> selected third audio -> real progressive jobs ->
    /// canonical leased readback. Timing is observed, without a real-time claim.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "real progressive inference; explicit speech qualification root/cache/engine/ffmpeg/ffprobe/TMPDIR required"]
    async fn pinned_progressive_worker_commits_both_tasks_for_selected_audio_two() {
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
        for selected in profile::profiles() {
            qualifications
                .push(qualify(&state, &selected, ModelAction::TranslationVerify, deadline).await);
        }
        let source = session.join("selected-audio-two.mkv");
        let fixture = session.join("german.wav");
        std::fs::write(&fixture, profile::SELF_TEST_FIXTURE).expect("compiled publisher fixture");
        let mut command = tokio::process::Command::new(required_path("PLURX_FFMPEG"));
        command.args("-v error -nostdin -threads 1 -f lavfi -i color=size=16x16:rate=1:duration=35 -f lavfi -i anullsrc=r=16000:cl=mono -f lavfi -i sine=frequency=550:sample_rate=16000 -stream_loop -1 -i".split_whitespace())
            .arg(&fixture).args("-map 0:v -map 1:a -map 2:a -map 3:a -t 35 -c:v ffv1 -threads 1 -c:a pcm_s16le -metadata:s:a:0 language=eng -metadata:s:a:1 language=fra -metadata:s:a:2 language=deu".split_whitespace())
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
                title: "35-second ordinal2 fixture".into(),
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
        assert_eq!(file.duration_ms, Some(35000));
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
        let mut operations = Vec::new();
        for selected in profile::profiles() {
            let settings = config::read(state.store.as_ref(), crate::state::clock_ms())
                .await
                .expect("normal configuration read");
            reply(api::update_settings(AdminUser(admin.clone()),State(state.clone()),Json(serde_json::from_value(json!({"version":1,"request_id":uuid::Uuid::new_v4().to_string(),
                "intent_expires_at_ms":crate::state::clock_ms()+300000,"expected_revision":settings.revision,"enabled":true,
                "model_id":selected.model_id,"profile_id":selected.id().expect("profile ID"),"preferred_spoken_language":"de"})).expect("typed CAS request"))).await.expect("normal settings CAS")).await;
            let started = std::time::Instant::now();
            let playback = uuid::Uuid::new_v4().to_string();
            let mut viewer=reply(api::create(AdminUser(admin.clone()),State(state.clone()),Path(file_id),Json(serde_json::from_value(json!({"version":1,"request_id":uuid::Uuid::new_v4().to_string(),
                "intent_expires_at_ms":crate::state::clock_ms()+300000,"source_size":file.size.to_string(),"source_mtime":file.mtime.to_string(),"audio_index":2,"spoken_language":"de",
                "output_language":selected.task.output_language("de"),"task":selected.task,"model_id":selected.model_id,"profile_id":selected.id().expect("profile ID"),
                "playback_id":playback,"position_ms":0,"playback_rate":1.0,"paused":false})).expect("typed create"))).await.expect("real producer request")).await;
            viewer["playback_id"] = playback.into();
            let operation_id = viewer["operation_id"]
                .as_str()
                .expect("operation ID")
                .to_owned();
            let mut manifest = json!(null);
            let mut first_ms = None;
            for _ in 0..4 {
                viewer_pass(&state, &admin, &mut viewer, deadline).await;
                let operation = reply(
                    api::operation(read(), st.clone(), Path(operation_id.clone()))
                        .await
                        .expect("typed operation read"),
                )
                .await;
                let Some(asset) = operation["asset_id"].as_str() else {
                    continue;
                };
                let query = Query(
                    serde_json::from_value(json!({"position_ms":0})).expect("manifest query"),
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
                if count == 2 {
                    break;
                }
            }
            assert_eq!(
                manifest["segments"].as_array().map(Vec::len),
                Some(2),
                "both real cores must commit: {manifest:?}"
            );
            assert_eq!(manifest["audio_index"], 2);
            assert_eq!(manifest["task"], selected.task.name());
            assert_eq!(manifest["profile_id"], selected.id().expect("profile"));
            assert_eq!(manifest["content_revision"], source_sha);
            assert_eq!(
                manifest["output_language"],
                selected.task.output_language("de")
            );
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
                    duration_ms: 35000,
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
            reply(api::release(AdminUser(admin.clone()),State(state.clone()),Path((operation_id.clone(),viewer["demand_id"].as_str().expect("demand").into())),Json(serde_json::from_value(json!({"version":1,"request_id":uuid::Uuid::new_v4().to_string(),
                "intent_expires_at_ms":crate::state::clock_ms()+300000,"expected_operation_epoch":viewer["operation_epoch"],"demand_generation":viewer["demand_generation"],"playback_id":viewer["playback_id"]})).expect("typed release"))).await.expect("release owned viewer")).await;
            operations.push(json!({"operation_id":operation_id,"task":selected.task,"profile_id":selected.id().expect("profile"),"first_committed_ms":first_ms,"complete_ms":started.elapsed().as_millis(),"segments":segments}));
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
            "duration_ms":35000,"audio_ordinal":2,"source_index":3,"source_sha256":source_sha,"file_id":file_id.to_string(),"fresh_boot":state.subtitle_models.boot_id,"engine_sha256":engine.binary_sha256,
            "model_sha256":profile::MODEL_SHA256,"qualifications":qualifications,"operations":operations,"jobs":jobs});
        let path = session.join("progressive-worker-qualification.json");
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
