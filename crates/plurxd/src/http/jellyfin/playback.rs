//! Authenticated direct playback and watch adapters; native services own bytes and effects.
use super::*;
use axum::extract::Query;
use axum::http::Method;
use plurx_core::domain::MediaFile;
use plurx_core::store::{
    JellyfinPlay, JellyfinPlayActivation, JellyfinPlayScope, JellyfinProgressProvenance,
    JellyfinProgressWrite, NewFileGrant, NewJellyfinPlay,
};

fn now_ms() -> Result<i64, ApiError> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| ApiError::ServiceUnavailable("server clock unavailable".into()))?
        .as_millis();
    i64::try_from(ms).map_err(|_| ApiError::ServiceUnavailable("server clock overflow".into()))
}
async fn scope(client: &ClientUser, state: &AppState) -> Result<JellyfinPlayScope, ApiError> {
    let scope = state
        .store
        .jellyfin_login_scope(client.token_hash.clone())
        .await?
        .ok_or(ApiError::Unauthorized)?;
    if scope.user_id != client.identity.native_id {
        return Err(ApiError::Unauthorized);
    }
    Ok(scope)
}
fn fingerprint(file: &MediaFile) -> Result<String, ApiError> {
    serde_json::to_string(file)
        .map(|json| plurx_core::auth::hash_token(&json))
        .map_err(|_| ApiError::ServiceUnavailable("source identity unavailable".into()))
}
async fn live_item(
    client: &ClientUser,
    state: &AppState,
    id: &str,
) -> Result<wire::Item, ApiError> {
    let mut query = browse(None)?;
    query.limit = 1;
    query.recursive = true;
    catalog(client, state, query, Some(wire_id(id)?))
        .await?
        .items
        .into_iter()
        .next()
        .ok_or(ApiError::NotFound("catalog item"))
}
async fn current_file(state: &AppState, play: &JellyfinPlay) -> Result<MediaFile, ApiError> {
    let file = state
        .store
        .get_file(play.negotiation.file_id)
        .await?
        .ok_or(ApiError::NotFound("play source"))?;
    if file.item_id != play.negotiation.item_id
        || fingerprint(&file)? != play.negotiation.source_fingerprint
    {
        return Err(ApiError::Conflict(
            "play source changed; renegotiate".into(),
        ));
    }
    Ok(file)
}
async fn binding(
    client: &ClientUser,
    state: &AppState,
    play_id: &str,
    item_id: &str,
    source_id: &str,
) -> Result<JellyfinPlay, ApiError> {
    let scope = scope(client, state).await?;
    let play_id = wire_id(play_id)?.to_hex();
    let play = state
        .store
        .jellyfin_play(&play_id, &scope)
        .await?
        .ok_or(ApiError::NotFound("play binding"))?;
    if wire_id(item_id)?.to_hex() != play.negotiation.item_wire_id
        || wire_id(source_id)?.to_hex() != play.negotiation.file_wire_id
    {
        return Err(ApiError::BadRequest(
            "item/source does not belong to play".into(),
        ));
    }
    Ok(play)
}
fn direct_key(play: &JellyfinPlay) -> crate::delivery::Key {
    crate::delivery::Key::new(
        play.negotiation.scope.user_id,
        play.negotiation.file_id,
        Some(&play.negotiation.playback_id),
    )
}
async fn release(state: &AppState, play: &JellyfinPlay) -> Result<(), ApiError> {
    state.direct_plays.remove_key(&direct_key(play));
    if let Some(id) = play.direct_grant_id.as_deref() {
        state
            .store
            .revoke_file_grant(id, play.negotiation.scope.user_id, now_ms()? / 1000)
            .await?;
    }
    Ok(())
}
async fn activate(state: &AppState, mut play: JellyfinPlay) -> Result<JellyfinPlay, ApiError> {
    current_file(state, &play).await?;
    if play.state == "active" {
        return Ok(play);
    }
    if play.state != "pending" || play.expires_at_ms <= now_ms()? {
        return Err(ApiError::Conflict("play is terminal or expired".into()));
    }
    let now = now_ms()?;
    let id = uuid::Uuid::new_v4().to_string();
    // This internal native reference is never returned as a public media capability.
    let secret = uuid::Uuid::new_v4().to_string();
    state
        .store
        .create_file_grant(NewFileGrant {
            id: id.clone(),
            token_hash: plurx_core::auth::hash_token(&secret),
            file_id: play.negotiation.file_id,
            user_id: play.negotiation.scope.user_id,
            source_token_hash: play.negotiation.scope.token_digest.clone(),
            created_at: now / 1000,
            expires_at: now / 1000 + 86_400,
        })
        .await?;
    let result = state
        .store
        .activate_jellyfin_play(
            &play.negotiation.play_id,
            &play.negotiation.scope,
            JellyfinPlayActivation::DirectGrant(id.clone()),
            now,
        )
        .await;
    match result {
        Ok(true) => {}
        Ok(false) => {
            state
                .store
                .revoke_file_grant(&id, play.negotiation.scope.user_id, now / 1000)
                .await?;
        }
        Err(error) => {
            let _ = state
                .store
                .revoke_file_grant(&id, play.negotiation.scope.user_id, now / 1000)
                .await;
            return Err(error.into());
        }
    }
    play = state
        .store
        .jellyfin_play(&play.negotiation.play_id, &play.negotiation.scope)
        .await?
        .ok_or(ApiError::NotFound("play binding"))?;
    if play.state != "active" {
        return Err(ApiError::Conflict("play activation refused".into()));
    }
    Ok(play)
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub(super) struct InfoRequest {
    user_id: Option<String>,
    media_source_id: Option<String>,
    start_time_ticks: Option<Ticks>,
    enable_direct_play: Option<bool>,
    audio_stream_index: Option<i64>,
    subtitle_stream_index: Option<i64>,
    device_profile: Option<Value>,
    max_streaming_bitrate: Option<i64>,
    current_play_session_id: Option<String>,
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, Value>,
}
fn profiles(request: &InfoRequest, file: &MediaFile) -> Vec<plurx_core::playback::DeviceProfile> {
    if request.enable_direct_play == Some(false) {
        return Vec::new();
    }
    let Some(profile) = request.device_profile.as_ref() else {
        return Vec::new();
    };
    let Some(object) = profile.as_object() else {
        return Vec::new();
    };
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "Id" | "Name"
                | "MaxStreamingBitrate"
                | "MaxStaticBitrate"
                | "MusicStreamingTranscodingBitrate"
                | "DirectPlayProfiles"
                | "TranscodingProfiles"
                | "SubtitleProfiles"
                | "CodecProfiles"
                | "ContainerProfiles"
                | "ResponseProfiles"
        )
    }) {
        return Vec::new();
    }
    // Unknown constraints do not grant capability. J4 translates supported conditions.
    if ["CodecProfiles", "ContainerProfiles"].iter().any(|key| {
        profile.get(key).is_some_and(|value| {
            !value.is_null()
                && value
                    .as_array()
                    .is_none_or(|conditions| !conditions.is_empty())
        })
    }) {
        return Vec::new();
    }
    let Some(profiles) = profile.get("DirectPlayProfiles").and_then(Value::as_array) else {
        return Vec::new();
    };
    if profiles.len() > 32 {
        return Vec::new();
    }
    let mut limits = Vec::new();
    if let Some(limit) = request.max_streaming_bitrate {
        limits.push(limit);
    }
    for key in ["MaxStreamingBitrate", "MaxStaticBitrate"] {
        if let Some(value) = profile.get(key) {
            let Some(limit) = value.as_i64() else {
                return Vec::new();
            };
            limits.push(limit);
        }
    }
    if limits.iter().any(|limit| *limit <= 0) || (!limits.is_empty() && file.bitrate.is_none()) {
        return Vec::new();
    }
    let max_bitrate = limits.into_iter().min();
    let split = |value: Option<&str>| {
        value
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>()
    };
    profiles
        .iter()
        .filter_map(|profile| {
            let object = profile.as_object()?;
            if object.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "Type" | "Container" | "VideoCodec" | "AudioCodec"
                )
            }) || profile.get("Type").and_then(Value::as_str) != Some("Video")
            {
                return None;
            }
            let containers = split(profile.get("Container").and_then(Value::as_str));
            let video = split(profile.get("VideoCodec").and_then(Value::as_str));
            let audio = split(profile.get("AudioCodec").and_then(Value::as_str));
            if !containers
                .iter()
                .any(|codec| file.container.as_ref() == Some(codec))
                || !video
                    .iter()
                    .any(|codec| file.video_codec.as_ref() == Some(codec))
            {
                return None;
            }
            let mut native =
                plurx_core::playback::caps_profile(containers, video, audio, None, false, false);
            native.max_bitrate = max_bitrate;
            Some(native)
        })
        .collect()
}
pub(super) async fn info_post(
    client: ClientUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
    Json(mut request): Json<InfoRequest>,
) -> Result<Json<Value>, ApiError> {
    merge_info_query(&client, raw.as_deref(), &mut request)?;
    info(&client, &state, &id, request).await
}
pub(super) async fn info_get(
    client: ClientUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let mut request = InfoRequest::default();
    merge_info_query(&client, raw.as_deref(), &mut request)?;
    info(&client, &state, &id, request).await
}
fn merge_info_query(
    client: &ClientUser,
    raw: Option<&str>,
    request: &mut InfoRequest,
) -> Result<(), ApiError> {
    fn integer(value: &str) -> Result<i64, ApiError> {
        value
            .parse()
            .map_err(|_| ApiError::BadRequest("invalid playback integer".into()))
    }
    fn assign<T: PartialEq>(field: &mut Option<T>, value: T) -> Result<(), ApiError> {
        if field.as_ref().is_some_and(|old| old != &value) {
            return Err(ApiError::BadRequest(
                "conflicting playback parameters".into(),
            ));
        }
        *field = Some(value);
        Ok(())
    }
    if let Some(user) = request.user_id.as_mut() {
        check_user(client, user)?;
        *user = wire_id(user)?.to_hex();
    }
    if let Some(play) = request.current_play_session_id.as_mut() {
        *play = wire_id(play)?.to_hex();
    }
    for (key, value) in query_pairs(raw)? {
        match key.to_ascii_lowercase().as_str() {
            "userid" => {
                check_user(client, &value)?;
                assign(&mut request.user_id, wire_id(&value)?.to_hex())?;
            }
            "mediasourceid" => {
                let normalized = wire_id(&value)?.to_hex();
                if request.media_source_id.as_ref().is_some_and(|old| {
                    wire_id(old).ok().map(|id| id.to_hex()) != Some(normalized.clone())
                }) {
                    return Err(ApiError::BadRequest("conflicting media source".into()));
                }
                request.media_source_id = Some(normalized);
            }
            "enabledirectplay" => assign(
                &mut request.enable_direct_play,
                match value.to_ascii_lowercase().as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(ApiError::BadRequest("invalid direct flag".into())),
                },
            )?,
            "maxstreamingbitrate" => assign(&mut request.max_streaming_bitrate, integer(&value)?)?,
            "audiostreamindex" => assign(&mut request.audio_stream_index, integer(&value)?)?,
            "subtitlestreamindex" => assign(&mut request.subtitle_stream_index, integer(&value)?)?,
            "starttimeticks" => assign(
                &mut request.start_time_ticks,
                Ticks::try_from(integer(&value)?)
                    .map_err(|_| ApiError::BadRequest("invalid playback time".into()))?,
            )?,
            "currentplaysessionid" => assign(
                &mut request.current_play_session_id,
                wire_id(&value)?.to_hex(),
            )?,
            "api_key"
            | "apikey"
            | "isplayback"
            | "autoopenlivestream"
            | "enabledirectstream"
            | "enabletranscoding"
            | "allowvideostreamcopy"
            | "allowaudiostreamcopy" => {}
            _ => return Err(ApiError::BadRequest("unsupported playback query".into())),
        }
    }
    Ok(())
}

