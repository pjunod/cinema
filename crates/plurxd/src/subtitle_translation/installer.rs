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
    let selected = profile::selected_profile();
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
        || accepted != &selected
        || accepted.id()? != selected.id()?
        || model_id != &spec.id
        || model_sha256 != &spec.sha256
        || adapter_contract != &selected.adapter_contract
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
) -> Result<std::sync::Arc<FragmentAdmission>, StoreError> {
    interrupted(cancel)?;
    phase(fence, SubtitlePhase::WaitingForCapacity, cancel).await?;
    let admitted = state
        .transcode
        .admit_fragment_queued(cancel)
        .await
        .filter(|admission| state.transcode.fragment_worker_idle(admission))
        .ok_or_else(|| failure("waiting_for_playback_capacity"))?;
    interrupted(cancel)?;
    phase(fence, SubtitlePhase::Verifying, cancel).await?;
    Ok(std::sync::Arc::new(admitted))
}

async fn hash_admitted(
    state: &AppState,
    cancel: &CancellationToken,
    admission: &std::sync::Arc<FragmentAdmission>,
) -> Result<ManagedModel, StoreError> {
    interrupted(cancel)?;
    if !state.transcode.fragment_worker_idle(admission) {
        return Err(failure("waiting_for_playback_capacity"));
    }
    let spec = profile::model_spec();
    let hash = crate::subtitle_models::admitted_work::scope(
        Some(admission),
        artifacts::open_model_cancellable(&state.subtitle_models.root, &spec, cancel),
    );
    tokio::pin!(hash);
    tokio::select! {
        result = &mut hash => result.map_err(StoreError::Task),
        () = crate::subtitle_transcription::watch_capacity(&state.transcode, admission) => {
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
    let selected = profile::selected_profile();
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
    crate::subtitle_models::admitted_work::scope(Some(&admission), async {
        interrupted(&cancel)?;
        if !state.transcode.fragment_worker_idle(&admission) {
            return Err(failure("waiting_for_playback_capacity"));
        }
        crate::subtitle_transcription::managed_artifacts_unchanged(engine, &model)?;
        phase(fence, SubtitlePhase::Verifying, &cancel).await?;
        runtime::self_test_translation(
            state, engine, &model, &selected, fence, &admission, &cancel,
        )
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
    })
    .await
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
