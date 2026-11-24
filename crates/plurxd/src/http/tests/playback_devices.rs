//! Playback devices (Curator ADR 0026 §5, §7): a signed-in device's
//! capabilities are kept from its decision request, listed for the account
//! and for a `playback:read` key, and turned into a per-version "plays on"
//! line on the title page.
use super::*;

async fn login(app: &Router, device: &str) -> String {
    let (status, body) = call(
        app,
        post(
            "/api/v1/auth/login",
            None,
            json!({"username": "paul", "password": "supersecret", "device": device}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["token"].as_str().expect("token").to_owned()
}

fn fel_player_caps() -> Value {
    json!({"caps": {"v": 2,
        "client": {"kind": "linux-player", "build": "test", "ua": "test"},
        "video": [{"codec": "hevc", "profiles": ["main10"], "dv_profiles": [5, 7, 8],
                   "present": ["pq", "hlg"]}, {"codec": "h264"}],
        "audio": ["aac", "truehd"], "containers": ["mkv", "mp4"],
        "display": {"hdr": true, "dolby_vision": true}}})
}

async fn decide(app: &Router, token: &str, file: i64, caps: Value) {
    let (status, body) = call(
        app,
        post(&format!("/api/v1/files/{file}/decision"), Some(token), caps),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_devices_reported_capabilities_are_listed_for_its_account() {
    let (app, state, _fixture) = test_app_with_fixture_store();
    let _admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;
    let living_room = login(&app, "Living room").await;

    let (status, body) = call(&app, get("/api/v1/playback/devices", Some(&living_room))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["devices"],
        json!([]),
        "nothing is known before a device reports"
    );

    decide(&app, &living_room, seed.file, fel_player_caps()).await;
    // The same abilities again write nothing new; the listing is unchanged.
    decide(&app, &living_room, seed.file, fel_player_caps()).await;

    let (status, body) = call(&app, get("/api/v1/playback/devices", Some(&living_room))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let devices = body["devices"].as_array().expect("devices");
    assert_eq!(devices.len(), 1, "{body}");
    let d = &devices[0];
    assert_eq!(d["device"], "Living room");
    assert_eq!(d["client"], "linux-player");
    assert_eq!(d["user"], "paul");
    assert_eq!(d["hdr"]["dolby_vision"], true);
    assert_eq!(d["hdr"]["fel"], true);
    assert_eq!(d["hdr"]["profile5"], true);
    assert_eq!(d["hdr"]["dv_profiles"], json!([5, 7, 8]));
    assert_eq!(d["hdr"]["hlg"], true);
    assert_eq!(d["hdr"]["hdr10plus"], Value::Null);

    // The title page says how each version plays on this device.
    let item_id = state
        .store
        .get_file(seed.file)
        .await
        .expect("file")
        .expect("file")
        .item_id;
    let (status, item) = call(
        &app,
        get(&format!("/api/v1/items/{item_id}"), Some(&living_room)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{item}");
    let plays_on = &item["files"][0]["plays_on"];
    assert_eq!(plays_on[0]["device"], "Living room", "{item}");
    assert!(
        plays_on[0]["label"].as_str().is_some_and(|l| !l.is_empty()),
        "{item}"
    );
    assert!(plays_on[0]["method"].is_string(), "{item}");
}

#[tokio::test]
async fn curator_reads_every_device_with_a_playback_key_and_nothing_with_another() {
    let (app, state, _fixture) = test_app_with_fixture_store();
    let admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;
    let phone = login(&app, "Phone").await;
    decide(&app, &phone, seed.file, fel_player_caps()).await;

    let key = |scopes: Value| {
        let app = app.clone();
        let admin = admin.clone();
        async move {
            let (status, created) = call(
                &app,
                post(
                    "/api/v1/keys",
                    Some(&admin),
                    json!({"name": "curator", "scopes": scopes}),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{created}");
            created["key_secret"].as_str().expect("secret").to_owned()
        }
    };
    let reader = key(json!(["playback:read"])).await;
    let (status, body) = call(&app, get("/api/v1/playback/devices", Some(&reader))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["devices"][0]["device"], "Phone", "{body}");

    let scanner = key(json!(["scan:trigger"])).await;
    let (status, _) = call(&app, get("/api/v1/playback/devices", Some(&scanner))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&app, get("/api/v1/playback/devices", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_signed_out_device_leaves_no_record_behind() {
    let (app, state, _fixture) = test_app_with_fixture_store();
    let _admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;
    let old = login(&app, "Old TV").await;
    decide(&app, &old, seed.file, fel_player_caps()).await;
    let (status, _) = call(&app, post("/api/v1/auth/logout", Some(&old), json!({}))).await;
    assert!(status.is_success(), "logout: {status}");

    let fresh = login(&app, "New TV").await;
    let (_, body) = call(&app, get("/api/v1/playback/devices", Some(&fresh))).await;
    assert_eq!(
        body["devices"],
        json!([]),
        "a signed-out device is not reported: {body}"
    );

    // The next device to report prunes the stale record from the store too.
    decide(&app, &fresh, seed.file, fel_player_caps()).await;
    let stored = state.store.settings_snapshot().await.expect("settings");
    let records = stored
        .keys()
        .filter(|k| k.starts_with("playback.device_caps."))
        .count();
    assert_eq!(records, 1, "{stored:?}");
}
