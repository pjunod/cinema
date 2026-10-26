//! One serial text-model/caption pass on the existing subtitle worker loop.
use super::{artifacts, config, engine, text, ADAPTER_CONTRACT, MODEL_ID};
use crate::{
    background_jobs::{ActiveBackgroundJob, JobFence},
    state::AppState,
    transcode::FragmentAdmission,
};
use plurx_core::{
    domain::{DownloadedSubtitle, MediaFile, SubtitleTextTranslation},
    error::StoreError,
    fs_secure::std_file_identity,
    store::{
        background_jobs::{CandidateQuery, ClaimJob, JobKind, JobPayload, JobSettlement},
        background_jobs_transcription::{
            artifact_key, ModelAction, SubtitleModelReceipt, SubtitlePhase,
        },
    },
};
use sha2::{Digest, Sha256};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
fn failure(reason: &str) -> StoreError {
    StoreError::Task(reason.into())
}
fn interrupted(cancel: &CancellationToken) -> Result<(), StoreError> {
    if cancel.is_cancelled() {
        Err(failure("worker_interrupted"))
    } else {
        Ok(())
    }
}
async fn phase(
    fence: &JobFence,
    phase: SubtitlePhase,
    done: usize,
    total: usize,
) -> Result<(), StoreError> {
    if !fence
        .observe_text_progress(
            phase,
            u32::try_from(done).map_err(|_| failure("output_invalid"))?,
            u32::try_from(total).map_err(|_| failure("output_invalid"))?,
        )
        .await?
    {
        return Err(failure("worker_interrupted"));
    }
    Ok(())
}
async fn infer(
    state: &AppState,
    bundle: Arc<artifacts::VerifiedBundle>,
    cues: Vec<text::TextCue>,
    fence: &JobFence,
    admission: Arc<FragmentAdmission>,
    cancel: &CancellationToken,
    (offset, overall): (usize, usize),
) -> Result<Vec<text::TextCue>, StoreError> {
    let total = cues.len();
    phase(fence, SubtitlePhase::Translating, offset, overall).await?;
    let progress = Arc::new(AtomicUsize::new(0));
    let keepalive: Arc<dyn Send + Sync> = admission.clone();
    let inference =
        engine::translate_with_progress(bundle, cues, cancel.clone(), keepalive, progress.clone());
    tokio::pin!(inference);
    let capacity =
        crate::subtitle_transcription::watch_speech_capacity(&state.transcode, &admission);
    tokio::pin!(capacity);
    let mut prior = 0;
    loop {
        tokio::select! {
            result=&mut inference=>{let output=result.map_err(|why|failure(safe_reason(&why)))?;phase(fence,SubtitlePhase::Validating,offset+total,overall).await?;return Ok(output);},
            ()=&mut capacity=>{cancel.cancel();let _=inference.await;return Err(failure("waiting_for_playback_capacity"));},
            ()=tokio::time::sleep(Duration::from_millis(500))=>{let done=progress.load(Ordering::Acquire).min(total);if done!=prior{if let Err(error)=phase(fence,SubtitlePhase::Translating,offset+done,overall).await { cancel.cancel(); let _=inference.await; return Err(error); }prior=done;}}
        }
    }
}
async fn current_source(
    state: &AppState,
    file: &MediaFile,
    held: &crate::fragment_index_cluster::SourceFence,
) -> Result<(), StoreError> {
    if !held.unchanged() {
        return Err(failure("source_changed"));
    }
    let current = state
        .store
        .get_file(file.id)
        .await?
        .filter(|current| current.size == file.size && current.mtime == file.mtime)
        .ok_or_else(|| failure("source_changed"))?;
    let named = crate::fragment_index_cluster::open_source_playback_fence(
        &current,
        Some(held.object_version()),
    )
    .await
    .map_err(|_| failure("source_changed"))?;
    if std_file_identity(&named.handle).ok() != std_file_identity(&held.handle).ok()
        || !named.unchanged()
    {
        return Err(failure("source_changed"));
    }
    Ok(())
}
async fn execute(
    state: &AppState,
    source: &JobPayload,
    fence: &JobFence,
    admission: Option<Arc<FragmentAdmission>>,
    cancel: &CancellationToken,
) -> Result<bool, StoreError> {
    interrupted(cancel)?;
    match source {
        JobPayload::SubtitleTextModelOperation {
            action,
            model_sha256,
            target_boot_id,
            ..
        } => {
            if target_boot_id != &state.subtitle_models.boot_id {
                return Err(failure("worker_changed"));
            }
            phase(fence, SubtitlePhase::Downloading, 0, 0).await?;
            let bundle = match action {
                ModelAction::Install => {
                    artifacts::install(&state.subtitle_models.root, cancel).await
                }
                ModelAction::Verify => {
                    artifacts::inspect(&state.subtitle_models.root, cancel).await
                }
                _ => return Err(failure("model_unverified")),
            }
            .map_err(|why| failure(safe_reason(&why)))?;
            if bundle.bundle_digest != *model_sha256 {
                return Err(failure("model_changed"));
            }
            phase(fence, SubtitlePhase::WaitingForCapacity, 0, 0).await?;
            let admission = Arc::new(
                state
                    .transcode
                    .admit_speech_queued(cancel)
                    .await
                    .ok_or_else(|| failure("waiting_for_playback_capacity"))?,
            );
            let bundle = Arc::new(bundle);
            let before = super::engine_identity()
                .await
                .map_err(|why| failure(safe_reason(&why)))?;
            phase(fence, SubtitlePhase::Verifying, 0, 3).await?;
            let keepalive: Arc<dyn Send + Sync> = admission.clone();
            let verification = engine::self_test(bundle.clone(), cancel.clone(), keepalive);
            tokio::pin!(verification);
            tokio::select! {
                result=&mut verification=>result.map_err(|why|failure(safe_reason(&why)))?,
                ()=crate::subtitle_transcription::watch_speech_capacity(&state.transcode,&admission)=>{cancel.cancel();let _=verification.await;return Err(failure("waiting_for_playback_capacity"));}
            }
            phase(fence, SubtitlePhase::Validating, 3, 3).await?;
            interrupted(cancel)?;
            let named = artifacts::inspect(&state.subtitle_models.root, cancel)
                .await
                .map_err(|why| failure(safe_reason(&why)))?;
            if named.identity() != bundle.identity()
                || super::engine_identity()
                    .await
                    .map_err(|why| failure(safe_reason(&why)))?
                    != before
            {
                return Err(failure("model_changed"));
            }
            fence
                .publish_subtitle_model(
                    source.clone(),
                    SubtitleModelReceipt {
                        node_id: String::new(),
                        boot_id: String::new(),
                        model_id: MODEL_ID.into(),
                        model_sha256: bundle.bundle_digest.clone(),
                        adapter_contract: ADAPTER_CONTRACT.into(),
                        engine_sha256: before.0,
                        model_file_identity: bundle.identity(),
                        engine_file_identity: before.1,
                        job_id: String::new(),
                        fence: 0,
                        verified_at_ms: 0,
                    },
                )
                .await
        }
        JobPayload::SubtitleTextTranslate {
            file_id,
            source_size,
            source_mtime,
            subtitle_index,
            input_sha256,
            source_language,
            target_language,
            model_sha256,
            engine_sha256,
            ..
        } => {
            let admission = admission.ok_or_else(|| failure("waiting_for_playback_capacity"))?;
            if !config::read(state)
                .await
                .map_err(|_| failure("configuration_conflict"))?
                .enabled
            {
                return Err(failure("disabled"));
            }
            if !super::workers(state)
                .await
                .map_err(|_| failure("model_unverified"))?
                .iter()
                .any(|worker| worker["status"] == "verified")
            {
                return Err(failure("model_unverified"));
            }
            let file = state
                .store
                .get_file(*file_id)
                .await?
                .filter(|file| file.size == *source_size && file.mtime == *source_mtime)
                .ok_or_else(|| failure("source_changed"))?;
            phase(fence, SubtitlePhase::CheckingSource, 0, 0).await?;
            let held = crate::fragment_index_cluster::open_source_playback_fence(&file, None)
                .await
                .map_err(|_| failure("source_changed"))?;
            let input = super::input_bytes(state, &file, *subtitle_index)
                .await
                .map_err(|_| failure("subtitle_unavailable"))?;
            if hex::encode(Sha256::digest(&input)) != *input_sha256 {
                return Err(failure("subtitle_changed"));
            }
            let cues = text::parse(&input).map_err(|_| failure("subtitle_invalid"))?;
            current_source(state, &file, &held).await?;
            let bundle = Arc::new(
                artifacts::inspect(&state.subtitle_models.root, cancel)
                    .await
                    .map_err(|why| failure(safe_reason(&why)))?,
            );
            if bundle.bundle_digest != *model_sha256 {
                return Err(failure("model_changed"));
            }
            let bundle_identity = bundle.identity();
            let engine = super::engine_identity()
                .await
                .map_err(|why| failure(safe_reason(&why)))?;
            if engine.0 != *engine_sha256 {
                return Err(failure("engine_changed"));
            }
            let mut translated = std::collections::BTreeMap::new();
            for saved in state
                .store
                .read_text_translation_cues(source.clone())
                .await?
            {
                let original = cues
                    .get(saved.cue_index as usize)
                    .ok_or_else(|| failure("subtitle_changed"))?;
                let input_digest = plurx_core::store::subtitle_translation::digest(original)?;
                if saved.input_sha256 != input_digest
                    || saved.id != original.id
                    || saved.start_ms != original.start_ms
                    || saved.end_ms != original.end_ms
                {
                    return Err(failure("subtitle_changed"));
                }
                translated.insert(
                    saved.cue_index as usize,
                    text::TextCue {
                        id: saved.id,
                        start_ms: saved.start_ms,
                        end_ms: saved.end_ms,
                        text: saved.text,
                    },
                );
            }
            let missing = (0..cues.len())
                .filter(|index| !translated.contains_key(index))
                .take(8)
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                let chunk = missing
                    .iter()
                    .map(|index| cues[*index].clone())
                    .collect::<Vec<_>>();
                let output = infer(
                    state,
                    bundle,
                    chunk.clone(),
                    fence,
                    admission.clone(),
                    cancel,
                    (translated.len(), cues.len()),
                )
                .await?;
                if output.len() != chunk.len()
                    || output.iter().zip(&chunk).any(|(translated, original)| {
                        translated.id != original.id
                            || translated.start_ms != original.start_ms
                            || translated.end_ms != original.end_ms
                    })
                {
                    return Err(failure("output_invalid"));
                }
                let drafts = output
                    .iter()
                    .zip(&missing)
                    .map(|(cue, index)| {
                        Ok(
                            plurx_core::store::subtitle_text_translation::TextTranslationCue {
                                cue_index: *index as u32,
                                input_sha256: plurx_core::store::subtitle_translation::digest(
                                    &cues[*index],
                                )?,
                                id: cue.id.clone(),
                                start_ms: cue.start_ms,
                                end_ms: cue.end_ms,
                                text: cue.text.clone(),
                            },
                        )
                    })
                    .collect::<Result<Vec<_>, StoreError>>()?;
                current_source(state, &file, &held).await?;
                interrupted(cancel)?;
                if !fence.publish_text_slice(source.clone(), drafts).await? {
                    return Err(failure("slice_publication_refused"));
                }
                for (cue, index) in output.into_iter().zip(missing) {
                    translated.insert(index, cue);
                }
            }
            if translated.len() < cues.len() {
                let checkpoint = serde_json::json!({"recipe_digest":artifact_key(source)?,"completed_cues":translated.len(),"total_cues":cues.len(),"phase":"translating","fence":fence.snapshot().await.ok_or_else(||failure("worker_interrupted"))?.fence});
                if !fence
                    .settle(JobSettlement::Yield {
                        error_code: None,
                        checkpoint: Some(checkpoint),
                        not_before_ms: crate::state::clock_ms() + 1,
                    })
                    .await?
                {
                    return Err(failure("worker_interrupted"));
                }
                return Ok(true);
            }
            let output = translated.into_values().collect::<Vec<_>>();
            let bytes = text::render(&output).map_err(|_| failure("output_invalid"))?;
            let vtt = String::from_utf8(bytes).map_err(|_| failure("output_invalid"))?;
            if vtt.len() > plurx_core::store::MAX_DOWNLOADED_SUBTITLE_BYTES {
                return Err(failure("subtitle_too_large"));
            }
            current_source(state, &file, &held).await?;
            let current_input = super::input_bytes(state, &file, *subtitle_index)
                .await
                .map_err(|_| failure("subtitle_unavailable"))?;
            if hex::encode(Sha256::digest(current_input)) != *input_sha256 {
                return Err(failure("subtitle_changed"));
            }
            if artifacts::inspect(&state.subtitle_models.root, cancel)
                .await
                .map_err(|why| failure(safe_reason(&why)))?
                .identity()
                != bundle_identity
                || super::engine_identity()
                    .await
                    .map_err(|why| failure(safe_reason(&why)))?
                    != engine
            {
                return Err(failure("model_changed"));
            }
            interrupted(cancel)?;
            phase(fence, SubtitlePhase::Publishing, output.len(), output.len()).await?;
            let key = artifact_key(source)?;
            let caption = DownloadedSubtitle {
                source_size: *source_size,
                source_mtime: *source_mtime,
                provider_file_id: 0,
                transcription: None,
                text_translation: Some(SubtitleTextTranslation {
                    artifact_key: key,
                    subtitle_index: *subtitle_index,
                    input_sha256: input_sha256.clone(),
                    source_language: source_language.clone(),
                    target_language: target_language.clone(),
                    model_id: MODEL_ID.into(),
                    model_sha256: model_sha256.clone(),
                    adapter_contract: ADAPTER_CONTRACT.into(),
                    engine_sha256: engine.0,
                    generated_at_ms: crate::state::clock_ms(),
                }),
                language: target_language.clone(),
                title: format!("English · translated from {source_language}"),
                hearing_impaired: false,
                forced: false,
                vtt,
            };
            // The admission remains alive until fenced publication completes.
            let published = fence.publish_transcription(source.clone(), caption).await;
            drop(admission);
            published
        }
        _ => Ok(false),
    }
}
pub(crate) async fn worker_pass(
    state: &AppState,
    shutdown: &CancellationToken,
) -> Result<bool, StoreError> {
    let authority = state.jobs.execution_authority();
    let candidates = state
        .store
        .job_candidates(CandidateQuery {
            node_id: state.node_id.clone(),
            kinds: vec![
                JobKind::SubtitleTextModelOperation,
                JobKind::SubtitleTextTranslate,
            ],
            after: None,
            now_ms: crate::state::clock_ms(),
            limit: 16,
        })
        .await?;
    for candidate in candidates.jobs {
        let source = candidate.supported_payload()?;
        if shutdown.is_cancelled() || !authority.may_execute_job(source.kind()).await {
            continue;
        }
        let admission = if matches!(source, JobPayload::SubtitleTextTranslate { .. }) {
            match state.transcode.admit_speech().await {
                Some(admission) => Some(Arc::new(admission)),
                None => continue,
            }
        } else {
            None
        };
        let now = crate::state::clock_ms();
        let Some((job, deadline)) = crate::background_jobs::claim_with_resolution(
            state.store.as_ref(),
            &candidate,
            ClaimJob {
                job_id: candidate.id.clone(),
                expected_revision: candidate.revision,
                node_id: state.node_id.clone(),
                boot_id: state.subtitle_models.boot_id.clone(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: source.kind(),
                payload_version: 1,
                now_ms: now,
                dispatched_at_ms: now,
            },
        )
        .await?
        else {
            continue;
        };
        let active = ActiveBackgroundJob::start(
            state.store.clone(),
            authority.clone(),
            job.token
                .clone()
                .ok_or_else(|| failure("worker_interrupted"))?,
            deadline,
            source.kind(),
        )?;
        let fence = active.fence();
        let cancel = fence.loss_token().child_token();
        let operation = plurx_core::process::bounded::cancellable(
            cancel.clone(),
            execute(state, &source, &fence, admission, &cancel),
        );
        tokio::pin!(operation);
        let result = tokio::select! {result=&mut operation=>result,()=shutdown.cancelled()=>{cancel.cancel();operation.await}};
        match result {
            Ok(true) => {}
            Ok(false) => {
                let _ = fence
                    .settle(JobSettlement::Stop {
                        error_code: "publication_refused".into(),
                    })
                    .await;
            }
            Err(error) => {
                let reason = safe_reason(&error.to_string());
                let settlement =
                    if cancel.is_cancelled() || reason == "waiting_for_playback_capacity" {
                        JobSettlement::Yield {
                            error_code: Some(reason.into()),
                            checkpoint: None,
                            not_before_ms: crate::state::clock_ms() + 1000,
                        }
                    } else {
                        JobSettlement::Fail {
                            error_code: reason.into(),
                        }
                    };
                let _ = fence.settle(settlement).await;
            }
        }
        active.finish().await;
        return Ok(true);
    }
    Ok(false)
}
pub(crate) fn safe_reason(message: &str) -> &'static str {
    for reason in [
        "waiting_for_playback_capacity",
        "worker_interrupted",
        "configuration_conflict",
        "disabled",
        "worker_changed",
        "source_changed",
        "subtitle_changed",
        "subtitle_unavailable",
        "subtitle_invalid",
        "subtitle_too_large",
        "model_missing",
        "model_not_installed",
        "model_unverified",
        "model_self_test_failed",
        "text_self_test_failed",
        "text_inference_timeout",
        "text_input_too_long",
        "slice_publication_refused",
        "model_changed",
        "engine_changed",
        "engine_unavailable",
        "inference_timeout",
        "output_invalid",
    ] {
        if message.contains(reason) {
            return reason;
        }
    }
    "text_translation_failed"
}
