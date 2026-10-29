//! Exercise real Library-channel handlers against the SQLite Store boundary.
use super::*;
use axum::response::IntoResponse;
use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult, User};
use serde_json::{json, Value};

async fn fixture() -> (AppState, User, User, User) {
    let state = crate::http::tests::test_state_without_router();
    let owner = state
        .store
        .create_user("channel-owner", "unused", false)
        .await
        .expect("owner");
    let other = state
        .store
        .create_user("channel-other", "unused", false)
        .await
        .expect("other");
    let admin = state
        .store
        .create_user("channel-admin", "unused", true)
        .await
        .expect("admin");
    (state, owner, other, admin)
}

fn request<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("request shape")
}

fn definition(id: &str, name: &str) -> Value {
    json!({"request_id":id,"name":name,"enabled":false,"recipe":LibraryChannelRecipe::default(),"preview_seed":"01".repeat(32)})
}

async fn refused<T>(result: Result<T, ApiError>, status: StatusCode) -> Value {
    let error = match result {
        Ok(_) => panic!("request unexpectedly succeeded"),
        Err(error) => error,
    };
    let response = error.into_response();
    assert_eq!(response.status(), status);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("error body");
    serde_json::from_slice(&bytes).expect("error JSON")
}

async fn catalogue(state: &AppState) -> Vec<(i64, i64)> {
    let library = state
        .store
        .create_library(&NewLibrary {
            name: "Channel movies".into(),
            kind: LibraryKind::Movies,
            paths: Vec::new(),
            anime: false,
        })
        .await
        .expect("library");
    let mut rows = Vec::new();
    for title in ["Alpha", "Bravo", "Charlie"] {
        let item = state
            .store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: title.into(),
                year: Some(2025),
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("movie");
        let probe = ProbeResult {
            video_codec: Some("h264".into()),
            duration_ms: Some(120_000),
            container: Some("mp4".into()),
            raw_json: Some(
                json!({"streams":[{"codec_type":"video","codec_name":"h264","duration":"120"}],
                "format":{"format_name":"mp4","duration":"120"}})
                .to_string(),
            ),
            ..Default::default()
        };
        let file = state
            .store
            .upsert_file(
                item,
                &format!("/channel-fixture/{title}.mp4"),
                1_000,
                1,
                &probe,
            )
            .await
            .expect("file");
        rows.push((item, file));
    }
    rows
}

