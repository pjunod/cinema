//! Contract bootstrap and session routes with no media effect: public and
//! authenticated server identity, the public user list, capability reports,
//! search hints and the download refusal.
use super::*;

/// Always empty: native Plurx has no policy that lists its users to anyone
/// who has not signed in, so the clients' manual sign-in is the only path.
pub(super) async fn public_users(_enabled: Enabled) -> Json<Vec<Value>> {
    Json(Vec::new())
}

/// The authenticated form of `System/Info`: the public identity plus the
/// state flags clients read, and nothing administrative (no paths, no
/// encoder, no update channel). `Version` stays the tested protocol baseline;
/// `PackageName` carries the Plurx build, so a support report names both.
pub(super) async fn system_info_authenticated(
    _client: ClientUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({
        "ServerName": server_name(&state).await?,
        "Version": plurx_compat_jellyfin::catalog::PROTOCOL_BASELINE_VERSION,
        "ProductName": plurx_compat_jellyfin::catalog::PROTOCOL_BASELINE_PRODUCT,
        "Id": server_id(&state).await?,
        "StartupWizardCompleted": state.store.count_users().await? > 0,
        "PackageName": format!("plurx {}", crate::version::LONG),
        "HasPendingRestart": false,
        "IsShuttingDown": false,
        "SupportsLibraryMonitor": false,
        "CanSelfRestart": false,
        "CanLaunchWebBrowser": false,
        "HasUpdateAvailable": false,
        "CompletedInstallations": [],
        "CastReceiverApplications": [],
    })))
}

/// `POST /Sessions/Capabilities` (query form) and `/Capabilities/Full`
/// (body form). Accepted and validated, not stored: the facade advertises no
/// remote control and has no session list, so nothing in Plurx would read a
/// stored report. A malformed report is still refused rather than ignored.
pub(super) async fn capabilities(
    _client: ClientUser,
    RawQuery(raw): RawQuery,
) -> Result<StatusCode, ApiError> {
    for (name, value) in query_pairs(raw.as_deref())? {
        match name.to_ascii_lowercase().as_str() {
            "supportsmediacontrol" | "supportspersistentidentifier" => {
                if !matches!(value.to_ascii_lowercase().as_str(), "true" | "false") {
                    return Err(ApiError::BadRequest("invalid capability flag".into()));
                }
            }
            _ => {}
        }
    }
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn capabilities_full(
    _client: ClientUser,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let Json(body) = body.map_err(|_| ApiError::BadRequest("invalid capability report".into()))?;
    if !body.is_object() {
        return Err(ApiError::BadRequest("invalid capability report".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /Search/Hints` over the same catalog query `Items?searchTerm=` uses.
/// Item types Plurx does not hold (people, music, channels) match nothing.
pub(super) async fn search_hints(
    client: ClientUser,
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let mut pairs = query_pairs(raw.as_deref())?;
    let mut asked_types = false;
    pairs.retain_mut(|(key, value)| {
        if !key.eq_ignore_ascii_case("includeItemTypes") {
            return true;
        }
        asked_types = true;
        *value = value
            .split(',')
            .filter(|kind| {
                ["Movie", "Series", "Season", "Episode"]
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(kind.trim()))
            })
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(",");
        !value.is_empty()
    });
    let supported_types = pairs
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case("includeItemTypes"));
    if asked_types && !supported_types {
        return Ok(Json(json!({"SearchHints": [], "TotalRecordCount": 0})));
    }
    let mut query = BrowseQuery::parse(
        &pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>(),
    )
    .map_err(|_| ApiError::BadRequest("invalid search query".into()))?;
    if query.search.as_deref().is_none_or(str::is_empty) {
        return Err(ApiError::BadRequest("searchTerm is required".into()));
    }
    query.recursive = true;
    let page = catalog(&client, &state, query, None).await?;
    let hints = page
        .items
        .iter()
        .map(|item| {
            let value = serde_json::to_value(item)?;
            Ok(json!({
                "Id": value["Id"],
                "ItemId": value["Id"],
                "Name": value["Name"],
                "Type": value["Type"],
                "IsFolder": value["IsFolder"],
                "MediaType": value["MediaType"],
                "ProductionYear": value["ProductionYear"],
                "IndexNumber": value["IndexNumber"],
                "ParentIndexNumber": value["ParentIndexNumber"],
                "RunTimeTicks": value["RunTimeTicks"],
                "Series": value["SeriesName"],
                "SeriesId": value["SeriesId"],
                "PrimaryImageTag": value["ImageTags"]["Primary"],
            }))
        })
        .collect::<Result<Vec<_>, serde_json::Error>>()
        .map_err(|_| ApiError::ServiceUnavailable("search DTO unavailable".into()))?;
    Ok(Json(json!({
        "SearchHints": hints,
        "TotalRecordCount": page.total_record_count,
    })))
}

/// Downloads are not offered through this facade (`EnableContentDownloading`
/// is false in every user policy it returns), and this says so for a mapped
/// item instead of answering a generic 404.
pub(super) async fn download(
    client: ClientUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    playback::live_item(&client, &state, &id).await?;
    Err(ApiError::typed(
        StatusCode::FORBIDDEN,
        "download_not_offered",
        "Downloads are not offered through the Jellyfin compatibility connection.",
    ))
}
