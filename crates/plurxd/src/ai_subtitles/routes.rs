use super::{selection, settings};
use crate::{
    http::{error::ApiError, extract::AuthUser},
    state::AppState,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use plurx_core::{
    domain::{MediaFile, User},
    store::{
        ai_subtitles::{Intent, IntentAccess, IntentRoute},
        subtitle_translation::{digest, SourceRevision, SpeechTask},
        subtitle_translation_mutation::{TranslationAction, TranslationMutation},
        subtitle_translation_read::{TranslationQuery, TranslationRead},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Deserialize)]
pub(crate) struct Options {
    target: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Start {
    request_id: String,
    target: String,
    #[serde(default)]
    route: selection::Route,
    spoken_language: Option<String>,
    playback_id: Option<String>,
    position_ms: Option<i64>,
}

fn source(file: &MediaFile) -> SourceRevision {
    SourceRevision {
        file_id: file.id,
        source_size: file.size,
        source_mtime: file.mtime,
    }
}

fn conflict(code: &'static str, message: &str) -> ApiError {
    ApiError::typed(axum::http::StatusCode::CONFLICT, code, message)
}

fn now() -> i64 {
    crate::state::clock_ms()
}

pub(crate) async fn start(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(file_id): Path<i64>,
    Json(input): Json<Start>,
) -> Result<Json<Value>, ApiError> {
    english(Some(&input.target))?;
    if uuid::Uuid::parse_str(&input.request_id).is_err()
        || input
            .playback_id
            .as_deref()
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_err())
        || input
            .position_ms
            .is_some_and(|position| !(0..=14_400_000).contains(&position))
    {
        return Err(ApiError::BadRequest(
            "Invalid AI subtitle request identity or position".into(),
        ));
    }
    let language = input
        .spoken_language
        .as_deref()
        .map(crate::subtitle_transcription::normalized_language);
    if language.as_ref().is_some_and(|language| {
        !plurx_core::store::background_jobs_transcription::whisper_language(language)
    }) || (language.is_some() && input.route == selection::Route::Text)
    {
        return Err(ApiError::BadRequest(
            "Choose a supported spoken language for audio subtitles".into(),
        ));
    }
    let fingerprint = digest(&json!({"file_id":file_id,"request":input}))?;
    let file = file(&state, file_id).await?;
    let mut intent = state
        .store
        .ai_subtitle_intent(
            user.id,
            now(),
            IntentAccess::Request {
                request_id: input.request_id.clone(),
            },
        )
        .await?;
    if intent.is_none() {
        super::may_start(&state, &user).await?;
        let options = settings::read(&state).await?.settings;
        let requested = if language.is_some() {
            selection::Route::Audio
        } else {
            input.route
        };
        let mut chosen = None;
        let choices = selection::candidates(&file, requested);
        let text_ready = options.translate_text
            && choices
                .iter()
                .any(|candidate| candidate.route() == selection::Route::Text)
            && crate::subtitle_text_translation::workers(&state)
                .await?
                .iter()
                .any(|worker| worker["status"] == "verified");
        for choice in choices {
            match choice {
                selection::Source::Text(track, language) if text_ready => {
                    if crate::subtitle_text_translation::input_bytes(&state, &file, track.index)
                        .await
                        .is_ok()
                    {
                        chosen = Some((IntentRoute::Text, track.index, Some(language)));
                        break;
                    }
                }
                selection::Source::Audio(track, _) if options.from_audio => {
                    chosen = Some((IntentRoute::Audio, track.index, language.clone()));
                    break;
                }
                _ => {}
            }
        }
        let (route, track_index, language) = chosen.ok_or_else(|| {
            conflict(
                "no_capable_node",
                "No enabled AI subtitle route can use the available tracks",
            )
        })?;
        let id = uuid::Uuid::new_v4().to_string();
        let issued_at_ms = now();
        intent = state
            .store
            .ai_subtitle_intent(
                user.id,
                issued_at_ms,
                IntentAccess::Reserve {
                    request_id: input.request_id.clone(),
                    intent: Intent {
                        playback_id: input
                            .playback_id
                            .clone()
                            .or_else(|| input.position_ms.map(|_| id.clone())),
                        id,
                        digest: fingerprint.clone(),
                        source: source(&file),
                        route,
                        track_index,
                        language,
                        position_ms: input.position_ms.unwrap_or(0),
                        issued_at_ms,
                        producer_id: None,
                    },
                },
            )
            .await?;
    }
    let mut intent = intent.ok_or_else(|| {
        conflict(
            "request_conflict",
            "This subtitle request could not be reserved",
        )
    })?;
    if intent.digest != fingerprint {
        return Err(conflict(
            "request_conflict",
            "This request ID was already used for a different action",
        ));
    }
    if intent.source != source(&file) {
        return Err(conflict("source_changed", "The media source changed"));
    }
    if intent.producer_id.is_none() {
        let producer_id = match intent.route {
            IntentRoute::Text => {
                let language = intent
                    .language
                    .clone()
                    .ok_or_else(|| ApiError::Internal("Missing subtitle language".into()))?;
                let response = crate::subtitle_text_translation::api::create(
                    AuthUser(user.clone()),
                    State(state.clone()),
                    Path((file.id, intent.track_index)),
                    Json(
                        crate::subtitle_text_translation::api::CreateRequest::ai_start(
                            intent.id.clone(),
                            &file,
                            language,
                        ),
                    ),
                )
                .await?
                .0;
                response["job_id"]
                    .as_str()
                    .ok_or_else(|| ApiError::Internal("Missing text translation job".into()))?
                    .to_owned()
            }
            IntentRoute::Audio => start_audio(&state, &user, &file, &intent).await?,
        };
        let id = intent.id.clone();
        intent = state
            .store
            .ai_subtitle_intent(
                user.id,
                now(),
                IntentAccess::Link {
                    request_id: input.request_id,
                    id,
                    producer_id,
                },
            )
            .await?
            .ok_or_else(|| conflict("request_conflict", "The subtitle request changed"))?;
    }
    Ok(Json(operation(&state, &user, &file, &intent).await?))
}

async fn start_audio(
    state: &AppState,
    user: &User,
    file: &MediaFile,
    intent: &Intent,
) -> Result<String, ApiError> {
    use crate::subtitle_translation::{config, eligible_worker, profile, selected_file_capability};
    super::may_start(state, user).await?;
    if let Some(receipt) = config::replay(
        state.store.as_ref(),
        user.id,
        &intent.id,
        &intent.digest,
        now(),
    )
    .await?
    {
        return operation_id(&receipt);
    }
    let language = intent.language.as_deref().unwrap_or("auto");
    let profile = profile::for_task(if language == "en" {
        SpeechTask::Transcribe
    } else {
        SpeechTask::TranslateToEnglish
    });
    let clock = selected_file_capability(state, file, intent.track_index)
        .await
        .map_err(|reason| conflict("source_clock_unsupported", reason))?;
    let recipe_key = digest(&json!({
        "source":intent.source,"profile":profile,"audio_index":intent.track_index,
        "spoken_language":language,"duration_ms":clock.duration_ms,
    }))?;
    let rows = state
        .store
        .read_subtitle_translation(TranslationRead {
            user_id: user.id,
            now_ms: now(),
            query: TranslationQuery::RecipeOperation { recipe_key },
        })
        .await?;
    let joining = rows.first().and_then(|row| row["operation_id"].as_str());
    super::may_start_new(state, user, joining).await?;
    let target_node_id =
        if let Some(node) = rows.first().and_then(|row| row["target_node_id"].as_str()) {
            node.to_owned()
        } else {
            eligible_worker(state, file, &profile).await?
        };
    let now_ms = now();
    let receipt = state
        .store
        .mutate_subtitle_translation(TranslationMutation {
            user_id: user.id,
            request_id: intent.id.clone(),
            request_digest: intent.digest.clone(),
            now_ms,
            intent_expires_at_ms: now_ms.saturating_add(60_000),
            action: TranslationAction::Create {
                source: intent.source.clone(),
                profile,
                audio_index: intent.track_index,
                spoken_language: language.into(),
                duration_ms: clock.duration_ms,
                target_node_id,
                playback_id: intent.playback_id.clone().unwrap_or_default(),
                position_ms: if intent.playback_id.is_some() {
                    intent.position_ms.min(clock.duration_ms)
                } else {
                    0
                },
                playback_rate: 1.0,
                paused: false,
                finish: true,
                continuity_id: None,
            },
        })
        .await?;
    let id = operation_id(&receipt)?;
    state.subtitle_models.translation.wake.notify_one();
    Ok(id)
}

fn operation_id(receipt: &Value) -> Result<String, ApiError> {
    if receipt["outcome"] == "accepted" {
        return receipt["operation_id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| ApiError::Internal("Missing audio subtitle operation".into()));
    }
    if receipt["outcome"] == "demand_quota" {
        return Err(ApiError::typed(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "ai_subtitles_busy",
            "AI subtitle work is at capacity",
        ));
    }
    Err(conflict(
        "state_conflict",
        "The audio subtitle operation could not start",
    ))
}

async fn audio_operation(state: &AppState, user: &User, producer: &str) -> Result<Value, ApiError> {
    let mut rows = state
        .store
        .read_subtitle_translation(TranslationRead {
            user_id: user.id,
            now_ms: now(),
            query: TranslationQuery::Operation {
                operation_id: producer.into(),
            },
        })
        .await?;
    rows.pop()
        .ok_or(ApiError::NotFound("audio subtitle operation"))
}

fn stored_track(file: &MediaFile, artifact: &str) -> Option<i64> {
    file.subtitle_streams
        .iter()
        .find(|track| {
            file.downloaded_subtitle(track.index)
                .is_some_and(|caption| caption.stable_id() == artifact)
        })
        .map(|track| track.index)
}

async fn operation(
    state: &AppState,
    user: &User,
    file: &MediaFile,
    intent: &Intent,
) -> Result<Value, ApiError> {
    let producer = intent
        .producer_id
        .as_deref()
        .ok_or(ApiError::NotFound("AI subtitle operation"))?;
    let (status, track, duration, progress, reason, language) = match intent.route {
        IntentRoute::Audio => {
            let row = audio_operation(state, user, producer).await?;
            let track = row["asset_id"]
                .as_str()
                .and_then(|asset| stored_track(file, asset));
            let duration = row["duration_ms"].as_i64().unwrap_or(0);
            let status = match row["state"].as_str() {
                Some("complete") if track.is_some() => "complete",
                Some("failed") => "failed",
                Some("stopped") => "stopped",
                Some("checking_source" | "waiting_model" | "waiting_capacity" | "suspended") => {
                    "queued"
                }
                _ => "working",
            };
            let progress = if duration > 0 {
                row["accepted_duration_ms"].as_i64().unwrap_or(0) as f64 / duration as f64
            } else {
                0.0
            };
            let language = row["spoken_language"]
                .as_str()
                .filter(|language| *language != "auto")
                .map(str::to_owned);
            (
                status,
                track,
                duration,
                progress,
                row["reason"].clone(),
                language,
            )
        }
        IntentRoute::Text => {
            let row = crate::subtitle_text_translation::api::job(
                AuthUser(user.clone()),
                State(state.clone()),
                Path(producer.to_owned()),
            )
            .await?
            .0;
            let track = row["result"]["downloaded_subtitle_id"]
                .as_str()
                .and_then(|id| stored_track(file, id));
            let status = match row["state"].as_str() {
                Some("succeeded") if track.is_some() => "complete",
                Some("failed" | "succeeded") => "failed",
                Some("cancelling" | "cancelled") => "stopped",
                _ if row["progress"]["completed"].as_i64().unwrap_or(0) > 0 => "working",
                Some("queued") => "queued",
                _ => "working",
            };
            let completed = row["progress"]["completed"].as_i64().unwrap_or(0);
            let total = row["progress"]["total"].as_i64().unwrap_or(0);
            let progress = if total > 0 {
                completed as f64 / total as f64
            } else {
                0.0
            };
            (
                status,
                track,
                file.duration_ms.unwrap_or_default(),
                progress,
                row["reason"].clone(),
                None,
            )
        }
    };
    Ok(json!({
        "id": intent.id, "route": intent.route, "state": status,
        "ready_from_ms": if status == "complete" { Some(0) } else { None },
        "ready_to_ms": if status == "complete" { duration } else { 0 },
        "duration_ms": duration, "progress": if status == "complete" { 1.0 } else { progress.clamp(0.0, 1.0) },
        "reason":reason, "track_index":track,
        "track_id":track.and_then(|index| file.downloaded_subtitle(index)).map(|caption| caption.stable_id()),
        "spoken_language":language,
    }))
}

async fn resolved_intent(
    state: &AppState,
    user: &User,
    id: String,
) -> Result<(Intent, MediaFile), ApiError> {
    if uuid::Uuid::parse_str(&id).is_err() {
        return Err(ApiError::BadRequest("Invalid AI subtitle operation".into()));
    }
    let intent = state
        .store
        .ai_subtitle_intent(user.id, now(), IntentAccess::Operation { id })
        .await?
        .ok_or(ApiError::NotFound("AI subtitle operation"))?;
    let file = file(state, intent.source.file_id).await?;
    if source(&file) != intent.source {
        return Err(conflict("source_changed", "The media source changed"));
    }
    Ok((intent, file))
}

#[derive(Deserialize)]
pub(crate) struct CueQuery {
    from_ms: i64,
    to_ms: i64,
}

pub(crate) async fn cues(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(window): Query<CueQuery>,
) -> Result<Json<Value>, ApiError> {
    let (from, to) = (window.from_ms, window.to_ms);
    if from < 0 || to <= from || to > 14_400_000 || to - from > 90_000 {
        return Err(ApiError::BadRequest(
            "Choose a subtitle cue window of up to 90 seconds".into(),
        ));
    }
    let (intent, file) = resolved_intent(&state, &user, id).await?;
    let summary = operation(&state, &user, &file, &intent).await?;
    let producer = intent
        .producer_id
        .as_deref()
        .ok_or(ApiError::NotFound("AI subtitle operation"))?;
    let (cues, coverage): (Vec<Value>, Option<(i64, i64)>) = if summary["state"] == "complete" {
        (
            Vec::new(),
            Some((0, summary["duration_ms"].as_i64().unwrap_or(0))),
        )
    } else {
        match intent.route {
            IntentRoute::Audio => {
                let row = audio_operation(&state, &user, producer).await?;
                if let Some(asset) = row["asset_id"].as_str() {
                    let (cues, coverage) = crate::subtitle_translation::api::ai_cue_window(
                        &state,
                        user.id,
                        asset,
                        row["profile"]["core_ms"].as_i64().unwrap_or(0),
                        from,
                        to,
                    )
                    .await?;
                    (cues.into_iter().map(|cue| json!({"start_ms":cue.start_ms,"end_ms":cue.end_ms,"text":cue.text})).collect(), coverage)
                } else {
                    (Vec::new(), None)
                }
            }
            IntentRoute::Text => {
                let job = state
                    .store
                    .background_job(producer)
                    .await?
                    .ok_or(ApiError::NotFound("text subtitle operation"))?;
                let cues = state
                    .store
                    .read_text_translation_cues(job.supported_payload()?)
                    .await?;
                let ready = cues
                    .iter()
                    .enumerate()
                    .take_while(|(i, cue)| *i as u32 == cue.cue_index)
                    .last()
                    .map(|(_, cue)| (0, cue.end_ms));
                let visible = cues
                    .into_iter()
                    .filter(|cue| cue.start_ms < to && cue.end_ms > from)
                    .map(|cue| json!({"start_ms":cue.start_ms,"end_ms":cue.end_ms,"text":cue.text}))
                    .collect();
                (visible, ready)
            }
        }
    };
    Ok(Json(
        json!({"cues":cues,"ready_from_ms":coverage.map(|(start,_)|start),
        "ready_to_ms":coverage.map_or(0, |(_,end)|end),"state":summary["state"],
        "progress":summary["progress"],"reason":summary["reason"],"track_index":summary["track_index"],
        "track_id":summary["track_id"],"spoken_language":summary["spoken_language"]}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ViewerUpdate {
    playback_id: Option<String>,
    position_ms: i64,
    playback_rate: f64,
}

#[derive(Deserialize)]
pub(crate) struct ViewerIdentity {
    playback_id: Option<String>,
}

fn playback(intent: &Intent, explicit: Option<String>) -> Result<String, ApiError> {
    let id = explicit
        .or_else(|| intent.playback_id.clone())
        .unwrap_or_else(|| intent.id.clone());
    uuid::Uuid::parse_str(&id)
        .map_err(|_| ApiError::BadRequest("Invalid playback identity".into()))?;
    Ok(id)
}

pub(crate) async fn update_viewer(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ViewerUpdate>,
) -> Result<Json<Value>, ApiError> {
    if !(0..=14_400_000).contains(&input.position_ms)
        || !input.playback_rate.is_finite()
        || input.playback_rate <= 0.0
        || input.playback_rate > 4.0
    {
        return Err(ApiError::BadRequest(
            "Invalid subtitle playhead position or rate".into(),
        ));
    }
    super::may_start(&state, &user).await?;
    let (intent, file) = resolved_intent(&state, &user, id).await?;
    if intent.route == IntentRoute::Audio {
        let playback_id = playback(&intent, input.playback_id)?;
        let operation_id = intent
            .producer_id
            .clone()
            .ok_or(ApiError::NotFound("AI subtitle operation"))?;
        let updated = state
            .store
            .ai_subtitle_viewer(plurx_core::store::ai_subtitles::Viewer {
                operation_id,
                playback_id,
                user_id: user.id,
                now_ms: now(),
                window: Some((input.position_ms, input.playback_rate)),
            })
            .await?;
        if updated {
            state.subtitle_models.translation.wake.notify_one();
        }
    }
    Ok(Json(operation(&state, &user, &file, &intent).await?))
}

pub(crate) async fn release_viewer(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(input): Query<ViewerIdentity>,
) -> Result<StatusCode, ApiError> {
    let (intent, _) = resolved_intent(&state, &user, id).await?;
    if intent.route == IntentRoute::Audio {
        let playback_id = playback(&intent, input.playback_id)?;
        let operation_id = intent
            .producer_id
            .clone()
            .ok_or(ApiError::NotFound("AI subtitle operation"))?;
        state
            .store
            .ai_subtitle_viewer(plurx_core::store::ai_subtitles::Viewer {
                operation_id,
                playback_id,
                user_id: user.id,
                now_ms: now(),
                window: None,
            })
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(crate) struct DeleteTrack {
    track_id: String,
}

pub(crate) async fn delete_track(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path((file_id, index)): Path<(i64, i64)>,
    Query(input): Query<DeleteTrack>,
) -> Result<StatusCode, ApiError> {
    super::may_start(&state, &user).await?;
    let file = file(&state, file_id).await?;
    let caption = file
        .downloaded_subtitle(index)
        .filter(|caption| caption.transcription.is_some() || caption.text_translation.is_some())
        .ok_or(ApiError::NotFound("AI subtitle track"))?;
    if caption.stable_id() != input.track_id {
        return Err(conflict("track_changed", "The subtitle list changed"));
    }
    state
        .store
        .delete_ai_subtitle_track(source(&file), &input.track_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn english(target: Option<&str>) -> Result<(), ApiError> {
    if target.is_none_or(|target| target == "en") {
        Ok(())
    } else {
        Err(ApiError::BadRequest("Choose English subtitles".into()))
    }
}

async fn file(state: &AppState, id: i64) -> Result<MediaFile, ApiError> {
    state
        .store
        .get_file(id)
        .await?
        .ok_or(ApiError::NotFound("file"))
}

fn block(code: &str, message: &str) -> Value {
    json!({ "code": code, "message": message })
}

pub(crate) async fn describe(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(file_id): Path<i64>,
    Query(input): Query<Options>,
) -> Result<Json<Value>, ApiError> {
    english(input.target.as_deref())?;
    let file = file(&state, file_id).await?;
    let current = settings::read(&state).await?.settings;
    let may_start = user.is_admin || current.who == settings::Who::Everyone;
    let existing = selection::existing_english(&file);
    let candidates = selection::candidates(&file, selection::Route::Auto);
    let text_ready = async {
        if !current.translate_text
            || !candidates
                .iter()
                .any(|source| source.route() == selection::Route::Text)
        {
            return Ok::<bool, ApiError>(false);
        }
        Ok(crate::subtitle_text_translation::workers(&state)
            .await?
            .iter()
            .any(|worker| worker["status"] == "verified"))
    };
    let audio_ready = async {
        if !current.from_audio
            || !candidates
                .iter()
                .any(|source| source.route() == selection::Route::Audio)
        {
            return Ok::<bool, ApiError>(false);
        }
        Ok(crate::subtitle_translation::workers_public(&state)
            .await?
            .iter()
            .any(|worker| worker["eligible"] == true))
    };
    let (text_ready, audio_ready) = if may_start && existing.is_none() {
        tokio::try_join!(text_ready, audio_ready)?
    } else {
        (false, false)
    };
    let selected = candidates.iter().find(|source| match source {
        selection::Source::Text(..) => text_ready,
        selection::Source::Audio(..) => audio_ready,
    });
    let blocked = if existing.is_some() {
        Value::Null
    } else if !may_start {
        block(
            "admins_only",
            "An administrator has limited AI subtitles to admins",
        )
    } else if !current.from_audio && !current.translate_text {
        block("disabled", "AI subtitle creation is off")
    } else if candidates.is_empty() {
        block(
            "no_audio",
            "This title has no compatible audio or subtitle track",
        )
    } else if selected.is_none() {
        block(
            "no_capable_node",
            "No worker is ready to create these subtitles",
        )
    } else {
        Value::Null
    };
    let source_descriptor = selected.map(|source| match source {
        selection::Source::Text(track, language) => {
            json!({"kind":"subtitle","index":track.index,"language":language,"detected":false})
        }
        selection::Source::Audio(track, language) => {
            json!({"kind":"audio","index":track.index,"language":language,"detected":false})
        }
    });
    let alternatives = if text_ready && audio_ready {
        vec![json!({"route":"audio","label":"Use the audio instead"})]
    } else {
        Vec::new()
    };
    let estimate = selected.map(|source| match source {
        selection::Source::Text(..) => "about a minute",
        selection::Source::Audio(..) => "starts in a few seconds",
    });
    let progress = match state
        .store
        .ai_subtitle_intent(
            user.id,
            now(),
            IntentAccess::Latest {
                source: source(&file),
            },
        )
        .await?
    {
        Some(intent) => Some(operation(&state, &user, &file, &intent).await?),
        None => None,
    };
    Ok(Json(json!({
        "target":"en",
        "allowed":may_start && blocked.is_null(),
        "blocked":blocked,
        "existing_index":existing,
        "route":selected.map(selection::Source::route),
        "label":selected.map(selection::Source::label),
        "estimate":estimate,
        "source":source_descriptor,
        "alternatives":alternatives,
        "operation":progress,
    })))
}
