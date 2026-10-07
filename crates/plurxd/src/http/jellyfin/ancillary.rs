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
/// Jellyfin's subtitle window query: `StartPositionTicks` (a query value only
/// on the four-segment route), `EndPositionTicks`, `CopyTimestamps` and
/// `AddVttTimeMap`. Credential carriers are read by the extractor; any other
/// key is ignored, as the upstream controller ignores it.
#[derive(Default)]
struct SubtitleWindow {
    start: Option<Ticks>,
    end: Option<Ticks>,
    copy_timestamps: bool,
    time_map: bool,
}
fn subtitle_window(raw: Option<&str>) -> Result<SubtitleWindow, ApiError> {
    fn assign<T: PartialEq>(slot: &mut Option<T>, value: T) -> Result<(), ApiError> {
        if slot.as_ref().is_some_and(|old| *old != value) {
            return Err(ApiError::BadRequest("conflicting subtitle query".into()));
        }
        *slot = Some(value);
        Ok(())
    }
    let ticks = |value: &str| {
        value
            .parse::<i64>()
            .ok()
            .and_then(|v| Ticks::try_from(v).ok())
            .ok_or_else(|| ApiError::BadRequest("invalid subtitle position".into()))
    };
    let flag = |value: &str| match value.to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ApiError::BadRequest("invalid subtitle flag".into())),
    };
    let (mut start, mut end, mut copy, mut map) = (None, None, None, None);
    for (name, value) in query_pairs(raw)? {
        match name.to_ascii_lowercase().as_str() {
            "startpositionticks" => assign(&mut start, ticks(&value)?)?,
            "endpositionticks" => assign(&mut end, ticks(&value)?)?,
            "copytimestamps" => assign(&mut copy, flag(&value)?)?,
            "addvtttimemap" => assign(&mut map, flag(&value)?)?,
            _ => {}
        }
    }
    Ok(SubtitleWindow {
        start,
        end,
        copy_timestamps: copy.unwrap_or(false),
        time_map: map.unwrap_or(false),
    })
}
/// `…/Subtitles/{index}/Stream.{format}`: the start position, if any, is a
/// query value.
pub(super) async fn subtitles(
    client: ClientUser,
    state: State<AppState>,
    Path((item_id, source_id, index, filename)): Path<(String, String, i64, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let window = subtitle_window(raw.as_deref())?;
    serve_subtitle(client, state, item_id, source_id, index, filename, window).await
}
/// `…/Subtitles/{index}/{startPositionTicks}/Stream.{format}`: the form both
/// pinned clients request and the form `DeliveryUrl` names. A query start,
/// when present, overrides the path value, as upstream.
pub(super) async fn subtitles_from(
    client: ClientUser,
    state: State<AppState>,
    Path((item_id, source_id, index, start, filename)): Path<(String, String, i64, i64, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let mut window = subtitle_window(raw.as_deref())?;
    if window.start.is_none() {
        window.start = Some(
            Ticks::try_from(start)
                .map_err(|_| ApiError::BadRequest("invalid subtitle position".into()))?,
        );
    }
    serve_subtitle(client, state, item_id, source_id, index, filename, window).await
}
async fn serve_subtitle(
    client: ClientUser,
    State(state): State<AppState>,
    item_id: String,
    source_id: String,
    index: i64,
    filename: String,
    window: SubtitleWindow,
) -> Result<Response, ApiError> {
    use plurx_compat_jellyfin::subtitle::SubtitleFormat;
    if index < 0 {
        return Err(ApiError::NotFound("subtitle track"));
    }
    let format = filename
        .split_once('.')
        .filter(|(stem, _)| stem.eq_ignore_ascii_case("stream"))
        .and_then(|(_, extension)| SubtitleFormat::parse(extension))
        .ok_or(ApiError::NotFound("subtitle format"))?;
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
    let start_ms = window.start.map_or(0, Ticks::milliseconds);
    let end_ms = window.end.map(Ticks::milliseconds).filter(|end| *end > 0);
    if format == SubtitleFormat::Vtt && start_ms == 0 && end_ms.is_none() && !window.time_map {
        // The native extraction itself, source times unchanged.
        return Ok(response);
    }
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .map_err(|_| ApiError::Internal("subtitle representation exceeds its bound".into()))?;
    let bytes = plurx_compat_jellyfin::subtitle::window_vtt(
        &bytes,
        u64::try_from(start_ms).unwrap_or(0),
        end_ms.and_then(|end| u64::try_from(end).ok()),
        window.copy_timestamps,
    )
    .map_err(|_| ApiError::BadRequest("subtitle window cannot be represented".into()))?;
    let bytes = match format {
        SubtitleFormat::Srt => plurx_compat_jellyfin::subtitle::vtt_to_srt(&bytes)
            .map_err(|_| ApiError::BadRequest("subtitle cannot be represented as SRT".into()))?,
        // Upstream's HLS segment marker; only meaningful to a VTT consumer.
        SubtitleFormat::Vtt if window.time_map => {
            let text = String::from_utf8(bytes)
                .map_err(|_| ApiError::BadRequest("subtitle is not UTF-8".into()))?;
            text.replacen(
                "WEBVTT",
                "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000",
                1,
            )
            .into_bytes()
        }
        SubtitleFormat::Vtt => bytes,
    };
    Ok((
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, format.content_type()),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response())
}
