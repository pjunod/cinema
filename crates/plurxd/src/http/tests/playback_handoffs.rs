//! Playback handoff (Linux player plan §3.1, M1): mint, redeem, scope,
//! revocation and expiry, through the real router.
use super::*;

const DEVICE: &str = "plurx Player (Linux)";
const DECISION_QUERY: &str = "vcodec=h264&acodec=aac&container=mp4&hdr=0";

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
}

/// Sign in again as the setup admin, as a second device would.
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

async fn mint(app: &Router, bearer: &str, file_id: i64) -> (String, String) {
    let (status, body) = call(
        app,
        post(
            "/api/v1/playback-handoffs",
            Some(bearer),
            json!({"file_id": file_id}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    (
        body["id"].as_str().expect("id").to_owned(),
        body["code"].as_str().expect("code").to_owned(),
    )
}

async fn redeem(app: &Router, code: &str) -> (StatusCode, Value) {
    call(
        app,
        post(
            "/api/v1/playback-handoffs/redeem",
            None,
            json!({"code": code, "device": DEVICE}),
        ),
    )
    .await
}

async fn redeemed_token(app: &Router, code: &str) -> String {
    let (status, body) = redeem(app, code).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["token"].as_str().expect("token").to_owned()
}

async fn decision_status(app: &Router, token: &str, file_id: i64) -> StatusCode {
    call(
        app,
        get(
            &format!("/api/v1/files/{file_id}/decision?{DECISION_QUERY}"),
            Some(token),
        ),
    )
    .await
    .0
}

async fn devices(app: &Router, bearer: &str) -> Vec<Value> {
    let (status, body) = call(app, get("/api/v1/me/devices", Some(bearer))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body.as_array().expect("device array").clone()
}

fn lists_device(rows: &[Value]) -> bool {
    rows.iter().any(|row| row["device"] == DEVICE)
}

/// The seeded file's own item, and a second file on another item.
async fn own_item_and_other_file(state: &AppState, seed: &Seed) -> (i64, i64, i64) {
    let own_item = state
        .store
        .get_file(seed.file)
        .await
        .expect("file")
        .expect("seeded file")
        .item_id;
    let other_item = if own_item == seed.movie {
        seed.ep
    } else {
        seed.movie
    };
    let other = state
        .store
        .upsert_file(
            other_item,
            "/media/handoff-other.mp4",
            1,
            1,
            &plurx_core::domain::ProbeResult::default(),
        )
        .await
        .expect("other file");
    assert_ne!(other, seed.file);
    (own_item, other_item, other)
}

#[tokio::test]
async fn handoff_mint_redeem_hands_off_one_file_once_and_lists_the_device() {
    let (app, state, _fixture) = test_app_with_fixture_store();
    let admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/playback-handoffs")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {admin}"))
        .header("origin", "http://media1:8096")
        .body(Body::from(
            json!({"file_id": seed.file, "start_ms": 90_000, "audio": 0, "subtitle": 1})
                .to_string(),
        ))
        .expect("mint request");
    let (status, minted) = call(&app, request).await;
    assert_eq!(status, StatusCode::CREATED, "{minted}");
    assert_eq!(minted["expires_in"], 120);
    let id = minted["id"].as_str().expect("id").to_owned();
    let code = minted["code"].as_str().expect("code").to_owned();
    assert!(plurx_core::auth::is_handoff_code(&code), "{code}");

    let status_uri = format!("/api/v1/playback-handoffs/{id}");
    let (status, body) = call(&app, get(&status_uri, Some(&admin))).await;
    assert_eq!((status, body), (StatusCode::OK, json!({"redeemed": false})));

    let (status, redeemed) = redeem(&app, &code).await;
    assert_eq!(status, StatusCode::OK, "{redeemed}");
    let token = redeemed["token"].as_str().expect("token");
    assert_eq!(token.len(), 64);
    assert_ne!(token, admin);
    assert_eq!(redeemed["file_id"], seed.file);
    let item = state
        .store
        .get_file(seed.file)
        .await
        .expect("file")
        .expect("seeded file")
        .item_id;
    assert_eq!(redeemed["item_id"], item);
    assert_eq!(redeemed["start_ms"], 90_000);
    assert_eq!(redeemed["audio"], 0);
    assert_eq!(redeemed["subtitle"], 1);
    assert_eq!(redeemed["user"]["username"], "paul");
    assert_eq!(redeemed["origin"], "http://media1:8096");
    let expires_at = redeemed["expires_at"].as_i64().expect("expiry");
    assert!(
        (expires_at - now_secs() - 6 * 60 * 60).abs() <= 5,
        "{expires_at}"
    );

    let (status, body) = call(&app, get(&status_uri, Some(&admin))).await;
    assert_eq!((status, body), (StatusCode::OK, json!({"redeemed": true})));
    assert!(lists_device(&devices(&app, &admin).await));

    // Single use: the same code again is 410 in the typed envelope.
    let (status, body) = redeem(&app, &code).await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    assert_eq!(body["code"], "handoff_gone");

    // Only the minting bearer may read the status; another session of the
    // same user may not.
    let other = login(&app, "other browser").await;
    let (status, _) = call(&app, get(&status_uri, Some(&other))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // An unknown or malformed code was never minted.
    let unknown = plurx_core::auth::generate_handoff_code().expect("code");
    assert_eq!(redeem(&app, &unknown).await.0, StatusCode::NOT_FOUND);
    assert_eq!(redeem(&app, "not-a-code").await.0, StatusCode::NOT_FOUND);
    // Minting needs a real file and a bearer.
    let (status, _) = call(
        &app,
        post(
            "/api/v1/playback-handoffs",
            Some(&admin),
            json!({"file_id": seed.file + 10_000}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        post(
            "/api/v1/playback-handoffs",
            None,
            json!({"file_id": seed.file}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// The pre-play picker sends subtitles Off as `-1`, which `/decision` also
/// reads as Off; the handoff carries it through. Other negatives are refused.
#[tokio::test]
async fn handoff_carries_subtitles_off_as_minus_one() {
    let (app, state, _fixture) = test_app_with_fixture_store();
    let admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;
    let (status, minted) = call(
        &app,
        post(
            "/api/v1/playback-handoffs",
            Some(&admin),
            json!({"file_id": seed.file, "audio": 0, "subtitle": -1}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{minted}");
    let (status, redeemed) = redeem(&app, minted["code"].as_str().expect("code")).await;
    assert_eq!(status, StatusCode::OK, "{redeemed}");
    assert_eq!(redeemed["subtitle"], -1);
    assert_eq!(redeemed["audio"], 0);
    for body in [
        json!({"file_id": seed.file, "subtitle": -2}),
        json!({"file_id": seed.file, "audio": -1}),
        json!({"file_id": seed.file, "start_ms": -1}),
    ] {
        let (status, _) = call(
            &app,
            post("/api/v1/playback-handoffs", Some(&admin), body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
}

#[tokio::test]
async fn handoff_token_is_accepted_only_on_its_own_files_playback_routes() {
    let (app, state, _fixture) = test_app_with_fixture_store();
    let admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;
    let (own_item, other_item, other) = own_item_and_other_file(&state, &seed).await;
    let (_, code) = mint(&app, &admin, seed.file).await;
    let token = redeemed_token(&app, &code).await;

    // Its own file: decision (GET and POST), HLS create, progress and the
    // client log all reach their handlers.
    assert_eq!(
        decision_status(&app, &token, seed.file).await,
        StatusCode::OK
    );
    let caps = json!({"caps": {"v": 2, "video": [{"codec": "h264"}],
                               "audio": ["aac"], "containers": ["mp4"]}});
    let (status, body) = call(
        &app,
        post(
            &format!("/api/v1/files/{}/decision", seed.file),
            Some(&token),
            caps.clone(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // A blank playback id is the handler's own 400: authentication passed.
    let (status, body) = call(
        &app,
        post(
            &format!("/api/v1/files/{}/hls/sessions", seed.file),
            Some(&token),
            json!({"playback_id": "   "}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = call(
        &app,
        post(
            &format!("/api/v1/items/{own_item}/progress"),
            Some(&token),
            json!({"position_ms": 60_000, "duration_ms": 600_000}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = call(
        &app,
        post(
            "/api/v1/client-log",
            Some(&token),
            json!({"level": "error", "event": "playback_failed"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Direct play of its own file, by header and by `?token=`, and its
    // subtitle track.
    let (status, _) = call_text(
        &app,
        get(&format!("/api/v1/files/{}/direct", seed.file), Some(&token)),
    )
    .await;
    assert!(status.is_success(), "direct: {status}");
    let (status, _) = call_text(
        &app,
        get(
            &format!("/api/v1/files/{}/direct?token={token}", seed.file),
            None,
        ),
    )
    .await;
    assert!(status.is_success(), "direct with ?token=: {status}");
    let (status, body) = call_text(
        &app,
        get(
            &format!("/api/v1/files/{}/subs/0.vtt", seed.file),
            Some(&token),
        ),
    )
    .await;
    assert_ne!(status, StatusCode::UNAUTHORIZED, "subs: {body}");
    assert_ne!(status, StatusCode::FORBIDDEN, "subs: {body}");

    // Another file's routes refuse it, and say why.
    for (request, label) in [
        (
            get(
                &format!("/api/v1/files/{other}/decision?{DECISION_QUERY}"),
                Some(&token),
            ),
            "decision",
        ),
        (
            post(
                &format!("/api/v1/files/{other}/decision"),
                Some(&token),
                caps.clone(),
            ),
            "decision post",
        ),
        (
            get(&format!("/api/v1/files/{other}/direct"), Some(&token)),
            "direct",
        ),
        (
            get(&format!("/api/v1/files/{other}/subs/0.vtt"), Some(&token)),
            "subs",
        ),
        (
            post(
                &format!("/api/v1/files/{other}/hls/sessions"),
                Some(&token),
                json!({"playback_id": "   "}),
            ),
            "hls create",
        ),
        (
            post(
                &format!("/api/v1/items/{other_item}/progress"),
                Some(&token),
                json!({"position_ms": 60_000, "duration_ms": 600_000}),
            ),
            "progress",
        ),
    ] {
        let (status, body) = call(&app, request).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{label}: {body}");
        assert_eq!(body["code"], "handoff_scope", "{label}");
    }

    // Every other user route refuses it as it would any unknown bearer.
    let item = own_item;
    let file = seed.file;
    for request in [
        get("/api/v1/me", Some(&token)),
        get("/api/v1/me/devices", Some(&token)),
        get("/api/v1/libraries", Some(&token)),
        get(&format!("/api/v1/items/{item}"), Some(&token)),
        get("/api/v1/hubs", Some(&token)),
        get("/api/v1/search?q=heat", Some(&token)),
        get("/api/v1/settings", Some(&token)),
        get(
            &format!("/api/v1/files/{file}/download?token={token}"),
            None,
        ),
        get(
            &format!("/api/v1/files/{file}/stream.mp4?token={token}"),
            None,
        ),
        get(&format!("/api/v1/files/{file}/hls/start"), Some(&token)),
        get(&format!("/api/v1/files/{file}/preparation"), Some(&token)),
        post(
            &format!("/api/v1/files/{file}/hls/continuous-sessions"),
            Some(&token),
            json!({}),
        ),
        post(
            &format!("/api/v1/items/{item}/scrobble"),
            Some(&token),
            json!({}),
        ),
        put(
            &format!("/api/v1/items/{item}/watchlist"),
            Some(&token),
            json!({}),
        ),
        post(
            "/api/v1/playback-handoffs",
            Some(&token),
            json!({"file_id": file}),
        ),
        post(
            &format!("/api/v1/files/{file}/grants"),
            Some(&token),
            json!({"purpose": "open_in"}),
        ),
        post("/api/v1/auth/logout", Some(&token), json!({})),
    ] {
        let uri = request.uri().to_string();
        let (status, body) = call(&app, request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}: {body}");
    }

    // The account bearer is untouched, and `/self` is for handoff tokens
    // only.
    assert_eq!(
        decision_status(&app, &admin, seed.file).await,
        StatusCode::OK
    );
    let (status, _) = call(&app, delete("/api/v1/playback-handoffs/self", Some(&admin))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        decision_status(&app, &token, seed.file).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn handoff_token_is_revoked_with_its_minter_by_self_and_from_sessions() {
    let (app, state, _fixture) = test_app_with_fixture_store();
    let admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;

    // Signing out of the minting session revokes the handoff token.
    let web = login(&app, "web").await;
    let (_, code) = mint(&app, &web, seed.file).await;
    let token = redeemed_token(&app, &code).await;
    assert_eq!(
        decision_status(&app, &token, seed.file).await,
        StatusCode::OK
    );
    let (status, _) = call(&app, post("/api/v1/auth/logout", Some(&web), json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        decision_status(&app, &token, seed.file).await,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        !lists_device(&devices(&app, &admin).await),
        "dead token listed"
    );

    // A code whose minting session ended is gone before redemption.
    let web = login(&app, "web again").await;
    let (_, code) = mint(&app, &web, seed.file).await;
    let (status, _) = call(&app, post("/api/v1/auth/logout", Some(&web), json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = redeem(&app, &code).await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    assert_eq!(body["code"], "handoff_gone");

    // The player revokes its own token on exit.
    let (_, code) = mint(&app, &admin, seed.file).await;
    let token = redeemed_token(&app, &code).await;
    let (status, _) = call(&app, delete("/api/v1/playback-handoffs/self", Some(&token))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        decision_status(&app, &token, seed.file).await,
        StatusCode::UNAUTHORIZED
    );
    assert!(!lists_device(&devices(&app, &admin).await));

    // The account owner can revoke it from the sessions list.
    let (_, code) = mint(&app, &admin, seed.file).await;
    let token = redeemed_token(&app, &code).await;
    let rows = devices(&app, &admin).await;
    let row = rows
        .iter()
        .find(|row| row["device"] == DEVICE)
        .expect("handoff session listed");
    let prefix = row["token_hash_prefix"].as_str().expect("prefix");
    let (status, body) = call(
        &app,
        delete(&format!("/api/v1/me/devices/{prefix}"), Some(&admin)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        decision_status(&app, &token, seed.file).await,
        StatusCode::UNAUTHORIZED
    );
    // None of this touched the account bearer.
    assert_eq!(
        decision_status(&app, &admin, seed.file).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn handoff_code_expires_after_120s_and_token_after_6h() {
    let (app, state, fixture) = test_app_with_fixture_store();
    let admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;

    // An expired code is 410, and its status stays unredeemed.
    let (id, code) = mint(&app, &admin, seed.file).await;
    fixture
        .fixture_expire_playback_handoff(&id, now_secs() - 1)
        .await
        .expect("expire code");
    let (status, body) = redeem(&app, &code).await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    assert_eq!(body["code"], "handoff_gone");
    let (_, body) = call(
        &app,
        get(&format!("/api/v1/playback-handoffs/{id}"), Some(&admin)),
    )
    .await;
    assert_eq!(body, json!({"redeemed": false}));

    // A redeemed token stops at its expiry and leaves the sessions list.
    let (id, code) = mint(&app, &admin, seed.file).await;
    let token = redeemed_token(&app, &code).await;
    assert_eq!(
        decision_status(&app, &token, seed.file).await,
        StatusCode::OK
    );
    fixture
        .fixture_expire_playback_handoff(&id, now_secs() - 1)
        .await
        .expect("expire token");
    assert_eq!(
        decision_status(&app, &token, seed.file).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, delete("/api/v1/playback-handoffs/self", Some(&token)))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert!(!lists_device(&devices(&app, &admin).await));

    // Pruning deletes the dead token and the stale codes, never a live one.
    let (_, code) = mint(&app, &admin, seed.file).await;
    let live = redeemed_token(&app, &code).await;
    let pruned = state
        .store
        .prune_playback_handoffs(now_secs(), now_secs())
        .await
        .expect("prune");
    assert!(pruned >= 2, "dead token and expired code: {pruned}");
    assert_eq!(
        decision_status(&app, &live, seed.file).await,
        StatusCode::OK
    );
    assert!(lists_device(&devices(&app, &admin).await));
}

async fn add_episode(state: &AppState, seed: &Seed, season: i64, number: i32) -> (i64, i64) {
    use plurx_core::domain::{ItemKind, NewItem, ProbeResult};
    let item = state
        .store
        .insert_item(&NewItem {
            library_id: seed.lib,
            kind: ItemKind::Episode,
            parent_id: Some(season),
            title: format!("Episode {number}"),
            year: None,
            season_number: None,
            episode_number: Some(number),
        })
        .await
        .expect("episode");
    let path = crate::test_temp_path(format!("handoff-episode-{}.mp4", uuid::Uuid::new_v4()));
    std::fs::write(&path, b"tiny episode").expect("episode fixture");
    let file = state
        .store
        .upsert_file(
            item,
            &path.to_string_lossy(),
            12,
            1,
            &ProbeResult::default(),
        )
        .await
        .expect("file");
    (item, file)
}

#[tokio::test]
async fn handoff_context_and_next_follow_episode_order_without_broadening_authority() {
    use plurx_core::domain::{ItemKind, NewItem};
    let (app, state, _fixture) = test_app_with_fixture_store();
    let admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;
    let first = state.store.files_for_item(seed.ep).await.expect("files")[0].id;
    let (second_item, second) = add_episode(&state, &seed, seed.season, 2).await;
    // A vanished higher-quality copy must not hide the available episode.
    state
        .store
        .upsert_file(
            second_item,
            &format!("/missing/handoff-{}.mkv", uuid::Uuid::new_v4()),
            1,
            1,
            &plurx_core::domain::ProbeResult {
                height: Some(2160),
                ..Default::default()
            },
        )
        .await
        .expect("missing preferred copy");
    let season2 = state
        .store
        .insert_item(&NewItem {
            library_id: seed.lib,
            kind: ItemKind::Season,
            parent_id: Some(seed.show),
            title: "Season 2".into(),
            year: None,
            season_number: Some(2),
            episode_number: None,
        })
        .await
        .expect("season");
    let (_, third) = add_episode(&state, &seed, season2, 1).await;
    let (_, code) = mint(&app, &admin, first).await;
    let token = redeemed_token(&app, &code).await;
    let context_path = "/api/v1/playback-handoffs/self/context";
    let next_path = "/api/v1/playback-handoffs/self/next";
    let (status, body) = call(&app, get(context_path, Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["file_id"], first);
    assert_eq!(body["metadata"]["title"], "The Target");
    assert_eq!(body["metadata"]["series_title"], "The Wire");
    assert_eq!(body["next"]["file_id"], second);
    assert_eq!(body["next"]["item_id"], second_item);
    assert!(body.get("files").is_none() && body.get("children").is_none());
    assert!(!body.to_string().contains("/media/") && !body.to_string().contains("token"));
    assert_eq!(
        call(
            &app,
            get(&format!("/api/v1/items/{}", seed.ep), Some(&token))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        decision_status(&app, &token, second).await,
        StatusCode::FORBIDDEN
    );
    let (status, body) = call(
        &app,
        post(next_path, Some(&token), json!({"expected_file_id": first})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["file_id"], second);
    assert_eq!(body["item_id"], second_item);
    assert_eq!(body["start_ms"], 0);
    assert!(body["audio"].is_null() && body["subtitle"].is_null());
    assert_eq!(body["next"]["file_id"], third);
    assert_eq!(
        decision_status(&app, &token, first).await,
        StatusCode::FORBIDDEN
    );
    assert!(!matches!(
        decision_status(&app, &token, second).await,
        StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
    ));
    let (status, body) = call(
        &app,
        post(next_path, Some(&token), json!({"expected_file_id": first})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "handoff_stale");
    let (status, body) = call(
        &app,
        post(next_path, Some(&token), json!({"expected_file_id": second})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["file_id"], third);
    assert!(body["next"].is_null());
    assert_eq!(
        call(
            &app,
            post(next_path, Some(&token), json!({"expected_file_id": third}))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, get(context_path, Some(&token))).await.1["file_id"],
        third
    );
    assert_eq!(
        call(&app, get(context_path, Some(&admin))).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, get(context_path, None)).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn handoff_context_and_next_refuse_revoked_expired_and_minter_revoked_tokens() {
    let (app, state, fixture) = test_app_with_fixture_store();
    let admin = setup_admin(&app).await;
    let seed = seed_content(&state).await;
    for reason in ["self", "expired", "minter"] {
        let web = login(&app, reason).await;
        let (id, code) = mint(&app, &web, seed.file).await;
        let token = redeemed_token(&app, &code).await;
        match reason {
            "self" => {
                call(&app, delete("/api/v1/playback-handoffs/self", Some(&token))).await;
            }
            "expired" => {
                fixture
                    .fixture_expire_playback_handoff(&id, now_secs() - 1)
                    .await
                    .expect("expire");
            }
            _ => {
                call(&app, post("/api/v1/auth/logout", Some(&web), json!({}))).await;
            }
        }
        assert_eq!(
            call(
                &app,
                get("/api/v1/playback-handoffs/self/context", Some(&token))
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED,
            "{reason}"
        );
        assert_eq!(
            call(
                &app,
                post(
                    "/api/v1/playback-handoffs/self/next",
                    Some(&token),
                    json!({"expected_file_id": seed.file})
                )
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED,
            "{reason}"
        );
    }
    assert_eq!(
        decision_status(&app, &admin, seed.file).await,
        StatusCode::OK
    );
}
