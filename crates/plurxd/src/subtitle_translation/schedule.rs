use super::{profile, selected_file_capability, AppState};
use crate::http::error::ApiError;
use plurx_core::{
    domain::{AudioStream, MediaFile},
    error::StoreError,
    store::{
        subtitle_translation::{digest, SourceRevision, SpeechTask},
        subtitle_translation_mutation::{TranslationAction, TranslationMutation},
    },
};
use serde_json::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const CURSOR_KEY: &str = "subtitles.ai.schedule_cursor.v1";

/// Item requests and the library sweep use the same durable operation as
/// playback, with no viewer window attached.
pub(crate) async fn start_finish_only(
    state: &AppState,
    file: &MediaFile,
    audio: &AudioStream,
    language: String,
    task: SpeechTask,
    requester: i64,
) -> Result<Value, ApiError> {
    if !plurx_core::store::background_jobs_transcription::whisper_language(&language) {
        return Err(ApiError::BadRequest("Unsupported spoken language".into()));
    }
    let profile = profile::for_task(task);
    let clock = selected_file_capability(state, file, audio.index)
        .await
        .map_err(|reason| ApiError::BadRequest(reason.into()))?;
    let worker = super::eligible_worker(state, file, &profile).await?;
    let action = TranslationAction::Create {
        source: SourceRevision {
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
        },
        profile,
        audio_index: audio.index,
        spoken_language: language,
        duration_ms: clock.duration_ms,
        target_node_id: worker,
        playback_id: String::new(),
        position_ms: 0,
        playback_rate: 1.0,
        paused: false,
        finish: true,
        continuity_id: None,
    };
    let now = crate::state::clock_ms();
    let result = state
        .store
        .mutate_subtitle_translation(TranslationMutation {
            user_id: requester,
            request_id: uuid::Uuid::new_v4().to_string(),
            request_digest: digest(&action)?,
            now_ms: now,
            intent_expires_at_ms: now.saturating_add(60_000),
            action,
        })
        .await?;
    if result["outcome"] == "accepted" {
        state.subtitle_models.translation.wake.notify_one();
    }
    Ok(result)
}

pub(crate) async fn run(state: AppState, shutdown: CancellationToken) {
    let mut ticks = tokio::time::interval(Duration::from_secs(60));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = ticks.tick() => {}
        }
        if let Err(error) = sweep(&state, &shutdown).await {
            tracing::debug!(%error, "AI subtitle library sweep stopped");
        }
    }
}

async fn sweep(state: &AppState, shutdown: &CancellationToken) -> Result<(), StoreError> {
    let settings = crate::ai_subtitles::settings::read_store(state.store.as_ref())
        .await
        .map_err(|_| StoreError::Task("ai_subtitles_settings_unavailable".into()))?;
    if !settings.settings.from_audio || !settings.settings.ahead_of_time {
        return Ok(());
    }
    if !state
        .jobs
        .execution_authority()
        .may_execute_job(plurx_core::store::background_jobs::JobKind::SubtitleTranslationPrepare)
        .await
    {
        return Ok(());
    }
    let Some(lease) = state
        .jobs
        .acquire_job("subtitle-translation:library-sweep".into())
        .await?
    else {
        return Ok(());
    };
    let result = sweep_with_lease(state, &lease, shutdown).await;
    lease.release().await?;
    result
}

async fn sweep_with_lease(
    state: &AppState,
    lease: &crate::job_lease::ActiveJobLease,
    shutdown: &CancellationToken,
) -> Result<(), StoreError> {
    let cursor = state
        .store
        .get_setting(CURSOR_KEY)
        .await?
        .and_then(|cursor| cursor.parse::<i64>().ok())
        .unwrap_or(0);
    let ids = state.store.subtitle_candidate_file_ids(cursor, 8).await?;
    let mut next = if ids.is_empty() { 0 } else { cursor };
    for id in ids {
        if shutdown.is_cancelled() {
            break;
        }
        if let Some(file) = state.store.get_file(id).await? {
            if let Some((audio, language)) = candidate(&file) {
                let task = if language == "en" {
                    SpeechTask::Transcribe
                } else {
                    SpeechTask::TranslateToEnglish
                };
                match start_finish_only(state, &file, audio, language, task, 0).await {
                    Ok(result)
                        if matches!(
                            result["outcome"].as_str(),
                            Some("disabled" | "demand_quota")
                        ) =>
                    {
                        break
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::debug!(
                            file_id = id,
                            ?error,
                            "AI subtitle sweep source unavailable"
                        )
                    }
                }
            }
        }
        next = id;
    }
    if next != cursor {
        lease
            .publisher(state.store.as_ref())
            .put_setting(CURSOR_KEY, &next.to_string())
            .await?;
    }
    Ok(())
}

fn candidate(file: &MediaFile) -> Option<(&AudioStream, String)> {
    use crate::subtitle_transcription::{eligible, normalized_language};
    if !eligible(file)
        || file.subtitle_streams.iter().any(|track| {
            !track.forced
                && !plurx_core::tracks::is_bitmap_subtitle(&track.codec)
                && track
                    .language
                    .as_deref()
                    .is_some_and(|tag| normalized_language(tag) == "en")
        })
    {
        return None;
    }
    let audio = file
        .audio_streams
        .iter()
        .find(|track| track.default)
        .or_else(|| file.audio_streams.first())?;
    let language = normalized_language(audio.language.as_deref()?);
    plurx_core::store::background_jobs_transcription::whisper_language(&language)
        .then_some((audio, language))
}