async fn info(
    client: &ClientUser,
    state: &AppState,
    id: &str,
    request: InfoRequest,
) -> Result<Json<Value>, ApiError> {
    if let Some(uid) = request.user_id.as_deref() {
        check_user(client, uid)?;
    }
    if request.extra.keys().any(|key| {
        !matches!(
            key.as_str(),
            "IsPlayback"
                | "AutoOpenLiveStream"
                | "EnableDirectStream"
                | "EnableTranscoding"
                | "AllowVideoStreamCopy"
                | "AllowAudioStreamCopy"
        )
    }) {
        return Err(ApiError::BadRequest(
            "unsupported playback constraint".into(),
        ));
    }
    let item = live_item(client, state, id).await?;
    let scope = scope(client, state).await?;
    let sources = item
        .media_sources
        .ok_or(ApiError::NotFound("play sources"))?;
    let source = sources
        .into_iter()
        .find(|s| {
            request
                .media_source_id
                .as_ref()
                .is_none_or(|id| wire_id(id).ok() == Some(s.id))
        })
        .ok_or(ApiError::NotFound("play source"))?;
    let file_id = state
        .store
        .jellyfin_resolve_entity(JellyfinEntityKind::File, &source.id.to_hex())
        .await?
        .ok_or(ApiError::NotFound("source"))?;
    let file = state
        .store
        .get_file(file_id)
        .await?
        .ok_or(ApiError::NotFound("source"))?;
    let mut selected_file = file.clone();
    if let Some(index) = request.audio_stream_index {
        let audio = source
            .media_streams
            .iter()
            .filter(|s| s.stream_type == wire::StreamType::Audio)
            .position(|s| s.index == index)
            .ok_or(ApiError::BadRequest(
                "audio index does not belong to source".into(),
            ))?;
        selected_file.audio_streams =
            vec![file
                .audio_streams
                .get(audio)
                .cloned()
                .ok_or(ApiError::BadRequest(
                    "audio probe indices are incomplete".into(),
                ))?];
    }
    let node = super::super::stream::render_caps(state).await;
    if !profiles(&request, &selected_file).iter().any(|profile| {
        plurx_core::playback::decide(&selected_file, profile, &node).method
            == plurx_core::playback::PlaybackMethod::DirectPlay
    }) {
        return Ok(Json(json!({"MediaSources":[],"ErrorCode":"NotSupported"})));
    }
    if let Some(index) = request.audio_stream_index {
        if index < 0
            || !source
                .media_streams
                .iter()
                .any(|s| s.stream_type == wire::StreamType::Audio && s.index == index)
        {
            return Err(ApiError::BadRequest(
                "audio index does not belong to source".into(),
            ));
        }
    }
    if let Some(index) = request.subtitle_stream_index {
        if index >= 0
            && !source
                .media_streams
                .iter()
                .any(|s| s.stream_type == wire::StreamType::Subtitle && s.index == index)
        {
            return Err(ApiError::BadRequest(
                "subtitle index does not belong to source".into(),
            ));
        }
        if index < -1 {
            return Err(ApiError::BadRequest("invalid subtitle selection".into()));
        }
    }
    let item_id = state
        .store
        .jellyfin_resolve_entity(JellyfinEntityKind::Item, &item.id.to_hex())
        .await?
        .ok_or(ApiError::NotFound("item"))?;
    if file.item_id != item_id {
        return Err(ApiError::BadRequest("source membership changed".into()));
    }
    let selection=json!({"audio":request.audio_stream_index,"subtitle":request.subtitle_stream_index,"source":{"size":file.size,"mtime":file.mtime}}).to_string();
    let profile_json = serde_json::to_string(&request.device_profile)
        .map_err(|_| ApiError::BadRequest("invalid device profile".into()))?;
    let play_id = uuid::Uuid::new_v4().simple().to_string();
    let play = NewJellyfinPlay {
        play_id: play_id.clone(),
        scope: scope.clone(),
        playback_id: format!("jellyfin:{play_id}"),
        item_id,
        file_id,
        item_wire_id: item.id.to_hex(),
        file_wire_id: source.id.to_hex(),
        source_fingerprint: fingerprint(&file)?,
        profile_fingerprint: plurx_core::auth::hash_token(&profile_json),
        native_request_fingerprint: plurx_core::auth::hash_token(&selection),
        selection_json: selection,
        source_origin_ms: request.start_time_ticks.map_or(0, Ticks::milliseconds),
        created_at_ms: now_ms()?,
    };
    if !state.store.create_jellyfin_play(play).await? {
        return Err(ApiError::ServiceUnavailable(
            "negotiation capacity exhausted".into(),
        ));
    }
    if let Some(previous) = request.current_play_session_id {
        if let Some(previous) = state
            .store
            .jellyfin_play(&wire_id(&previous)?.to_hex(), &scope)
            .await?
        {
            if previous.state == "pending" {
                state
                    .store
                    .end_jellyfin_play(&previous.negotiation.play_id, &scope, now_ms()?)
                    .await?;
            }
        }
    }
    let mut source = serde_json::to_value(source)
        .map_err(|_| ApiError::ServiceUnavailable("source DTO unavailable".into()))?;
    source["SupportsDirectPlay"] = json!(true);
    if let Some(index) = request.audio_stream_index {
        source["DefaultAudioStreamIndex"] = json!(index);
    }
    if let Some(index) = request.subtitle_stream_index {
        source["DefaultSubtitleStreamIndex"] = json!(index);
    }
    source["DirectStreamUrl"] = json!(format!(
        "/jellyfin/Videos/{}/stream?MediaSourceId={}&PlaySessionId={play_id}&Static=true",
        item.id.to_hex(),
        source["Id"].as_str().unwrap_or_default()
    ));
    Ok(Json(
        json!({"MediaSources":[source],"PlaySessionId":play_id}),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct PlayingEvent {
    item_id: String,
    media_source_id: String,
    play_session_id: String,
    position_ticks: Option<Ticks>,
    run_time_ticks: Option<Ticks>,
    user_id: Option<String>,
    play_method: Option<String>,
}
async fn resolve_event(
    client: &ClientUser,
    state: &AppState,
    event: &PlayingEvent,
) -> Result<JellyfinPlay, ApiError> {
    if let Some(uid) = event.user_id.as_deref() {
        check_user(client, uid)?;
    }
    if event
        .play_method
        .as_deref()
        .is_some_and(|m| m != "DirectPlay")
    {
        return Err(ApiError::BadRequest(
            "play method does not match negotiation".into(),
        ));
    }
    binding(
        client,
        state,
        &event.play_session_id,
        &event.item_id,
        &event.media_source_id,
    )
    .await
}
fn progress_write(
    play: &JellyfinPlay,
    event: &PlayingEvent,
    final_commit: bool,
) -> Option<JellyfinProgressWrite> {
    event.position_ticks.map(|position| JellyfinProgressWrite {
        provenance: JellyfinProgressProvenance {
            play_id: play.negotiation.play_id.clone(),
            scope: play.negotiation.scope.clone(),
            manual_revision: play.manual_revision,
        },
        item_id: play.negotiation.item_id,
        position_ms: position.milliseconds(),
        duration_ms: event.run_time_ticks.map(Ticks::milliseconds),
        final_commit,
    })
}
pub(super) async fn playing(
    client: ClientUser,
    State(state): State<AppState>,
    Json(event): Json<PlayingEvent>,
) -> Result<StatusCode, ApiError> {
    let play = activate(&state, resolve_event(&client, &state, &event).await?).await?;
    if let Some(write) = progress_write(&play, &event, false) {
        super::super::watch::apply_jellyfin_progress(&state, write, &direct_key(&play)).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn progress(
    client: ClientUser,
    State(state): State<AppState>,
    Json(report): Json<PlayingEvent>,
) -> Result<StatusCode, ApiError> {
    let play = resolve_event(&client, &state, &report).await?;
    if play.state != "active" {
        return Err(ApiError::Conflict("play is not active".into()));
    }
    current_file(&state, &play).await?;
    if let Some(write) = progress_write(&play, &report, false) {
        super::super::watch::apply_jellyfin_progress(&state, write, &direct_key(&play)).await?;
    } else {
        state.direct_plays.touch_key(&direct_key(&play));
    }
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn stopped(
    client: ClientUser,
    State(state): State<AppState>,
    Json(report): Json<PlayingEvent>,
) -> Result<StatusCode, ApiError> {
    let play = resolve_event(&client, &state, &report).await?;
    if play.state == "ended" {
        release(&state, &play).await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    // Cleanup follows every result, including durable failure. An active row is
    // retained on failure so a repeated stop can retry its terminal reconciliation.
    let result = async {
        if play.state == "active" {
            if let Some(write) = progress_write(&play, &report, true) {
                super::super::watch::apply_jellyfin_progress(&state, write, &direct_key(&play))
                    .await?;
            }
        }
        state
            .store
            .end_jellyfin_play(
                &play.negotiation.play_id,
                &play.negotiation.scope,
                now_ms()?,
            )
            .await?;
        Ok::<(), ApiError>(())
    }
    .await;
    let released = release(&state, &play).await;
    result?;
    released?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
pub(super) struct DirectRequest {
    #[serde(rename = "MediaSourceId")]
    media_source_id: String,
    #[serde(rename = "PlaySessionId")]
    play_session_id: String,
}
async fn serve_direct(
    client: ClientUser,
    state: AppState,
    item_id: String,
    request: DirectRequest,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    live_item(&client, &state, &item_id).await?;
    let play = activate(
        &state,
        binding(
            &client,
            &state,
            &request.play_session_id,
            &item_id,
            &request.media_source_id,
        )
        .await?,
    )
    .await?;
    let grant = state
        .store
        .file_grant_by_id(
            play.direct_grant_id
                .as_deref()
                .ok_or(ApiError::Unauthorized)?,
        )
        .await?
        .ok_or(ApiError::Unauthorized)?;
    if grant.user_id != client.identity.native_id
        || grant.file_id != play.negotiation.file_id
        || !grant.source_active
        || grant.revoked_at.is_some()
        || grant.expires_at <= now_ms()? / 1000
    {
        return Err(ApiError::Unauthorized);
    }
    let user = state
        .store
        .get_user(client.identity.native_id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    super::super::stream::direct(
        super::super::extract::AuthUser(user),
        State(state),
        Path(play.negotiation.file_id),
        Query(super::super::stream::DirectQuery {
            stream: Some(play.negotiation.playback_id),
        }),
        method,
        headers,
    )
    .await
}
pub(super) async fn direct(
    client: ClientUser,
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    Query(request): Query<DirectRequest>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    serve_direct(client, state, item_id, request, method, headers).await
}
pub(super) async fn direct_extension(
    client: ClientUser,
    State(state): State<AppState>,
    Path((item_id, filename)): Path<(String, String)>,
    Query(request): Query<DirectRequest>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if !matches!(
        filename.as_str(),
        "stream.mp4" | "stream.mkv" | "stream.webm" | "stream.ts" | "stream.avi"
    ) {
        return Err(ApiError::NotFound("media route"));
    }
    serve_direct(client, state, item_id, request, method, headers).await
}
async fn manual(
    client: ClientUser,
    state: AppState,
    uid: String,
    id: String,
    watched: bool,
) -> Result<Json<Value>, ApiError> {
    check_user(&client, &uid)?;
    let item = live_item(&client, &state, &id).await?;
    let native = state
        .store
        .jellyfin_resolve_entity(JellyfinEntityKind::Item, &item.id.to_hex())
        .await?
        .ok_or(ApiError::NotFound("item"))?;
    let origin = scope(&client, &state).await?;
    super::super::watch::apply_watched_with_origin(
        &state,
        origin.user_id,
        native,
        watched,
        Some(&origin),
    )
    .await?;
    let fresh = live_item(&client, &state, &id).await?;
    Ok(Json(serde_json::to_value(fresh.user_data).map_err(
        |_| ApiError::ServiceUnavailable("watch DTO unavailable".into()),
    )?))
}
pub(super) async fn mark_played(
    client: ClientUser,
    State(state): State<AppState>,
    Path((uid, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    manual(client, state, uid, id, true).await
}
pub(super) async fn mark_unplayed(
    client: ClientUser,
    State(state): State<AppState>,
    Path((uid, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    manual(client, state, uid, id, false).await
}

/// Reuse native digest exclusion and token deletion, retaining other logins.
pub(super) async fn logout(
    client: ClientUser,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    let scope = scope(&client, &state).await?;
    let exclusion = crate::http::internal_auth_revocation::ClusterCacheRevocation::begin_digest(
        &state,
        &client.token_hash,
    )
    .await?;
    let plays = state
        .store
        .end_jellyfin_login_plays(&scope, now_ms()?)
        .await?;
    for play in plays {
        release(&state, &play).await?;
    }
    auth::revoke_token_under_exclusion(&state, &client.token_hash, exclusion).await?;
    Ok(StatusCode::NO_CONTENT)
}
