use super::*;
use plurx_core::domain::{AudioStream, ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};

async fn subtitle_file(state: &AppState) -> i64 {
    let library = state
        .store
        .create_library(&NewLibrary {
            name: "Subtitle route".into(),
            kind: LibraryKind::Movies,
            paths: vec!["/media/subtitles".into()],
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
            title: "Subtitle route".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    state
        .store
        .upsert_file(
            item,
            "/media/subtitles/source.mkv",
            9_007_199_254_740_993,
            9_007_199_254_740_995,
            &ProbeResult {
                duration_ms: Some(1000),
                video_codec: Some("h264".into()),
                audio_streams: vec![
                    AudioStream {
                        index: 0,
                        language: Some("en".into()),
                        default: true,
                        ..Default::default()
                    },
                    AudioStream {
                        index: 7,
                        language: Some("eng".into()),
                        ..Default::default()
                    },
                    AudioStream {
                        index: 2,
                        language: Some(" UND ".into()),
                        ..Default::default()
                    },
                ],
                raw_json: Some("{}".into()),
                ..Default::default()
            },
        )
        .await
        .expect("source")
}
async fn enable_managed(app: &Router, admin: &str) {
    let (status, current) = call(
        app,
        get("/api/v1/subtitle-transcription/managed", Some(admin)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status,_)=call(app,put("/api/v1/subtitle-transcription/managed",Some(admin),json!({"version":1,"expected_revision":current["revision"],"mode":"managed","enabled":true,"interval_mins":0,"language":"en","model_id":"base.en"}))).await;
    assert_eq!(status, StatusCode::OK);
}
#[tokio::test]
async fn managed_generation_route_returns_exact_file_job_and_source_and_preserves_selected_audio() {
    let (app, state) = test_app_with_state();
    let admin = setup_admin(&app).await;
    let id = subtitle_file(&state).await;
    enable_managed(&app, &admin).await;
    let uri = format!("/api/v1/files/{id}/subtitles/transcribe");
    let (status, options) = call(&app, get(&uri, Some(&admin))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        options["audio_tracks"][2]["normalized_language"],
        Value::Null
    );
    let request = |audio| json!({"version":1,"audio_index":audio,"language":"en","model_id":"base.en","source_size":options["source_size"],"source_mtime":options["source_mtime"]});
    let (status, accepted) = call(&app, post(&uri, Some(&admin), request(7))).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["file_id"], id.to_string());
    assert_eq!(accepted["source_size"], "9007199254740993");
    assert_eq!(accepted["source_mtime"], "9007199254740995");
    assert_eq!(
        accepted["artifact_key"].as_str().expect("artifact").len(),
        64
    );
    let job_id = accepted["job_id"].as_str().expect("exact durable job");
    let job = state
        .store
        .background_job(job_id)
        .await
        .expect("read job")
        .expect("job");
    assert!(
        matches!(job.supported_payload().expect("closed payload"),plurx_core::store::background_jobs::JobPayload::SubtitleTranscribeManaged{audio_index:7,language,..} if language=="en")
    );
    let (status, untagged) = call(&app, post(&uri, Some(&admin), request(2))).await;
    assert_eq!(status, StatusCode::OK, "{untagged}");
    assert_ne!(untagged["artifact_key"], accepted["artifact_key"]);
    let (status, replayed) = call(&app, post(&uri, Some(&admin), request(7))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replayed["job_id"], accepted["job_id"]);
    let mut stale = request(7);
    stale["source_size"] = json!("9007199254740994");
    assert_eq!(
        call(&app, post(&uri, Some(&admin), stale)).await.0,
        StatusCode::CONFLICT
    );
    let mut unknown = request(7);
    unknown["version"] = json!(2);
    assert_eq!(
        call(&app, post(&uri, Some(&admin), unknown)).await.0,
        StatusCode::CONFLICT
    );
    let mut extra = request(7);
    extra["model_path"] = json!("/arbitrary/private");
    assert_eq!(
        call(&app, post(&uri, Some(&admin), extra)).await.0,
        StatusCode::BAD_REQUEST
    );
    let (status, safe) = call(
        &app,
        get(
            &format!("/api/v1/subtitle-transcription/jobs/{job_id}"),
            Some(&admin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(safe["artifact_key"], accepted["artifact_key"]);
    assert!(!safe.to_string().contains("/media/subtitles"));
}
#[tokio::test]
async fn subtitle_model_install_routes_are_admin_only_and_return_per_member_refusals() {
    let (app, state) = test_app_with_state();
    let uri = "/api/v1/subtitle-transcription/models/base.en/install";
    assert_eq!(
        call(
            &app,
            post(uri, None, json!({"version":1,"node_ids":["test-node"]}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let admin = setup_admin(&app).await;
    for ids in [json!([]), json!(["test-node", "test-node"])] {
        assert_eq!(
            call(
                &app,
                post(uri, Some(&admin), json!({"version":1,"node_ids":ids}))
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let (status, result) = call(
        &app,
        post(
            uri,
            Some(&admin),
            json!({"version":1,"node_ids":["test-node","not-a-current-member"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["operations"][0]["outcome"], "accepted");
    assert!(result["operations"][0]["job_id"].is_string());
    assert_eq!(result["operations"][1]["outcome"], "worker_not_current");
    assert!(result["operations"][1]["job_id"].is_null());
    let job = state
        .store
        .background_job(result["operations"][0]["job_id"].as_str().expect("job ID"))
        .await
        .expect("read")
        .expect("queued");
    assert_eq!(
        job.state,
        plurx_core::store::background_jobs::JobState::Queued
    );
}
#[tokio::test]
async fn translation_model_install_and_test_return_the_setup_operation_contract() {
    use plurx_core::store::background_jobs::JobPayload;
    use plurx_core::store::background_jobs_transcription::ModelAction;

    let (app, state) = test_app_with_state();
    let admin = setup_admin(&app).await;
    let profile = crate::subtitle_translation::profile::selected_profile();
    let profile_id = profile.id().expect("profile identity");
    for (route, action) in [
        ("install", ModelAction::TranslationInstall),
        ("verify", ModelAction::TranslationVerify),
    ] {
        let uri = format!("/api/v1/subtitle-transcription/models/small/{route}");
        let request = json!({
            "version":2, "purpose":"translation", "profile_id":profile_id,
            "request_id":uuid::Uuid::new_v4().to_string(),
            "intent_expires_at_ms":crate::state::clock_ms()+60000,
            "node_ids":["test-node","other-node"]
        });
        let (status, result) = call(&app, post(&uri, Some(&admin), request.clone())).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        // These are the discriminators the shipped setup validates before it
        // follows the returned durable jobs. A successful enqueue must not
        // become an "Invalid model operation response" in the browser.
        assert_eq!(result["version"], 2);
        assert_eq!(result["purpose"], "translation");
        assert_eq!(result["model_id"], "small");
        assert_eq!(result["profile_id"], profile_id);
        assert_eq!(
            result["intent_expires_at_ms"],
            request["intent_expires_at_ms"]
        );
        assert!(result["server_now_ms"].is_i64());
        let rows = result["operations"].as_array().expect("operation rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["outcome"], "accepted");
        assert_eq!(rows[0]["node_id"], "test-node");
        assert_eq!(rows[1]["outcome"], "worker_not_current");
        assert!(rows[1]["job_id"].is_null());
        let job_id = rows[0]["job_id"].as_str().expect("durable job ID");
        uuid::Uuid::parse_str(job_id).expect("browser-compatible job ID");
        let job = state
            .store
            .background_job(job_id)
            .await
            .expect("read")
            .expect("job");
        assert!(matches!(job.supported_payload().expect("payload"),
            JobPayload::SubtitleModelOperation { action: actual, translation_profile: Some(actual_profile), .. }
            if actual == action && actual_profile == profile));
        let (status, replayed) = call(&app, post(&uri, Some(&admin), request)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replayed["purpose"], "translation");
        assert_eq!(replayed["operations"][0]["job_id"], job_id);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn advanced_generation_preserves_empty_body_and_selected_v1_requests() {
    use std::os::unix::fs::PermissionsExt;
    let (app, state) = test_app_with_state();
    let admin = setup_admin(&app).await;
    let id = subtitle_file(&state).await;
    let temporary = crate::test_tempdir().expect("private Advanced fixture");
    let command = temporary.path().join("recognizer");
    std::fs::write(&command, b"#!/bin/sh\nexit 0\n").expect("fixture command");
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700)).expect("executable");
    let model = temporary.path().join("model.bin");
    let mut bytes = 0x67676d6c_u32.to_le_bytes().to_vec();
    bytes.extend_from_slice(&51864_u32.to_le_bytes());
    std::fs::write(&model, bytes).expect("English header fixture");
    let (status,_)=call(&app,put("/api/v1/subtitle-transcription",Some(&admin),json!({"enabled":true,"interval_mins":0,"command":command,"model_path":model,"language":"en"}))).await;
    assert_eq!(status, StatusCode::OK);
    let uri = format!("/api/v1/files/{id}/subtitles/transcribe");
    let (status, legacy) = call(&app, post(&uri, Some(&admin), json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{legacy}");
    assert!(legacy["job_id"].is_string());
    let (status,selected)=call(&app,post(&uri,Some(&admin),json!({"version":1,"audio_index":7,"language":"en","model_id":"advanced","source_size":"9007199254740993","source_mtime":"9007199254740995"}))).await;
    assert_eq!(status, StatusCode::OK, "{selected}");
    assert_eq!(selected["file_id"], id.to_string());
    let job = state
        .store
        .background_job(selected["job_id"].as_str().expect("job"))
        .await
        .expect("read")
        .expect("job");
    assert!(matches!(
        job.supported_payload().expect("payload"),
        plurx_core::store::background_jobs::JobPayload::SubtitleTranscribe { audio_index: 7, .. }
    ));
    let advanced_job_uri = format!(
        "/api/v1/subtitle-transcription/jobs/{}",
        selected["job_id"].as_str().expect("job")
    );
    assert_eq!(
        call(&app, get(&advanced_job_uri, Some(&admin))).await.1["reason"],
        "waiting_for_worker"
    );
    enable_managed(&app, &admin).await;
    assert_eq!(
        call(&app, get(&advanced_job_uri, Some(&admin))).await.1["reason"],
        "legacy_profile_incompatible"
    );
    let (status, managed) = call(&app, post(&uri, Some(&admin), json!({"version":1,"audio_index":7,"language":"en","model_id":"base.en","source_size":"9007199254740993","source_mtime":"9007199254740995"}))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, put("/api/v1/subtitle-transcription", Some(&admin), json!({"enabled":true,"interval_mins":0,"command":command,"model_path":model,"language":"en"}))).await;
    assert_eq!(status, StatusCode::OK);
    let managed_job_uri = format!(
        "/api/v1/subtitle-transcription/jobs/{}",
        managed["job_id"].as_str().expect("managed job")
    );
    assert_eq!(
        call(&app, get(&managed_job_uri, Some(&admin))).await.1["reason"],
        "disabled"
    );
}
