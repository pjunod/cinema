use super::*;
use plurx_core::store::keys;

#[tokio::test]
async fn capacity_api_requires_admin_and_atomic_revision_and_preserves_untouched_echoes() {
    let (app, state) = test_app_with_state();
    let admin = setup_admin(&app).await;
    let (status, before) = call(&app, get("/api/v1/settings", Some(&admin))).await;
    assert_eq!(status, StatusCode::OK);
    let revision = before["transcode_capacity_revision"]
        .as_i64()
        .expect("verified schema revision");
    let (status, body) = call(
        &app,
        put(
            "/api/v1/settings",
            Some(&admin),
            json!({
                "transcode_max_hw_sessions":before["transcode_max_hw_sessions"],
                "transcode_software_pool_threads":before["transcode_software_pool_threads"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["transcode_capacity_revision"], revision);
    assert!(state
        .store
        .get_setting(keys::MAX_HW_SESSIONS)
        .await
        .expect("stored cap")
        .is_none());
    assert!(state
        .store
        .get_setting(keys::SW_POOL_THREADS)
        .await
        .expect("stored cap")
        .is_none());
    let update = json!({"expected_revision":revision,"hardware_sessions":{"mode":"explicit","value":0},"software_pool_threads":{"mode":"automatic"}});
    let (status, _) = call(
        &app,
        put("/api/v1/settings/transcode-capacity", None, update.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, after) = call(
        &app,
        put(
            "/api/v1/settings/transcode-capacity",
            Some(&admin),
            update.clone(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(
        after["transcode_capacity_settings"]["hardware_sessions"]["state"],
        "explicit"
    );
    assert_eq!(
        after["transcode_capacity_settings"]["hardware_sessions"]["stored_value"],
        0
    );
    assert_eq!(
        after["transcode_capacity_settings"]["software_pool_threads"]["state"],
        "automatic"
    );
    assert!(state
        .store
        .get_setting(keys::SW_POOL_THREADS)
        .await
        .expect("no replicated core count")
        .is_none());
    let (status, _) = call(
        &app,
        put("/api/v1/settings/transcode-capacity", Some(&admin), update),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    // A numeric echo is not an implicit Explicit choice, nor a mode reset.
    let (status, echo) = call(
        &app,
        put(
            "/api/v1/settings",
            Some(&admin),
            json!({
                "transcode_max_hw_sessions":after["transcode_max_hw_sessions"],
                "transcode_software_pool_threads":after["transcode_software_pool_threads"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{echo}");
    assert_eq!(
        echo["transcode_capacity_revision"],
        after["transcode_capacity_revision"]
    );
    assert_eq!(
        echo["transcode_capacity_settings"],
        after["transcode_capacity_settings"]
    );
}

#[tokio::test]
async fn capacity_api_rejects_invalid_choices_before_any_write() {
    let (app, _) = test_app_with_state();
    let admin = setup_admin(&app).await;
    let (_, before) = call(&app, get("/api/v1/settings", Some(&admin))).await;
    let revision = before["transcode_capacity_revision"]
        .as_i64()
        .expect("revision");
    for update in [
        json!({"expected_revision":revision}),
        json!({"expected_revision":-1,"hardware_sessions":{"mode":"automatic"}}),
        json!({"expected_revision":revision,"hardware_sessions":{"mode":"explicit","value":u64::MAX}}),
    ] {
        let (status, body) = call(
            &app,
            put("/api/v1/settings/transcode-capacity", Some(&admin), update),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let (_, after) = call(&app, get("/api/v1/settings", Some(&admin))).await;
        assert_eq!(after["transcode_capacity_revision"], revision);
        assert_eq!(
            after["transcode_capacity_settings"],
            before["transcode_capacity_settings"]
        );
    }
}
