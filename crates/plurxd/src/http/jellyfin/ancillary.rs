//! Authenticated observed ancillary calls over native catalog and subtitles.
use super::*;
async fn mapped_file(
    state: &AppState,
    item: &wire::Item,
    source: &wire::MediaSource,
) -> Result<plurx_core::domain::MediaFile, ApiError> {
    let item_id = state
        .store
        .jellyfin_resolve_entity(JellyfinEntityKind::Item, &item.id.to_hex())
        .await?
        .ok_or(ApiError::NotFound("item"))?;
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
    if file.item_id != item_id {
        return Err(ApiError::NotFound("source membership"));
    }
    Ok(file)
}
async fn media_item(
    client: &ClientUser,
    state: &AppState,
    id: &str,
) -> Result<wire::Item, ApiError> {
    let mut query = browse(None)?;
    query.limit = Some(1);
    query.recursive = true;
    let item = catalog(client, state, query, Some(wire_id(id)?))
        .await?
        .items
        .into_iter()
        .next()
        .ok_or(ApiError::NotFound("catalog item"))?;
    if item.media_sources.as_ref().is_none_or(Vec::is_empty) {
        return Err(ApiError::NotFound("media item"));
    }
    Ok(item)
}
pub(super) async fn intros(
    client: ClientUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
    for (name, value) in query_pairs(raw.as_deref())? {
        match name.to_ascii_lowercase().as_str() {
            "userid" => check_user(&client, &value)?,
            "api_key" | "apikey" => {}
            _ => return Err(ApiError::BadRequest("unsupported intro query".into())),
        }
    }
    media_item(&client, &state, &id).await?;
    // Native Plurx has no pre-roll item collection. Skip markers are exposed
    // separately through MediaSegments, not invented as playable intro items.
    Ok(Json(
        json!({"Items":[], "TotalRecordCount":0, "StartIndex":0}),
    ))
}
pub(super) async fn segments(
    client: ClientUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let mut included = Vec::new();
    for (name, value) in query_pairs(raw.as_deref())? {
        match name.to_ascii_lowercase().as_str() {
            "includesegmenttypes" => {
                for kind in value.split(',') {
                    if !matches!(
                        kind,
                        "Unknown" | "Commercial" | "Preview" | "Recap" | "Outro" | "Intro"
                    ) {
                        return Err(ApiError::BadRequest("unsupported segment type".into()));
                    }
                    if included.len() >= 32 {
                        return Err(ApiError::BadRequest("too many segment types".into()));
                    }
                    included.push(kind.to_owned());
                }
            }
            "api_key" | "apikey" => {}
            _ => return Err(ApiError::BadRequest("unsupported segment query".into())),
        }
    }
    let item = media_item(&client, &state, &id).await?;
    let source = item
        .media_sources
        .as_ref()
        .and_then(|v| v.first())
        .ok_or(ApiError::NotFound("source"))?;
    let file = mapped_file(&state, &item, source).await?;
    let mut output = Vec::new();
    for marker in super::super::stream::markers_for(&state, &file).await {
        let kind = match marker.kind.as_str() {
            "intro" => "Intro",
            "credits" => "Outro",
            "preview" => "Preview",
            _ => continue,
        };
        if !included.is_empty() && !included.iter().any(|s| s == kind) {
            continue;
        }
        let start = Ticks::from_milliseconds(marker.start_ms)
            .map_err(|_| ApiError::Internal("invalid native segment".into()))?;
        let end = Ticks::from_milliseconds(marker.end_ms)
            .map_err(|_| ApiError::Internal("invalid native segment".into()))?;
        if start.value() >= end.value() {
            continue;
        }
        let key = plurx_core::auth::hash_token(&format!(
            "plurx/jellyfin/segment/v1:{}:{}:{kind}:{}:{}",
            item.id.to_hex(),
            source.id.to_hex(),
            start.value(),
            end.value()
        ));
        output.push(json!({"Id":&key[..32], "ItemId":item.id, "Type":kind,"StartTicks":start,"EndTicks":end}));
    }
    Ok(Json(
        json!({"TotalRecordCount":output.len(), "Items":output, "StartIndex":0}),
    ))
}
pub(super) async fn subtitles(
    client: ClientUser,
    State(state): State<AppState>,
    Path((item_id, source_id, index, filename)): Path<(String, String, i64, String)>,
) -> Result<Response, ApiError> {
    if index < 0 {
        return Err(ApiError::NotFound("subtitle track"));
    }
    let srt = match filename.as_str() {
        "Stream.vtt" | "stream.vtt" => false,
        "Stream.srt" | "stream.srt" => true,
        _ => return Err(ApiError::NotFound("subtitle format")),
    };
    let item = media_item(&client, &state, &item_id).await?;
    let source_wire = wire_id(&source_id)?;
    let source = item
        .media_sources
        .as_ref()
        .and_then(|v| v.iter().find(|s| s.id == source_wire))
        .ok_or(ApiError::NotFound("source"))?;
    let ordinal = source
        .media_streams
        .iter()
        .filter(|s| s.stream_type == wire::StreamType::Subtitle)
        .position(|s| s.index == index)
        .ok_or(ApiError::NotFound("subtitle track"))?;
    let file_id = mapped_file(&state, &item, source).await?.id;
    let user = state
        .store
        .get_user(client.identity.native_id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let response = super::super::stream::subtitles_vtt(
        super::super::extract::AuthUser(user),
        State(state),
        Path((file_id, format!("{ordinal}.vtt"))),
    )
    .await?;
    if !srt {
        return Ok(response);
    }
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .map_err(|_| ApiError::Internal("subtitle representation exceeds its bound".into()))?;
    let bytes = plurx_compat_jellyfin::subtitle::vtt_to_srt(&bytes)
        .map_err(|_| ApiError::BadRequest("subtitle cannot be represented as SRT".into()))?;
    Ok((
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/x-subrip; charset=utf-8",
            ),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response())
}
