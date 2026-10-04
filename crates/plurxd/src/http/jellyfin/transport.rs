//! Fresh login and exact native incarnation admission for every mapped HLS resource.
use super::*;
use axum::{
    body::{to_bytes, Body},
    extract::Query,
    http::{header, Method},
};
use plurx_compat_jellyfin::hls::{rewrite_manifest, Resource};
use plurx_core::store::JellyfinPlay;

pub(super) async fn route(state: &AppState, play: &JellyfinPlay) -> Result<String, ApiError> {
    playback::current_file(state, play).await?;
    let id = play
        .native_incarnation_id
        .as_deref()
        .ok_or(ApiError::Conflict("native play is not active".into()))?;
    let native = state
        .store
        .media_session_route_by_incarnation(id)
        .await?
        .ok_or(ApiError::NotFound("native play"))?;
    let current = state
        .store
        .media_session_route_for_playback(
            play.negotiation.scope.user_id,
            &play.negotiation.playback_id,
        )
        .await?
        .ok_or(ApiError::Conflict("native play replaced".into()))?;
    if native.incarnation_id != current.incarnation_id
        || native.user_id != play.negotiation.scope.user_id
        || native.playback_id != play.negotiation.playback_id
        || native.state != "active"
        || native.publication_ready_at_ms != 0
        || native.lease_expires_at_ms <= playback::now_ms()?
        || native.request_fingerprint != play.negotiation.native_request_fingerprint
        || native.media_origin_ms != play.negotiation.source_origin_ms
        || play.state != "active"
    {
        return Err(ApiError::Conflict("native play replaced or expired".into()));
    }
    Ok(native.session_id)
}

pub(super) async fn root(
    client: ClientUser,
    state: AppState,
    item: String,
    request: playback::DirectRequest,
    filename: String,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let pending = playback::binding(
        &client,
        &state,
        &request.play_session_id,
        &item,
        &request.media_source_id,
    )
    .await?;
    let selection: Value = serde_json::from_str(&pending.negotiation.selection_json)?;
    if selection.get("vod").is_none_or(|v| v.is_null()) {
        return Err(ApiError::BadRequest(
            "HLS does not match negotiated delivery".into(),
        ));
    }
    let play = Box::pin(playback::activate(&state, pending)).await?;
    let resource = if filename == "master.m3u8" {
        Resource::Master
    } else {
        Resource::Media
    };
    Box::pin(serve(&client, &state, play, resource, method, headers)).await
}

pub(super) async fn resource(
    client: ClientUser,
    State(state): State<AppState>,
    Path((item, play_id, path)): Path<(String, String, String)>,
    RawQuery(raw): RawQuery,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    for (key, _) in query_pairs(raw.as_deref())? {
        if !matches!(key.to_ascii_lowercase().as_str(), "api_key" | "apikey") {
            return Err(ApiError::BadRequest(
                "unsupported HLS resource query".into(),
            ));
        }
    }
    let scope = state
        .store
        .jellyfin_login_scope(client.token_hash.clone())
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let play = state
        .store
        .jellyfin_play(&wire_id(&play_id)?.to_hex(), &scope)
        .await?
        .ok_or(ApiError::NotFound("play binding"))?;
    if play.negotiation.item_wire_id != wire_id(&item)?.to_hex()
        || scope.user_id != client.identity.native_id
    {
        return Err(ApiError::NotFound("play binding"));
    }
    let selection: Value = serde_json::from_str(&play.negotiation.selection_json)?;
    let inline = selection["vod"]["inline_init"] == true;
    let native_path = if inline && path.ends_with(".ts") {
        format!("{}.m4s", path.trim_end_matches(".ts"))
    } else {
        path
    };
    let resource = Resource::parse(&native_path).ok_or(ApiError::NotFound("media resource"))?;
    Box::pin(serve(&client, &state, play, resource, method, headers)).await
}

