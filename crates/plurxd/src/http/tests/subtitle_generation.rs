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
#[tokio::test]
async fn retired_subtitle_routes_cannot_start_work_or_change_settings() {
    let (app, state) = test_app_with_state();
    let admin = setup_admin(&app).await;
    let id = subtitle_file(&state).await;
    for path in [
        "/api/v1/subtitle-transcription",
        "/api/v1/subtitle-transcription/managed",
        "/api/v1/subtitle-translation",
        "/api/v1/subtitle-text-translation/config",
    ] {
        assert_eq!(
            call(&app, get(path, Some(&admin))).await.0,
            StatusCode::NOT_FOUND,
            "{path}"
        );
        assert_eq!(
            call(&app, put(path, Some(&admin), json!({}))).await.0,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    // The generic track-delete path accepts this URL shape, but no start method.
    let transcribe = format!("/api/v1/files/{id}/subtitles/transcribe");
    assert_eq!(
        call(&app, post(&transcribe, Some(&admin), json!({})))
            .await
            .0,
        StatusCode::METHOD_NOT_ALLOWED
    );
    for path in [
        format!("/api/v1/files/{id}/subtitle-translation"),
        format!("/api/v1/files/{id}/subtitles/0/translation"),
        "/api/v1/subtitle-transcription/models/base.en/install".into(),
        "/api/v1/subtitle-text-translation/models/verify".into(),
    ] {
        let (status, _) = call(&app, post(&path, Some(&admin), json!({}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
    let (status, _) = call(&app, get("/api/v1/ai-subtitles/settings", Some(&admin))).await;
    assert_eq!(status, StatusCode::OK);
}
