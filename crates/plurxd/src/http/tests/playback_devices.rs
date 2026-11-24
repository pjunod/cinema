//! Playback devices (Curator ADR 0026 §5, §7): a signed-in device's
//! capabilities are kept from its successful decision request, as a record
//! of its login token that goes with the token; listed for the account and
//! for a `playback:read` key; and turned into a per-version "plays on" line
//! on the title page by the same verdict Play reaches.
use super::*;

async fn login_as(app: &Router, username: &str, password: &str, device: &str) -> String {
    let (status, body) = call(
        app,
        post(
            "/api/v1/auth/login",
            None,
            json!({"username": username, "password": password, "device": device}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["token"].as_str().expect("token").to_owned()
}

async fn login(app: &Router, device: &str) -> String {
    login_as(app, "paul", "supersecret", device).await
}

fn fel_player_caps() -> Value {
    json!({"caps": {"v": 2,
        "client": {"kind": "linux-player", "build": "test", "ua": "test"},
        "video": [{"codec": "hevc", "profiles": ["main10"], "dv_profiles": [5, 7, 8],
                   "present": ["pq", "hlg"]}, {"codec": "h264"}],
        "audio": ["aac", "truehd"], "containers": ["mkv", "mp4"],
        "display": {"hdr": true, "dolby_vision": true}}})
}

async fn decide(app: &Router, token: &str, file: i64, caps: Value) -> Value {
    let (status, body) = call(
        app,
        post(&format!("/api/v1/files/{file}/decision"), Some(token), caps),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn listed(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, get("/api/v1/playback/devices", Some(token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["devices"].clone()
}

#[tokio::test]
async fn a_devices_reported_capabilities_are_listed_for_its_account() {
    let (app, state, fixture) = test_app_with_fixture_store();
    let _admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;
    let living_room = login(&app, "Living room").await;

    assert_eq!(
        listed(&app, &living_room).await,
        json!([]),
        "nothing is known before a device reports"
    );

    decide(&app, &living_room, seed.file, fel_player_caps()).await;
    let first = listed(&app, &living_room).await[0]["recorded_at"].clone();
    // The same abilities again, from another build and user agent, write
    // nothing new: the record keeps only what changes a verdict.
    let mut again = fel_player_caps();
    again["caps"]["client"] = json!({"kind": "linux-player", "build": "next", "ua": "other"});
    decide(&app, &living_room, seed.file, again).await;
    assert_eq!(fixture.fixture_device_caps_rows().await.expect("rows"), 1);

    let devices = listed(&app, &living_room).await;
    let devices = devices.as_array().expect("devices");
    assert_eq!(devices.len(), 1, "{devices:?}");
    let d = &devices[0];
    assert_eq!(d["device"], "Living room");
    assert_eq!(d["client"], "linux-player");
    assert_eq!(d["user"], "paul");
    assert_eq!(
        d["recorded_at"], first,
        "an unchanged document is not rewritten"
    );
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
    // The key in the header every *arr application uses answers the same.
    let mut request = get("/api/v1/playback/devices", None);
    request
        .headers_mut()
        .insert("x-api-key", reader.parse().expect("header"));
    let (status, body) = call(&app, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["devices"][0]["device"], "Phone", "{body}");

    let scanner = key(json!(["scan:trigger"])).await;
    let (status, _) = call(&app, get("/api/v1/playback/devices", Some(&scanner))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&app, get("/api/v1/playback/devices", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// The record is the sign-in's: logout, revoking the device from another,
/// a password reset and deleting the account each leave no row in the
/// store — not merely an unlisted one — with no step of their own.
#[tokio::test]
async fn every_way_a_sign_in_ends_takes_its_device_record_with_it() {
    let (app, state, fixture) = test_app_with_fixture_store();
    let admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;
    let rows = || async { fixture.fixture_device_caps_rows().await.expect("rows") };

    // Logout.
    let old = login(&app, "Old TV").await;
    decide(&app, &old, seed.file, fel_player_caps()).await;
    assert_eq!(rows().await, 1);
    let (status, _) = call(&app, post("/api/v1/auth/logout", Some(&old), json!({}))).await;
    assert!(status.is_success(), "logout: {status}");
    assert_eq!(rows().await, 0, "logout");

    // Revoking a device from another of the account's devices.
    let tv = login(&app, "Living room").await;
    decide(&app, &tv, seed.file, fel_player_caps()).await;
    let devices = listed(&app, &admin).await;
    let prefix = devices[0]["id"].as_str().expect("prefix").to_owned();
    let (status, body) = call(
        &app,
        delete(&format!("/api/v1/me/devices/{prefix}"), Some(&admin)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(rows().await, 0, "device revoke");

    // A password reset signs every device of the account out.
    let (status, created) = call(
        &app,
        post(
            "/api/v1/users",
            Some(&admin),
            json!({"username": "kim", "password": "kimsecret1"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let kim = created["id"].as_i64().expect("user id");
    for device in ["Kim's phone", "Kim's tablet"] {
        let token = login_as(&app, "kim", "kimsecret1", device).await;
        decide(&app, &token, seed.file, fel_player_caps()).await;
    }
    assert_eq!(rows().await, 2);
    let (status, body) = call(
        &app,
        put(
            &format!("/api/v1/users/{kim}"),
            Some(&admin),
            json!({"password": "kimsecret2"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(rows().await, 0, "password reset");

    // Deleting the account.
    let token = login_as(&app, "kim", "kimsecret2", "Kim's laptop").await;
    decide(&app, &token, seed.file, fel_player_caps()).await;
    assert_eq!(rows().await, 1);
    let (status, body) = call(&app, delete(&format!("/api/v1/users/{kim}"), Some(&admin))).await;
    assert!(status.is_success(), "delete user: {status} {body}");
    assert_eq!(rows().await, 0, "user deletion");
}

/// An expired sign-in is listed no more than it authenticates.
#[tokio::test]
async fn an_idle_expired_sign_in_is_not_listed() {
    let (app, state, fixture) = test_app_with_fixture_store();
    let admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;
    let tv = login(&app, "Living room").await;
    decide(&app, &tv, seed.file, fel_player_caps()).await;
    let now = users::unix_now();
    users::start_token_expiry_clock(state.store.as_ref(), now - 200 * 86_400)
        .await
        .expect("start the clock");
    assert!(fixture
        .fixture_set_token_last_seen(&plurx_core::auth::hash_token(&tv), now - 91 * 86_400)
        .await
        .expect("age the living-room box"));
    assert_eq!(listed(&app, &admin).await, json!([]));
}

/// A document the decision refused, or one sent for a file that does not
/// exist, is not kept: only a decision that succeeded records its caps.
#[tokio::test]
async fn a_capability_document_the_decision_refused_is_not_recorded() {
    let (app, state, fixture) = test_app_with_fixture_store();
    let _admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;
    let tv = login(&app, "Living room").await;

    let mut refused = fel_player_caps();
    refused["caps"]["decoder_compaction"] = json!("not-a-contract");
    let (status, body) = call(
        &app,
        post(
            &format!("/api/v1/files/{}/decision", seed.file),
            Some(&tv),
            refused,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = call(
        &app,
        post(
            "/api/v1/files/999999/decision",
            Some(&tv),
            fel_player_caps(),
        ),
    )
    .await;
    assert!(status.is_client_error(), "{status} {body}");

    assert_eq!(fixture.fixture_device_caps_rows().await.expect("rows"), 0);
    assert_eq!(listed(&app, &tv).await, json!([]));
}

/// The title page's line is Play's verdict, not a second opinion: a 4K
/// Profile 7 version on a device whose decoder stops at 1080p transcodes on
/// Play, and the title page says so.
#[tokio::test]
async fn plays_on_reaches_the_verdict_play_does_for_a_4k_profile_7_on_a_1080p_device() {
    use plurx_core::domain::{AudioStream, DolbyVisionFacts, ItemKind, NewItem, ProbeResult};

    let (app, state, _fixture) = test_app_with_fixture_store();
    let _admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;
    let movie = state
        .store
        .insert_item(&NewItem {
            library_id: seed.lib,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Dune".into(),
            year: Some(2021),
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("movie");
    let dir = crate::test_temp_path(format!("plurx-media-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("media dir");
    let path = dir.join("Dune.mkv");
    std::fs::write(&path, b"\x1a\x45\xdf\xa3 profile 7 placeholder").expect("write");
    let metadata = std::fs::metadata(&path).expect("metadata");
    let file = state
        .store
        .upsert_file(
            movie,
            &path.to_string_lossy(),
            metadata.len() as i64,
            metadata
                .modified()
                .expect("mtime")
                .duration_since(std::time::UNIX_EPOCH)
                .expect("epoch")
                .as_secs() as i64,
            &ProbeResult {
                duration_ms: Some(9_000_000),
                container: Some("mkv".into()),
                video_codec: Some("hevc".into()),
                video_profile: Some("Main 10".into()),
                width: Some(3840),
                height: Some(2160),
                bit_depth: Some(10),
                hdr: Some("dolby_vision".into()),
                hdr_format: Some("Dolby Vision · Profile 7 (HDR10-compatible)".into()),
                dolby_vision: DolbyVisionFacts {
                    profile: Some(7),
                    level: Some(6),
                    bl_compat_id: Some(1),
                    el_present: Some(true),
                    rpu_present: Some(true),
                },
                bitrate: Some(60_000_000),
                audio_streams: vec![AudioStream {
                    index: 0,
                    codec: "aac".into(),
                    channels: Some(2),
                    language: Some("eng".into()),
                    default: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .await
        .expect("Profile 7 source");

    let hd_tv = json!({"caps": {"v": 2,
        "client": {"kind": "android", "build": "109"},
        "video": [{"codec": "hevc", "profiles": ["main10"], "max_height": 1080,
                   "present": ["sdr", "pq"], "dv_profiles": [5, 8]},
                  {"codec": "h264", "max_height": 1080}],
        "audio": ["aac"], "containers": ["mp4", "mkv"], "transports": ["hls"],
        "display": {"hdr": true, "dolby_vision": true}}});
    let tv = login(&app, "1080p TV").await;
    let play = decide(&app, &tv, file, hd_tv).await;
    assert_eq!(play["method"], "transcode", "{play}");

    let (status, item) = call(&app, get(&format!("/api/v1/items/{movie}"), Some(&tv))).await;
    assert_eq!(status, StatusCode::OK, "{item}");
    let line = &item["files"][0]["plays_on"][0];
    assert_eq!(line["device"], "1080p TV", "{item}");
    assert_eq!(line["method"], play["method"], "{item}");
    assert_eq!(
        line["full_quality"], false,
        "a re-encode is a fallback: {item}"
    );
}
