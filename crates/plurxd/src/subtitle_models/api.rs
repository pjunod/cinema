//! Administrator-only, revision-bound subtitle setup and progress projections.
//! Caption bodies, paths, payloads and claim authority never enter these DTOs.
use super::config::{self, Mode, Snapshot, UpdateRequest};
use crate::{
    http::{error::ApiError, extract::AdminUser},
    state::AppState,
};
use axum::{
    extract::{Path as ApiPath, State},
    Json,
};
use plurx_core::{
    domain::MediaFile,
    store::{
        background_jobs::{BackgroundJob, JobAttemptObservation, JobPayload, JobState},
        background_jobs_transcription::artifact_key,
    },
};
use serde_json::{json, Value};

pub async fn settings(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    encode(config::read(state.store.as_ref()).await?)
}

pub async fn update_settings(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(request): Json<UpdateRequest>,
) -> Result<Json<Value>, ApiError> {
    encode(config::save(state.store.as_ref(), request, &super::catalog().models).await?)
}

fn encode(value: impl serde::Serialize) -> Result<Json<Value>, ApiError> {
    serde_json::to_value(value)
        .map(Json)
        .map_err(|_| ApiError::Internal("Could not encode subtitle observations".into()))
}

pub async fn models(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let mut value = super::catalog_public();
    let object = value
        .as_object_mut()
        .ok_or_else(|| ApiError::Internal("Subtitle catalog is unavailable".into()))?;
    object.insert(
        "workers".into(),
        json!(super::workers_public(&state).await?),
    );
    Ok(Json(value))
}

pub async fn options(
    _admin: AdminUser,
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
) -> Result<Json<Value>, ApiError> {
    let (file, configuration) =
        tokio::try_join!(state.store.get_file(id), config::read(state.store.as_ref()))?;
    let file = file.ok_or(ApiError::NotFound("file"))?;
    let languages = match configuration.mode {
        Mode::Managed => super::catalog()
            .models
            .iter()
            .find(|model| model.id == configuration.model_id)
            .map(|model| model.languages.clone())
            .unwrap_or_default(),
        Mode::Advanced => {
            crate::subtitle_transcription::model_languages(&configuration.advanced.model_path).await
        }
    };
    Ok(Json(file_options(&file, &configuration, languages)))
}

fn file_options(file: &MediaFile, configuration: &Snapshot, languages: Vec<String>) -> Value {
    let captions = file
        .subtitle_streams
        .iter()
        .filter_map(|stream| {
            let caption = file.downloaded_subtitle(stream.index)?;
            let provenance = caption.transcription.as_ref()?;
            if provenance.artifact_key.len() != 64
                || !provenance
                    .artifact_key
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                return None;
            }
            (caption.source_size == file.size && caption.source_mtime == file.mtime).then(|| {
                json!({
                    "index": stream.index,
                    "artifact_key": provenance.artifact_key,
                    "source_size": caption.source_size.to_string(),
                    "source_mtime": caption.source_mtime.to_string(),
                })
            })
        })
        .collect::<Vec<_>>();
    json!({
        "version": 1,
        "file_id": file.id.to_string(),
        "source_size": file.size.to_string(),
        "source_mtime": file.mtime.to_string(),
        "audio_tracks": file.audio_streams.iter().map(|audio| json!({
            "index": audio.index, "title": audio.title, "language": audio.language,
            "default": audio.default,
            "normalized_language": super::spoken_language_tag(audio.language.as_deref()),
        })).collect::<Vec<_>>(),
        // These facts and the acquired-caption mapping come from one MediaFile
        // snapshot. An index is selectable only in this exact source revision.
        "subtitle_streams": file.subtitle_streams,
        "captions": captions,
        "mode": configuration.mode,
        "model_id": if configuration.mode == Mode::Advanced { "advanced" } else { &configuration.model_id },
        "language": configuration.language,
        "enabled": configuration.enabled,
        "effective_enabled": configuration.effective_enabled,
        "supported_languages": languages,
        "reason": configuration.reason.as_ref().map(|_| "configuration_conflict"),
    })
}

