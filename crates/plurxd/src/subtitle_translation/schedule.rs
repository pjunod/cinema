use super::{profile, selected_file_capability, AppState};
use crate::ai_subtitles::{selection, settings::Settings};
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
    if !(plurx_core::store::background_jobs_transcription::whisper_language(&language)
        || (requester == 0 && language == "auto"))
    {
        return Err(ApiError::BadRequest("Unsupported spoken language".into()));
    }
    let quality = crate::ai_subtitles::settings::read(state)
        .await?
        .settings
        .quality;
    let profile = profile::for_quality(task, quality);
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
    if !(settings.settings.from_audio || settings.settings.translate_text)
        || !settings.settings.ahead_of_time
    {
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
    let result = sweep_with_lease(state, &lease, shutdown, &settings.settings).await;
    lease.release().await?;
    result
}

async fn sweep_with_lease(
    state: &AppState,
    lease: &crate::job_lease::ActiveJobLease,
    shutdown: &CancellationToken,
    settings: &Settings,
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
        if let Some(mut file) = state.store.get_file(id).await? {
            if settings.download_match && selection::existing_english(&file).is_none() {
                let _ = crate::http::subtitle_downloads::try_english_match(state, id).await;
                if let Some(latest) = state.store.get_file(id).await? {
                    file = latest;
                }
            }
            if selection::existing_english(&file).is_some() {
                next = id;
                continue;
            }
            if settings.translate_text && schedule_text(state, &file).await {
                next = id;
                continue;
            }
            if let Some((audio, language)) = settings.from_audio.then(|| candidate(&file)).flatten()
            {
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

async fn schedule_text(state: &AppState, file: &MediaFile) -> bool {
    for candidate in selection::candidates(file, selection::Route::Text, None) {
        let selection::Source::Text(track, language) = candidate else {
            continue;
        };
        if crate::subtitle_text_translation::api::schedule(state, file, track.index, language)
            .await
            .is_ok()
        {
            return true;
        }
    }
    false
}

fn candidate(file: &MediaFile) -> Option<(&AudioStream, String)> {
    use crate::subtitle_transcription::{eligible, normalized_language};
    if !eligible(file) {
        return None;
    }
    let audio = file
        .audio_streams
        .iter()
        .find(|track| track.default)
        .or_else(|| file.audio_streams.first())?;
    let language = audio
        .language
        .as_deref()
        .map(normalized_language)
        .filter(|language| {
            plurx_core::store::background_jobs_transcription::whisper_language(language)
        })
        .unwrap_or_else(|| "auto".into());
    Some((audio, language))
}