#[tokio::test]
async fn definitions_replay_exactly_and_private_channels_enforce_visibility_and_revision() {
    let (state, owner, other, admin) = fixture().await;
    let body = definition("create-private", "  Private rotation  ");
    let (status, Json(created)) = create(
        AuthUser(owner.clone()),
        State(state.clone()),
        Json(request(body.clone())),
    )
    .await
    .expect("create");
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created.channel.name, "Private rotation");
    let id = created.channel.id;
    let (status, Json(replayed)) = create(
        AuthUser(owner.clone()),
        State(state.clone()),
        Json(request(body.clone())),
    )
    .await
    .expect("replay");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replayed.channel.id, id);
    let mut changed = body;
    changed["name"] = json!("Different intent");
    refused(
        create(
            AuthUser(owner.clone()),
            State(state.clone()),
            Json(request(changed)),
        )
        .await,
        StatusCode::CONFLICT,
    )
    .await;
    refused(
        get_one(
            AuthUser(other.clone()),
            State(state.clone()),
            Path(id.clone()),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    refused(
        favourite(
            AuthUser(other.clone()),
            State(state.clone()),
            Path(id.clone()),
            Json(FavouriteBody { favourite: true }),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    refused(
        list(
            AuthUser(other.clone()),
            State(state.clone()),
            Query(request(json!({"management":true}))),
        )
        .await,
        StatusCode::FORBIDDEN,
    )
    .await;
    let Json(others) = list(
        AuthUser(other.clone()),
        State(state.clone()),
        Query(request(json!({}))),
    )
    .await
    .expect("private list");
    assert!(others.is_empty());
    let Json(managed) = list(
        AuthUser(admin.clone()),
        State(state.clone()),
        Query(request(json!({"management":true}))),
    )
    .await
    .expect("admin list");
    assert_eq!(managed.len(), 1);
    assert!(managed[0].can_edit);
    let Json(detail) = get_one(AuthUser(admin), State(state.clone()), Path(id.clone()))
        .await
        .expect("admin inspection");
    assert!(detail.can_share && detail.can_edit && detail.can_delete);
    assert_eq!(
        favourite(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(id.clone()),
            Json(FavouriteBody { favourite: true })
        )
        .await
        .expect("favourite"),
        StatusCode::NO_CONTENT
    );
    let mut edit = definition("rename-private", "Renamed rotation");
    edit["expected_revision"] = json!(1);
    let (status, Json(edited)) = update(
        AuthUser(owner.clone()),
        State(state.clone()),
        Path(id.clone()),
        Json(request(edit.clone())),
    )
    .await
    .expect("edit");
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(edited.channel.revision, 2);
    let (_, Json(replay)) = update(
        AuthUser(owner.clone()),
        State(state.clone()),
        Path(id.clone()),
        Json(request(edit)),
    )
    .await
    .expect("update replay");
    assert_eq!(replay.channel.revision, 2);
    let mut stale = definition("stale-edit", "Lost update");
    stale["expected_revision"] = json!(1);
    refused(
        update(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(id.clone()),
            Json(request(stale)),
        )
        .await,
        StatusCode::CONFLICT,
    )
    .await;
    refused(
        delete_one(
            AuthUser(other),
            State(state.clone()),
            Path(id.clone()),
            Query(DeleteQuery {
                expected_revision: 2,
                request_id: "other-delete".into(),
            }),
        )
        .await,
        StatusCode::FORBIDDEN,
    )
    .await;
    let deletion = || {
        Query(DeleteQuery {
            expected_revision: 2,
            request_id: "owner-delete".into(),
        })
    };
    assert_eq!(
        delete_one(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(id.clone()),
            deletion()
        )
        .await
        .expect("delete"),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        delete_one(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(id.clone()),
            deletion()
        )
        .await
        .expect("delete replay"),
        StatusCode::NO_CONTENT
    );
    refused(
        get_one(AuthUser(owner), State(state), Path(id)).await,
        StatusCode::NOT_FOUND,
    )
    .await;
}

#[tokio::test]
async fn shared_channels_are_readable_but_only_admins_can_publish_or_edit_them() {
    let (state, owner, _, admin) = fixture().await;
    let mut body = definition("shared-create", "Shared rotation");
    body["visibility"] = json!("shared");
    body["enabled"] = json!(true);
    refused(
        create(
            AuthUser(owner.clone()),
            State(state.clone()),
            Json(request(body.clone())),
        )
        .await,
        StatusCode::FORBIDDEN,
    )
    .await;
    let (_, Json(created)) = create(
        AuthUser(admin.clone()),
        State(state.clone()),
        Json(request(body)),
    )
    .await
    .expect("admin publishes shared");
    let id = created.channel.id;
    let Json(rows) = list(
        AuthUser(owner.clone()),
        State(state.clone()),
        Query(request(json!({"limit":0}))),
    )
    .await
    .expect("shared visible");
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].can_edit);
    assert_eq!(rows[0].source, "library");
    refused(
        get_one(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(id.clone()),
        )
        .await,
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(
        favourite(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(id.clone()),
            Json(FavouriteBody { favourite: true })
        )
        .await
        .expect("viewer favourite"),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        favourite(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(id.clone()),
            Json(FavouriteBody { favourite: false })
        )
        .await
        .expect("viewer unfavourite"),
        StatusCode::NO_CONTENT
    );
    refused(rebuild(AuthUser(owner.clone()), State(state.clone()), Path(id.clone()), Json(request(json!({"expected_revision":1,"request_id":"viewer-rebuild","activation":"next_rotation"})))).await, StatusCode::FORBIDDEN).await;
    refused(
        delete_one(
            AuthUser(owner),
            State(state.clone()),
            Path(id.clone()),
            Query(DeleteQuery {
                expected_revision: 1,
                request_id: "viewer-delete".into(),
            }),
        )
        .await,
        StatusCode::FORBIDDEN,
    )
    .await;
    let Json(build) = build_state(AuthUser(admin), State(state), Path(id))
        .await
        .expect("build projection");
    assert!(build.active_generation_id.is_none());
}

#[tokio::test]
async fn definition_validation_refuses_bad_names_seeds_and_retry_ids_before_any_write() {
    let (state, owner, _, _) = fixture().await;
    for (field, value) in [
        ("name", json!(" ")),
        ("name", json!("x".repeat(CHANNEL_NAME_MAX + 1))),
        (
            "description",
            json!("x".repeat(CHANNEL_DESCRIPTION_MAX + 1)),
        ),
        ("request_id", json!(" ")),
        ("request_id", json!("x".repeat(129))),
        ("preview_seed", json!("bad-seed")),
    ] {
        let mut body = definition("invalid-definition", "Valid name");
        body[field] = value;
        refused(
            create(
                AuthUser(owner.clone()),
                State(state.clone()),
                Json(request(body)),
            )
            .await,
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    let Json(rows) = list(AuthUser(owner), State(state), Query(request(json!({}))))
        .await
        .expect("no partial definitions");
    assert!(rows.is_empty());
}

#[tokio::test]
async fn preview_pagination_binds_the_viewer_recipe_and_catalogue_snapshot() {
    let (state, owner, other, _) = fixture().await;
    let items = catalogue(&state).await;
    let recipe = LibraryChannelRecipe {
        match_all_in_scope: true,
        ..Default::default()
    };
    let body = || json!({"recipe":recipe,"preview_seed":"02".repeat(32),"limit":1});
    let Json(first) = preview(
        AuthUser(owner.clone()),
        State(state.clone()),
        Json(request(body())),
    )
    .await
    .expect("preview");
    assert_eq!(first.eligible_count, 3);
    assert_eq!(first.unique_duration_ms, 360_000);
    assert_eq!(first.matches.len(), 1);
    assert_eq!(first.first_ten.len(), 3);
    let mut next = body();
    next["cursor"] = json!(first.next_cursor.expect("next page"));
    let Json(second) = preview(
        AuthUser(owner.clone()),
        State(state.clone()),
        Json(request(next.clone())),
    )
    .await
    .expect("second page");
    assert_eq!(second.matches.len(), 1);
    assert_ne!(
        first.matches[0].candidate.item_id,
        second.matches[0].candidate.item_id
    );
    assert_eq!(first.content_digest, second.content_digest);
    refused(
        preview(
            AuthUser(other),
            State(state.clone()),
            Json(request(next.clone())),
        )
        .await,
        StatusCode::CONFLICT,
    )
    .await;
    next["recipe"]["exclude_item_ids"] = json!([first.matches[0].candidate.item_id]);
    refused(
        preview(
            AuthUser(owner.clone()),
            State(state.clone()),
            Json(request(next)),
        )
        .await,
        StatusCode::CONFLICT,
    )
    .await;
    let mut bad = body();
    bad["cursor"] = json!("not-hex");
    refused(
        preview(
            AuthUser(owner.clone()),
            State(state.clone()),
            Json(request(bad)),
        )
        .await,
        StatusCode::BAD_REQUEST,
    )
    .await;
    let mut filtered = body();
    filtered["recipe"]["include_item_ids"] = json!([first.matches[0].candidate.item_id]);
    filtered["recipe"]["exclude_item_ids"] =
        json!(items.iter().map(|(item, _)| *item).collect::<Vec<_>>());
    let Json(empty) = preview(AuthUser(owner), State(state), Json(request(filtered)))
        .await
        .expect("exclusions override inclusion");
    assert_eq!(empty.eligible_count, 0);
    assert!(empty.matches.is_empty() && empty.next_cursor.is_none());
}

#[tokio::test]
async fn empty_rebuild_retains_the_definition_and_playback_distinguishes_disabled_from_empty() {
    let (state, owner, _, _) = fixture().await;
    let (_, Json(created)) = create(
        AuthUser(owner.clone()),
        State(state.clone()),
        Json(request(definition("empty-create", "Empty channel"))),
    )
    .await
    .expect("save empty definition");
    let id = created.channel.id;
    refused(
        resolve_now(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(id.clone()),
        )
        .await,
        StatusCode::GONE,
    )
    .await;
    state
        .store
        .put_setting(plurx_core::store::keys::LIBRARY_CHANNELS_ENABLED, "true")
        .await
        .expect("enable playback");
    refused(
        resolve_now(
            AuthUser(owner.clone()),
            State(state.clone()),
            Path(id.clone()),
        )
        .await,
        StatusCode::GONE,
    )
    .await;
    let error = refused(rebuild(AuthUser(owner.clone()), State(state.clone()), Path(id.clone()), Json(request(json!({"expected_revision":1,"request_id":"rebuild-empty","activation":"next_rotation","reshuffle":true})))).await, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(error["code"], "channel_empty");
    let Json(saved) = get_one(
        AuthUser(owner.clone()),
        State(state.clone()),
        Path(id.clone()),
    )
    .await
    .expect("definition survives");
    assert_eq!(saved.channel.name, "Empty channel");
    let Json(build) = build_state(
        AuthUser(owner.clone()),
        State(state.clone()),
        Path(id.clone()),
    )
    .await
    .expect("failed build projection");
    assert_eq!(build.error_code.as_deref(), Some("channel_empty"));
    assert!(build.active_generation_id.is_none() && build.pending_generation_id.is_none());
    let mut enable = definition("enable-empty", "Empty channel");
    enable["expected_revision"] = json!(saved.channel.revision);
    enable["enabled"] = json!(true);
    let (status, Json(enabled)) = update(
        AuthUser(owner.clone()),
        State(state.clone()),
        Path(id.clone()),
        Json(request(enable)),
    )
    .await
    .expect("enable empty channel");
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(enabled.channel.enabled);
    let error = refused(
        resolve_now(AuthUser(owner), State(state), Path(id)).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(error["code"], "channel_empty");
}

#[tokio::test]
async fn published_rotation_has_a_real_guide_and_refuses_catalogue_identity_changes() {
    let (state, owner, _, _) = fixture().await;
    let items = catalogue(&state).await;
    state
        .store
        .put_setting(plurx_core::store::keys::LIBRARY_CHANNELS_ENABLED, "true")
        .await
        .expect("enable playback");
    let mut body = definition("publish-real", "Movie rotation");
    body["enabled"] = json!(true);
    body["recipe"]["match_all_in_scope"] = json!(true);
    let (_, Json(created)) = create(
        AuthUser(owner.clone()),
        State(state.clone()),
        Json(request(body)),
    )
    .await
    .expect("create");
    let id = created.channel.id.clone();
    let evaluation = matching_catalogue(&state, &created.channel.recipe, None)
        .await
        .expect("catalogue");
    build_channel(
        &state,
        &owner,
        &created.channel,
        evaluation,
        Activation::Initial,
        false,
    )
    .await
    .expect("synchronous publication");
    let Json(resolved) = resolve_now(
        AuthUser(owner.clone()),
        State(state.clone()),
        Path(id.clone()),
    )
    .await
    .expect("current programme");
    let resolved_json = serde_json::to_value(&resolved).expect("resolved JSON");
    assert_eq!(resolved_json["channel_id"], id);
    let start = crate::media_sessions::unix_ms();
    let (_, Json(programmes)) = guide(
        AuthUser(owner.clone()),
        State(state.clone()),
        Query(request(
            json!({"channel_ids":format!("{id}, {id}"),"start_ms":start,"end_ms":start+600_000}),
        )),
    )
    .await
    .expect("guide");
    assert!(programmes.len() >= 5);
    assert!(programmes
        .iter()
        .all(|programme| ["Alpha", "Bravo", "Charlie"].contains(&programme.title.as_str())));
    assert!(programmes
        .windows(2)
        .all(|pair| pair[0].ends_at_ms == pair[1].starts_at_ms));
    assert!(programmes
        .iter()
        .all(|programme| programme.channel_id == id));
    let current = state
        .store
        .get_library_channel(owner.id, false, &id)
        .await
        .expect("channel")
        .expect("saved");
    let (_, occurrence) = occurrence_at(&state, &owner, &current, crate::media_sessions::unix_ms())
        .await
        .expect("current occurrence");
    let (item_id, file_id) = items
        .iter()
        .copied()
        .find(|(_, file)| *file == occurrence.entry.file_id)
        .expect("current file");
    let file = state
        .store
        .get_file(file_id)
        .await
        .expect("read file")
        .expect("file");
    state
        .store
        .upsert_file(
            item_id,
            &file.path.to_string_lossy(),
            file.size + 1,
            file.mtime + 1,
            &ProbeResult {
                video_codec: Some("h264".into()),
                duration_ms: Some(120_000),
                ..Default::default()
            },
        )
        .await
        .expect("replacement");
    let error = refused(
        resolve_now(AuthUser(owner), State(state), Path(id)).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(error["code"], "scheduled_media_unavailable");
}

#[tokio::test]
async fn guide_rejects_unbound_or_invalid_windows_and_does_not_publish_disabled_programmes() {
    let (state, owner, other, _) = fixture().await;
    let (_, Json(created)) = create(
        AuthUser(owner.clone()),
        State(state.clone()),
        Json(request(definition("guide-disabled", "Disabled schedule"))),
    )
    .await
    .expect("definition");
    let id = created.channel.id;
    let (headers, Json(rows)) = guide(
        AuthUser(owner.clone()),
        State(state.clone()),
        Query(request(
            json!({"channel_ids":id,"start_ms":1000,"end_ms":2000}),
        )),
    )
    .await
    .expect("disabled guide");
    assert!(rows.is_empty() && headers.is_empty());
    for query in [
        json!({"channel_ids":""}),
        json!({"channel_ids":id,"start_ms":1000,"end_ms":1000}),
        json!({"channel_ids":id,"start_ms":1000,"end_ms":GUIDE_MAX_MS+1001}),
        json!({"channel_ids":id,"cursor":"not-hex"}),
    ] {
        refused(
            guide(
                AuthUser(owner.clone()),
                State(state.clone()),
                Query(request(query)),
            )
            .await,
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    refused(
        guide(
            AuthUser(other),
            State(state.clone()),
            Query(request(json!({"channel_ids":id}))),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    let cursor = encode_guide_cursor(&GuideCursor {
        actor_user_id: owner.id + 1,
        channel_digest: "wrong".into(),
        start_ms: 1000,
        end_ms: 2000,
        last_starts_at_ms: 1000,
        last_channel_id: id.clone(),
    });
    refused(
        guide(
            AuthUser(owner),
            State(state),
            Query(request(json!({"channel_ids":id,"cursor":cursor}))),
        )
        .await,
        StatusCode::CONFLICT,
    )
    .await;
}