pub async fn job(
    _admin: AdminUser,
    State(state): State<AppState>,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<Value>, ApiError> {
    if uuid::Uuid::parse_str(&id).is_err() {
        return Err(ApiError::BadRequest("Choose a valid subtitle job".into()));
    }
    let job = state
        .store
        .background_job(&id)
        .await?
        .ok_or(ApiError::NotFound("subtitle job"))?;
    // Decode before the second read. Unknown versions and unrelated kinds are
    // not permission to expose arbitrary private checkpoint or payload fields.
    let payload = subtitle_payload(&job)?;
    let attempts = state.store.job_attempts(&id).await?;
    let mut value = job_public(&job, &payload, &attempts, crate::state::clock_ms())?;
    if job.state == JobState::Queued {
        let reason = super::job_wait_reason(&state, &job).await?;
        let reason = safe_reason(reason.as_deref());
        if matches!(
            reason,
            Some("disabled" | "configuration_conflict" | "legacy_profile_incompatible")
        ) || value["reason"].is_null()
        {
            value["reason"] = json!(reason);
        }
    }
    Ok(Json(value))
}

fn subtitle_payload(job: &BackgroundJob) -> Result<JobPayload, ApiError> {
    let payload = job.supported_payload().map_err(|_| {
        ApiError::BadRequest("This subtitle job uses an unsupported payload".into())
    })?;
    if !matches!(
        payload,
        JobPayload::SubtitleTranscribe { .. }
            | JobPayload::SubtitleTranscribeManaged { .. }
            | JobPayload::SubtitleModelOperation { .. }
    ) {
        return Err(ApiError::BadRequest(
            "This is not a subtitle generation or model job".into(),
        ));
    }
    Ok(payload)
}

fn safe_reason(reason: Option<&str>) -> Option<&'static str> {
    Some(match reason? {
        "disabled" => "disabled",
        "missing_model" | "model_not_installed" => "missing_model",
        "insufficient_disk"
        | "model_disk_reservation_unavailable"
        | "model_disk_full"
        | "model_disk_reservation_failed"
        | "model_disk_space_unavailable" => "insufficient_disk",
        "model_download_failed"
        | "model_download_client_unavailable"
        | "model_download_http_failure"
        | "model_download_interrupted"
        | "model_download_length_or_encoding_refused"
        | "model_download_oversized"
        | "model_download_truncated"
        | "model_download_write_failed"
        | "model_redirect_invalid"
        | "model_redirect_limit"
        | "model_redirect_refused" => "model_download_failed",
        "artifact_digest_mismatch" | "artifact_length_mismatch" | "artifact_changed" => {
            "model_verification_failed"
        }
        "engine_unavailable"
        | "engine_staging_failed"
        | "packaged_engine_changed"
        | "engine_verification_task_failed"
        | "packaged_engine_companion_changed"
        | "packaged_engine_companion_missing"
        | "packaged_engine_companions_invalid"
        | "packaged_engine_companions_unqualified"
        | "packaged_engine_dependencies_invalid"
        | "packaged_engine_dependencies_unqualified"
        | "packaged_engine_location_unavailable"
        | "packaged_engine_manifest_changed"
        | "packaged_engine_manifest_invalid"
        | "packaged_engine_manifest_unavailable"
        | "packaged_engine_not_executable"
        | "packaged_engine_path_not_absolute"
        | "packaged_engine_pin_invalid"
        | "packaged_engine_platform_mismatch"
        | "packaged_engine_unavailable"
        | "packaged_engine_unqualified" => "engine_unavailable",
        "model_verification_failed" | "model_self_test_failed" => "model_verification_failed",
        "no_compatible_worker" => "no_compatible_worker",
        "source_unavailable" => "source_unavailable",
        "waiting_for_playback_capacity" => "waiting_for_playback_capacity",
        "waiting_for_worker" => "waiting_for_worker",
        "worker_source_unavailable" => "worker_source_unavailable",
        "worker_observation_unavailable" => "worker_observation_unavailable",
        "worker_not_current" => "worker_not_current",
        "worker_interrupted" => "worker_interrupted",
        "decode_failed" => "decode_failed",
        "no_speech_detected" => "no_speech_detected",
        "inference_failed" => "inference_failed",
        "inference_timeout" => "inference_timeout",
        "output_invalid" => "output_invalid",
        "output_too_large" => "output_too_large",
        "source_changed" => "source_changed",
        "legacy_profile_incompatible" => "legacy_profile_incompatible",
        "configuration_conflict" => "configuration_conflict",
        _ => "worker_failed",
    })
}

