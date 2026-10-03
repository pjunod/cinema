use super::*;

#[tokio::test]
async fn source_start_budget_uses_actual_vod_settings_and_admission_policy() {
    let store: Arc<dyn Store> =
        Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("actual settings store"));
    let directory = crate::test_tempdir().expect("work directory");
    let manager = TranscodeManager::new(
        Arc::clone(&store),
        directory.path().to_owned(),
        EncoderCaps::default(),
        Pipeline::Cpu,
    );
    let request = SessionRequest {
        quality_catalog: None,
        candidate_context: None,
        control_sequence: None,
        file_id: 1,
        playback_id: "source-start-budget".into(),
        request_id: None,
        automatic: false,
        previous_session_id: None,
        reopen_reason: None,
        kind: SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        },
        start_seconds: 0.0,
        audio_index: None,
        subtitle_burn: None,
        audio_offset_ms: 0,
        hdr10: false,
        presentation: Default::default(),
        block_budget_secs: Some(0.001),
        transport: None,
    };
    for (stored, seconds) in [
        (None, 30),
        (Some("42"), 42),
        (Some("999"), 300),
        (Some("NaN"), 30),
    ] {
        if let Some(value) = stored {
            store
                .put_setting(keys::VOD_MATERIALIZE_BUDGET_SECS, value)
                .await
                .expect("set materialization budget");
        }
        assert_eq!(
            manager
                .source_start_budget_for_request(&request)
                .await
                .expect("budget"),
            crate::admission::QUEUE_WAIT + Duration::from_secs(seconds)
        );
    }
    store
        .put_setting(keys::VOD_PRESENTATION, " 0 ")
        .await
        .expect("maintenance");
    assert!(manager
        .source_start_budget_for_request(&request)
        .await
        .is_err());
    assert!(
        manager.vod.session_ids().await.is_empty(),
        "budget observation allocates no session"
    );
    assert_eq!(manager.admissions.software_in_use(), 0);
}
