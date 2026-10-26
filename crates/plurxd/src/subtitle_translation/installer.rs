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
    use plurx_core::store::{
        background_jobs::{JobPayload, JobState},
        keys,
        subtitle_translation_read::{TranslationQuery, TranslationRead},
        BackgroundJobStore, SqliteStore, Store,
    };
    use serde_json::json;
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

    /// Downloads/reuses the pinned small model and exercises real decoding,
    /// Whisper inference, task-specific cue checks and durable receipt publish.
    /// It proves the compiled German fixture, not arbitrary media or languages.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "real pinned model download/inference; requires PLURX_SPEECH_QUALIFICATION_ROOT, PLURX_SUBTITLE_ENGINE, PLURX_FFMPEG and owned TMPDIR"]
    async fn pinned_whisper_install_and_verify_qualify_both_speech_tasks() {
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
        let engine = artifacts::inspect_engine()
            .await
            .expect("verify real packaged engine and companions");
        let spec = profile::model_spec();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20 * 60);
        let mut evidence = Vec::new();
        for selected in profile::profiles() {
            let profile_id = selected.id().expect("registered profile ID");
            for action in [
                ModelAction::TranslationInstall,
                ModelAction::TranslationVerify,
            ] {
                let request = json!({
                    "version":2, "request_id":uuid::Uuid::new_v4().to_string(),
                    "intent_expires_at_ms":crate::state::clock_ms()+300000,
                    "purpose":"translation", "profile_id":profile_id,
                    "node_ids":[state.node_id],
                });
                let axum::Json(reply) =
                    crate::subtitle_translation::model_operation(&state, &spec.id, action, request)
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
                        if accepted == &selected && accepted_action == action)
                );
                let cancel = CancellationToken::new();
                let worker = crate::subtitle_translation::manager::worker_pass(&state, &cancel);
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
                assert_eq!(completed.state, JobState::Succeeded,
                    "real task {:?} action {action:?}: error={:?}, checkpoint={:?}, attempt_errors={}",
                    selected.task, completed.last_error_code, completed.checkpoint, completed.attempt_errors);
                assert!(completed.fence > 0, "actual durable claim required");
                assert_eq!(
                    state.transcode.admissions().software_in_use(),
                    0,
                    "production resource guards released after publish"
                );
                let workers = crate::subtitle_translation::workers_public(&state)
                    .await
                    .expect("live physical readiness inspection");
                assert!(
                    workers
                        .iter()
                        .any(
                            |worker| worker["capabilities"].as_array().is_some_and(|caps| caps
                                .iter()
                                .any(|cap| cap["profile_id"] == profile_id
                                    && cap["status"] == "verified"))
                        ),
                    "actual published receipt must qualify its own task profile: {workers:?}"
                );
                evidence.push(
                    json!({"job_id":job_id,"task":selected.task,"profile_id":profile_id,
                    "action":action,"state":"succeeded","fence":completed.fence}),
                );
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
}