fn job_public(
    job: &BackgroundJob,
    payload: &JobPayload,
    attempts: &[JobAttemptObservation],
    now_ms: i64,
) -> Result<Value, ApiError> {
    let transcription = matches!(
        payload,
        JobPayload::SubtitleTranscribe { .. } | JobPayload::SubtitleTranscribeManaged { .. }
    );
    let artifact = transcription.then(|| artifact_key(payload)).transpose()?;
    let expected_result = match payload {
        JobPayload::SubtitleModelOperation { model_sha256, .. } => Some(model_sha256.as_str()),
        _ => artifact.as_deref(),
    };
    let result = (job.state == JobState::Succeeded && job.result_ref.as_deref() == expected_result)
        .then_some(expected_result)
        .flatten();
    let live_token = job.token.as_ref().filter(|token| {
        token.job_id == job.id && token.fence == job.fence && token.lease_expires_ms > now_ms
    });
    let checkpoint = job.checkpoint.as_ref().filter(|checkpoint| {
        live_token.is_some() && checkpoint.get("fence").and_then(Value::as_i64) == Some(job.fence)
    });
    let observed_phase = checkpoint
        .and_then(|checkpoint| checkpoint.get("phase"))
        .and_then(Value::as_str)
        .filter(|phase| {
            matches!(
                *phase,
                "downloading"
                    | "waiting_for_capacity"
                    | "verifying"
                    | "decoding"
                    | "transcribing"
                    | "validating"
                    | "publishing"
            )
        });
    let phase = match job.state {
        JobState::Queued => "queued",
        JobState::Succeeded => "succeeded",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
        JobState::Running | JobState::Cancelling => observed_phase.unwrap_or("queued"),
    };
    let expired =
        matches!(job.state, JobState::Running | JobState::Cancelling) && live_token.is_none();
    let reason = if matches!(job.state, JobState::Succeeded | JobState::Cancelled) {
        None
    } else if expired {
        Some("worker_interrupted")
    } else {
        safe_reason(job.last_error_code.as_deref())
    };
    let current_attempt = attempts.iter().find(|attempt| {
        attempt.fence == job.fence
            && live_token.is_none_or(|token| token.node_id == attempt.node_id)
    });
    let updated = checkpoint
        .filter(|_| observed_phase.is_some())
        .and_then(|checkpoint| checkpoint.get("phase_at_ms"))
        .and_then(Value::as_i64)
        .unwrap_or(job.updated_at_ms)
        .max(job.updated_at_ms);
    let node = live_token
        .map(|token| token.node_id.as_str())
        .or_else(|| payload.target_node_id());
    let source = |key: &str| {
        transcription
            .then(|| {
                job.payload
                    .get(key)
                    .and_then(Value::as_i64)
                    .map(|value| value.to_string())
            })
            .flatten()
    };
    Ok(json!({
        "job_id": job.id, "kind": payload.kind(), "state": job.state,
        "phase": phase, "reason": reason, "node_id": node,
        "attempt": job.fence, "fence": job.fence, "revision": job.revision,
        "started_at_ms": current_attempt.map(|attempt| attempt.started_at_ms),
        "updated_at_ms": updated,
        "yield_count": job.yield_count,
        "restarted_count": job.yield_count.saturating_add(job.abandoned_count),
        "file_id": source("file_id"), "source_size": source("source_size"),
        "source_mtime": source("source_mtime"),
        "artifact_key": artifact, "result_ref": result,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitle_models::config::{Advanced, ModeStatus};
    use plurx_core::{
        domain::{
            AudioStream, DownloadedSubtitle, ItemKind, LibraryKind, NewItem, NewLibrary,
            ProbeResult, SubtitleStream, SubtitleTranscription,
        },
        store::{
            background_jobs::JobToken,
            background_jobs_transcription::{ManagedProfile, ModelAction},
            LibraryStore, MediaStore, SqliteStore,
        },
    };

    fn configuration() -> Snapshot {
        Snapshot {
            version: 1,
            revision: format!("0.{}", "a".repeat(64)),
            mode: Mode::Managed,
            enabled: true,
            effective_enabled: true,
            interval_mins: 0,
            language: "en".into(),
            model_id: "base.en".into(),
            advanced: Advanced {
                command: "/srv/private/whisper-cli".into(),
                model_path: "/srv/private/model.bin".into(),
            },
            mode_status: ModeStatus::Ok,
            reason: None,
        }
    }

    fn source() -> JobPayload {
        JobPayload::SubtitleTranscribeManaged {
            file_id: 9_007_199_254_740_993,
            audio_index: 2,
            source_size: 9_007_199_254_740_995,
            source_mtime: 9_007_199_254_740_997,
            language: "en".into(),
            model_sha256: "a".repeat(64),
            pipeline_digest: "b".repeat(64),
            profile: ManagedProfile {
                model_id: "base.en".into(),
                catalog_revision: "reviewed-catalog-v1".into(),
                adapter_contract: "whisper.cpp-1.9.2-cpu-v1".into(),
            },
        }
    }

    fn background(payload: JobPayload) -> BackgroundJob {
        let id = uuid::Uuid::new_v4().to_string();
        BackgroundJob {
            id: id.clone(),
            payload_version: 1,
            payload: serde_json::to_value(payload).expect("valid background fixture"),
            dedupe_key: "/srv/private/dedupe".into(),
            priority: 0,
            state: JobState::Running,
            token: Some(JobToken {
                job_id: id,
                node_id: "worker-one".into(),
                boot_id: "private-boot-id".into(),
                claim_id: "private-claim-id".into(),
                fence: 2,
                revision: 3,
                lease_expires_ms: 2000,
            }),
            fence: 2,
            revision: 3,
            failed_attempts: 0,
            attempt_limit: 3,
            retry_deadline_ms: 5000,
            attempt_errors: "private process output".into(),
            index_diagnostic_json: "/srv/private/diagnostics".into(),
            yield_count: 4,
            abandoned_count: 2,
            not_before_ms: 0,
            checkpoint: Some(
                json!({"fence":2,"phase":"transcribing","phase_at_ms":1200,"path":"/srv/private/temporary.wav"}),
            ),
            result_ref: None,
            last_error_code: Some("private stdout /srv/private/model.bin".into()),
            created_at_ms: 1000,
            updated_at_ms: 1100,
        }
    }

    fn attempts() -> Vec<JobAttemptObservation> {
        vec![JobAttemptObservation {
            fence: 2,
            node_id: "worker-one".into(),
            started_at_ms: 1050,
            finished_at_ms: None,
            outcome: None,
            error_code: None,
        }]
    }

    #[test]
    fn subtitle_job_projection_is_closed_and_preserves_exact_source_identity() {
        let job = background(source());
        let payload = subtitle_payload(&job).expect(
            "valid subtitle job projection is closed and preserves exact source identity fixture",
        );
        let value = job_public(&job, &payload, &attempts(), 1300).expect(
            "valid subtitle job projection is closed and preserves exact source identity fixture",
        );
        let wire = serde_json::to_string(&value).expect(
            "valid subtitle job projection is closed and preserves exact source identity fixture",
        );
        assert_eq!(value["file_id"], "9007199254740993");
        assert_eq!(value["source_size"], "9007199254740995");
        assert_eq!(value["source_mtime"], "9007199254740997");
        assert_eq!(value["phase"], "transcribing");
        assert_eq!(value["reason"], "worker_failed");
        assert_eq!(value["started_at_ms"], 1050);
        assert_eq!(value["updated_at_ms"], 1200);
        assert_eq!(value["restarted_count"], 6);
        assert!(value["result_ref"].is_null());
        for private in [
            "/srv/private",
            "private-boot-id",
            "private-claim-id",
            "private stdout",
            "payload",
            "checkpoint",
            "lease_expires_ms",
        ] {
            assert!(!wire.contains(private), "leaked {private}");
        }
    }

    #[test]
    fn only_the_current_live_claim_can_report_a_bounded_execution_stage() {
        let mut job = background(source());
        let payload = subtitle_payload(&job).expect(
            "valid only the current live claim can report a bounded execution stage fixture",
        );
        for checkpoint in [
            json!({"fence":1,"phase":"publishing"}),
            json!({"fence":2,"phase":"/srv/private/output"}),
        ] {
            job.checkpoint = Some(checkpoint);
            assert_eq!(
                job_public(&job, &payload, &[], 1300).expect("valid only the current live claim can report a bounded execution stage fixture")["phase"],
                "queued"
            );
        }
        job.checkpoint = Some(json!({"fence":2,"phase":"waiting_for_capacity"}));
        assert_eq!(
            job_public(&job, &payload, &[], 1300).expect("live wait stage")["phase"],
            "waiting_for_capacity"
        );
        job.checkpoint = Some(json!({"fence":2,"phase":"transcribing"}));
        job.token
            .as_mut()
            .expect(
                "valid only the current live claim can report a bounded execution stage fixture",
            )
            .lease_expires_ms = 1200;
        let expired = job_public(&job, &payload, &[], 1300).expect(
            "valid only the current live claim can report a bounded execution stage fixture",
        );
        assert_eq!(expired["phase"], "queued");
        assert_eq!(expired["reason"], "worker_interrupted");
        job.token
            .as_mut()
            .expect(
                "valid only the current live claim can report a bounded execution stage fixture",
            )
            .lease_expires_ms = 2000;
        for (state, phase) in [
            (JobState::Queued, "queued"),
            (JobState::Failed, "failed"),
            (JobState::Cancelled, "cancelled"),
            (JobState::Succeeded, "succeeded"),
        ] {
            job.state = state;
            assert_eq!(
                job_public(&job, &payload, &[], 1300).expect("valid only the current live claim can report a bounded execution stage fixture")["phase"],
                phase
            );
        }
    }

    #[test]
    fn publication_reference_is_revealed_only_after_matching_durable_success() {
        let mut job = background(source());
        let payload = subtitle_payload(&job).expect(
            "valid publication reference is revealed only after matching durable success fixture",
        );
        let artifact = artifact_key(&payload).expect(
            "valid publication reference is revealed only after matching durable success fixture",
        );
        job.result_ref = Some(artifact.clone());
        assert!(job_public(&job, &payload, &[], 1300).expect(
            "valid publication reference is revealed only after matching durable success fixture"
        )["result_ref"]
            .is_null());
        job.state = JobState::Succeeded;
        assert_eq!(
            job_public(&job, &payload, &[], 1300).expect("valid publication reference is revealed only after matching durable success fixture")["result_ref"],
            artifact
        );
        job.result_ref = Some("/srv/private/result".into());
        assert!(job_public(&job, &payload, &[], 1300).expect(
            "valid publication reference is revealed only after matching durable success fixture"
        )["result_ref"]
            .is_null());
    }

    #[test]
    fn model_jobs_have_no_source_and_cannot_leak_payload_paths_or_claim_authority() {
        let model = JobPayload::SubtitleModelOperation {
            translation_profile: None,
            action: ModelAction::Install,
            model_id: "base.en".into(),
            catalog_revision: "reviewed-catalog-v1".into(),
            model_sha256: "a".repeat(64),
            adapter_contract: "whisper.cpp-1.9.2-cpu-v1".into(),
            target_node_id: "worker-one".into(),
        };
        let mut job = background(model);
        job.state = JobState::Queued;
        job.token = None;
        let payload = subtitle_payload(&job).expect("valid model jobs have no source and cannot leak payload paths or claim authority fixture");
        let value = job_public(&job, &payload, &[], 1300).expect("valid model jobs have no source and cannot leak payload paths or claim authority fixture");
        assert_eq!(value["node_id"], "worker-one");
        for key in [
            "file_id",
            "source_size",
            "source_mtime",
            "artifact_key",
            "result_ref",
        ] {
            assert!(value[key].is_null());
        }
        assert!(!serde_json::to_string(&value)
            .expect("valid model jobs have no source and cannot leak payload paths or claim authority fixture")
            .contains("/srv/private"));
        let unrelated = background(JobPayload::LibraryScan {
            library_id: 1,
            generation: uuid::Uuid::new_v4().to_string(),
        });
        assert!(subtitle_payload(&unrelated).is_err());
        job.payload_version = 2;
        assert!(subtitle_payload(&job).is_err());
    }

    #[tokio::test]
    async fn file_options_serialize_public_tracks_and_exact_acquired_caption_indexes_without_vtt() {
        let store = SqliteStore::open_in_memory().expect("valid file options serialize public tracks and exact acquired caption indexes without vtt fixture");
        let library = store
            .create_library(&NewLibrary {
                name: "caption wire fixture".into(),
                kind: LibraryKind::Movies,
                paths: vec!["/srv/private".into()],
                anime: false,
            })
            .await
            .expect("valid file options serialize public tracks and exact acquired caption indexes without vtt fixture");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "caption wire fixture".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("valid file options serialize public tracks and exact acquired caption indexes without vtt fixture");
        let size = 9_007_199_254_740_993_i64;
        let mtime = 9_007_199_254_740_995_i64;
        let id = store
            .upsert_file(
                item,
                "/srv/private/source.mkv",
                size,
                mtime,
                &ProbeResult {
                    // Acquired captions require a completed probe, not an
                    // unprobed scanner row with only provisional track facts.
                    raw_json: Some("{}".into()),
                    duration_ms: Some(1000),
                    video_codec: Some("h264".into()),
                    audio_streams: ["por", "swe", "en-US", "und"]
                        .into_iter()
                        .enumerate()
                        .map(|(index, tag)| AudioStream {
                            index: index as i64,
                            language: Some(tag.into()),
                            ..Default::default()
                        })
                        .collect(),
                    subtitle_streams: vec![
                        SubtitleStream {
                            index: 0,
                            language: Some("fr".into()),
                            ..Default::default()
                        },
                        SubtitleStream {
                            index: 1,
                            language: Some("es".into()),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            )
            .await
            .expect("valid file options serialize public tracks and exact acquired caption indexes without vtt fixture");
        let stored = store
            .get_file(id)
            .await
            .expect("read probed source fixture")
            .expect("probed source fixture exists");
        assert_eq!((stored.size, stored.mtime), (size, mtime));
        let body = "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nPRIVATE CAPTION BODY\n";
        let provider = DownloadedSubtitle {
            text_translation: None,
            source_size: size,
            source_mtime: mtime,
            provider_file_id: 42,
            transcription: None,
            language: "de".into(),
            title: "Provider German".into(),
            hearing_impaired: false,
            forced: false,
            vtt: body.into(),
        };
        assert!(store.add_downloaded_subtitle(id, &provider).await.expect("valid file options serialize public tracks and exact acquired caption indexes without vtt fixture"));
        let generated = DownloadedSubtitle {
            provider_file_id: 0,
            transcription: Some(SubtitleTranscription {
                audio_index: 2,
                artifact_key: "c".repeat(64),
                model_sha256: "a".repeat(64),
                pipeline_digest: "b".repeat(64),
                adapter: "whisper.cpp".into(),
                execution: None,
                generated_at_ms: 1200,
            }),
            language: "en".into(),
            title: "Machine generated English".into(),
            ..provider.clone()
        };
        assert!(store.add_downloaded_subtitle(id, &generated).await.expect("valid file options serialize public tracks and exact acquired caption indexes without vtt fixture"));
        let mut file = store.get_file(id).await.expect("valid file options serialize public tracks and exact acquired caption indexes without vtt fixture").expect("valid file options serialize public tracks and exact acquired caption indexes without vtt fixture");
        assert!(serde_json::to_value(&file)
            .expect("valid file options serialize public tracks and exact acquired caption indexes without vtt fixture")
            .get("downloaded_subtitles")
            .is_none());
        let value = file_options(&file, &configuration(), vec!["en".into()]);
        assert_eq!(value["source_size"], size.to_string());
        assert_eq!(value["source_mtime"], mtime.to_string());
        assert_eq!(value["file_id"], id.to_string());
        assert_eq!(value["captions"].as_array().expect("valid file options serialize public tracks and exact acquired caption indexes without vtt fixture").len(), 1);
        assert_eq!(value["captions"][0]["index"], 3);
        assert_eq!(value["captions"][0]["artifact_key"], "c".repeat(64));
        assert_eq!(
            value["subtitle_streams"][3]["title"],
            "Machine generated English"
        );
        for (index, language) in [Some("pt"), Some("sv"), Some("en"), None]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                value["audio_tracks"][index]["normalized_language"],
                json!(language)
            );
        }
        file.downloaded_subtitles[1].source_mtime = mtime + 1;
        assert!(
            file_options(&file, &configuration(), vec!["en".into()])["captions"]
                .as_array()
                .expect("valid file options serialize public tracks and exact acquired caption indexes without vtt fixture")
                .is_empty()
        );
        let wire = serde_json::to_string(&value).expect("valid file options serialize public tracks and exact acquired caption indexes without vtt fixture");
        for private in [
            "/srv/private",
            "PRIVATE CAPTION BODY",
            "downloaded_subtitles",
            "model_path",
            "command",
            "legacy_snapshot",
        ] {
            assert!(!wire.contains(private), "leaked {private}");
        }
    }

    #[test]
    fn model_failures_keep_actionable_network_space_and_integrity_reasons_without_details() {
        for (code, reason) in [
            ("model_disk_full", "insufficient_disk"),
            ("model_disk_reservation_unavailable", "insufficient_disk"),
            ("model_download_http_failure", "model_download_failed"),
            ("artifact_digest_mismatch", "model_verification_failed"),
            ("model_self_test_failed", "model_verification_failed"),
            ("waiting_for_worker", "waiting_for_worker"),
            ("worker_source_unavailable", "worker_source_unavailable"),
            (
                "worker_observation_unavailable",
                "worker_observation_unavailable",
            ),
            ("worker_not_current", "worker_not_current"),
            ("packaged_engine_companion_changed", "engine_unavailable"),
            ("packaged_engine_changed", "engine_unavailable"),
            ("engine_staging_failed", "engine_unavailable"),
            ("model_not_installed", "missing_model"),
            ("no_speech_detected", "no_speech_detected"),
        ] {
            assert_eq!(safe_reason(Some(code)), Some(reason));
        }
        assert_eq!(
            safe_reason(Some("/srv/private/stdout with secrets")),
            Some("worker_failed")
        );
    }
}