async fn serve(
    client: &ClientUser,
    state: &AppState,
    play: JellyfinPlay,
    resource: Resource,
    method: Method,
    mut headers: HeaderMap,
) -> Result<Response, ApiError> {
    playback::live_item(client, state, &play.negotiation.item_wire_id).await?;
    let session = route(state, &play).await?;
    let selection: Value = serde_json::from_str(&play.negotiation.selection_json)?;
    let inline = selection["vod"]["inline_init"] == true;
    let manifest = matches!(
        resource,
        Resource::Master | Resource::Media | Resource::SubtitlePlaylist(_)
    );
    let deadline = super::super::hls::response_publication_deadline();
    let owner = if manifest {
        Some(
            super::super::hls::session_file(state, &session, deadline)
                .await?
                .2,
        )
    } else {
        None
    };
    if manifest {
        headers.remove(header::IF_NONE_MATCH);
        headers.remove(header::IF_RANGE);
        headers.remove(header::RANGE);
    }
    let query = super::super::hls::PlaylistQuery {
        native: None,
        subtitle: selection["vod"]["body"]["subtitle"].as_i64(),
        diagnostic: None,
    };
    let response = match &resource {
        Resource::Master => {
            if selection["vod"]["body"]["native_subtitles"] == true {
                Box::pin(super::super::hls::master_playlist_response(
                    State(state.clone()),
                    Path(session.clone()),
                    Query(query),
                    headers,
                ))
                .await?
            } else {
                Box::pin(super::super::hls::playlist(
                    State(state.clone()),
                    Path(session.clone()),
                    Query(query),
                    headers,
                ))
                .await?
            }
        }
        Resource::Media => {
            Box::pin(super::super::hls::playlist(
                State(state.clone()),
                Path(session.clone()),
                Query(query),
                headers,
            ))
            .await?
        }
        Resource::Init => {
            Box::pin(super::super::hls::segment(
                State(state.clone()),
                Path((session.clone(), "init.mp4".into())),
                headers,
            ))
            .await?
        }
        Resource::Segment(index) if inline => {
            Box::pin(super::representation::inline_native_fragment(
                state,
                &session,
                &format!("seg{index}.m4s"),
                &headers,
            ))
            .await?
        }
        Resource::Segment(index) => {
            Box::pin(super::super::hls::segment(
                State(state.clone()),
                Path((session.clone(), format!("seg{index}.m4s"))),
                headers,
            ))
            .await?
        }
        Resource::SubtitlePlaylist(track) => {
            Box::pin(super::super::hls::subtitle_playlist(
                State(state.clone()),
                Path((session.clone(), i64::from(*track))),
                headers,
            ))
            .await?
        }
        Resource::SubtitleSegment(track, index) => {
            Box::pin(super::super::hls::subtitle_vtt(
                State(state.clone()),
                Path((
                    session.clone(),
                    i64::from(*track),
                    format!("seg{index}.vtt"),
                )),
                headers,
            ))
            .await?
        }
    };
    // Native errors are typed locally; private capability-bearing response bodies are never relayed.
    if !response.status().is_success() && response.status() != StatusCode::NOT_MODIFIED {
        return Err(ApiError::ServiceUnavailable(
            "native media temporarily unavailable".into(),
        ));
    }
    let mut response = if manifest {
        let bytes = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            to_bytes(response.into_body(), 8 * 1024 * 1024),
        )
        .await
        .map_err(|_| ApiError::ServiceUnavailable("manifest publication timeout".into()))?
        .map_err(|_| ApiError::ServiceUnavailable("native manifest unavailable".into()))?;
        let input = std::str::from_utf8(&bytes).map_err(|_| {
            ApiError::ServiceUnavailable("native manifest encoding unavailable".into())
        })?;
        let base = format!(
            "/jellyfin/Videos/{}/{}/hls/",
            play.negotiation.item_wire_id, play.negotiation.play_id
        );
        let manifest_resource = if resource == Resource::Master
            && input.lines().any(|line| line.starts_with("#EXTINF:"))
        {
            Resource::Media
        } else {
            resource.clone()
        };
        let rewritten = rewrite_manifest(input, &manifest_resource, &session, &base, inline)
            .map_err(|_| {
                ApiError::ServiceUnavailable("native manifest representation unsupported".into())
            })?;
        let response = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")
            .header(header::CACHE_CONTROL, "private, no-store")
            .header(header::CONTENT_LENGTH, rewritten.len())
            .body(Body::from(rewritten))
            .map_err(|_| ApiError::Internal("manifest response construction failed".into()))?;
        super::super::hls::complete_buffered_response_before(
            state,
            &session,
            owner.as_ref().expect("manifest owner"),
            if manifest_resource == Resource::Master {
                crate::transcode::MediaResponsePublication::generation_metadata("jellyfin-master")
            } else {
                crate::transcode::MediaResponsePublication::attempt_media(
                    "jellyfin-manifest",
                    Some(&manifest_resource.native_path()),
                )
            },
            false,
            response,
            deadline,
        )
        .await?
    } else {
        response
    };
    // Repeat the authenticated binding after buffered adaptation and before publishing headers.
    let fresh = playback::binding(
        client,
        state,
        &play.negotiation.play_id,
        &play.negotiation.item_wire_id,
        &play.negotiation.file_wire_id,
    )
    .await?;
    if route(state, &fresh).await? != session {
        return Err(ApiError::Conflict("native play replaced".into()));
    }
    if method == Method::HEAD {
        *response.body_mut() = Body::empty();
    }
    Ok(response)
}
