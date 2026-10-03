//! Jellyfin connection and catalog facade over native authentication and Store.
mod playback;
use super::{auth, error::ApiError};
use crate::state::AppState;
use axum::{
    extract::{DefaultBodyLimit, FromRequestParts, Path, RawQuery, State},
    http::{request::Parts, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use plurx_compat_jellyfin::{
    catalog as wire, credentials,
    identity::WireId,
    query::{BrowseQuery, Sort},
    ticks::Ticks,
};
use plurx_core::store::{
    JellyfinCatalogIdentity, JellyfinCatalogMode, JellyfinCatalogQuery, JellyfinCatalogSort,
    JellyfinClientFamily, JellyfinEntityKind,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(not_found))
        .route("/System/Info/Public", get(system_info))
        .route(
            "/Users/AuthenticateByName",
            post(login).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/Items/{item_id}/PlaybackInfo",
            get(playback::info_get)
                .post(playback::info_post)
                .layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route("/Videos/{item_id}/stream", get(playback::direct))
        .route(
            "/Videos/{item_id}/{filename}",
            get(playback::direct_extension),
        )
        .route(
            "/Sessions/Playing",
            post(playback::playing).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/Sessions/Playing/Progress",
            post(playback::progress).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/Sessions/Playing/Stopped",
            post(playback::stopped).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/Users/{user_id}/PlayedItems/{item_id}",
            post(playback::mark_played).delete(playback::mark_unplayed),
        )
        .route("/Sessions/Logout", post(playback::logout))
        .route("/Users/{user_id}", get(user))
        .route("/Users/Me", get(me))
        .route("/Users/{user_id}/Views", get(views))
        .route("/UserViews/GroupingOptions", get(grouping_options))
        .route("/Library/VirtualFolders", get(virtual_folders))
        .route("/DisplayPreferences/{id}", get(display_preferences))
        .route("/Items/{item_id}/LocalTrailers", get(local_extras))
        .route("/Items/{item_id}/SpecialFeatures", get(local_extras))
        .route("/UserViews", get(current_views))
        .route("/Items/Latest", get(latest))
        .route("/Items/Resume", get(resume))
        .route("/UserItems/Resume", get(resume))
        .route("/Users/{user_id}/Items/Latest", get(user_latest))
        .route("/Users/{user_id}/Items/Resume", get(user_resume))
        .route("/Shows/Upcoming", get(upcoming))
        .route("/Items/{item_id}/Similar", get(similar))
        .route("/Shows/NextUp", get(next_up))
        .route("/Items", get(items))
        .route("/Users/{user_id}/Items", get(user_items))
        .route("/Items/{item_id}/Images/{kind}", get(image))
        .route("/Items/{item_id}/Images/{kind}/{index}", get(indexed_image))
        .route("/Items/{item_id}", get(item))
        .route("/Users/{user_id}/Items/{item_id}", get(user_item))
        .route("/Shows/{item_id}/Seasons", get(seasons))
        .route("/Shows/{item_id}/Episodes", get(episodes))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
}
pub(super) async fn not_found() -> ApiError {
    ApiError::NotFound("Jellyfin endpoint")
}
async fn method_not_allowed() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({"error":"method not allowed"})),
    )
        .into_response()
}

struct Enabled(String);
impl FromRequestParts<AppState> for Enabled {
    type Rejection = ApiError;
    async fn from_request_parts(_parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let setting = state.store.jellyfin_compatibility_state().await?;
        if !setting.enabled {
            return Err(ApiError::NotFound("Jellyfin endpoint"));
        }
        setting.generation.map(Self).ok_or_else(|| {
            ApiError::ServiceUnavailable("compatibility configuration is incomplete".into())
        })
    }
}
fn query_pairs(raw: Option<&str>) -> Result<Vec<(String, String)>, ApiError> {
    let raw = raw.unwrap_or("");
    if raw.len() > 8192 {
        return Err(ApiError::BadRequest(
            "catalog query exceeds its bound".into(),
        ));
    }
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(raw)
        .map_err(|_| ApiError::BadRequest("invalid catalog query".into()))?;
    if pairs.len() > 64 {
        return Err(ApiError::BadRequest(
            "catalog query exceeds its bound".into(),
        ));
    }
    Ok(pairs)
}
fn carriers(
    headers: &HeaderMap,
    pairs: &[(String, String)],
) -> Result<Vec<(String, String)>, ApiError> {
    let mut result = Vec::new();
    for (name, value) in headers {
        if matches!(
            name.as_str(),
            "authorization" | "x-emby-authorization" | "x-emby-token"
        ) {
            result.push((
                name.as_str().into(),
                value
                    .to_str()
                    .map_err(|_| ApiError::BadRequest("invalid credential carrier".into()))?
                    .into(),
            ));
        }
    }
    for (name, value) in pairs {
        if name.eq_ignore_ascii_case("api_key") || name.eq_ignore_ascii_case("apikey") {
            result.push((format!("query:{name}"), value.clone()));
        }
    }
    Ok(result)
}
struct ClientUser {
    identity: JellyfinCatalogIdentity,
    token_hash: String,
}
impl FromRequestParts<AppState> for ClientUser {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        Enabled::from_request_parts(parts, state).await?;
        let pairs = query_pairs(parts.uri.query())?;
        let values = carriers(&parts.headers, &pairs)?;
        let refs = values
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>();
        let token = credentials::parse_user_token(&refs)
            .map_err(|_| ApiError::BadRequest("invalid credential carrier".into()))?
            .ok_or(ApiError::Unauthorized)?;
        super::extract::authenticate_user_token(state, token.expose()).await?;
        let token_hash = plurx_core::auth::hash_token(token.expose());
        let identity = identity(state, &token_hash).await?;
        Ok(Self {
            identity,
            token_hash,
        })
    }
}
async fn identity(state: &AppState, hash: &str) -> Result<JellyfinCatalogIdentity, ApiError> {
    for _ in 0..4 {
        let current = state
            .store
            .jellyfin_catalog_identity(hash.to_owned())
            .await?
            .ok_or(ApiError::Unauthorized)?;
        if current.libraries.len() > 500 {
            return Err(ApiError::ServiceUnavailable(
                "supported library count exceeds its bound".into(),
            ));
        }
        let missing = current
            .libraries
            .iter()
            .filter(|l| l.wire_id.is_none())
            .map(|l| l.native_id)
            .collect::<Vec<_>>();
        if current.wire_id.is_some() && missing.is_empty() {
            return Ok(current);
        }
        if current.wire_id.is_none() {
            state
                .store
                .jellyfin_entity_ids(JellyfinEntityKind::User, &[current.native_id])
                .await?;
        }
        if !missing.is_empty() {
            state
                .store
                .jellyfin_entity_ids(JellyfinEntityKind::Library, &missing)
                .await?;
        }
        // Never attach newly allocated IDs to the old name/library snapshot.
    }
    Err(ApiError::ServiceUnavailable(
        "catalog changed during identity preparation".into(),
    ))
}
fn wire_id(value: &str) -> Result<WireId, ApiError> {
    WireId::parse(value).map_err(|_| ApiError::BadRequest("invalid item identity".into()))
}
fn check_user(client: &ClientUser, id: &str) -> Result<(), ApiError> {
    let id = wire_id(id)?;
    if client.identity.wire_id.as_deref() != Some(id.to_hex().as_str()) {
        return Err(ApiError::Forbidden);
    }
    Ok(())
}
async fn server_id(state: &AppState) -> Result<String, ApiError> {
    Ok(state.store.instance_id().await?)
}
async fn server_name(state: &AppState) -> Result<String, ApiError> {
    Ok(state
        .store
        .get_setting(plurx_core::store::keys::SERVER_NAME)
        .await?
        .unwrap_or_else(|| state.server_name.clone()))
}
async fn system_info(
    _enabled: Enabled,
    State(state): State<AppState>,
) -> Result<Json<wire::PublicSystemInfo>, ApiError> {
    Ok(Json(wire::PublicSystemInfo {
        server_name: server_name(&state).await?,
        version: env!("CARGO_PKG_VERSION"),
        product_name: "Plurx",
        id: server_id(&state).await?,
        startup_wizard_completed: state.store.count_users().await? > 0,
    }))
}
fn user_dto(
    identity: &JellyfinCatalogIdentity,
    server_id: String,
    server_name: String,
) -> Result<wire::User, ApiError> {
    Ok(wire::User {
        id: wire_id(identity.wire_id.as_deref().ok_or(ApiError::Unauthorized)?)?,
        name: identity.name.clone(),
        server_id,
        server_name,
        has_password: true,
        has_configured_password: true,
        enable_auto_login: false,
        policy: wire::UserPolicy {
            is_administrator: false,
            is_hidden: false,
            is_disabled: false,
            enable_media_playback: true,
            enable_remote_control_of_other_users: false,
            enable_shared_device_control: false,
            enable_content_downloading: false,
            enable_all_folders: true,
        },
    })
}
#[derive(Deserialize)]
struct LoginBody {
    #[serde(rename = "Username")]
    username: String,
    #[serde(rename = "Pw")]
    password: String,
}
async fn login(
    Enabled(generation): Enabled,
    auth::ClientPeer(peer): auth::ClientPeer,
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<LoginBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<wire::AuthenticationResult>, ApiError> {
    let Json(body) =
        body.map_err(|_| ApiError::BadRequest("invalid authentication body".into()))?;
    let values = carriers(&headers, &[])?;
    let refs = values
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect::<Vec<_>>();
    let metadata = credentials::parse_client_identity(&refs)
        .map_err(|_| ApiError::BadRequest("invalid client metadata".into()))?
        .ok_or_else(|| ApiError::BadRequest("client metadata is required".into()))?;
    let family = match metadata.client.as_deref() {
        Some("Infuse-Direct") | Some("Infuse") => JellyfinClientFamily::Infuse,
        Some("Jellyfin+Android+TV") | Some("Jellyfin Android TV") => {
            JellyfinClientFamily::AndroidTv
        }
        _ => return Err(ApiError::BadRequest("unsupported client family".into())),
    };
    let device_id = metadata
        .device_id
        .ok_or_else(|| ApiError::BadRequest("client device ID is required".into()))?;
    let authenticated = auth::login_jellyfin_user_if_enabled(
        &state,
        peer,
        &headers,
        auth::LoginRequest {
            username: body.username,
            password: body.password,
            device: metadata.device,
        },
        &device_id,
        family,
        &generation,
    )
    .await?;
    let identity = identity(&state, &plurx_core::auth::hash_token(&authenticated.token)).await?;
    let id = server_id(&state).await?;
    Ok(Json(wire::AuthenticationResult {
        user: user_dto(&identity, id.clone(), server_name(&state).await?)?,
        access_token: authenticated.token,
        server_id: id,
    }))
}
async fn me(
    client: ClientUser,
    State(state): State<AppState>,
) -> Result<Json<wire::User>, ApiError> {
    Ok(Json(user_dto(
        &client.identity,
        server_id(&state).await?,
        server_name(&state).await?,
    )?))
}
async fn user(
    client: ClientUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<wire::User>, ApiError> {
    check_user(&client, &id)?;
    me(client, State(state)).await
}
async fn current_views(
    client: ClientUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let id = server_id(&state).await?;
    let items=client.identity.libraries.iter().map(|l|json!({"Id":l.wire_id,"ServerId":id,"Name":l.name,"SortName":l.name,"Type":"CollectionFolder","IsFolder":true,"CollectionType":if l.kind=="movies" {"movies"} else {"tvshows"},"LocationType":"Virtual"})).collect::<Vec<_>>();
    Ok(Json(
        json!({"TotalRecordCount":items.len(),"StartIndex":0,"Items":items}),
    ))
}
async fn views(
    client: ClientUser,
    state: State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    check_user(&client, &id)?;
    current_views(client, state).await
}
async fn grouping_options(client: ClientUser) -> Json<Value> {
    Json(Value::Array(
        client
            .identity
            .libraries
            .iter()
            .map(|l| json!({"Id":l.wire_id,"Name":l.name}))
            .collect(),
    ))
}
async fn virtual_folders(client: ClientUser) -> Json<Value> {
    // Internal scan roots are not Jellyfin source locations and never cross
    // the compatibility DTO boundary. These are logical mapped library views.
    Json(Value::Array(client.identity.libraries.iter().map(|l|json!({"Name":l.name,"ItemId":l.wire_id,"CollectionType":if l.kind=="movies" {"movies"} else {"tvshows"},"Locations":[]})).collect()))
}
async fn display_preferences(
    client: ClientUser,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(ApiError::BadRequest(
            "invalid display preference identity".into(),
        ));
    }
    let pairs = query_pairs(raw.as_deref())?;
    let query = BrowseQuery::parse(
        &pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>(),
    )
    .map_err(|_| ApiError::BadRequest("invalid display preference query".into()))?;
    if let Some(uid) = query.user {
        check_user(&client, &uid.to_hex())?;
    }
    let mut client_name = None;
    for (key, value) in &pairs {
        if key.eq_ignore_ascii_case("client") {
            if value.len() > 64
                || value.chars().any(char::is_control)
                || client_name.as_ref().is_some_and(|prior| prior != value)
            {
                return Err(ApiError::BadRequest("invalid preference client".into()));
            }
            client_name = Some(value.clone());
        }
    }
    // No persisted per-client view preference exists in native Plurx. Return
    // its initial presentation explicitly; unknown writes remain honest 405s.
    Ok(Json(
        json!({"Id":id,"Client":client_name,"ViewType":"Poster","SortBy":"SortName","IndexBy":null,"RememberIndexing":false,"PrimaryImageHeight":250,"PrimaryImageWidth":165,"CustomPrefs":{},"ScrollDirection":"Horizontal","ShowBackdrop":true,"RememberSorting":false,"SortOrder":"Ascending","ShowSidebar":false}),
    ))
}
async fn local_extras(
    client: ClientUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Value>>, ApiError> {
    let q = browse(None)?;
    let page = catalog(&client, &state, q, Some(wire_id(&id)?)).await?;
    if page.items.is_empty() {
        return Err(ApiError::NotFound("catalog item"));
    }
    // Native Movies/TV has no separately classified trailers or special
    // features. Alternate source files are not extras and stay in MediaSources.
    Ok(Json(vec![]))
}

fn browse(raw: Option<&str>) -> Result<BrowseQuery, ApiError> {
    let pairs = query_pairs(raw)?;
    BrowseQuery::parse(
        &pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>(),
    )
    .map_err(|_| ApiError::BadRequest("invalid catalog query".into()))
}
async fn catalog(
    client: &ClientUser,
    state: &AppState,
    q: BrowseQuery,
    target: Option<WireId>,
) -> Result<wire::Items<wire::Item>, ApiError> {
    catalog_mode(client, state, q, target, JellyfinCatalogMode::Browse).await
}
async fn catalog_mode(
    client: &ClientUser,
    state: &AppState,
    q: BrowseQuery,
    target: Option<WireId>,
    mode: JellyfinCatalogMode,
) -> Result<wire::Items<wire::Item>, ApiError> {
    if let Some(user) = &q.user {
        check_user(client, &user.to_hex())?;
    }
    if q.series.is_some() || q.season.is_some() || q.kinds.iter().any(|k| k == "library") {
        return Err(ApiError::BadRequest("unsupported catalog filter".into()));
    }
    let query = JellyfinCatalogQuery {
        mode,
        today: if matches!(mode, JellyfinCatalogMode::Upcoming) {
            Some(plurx_core::scan::home::date_from_unix(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64,
            ))
        } else {
            None
        },
        user_wire: client
            .identity
            .wire_id
            .clone()
            .ok_or(ApiError::Unauthorized)?,
        token_hash: client.token_hash.clone(),
        parent_wire: q.parent.map(|id| id.to_hex()),
        item_wire: target.map(|id| id.to_hex()),
        recursive: q.recursive,
        start: q.start,
        limit: q.limit,
        kinds: q.kinds,
        search: q.search,
        descending: q.descending,
        sorts: q
            .sorts
            .into_iter()
            .map(|s| match s {
                Sort::SortName => JellyfinCatalogSort::SortName,
                Sort::Name => JellyfinCatalogSort::Name,
                Sort::Added => JellyfinCatalogSort::Added,
                Sort::Year => JellyfinCatalogSort::Year,
                Sort::PremiereDate => JellyfinCatalogSort::PremiereDate,
                Sort::Index => JellyfinCatalogSort::Index,
                Sort::ParentIndex => JellyfinCatalogSort::ParentIndex,
            })
            .collect(),
    };
    let mut query = query;
    match mode {
        JellyfinCatalogMode::Resume => {
            query.sorts = vec![JellyfinCatalogSort::WatchUpdated];
            query.descending = true;
        }
        JellyfinCatalogMode::Upcoming => {
            query.sorts = vec![JellyfinCatalogSort::PremiereDate];
            query.descending = false;
        }
        JellyfinCatalogMode::Latest => {
            query.sorts = vec![JellyfinCatalogSort::Added];
            query.descending = true;
        }
        _ => {}
    }
    for _ in 0..4 {
        let page = state.store.jellyfin_catalog_page(query.clone()).await?;
        if !page.authorized {
            return Err(ApiError::Unauthorized);
        }
        if !page.parent_valid {
            return Err(ApiError::NotFound("catalog parent"));
        }
        if page.source_overflow {
            return Err(ApiError::ServiceUnavailable(
                "catalog source count exceeds its bound".into(),
            ));
        }
        if page.missing.is_empty() {
            let server = server_id(state).await?;
            let items = page
                .items
                .iter()
                .map(|row| item_dto(row, &server))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(wire::Items {
                items,
                total_record_count: page.total as u64,
                start_index: q.start as u64,
            });
        }
        for (name, kind) in [
            ("item", JellyfinEntityKind::Item),
            ("file", JellyfinEntityKind::File),
            ("library", JellyfinEntityKind::Library),
        ] {
            let ids = page
                .missing
                .iter()
                .filter(|m| m.kind == name)
                .map(|m| m.id)
                .collect::<Vec<_>>();
            for chunk in ids.chunks(1000) {
                state.store.jellyfin_entity_ids(kind, chunk).await?;
            }
        }
    }
    Err(ApiError::ServiceUnavailable(
        "catalog changed during page preparation".into(),
    ))
}
async fn items(
    client: ClientUser,
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<wire::Items<wire::Item>>, ApiError> {
    Ok(Json(
        catalog(&client, &state, browse(raw.as_deref())?, None).await?,
    ))
}
async fn user_items(
    client: ClientUser,
    state: State<AppState>,
    Path(id): Path<String>,
    raw: RawQuery,
) -> Result<Json<wire::Items<wire::Item>>, ApiError> {
    check_user(&client, &id)?;
    items(client, state, raw).await
}
async fn item(
    client: ClientUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<wire::Item>, ApiError> {
    let mut q = browse(raw.as_deref())?;
    q.start = 0;
    q.limit = 1;
    q.recursive = true;
    catalog(&client, &state, q, Some(wire_id(&id)?))
        .await?
        .items
        .into_iter()
        .next()
        .map(Json)
        .ok_or(ApiError::NotFound("catalog item"))
}
async fn user_item(
    client: ClientUser,
    state: State<AppState>,
    Path((uid, id)): Path<(String, String)>,
    raw: RawQuery,
) -> Result<Json<wire::Item>, ApiError> {
    check_user(&client, &uid)?;
    item(client, state, Path(id), raw).await
}
async fn seasons(
    client: ClientUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<wire::Items<wire::Item>>, ApiError> {
    let mut q = browse(raw.as_deref())?;
    q.parent = Some(wire_id(&id)?);
    q.recursive = false;
    q.kinds = vec!["season".into()];
    Ok(Json(catalog(&client, &state, q, None).await?))
}
async fn episodes(
    client: ClientUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<wire::Items<wire::Item>>, ApiError> {
    let mut q = browse(raw.as_deref())?;
    q.parent = Some(q.season.take().unwrap_or(wire_id(&id)?));
    q.recursive = true;
    q.kinds = vec!["episode".into()];
    Ok(Json(catalog(&client, &state, q, None).await?))
}
async fn resume(
    client: ClientUser,
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<wire::Items<wire::Item>>, ApiError> {
    let mut q = browse(raw.as_deref())?;
    q.recursive = true;
    Ok(Json(
        catalog_mode(&client, &state, q, None, JellyfinCatalogMode::Resume).await?,
    ))
}
async fn user_resume(
    client: ClientUser,
    state: State<AppState>,
    Path(uid): Path<String>,
    raw: RawQuery,
) -> Result<Json<wire::Items<wire::Item>>, ApiError> {
    check_user(&client, &uid)?;
    resume(client, state, raw).await
}
async fn latest(
    client: ClientUser,
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Vec<wire::Item>>, ApiError> {
    let mut q = browse(raw.as_deref())?;
    q.recursive = true;
    Ok(Json(
        catalog_mode(&client, &state, q, None, JellyfinCatalogMode::Latest)
            .await?
            .items,
    ))
}
async fn user_latest(
    client: ClientUser,
    state: State<AppState>,
    Path(uid): Path<String>,
    raw: RawQuery,
) -> Result<Json<Vec<wire::Item>>, ApiError> {
    check_user(&client, &uid)?;
    latest(client, state, raw).await
}
async fn next_up(
    client: ClientUser,
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<wire::Items<wire::Item>>, ApiError> {
    let mut q = browse(raw.as_deref())?;
    q.recursive = true;
    if let Some(series) = q.series.take() {
        q.parent = Some(series);
    }
    Ok(Json(
        catalog_mode(&client, &state, q, None, JellyfinCatalogMode::NextUp).await?,
    ))
}

async fn upcoming(
    client: ClientUser,
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<wire::Items<wire::Item>>, ApiError> {
    let mut q = browse(raw.as_deref())?;
    q.recursive = true;
    Ok(Json(
        catalog_mode(&client, &state, q, None, JellyfinCatalogMode::Upcoming).await?,
    ))
}
async fn similar(
    client: ClientUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<wire::Items<wire::Item>>, ApiError> {
    let mut q = browse(raw.as_deref())?;
    q.recursive = true;
    Ok(Json(
        catalog_mode(
            &client,
            &state,
            q,
            Some(wire_id(&id)?),
            JellyfinCatalogMode::Similar,
        )
        .await?,
    ))
}

fn string(row: &Value, key: &str) -> Option<String> {
    row[key].as_str().map(str::to_owned)
}
fn optional_wire(row: &Value, key: &str) -> Result<Option<WireId>, ApiError> {
    row[key].as_str().map(wire_id).transpose()
}
fn optional_ticks(value: Option<i64>) -> Result<Option<Ticks>, ApiError> {
    value
        .map(Ticks::from_milliseconds)
        .transpose()
        .map_err(|_| ApiError::Internal("invalid catalog duration".into()))
}
fn item_dto(row: &Value, server: &str) -> Result<wire::Item, ApiError> {
    let kind = match row["kind"].as_str() {
        Some("movie") => wire::ItemType::Movie,
        Some("show") => wire::ItemType::Series,
        Some("season") => wire::ItemType::Season,
        Some("episode") => wire::ItemType::Episode,
        _ => return Err(ApiError::NotFound("catalog item")),
    };
    let folder = matches!(kind, wire::ItemType::Series | wire::ItemType::Season);
    let count = row["playable_count"].as_u64().unwrap_or(0);
    let watched_count = row["watched_count"].as_u64().unwrap_or(0);
    let played = if folder {
        count > 0 && watched_count == count
    } else {
        row["watched"].as_i64() == Some(1)
    };
    let pos = row["position_ms"].as_i64().unwrap_or(0);
    let mut providers = std::collections::BTreeMap::new();
    if let Some(id) = row["tmdb_id"].as_i64() {
        providers.insert("Tmdb".into(), id.to_string());
    }
    if let Some(id) = string(row, "imdb_id") {
        providers.insert("Imdb".into(), id);
    }
    let sources = row["sources"]
        .as_array()
        .ok_or_else(|| ApiError::Internal("invalid catalog sources".into()))?
        .iter()
        .map(source_dto)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(wire::Item {
        id: wire_id(
            row["wire_id"]
                .as_str()
                .ok_or(ApiError::NotFound("catalog item"))?,
        )?,
        server_id: server.into(),
        name: string(row, "title").unwrap_or_default(),
        sort_name: string(row, "sort_title").unwrap_or_default(),
        item_type: kind,
        is_folder: folder,
        parent_id: optional_wire(row, "parent_wire")?.or(optional_wire(row, "library_wire")?),
        series_id: match kind {
            wire::ItemType::Episode => optional_wire(row, "grandparent_wire")?,
            wire::ItemType::Season => optional_wire(row, "parent_wire")?,
            _ => None,
        },
        season_id: if kind == wire::ItemType::Episode {
            optional_wire(row, "parent_wire")?
        } else {
            None
        },
        series_name: match kind {
            wire::ItemType::Episode => string(row, "grandparent_title"),
            wire::ItemType::Season => string(row, "parent_title"),
            _ => None,
        },
        season_name: if kind == wire::ItemType::Episode {
            string(row, "parent_title")
        } else {
            None
        },
        collection_type: None,
        production_year: row["year"].as_i64().and_then(|n| i32::try_from(n).ok()),
        overview: string(row, "overview"),
        run_time_ticks: optional_ticks(row["runtime_ms"].as_i64())?,
        index_number: row[if kind == wire::ItemType::Episode {
            "episode_number"
        } else {
            "season_number"
        }]
        .as_i64()
        .and_then(|n| i32::try_from(n).ok()),
        parent_index_number: row["season_number"]
            .as_i64()
            .and_then(|n| i32::try_from(n).ok()),
        genres: row["genres"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        provider_ids: providers,
        image_tags: string(row, "poster_path")
            .map(|name| {
                std::collections::BTreeMap::from([(
                    "Primary".into(),
                    image_tag(row["wire_id"].as_str().unwrap_or_default(), &name),
                )])
            })
            .unwrap_or_default(),
        backdrop_image_tags: string(row, "backdrop_path")
            .map(|name| {
                vec![image_tag(
                    row["wire_id"].as_str().unwrap_or_default(),
                    &name,
                )]
            })
            .unwrap_or_default(),
        user_data: wire::UserData {
            playback_position_ticks: Ticks::from_milliseconds(pos)
                .map_err(|_| ApiError::Internal("invalid catalog position".into()))?,
            played,
            play_count: u64::from(played),
            is_favorite: false,
            played_percentage: row["runtime_ms"]
                .as_i64()
                .filter(|d| *d > 0)
                .map(|d| (pos as f64 * 100.0 / d as f64).clamp(0.0, 100.0)),
            unplayed_item_count: folder.then_some(count.saturating_sub(watched_count)),
        },
        media_type: (!folder).then(|| "Video".into()),
        child_count: row["child_count"].as_u64().unwrap_or(0),
        recursive_item_count: row["recursive_count"].as_u64().unwrap_or(0),
        media_source_count: sources.len() as u64,
        date_created: timestamp(row["added_at"].as_i64()),
        media_sources: Some(sources),
        location_type: "FileSystem",
        etag: format!(
            "{}:{}",
            row["wire_id"].as_str().unwrap_or_default(),
            row["updated_at"].as_i64().unwrap_or(0)
        ),
    })
}
fn timestamp(value: Option<i64>) -> Option<String> {
    let seconds = value.filter(|s| (0..=253_402_300_799).contains(s))?;
    let time = seconds % 86_400;
    Some(format!(
        "{}T{:02}:{:02}:{:02}Z",
        plurx_core::scan::home::date_from_unix(seconds),
        time / 3600,
        (time % 3600) / 60,
        time % 60
    ))
}

fn source_dto(row: &Value) -> Result<wire::MediaSource, ApiError> {
    let mut streams = Vec::new();
    if let Some(probe) = row["streams"].as_array() {
        for stream in probe {
            let (Some(index), Some(codec), Some(kind)) = (
                stream["index"].as_i64().filter(|i| *i >= 0),
                stream["codec_name"].as_str(),
                stream["codec_type"].as_str(),
            ) else {
                continue;
            };
            let stream_type = match kind {
                "video" => wire::StreamType::Video,
                "audio" => wire::StreamType::Audio,
                "subtitle" => wire::StreamType::Subtitle,
                _ => continue,
            };
            streams.push(wire::MediaStream {
                index,
                stream_type,
                codec: codec.into(),
                language: stream["tags"]["language"].as_str().map(str::to_owned),
                display_title: stream["tags"]["title"].as_str().map(str::to_owned),
                is_default: stream["disposition"]["default"].as_i64() == Some(1),
                is_forced: stream["disposition"]["forced"].as_i64() == Some(1),
                is_external: false,
                width: stream["width"].as_i64(),
                height: stream["height"].as_i64(),
                channels: stream["channels"].as_i64(),
                sample_rate: stream["sample_rate"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .or_else(|| stream["sample_rate"].as_i64()),
                bit_depth: stream["bits_per_raw_sample"]
                    .as_str()
                    .and_then(|s| s.parse().ok()),
                profile: stream["profile"].as_str().map(str::to_owned),
            });
        }
    }
    Ok(wire::MediaSource {
        id: wire_id(
            row["wire_id"]
                .as_str()
                .ok_or(ApiError::NotFound("catalog source"))?,
        )?,
        name: "Source".into(),
        container: string(row, "container"),
        run_time_ticks: optional_ticks(row["duration_ms"].as_i64())?,
        media_streams: streams,
        supports_direct_play: false,
        supports_direct_stream: false,
        supports_transcoding: false,
        default_audio_stream_index: None,
        default_subtitle_stream_index: None,
    })
}

pub(super) async fn enabled_gate(
    State(state): State<AppState>,
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Response {
    match state.store.jellyfin_compatibility_state().await {
        Ok(setting) if setting.enabled => next.run(request).await,
        Ok(_) => not_found().await.into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

fn image_tag(wire: &str, name: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(format!("{wire}:{name}")))
}
// Safe interim access policy: the shared ClientUser extractor verifies native
// token expiry/revocation and facade membership before any mapped artwork read.
// Anonymous access remains pending the user's explicit approval decision.
async fn image(
    _client: ClientUser,
    auth::ClientPeer(peer): auth::ClientPeer,
    State(state): State<AppState>,
    Path((id, kind)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    mapped_image(&state, peer, &headers, &id, &kind, raw.as_deref()).await
}
async fn indexed_image(
    _client: ClientUser,
    auth::ClientPeer(peer): auth::ClientPeer,
    State(state): State<AppState>,
    Path((id, kind, index)): Path<(String, String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if index != "0" {
        return Err(ApiError::NotFound("image index"));
    }
    mapped_image(&state, peer, &headers, &id, &kind, raw.as_deref()).await
}
async fn mapped_image(
    state: &AppState,
    peer: Option<std::net::SocketAddr>,
    headers: &HeaderMap,
    id: &str,
    kind: &str,
    raw: Option<&str>,
) -> Result<Response, ApiError> {
    let id = wire_id(id)?.to_hex();
    let backdrop = match kind {
        "Primary" => false,
        "Backdrop" => true,
        _ => return Err(ApiError::NotFound("image kind")),
    };
    let pairs = query_pairs(raw)?;
    let mut width = None;
    for (key, value) in &pairs {
        if key.eq_ignore_ascii_case("maxWidth") || key.eq_ignore_ascii_case("width") {
            let parsed = value
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=4096).contains(n))
                .ok_or_else(|| ApiError::BadRequest("invalid image width".into()))?;
            if width.is_some_and(|old| old != parsed) {
                return Err(ApiError::BadRequest("conflicting image widths".into()));
            }
            width = Some(parsed);
        }
    }
    let address = auth::client_ip(headers, peer, &state.trusted_proxies);
    super::images::admit_jellyfin_artwork(state, address).await?;
    let mapped = state
        .store
        .jellyfin_catalog_artwork(id, backdrop)
        .await?
        .ok_or(ApiError::NotFound("image"))?;
    super::images::serve_jellyfin_artwork(state, mapped, backdrop, width.unwrap_or(500), headers)
        .await
}

pub(super) async fn cache_policy(
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    if !response
        .headers()
        .contains_key(axum::http::header::CACHE_CONTROL)
    {
        response.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    fn request(method: &str, path: &str, token: Option<&str>, body: Value) -> Request<Body> {
        let mut r = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(token) = token {
            r = r
                .header("x-emby-token", token)
                .header("authorization", format!("Bearer {token}"));
        }
        r.body(Body::from(body.to_string())).expect("request")
    }
    async fn json_call(app: &Router, r: Request<Body>) -> (StatusCode, Value) {
        let path = r.uri().path().to_owned();
        let response = app.clone().oneshot(r).await.expect("response");
        let status = response.status();
        assert!(
            response
                .headers()
                .get("content-type")
                .expect("JSON content type")
                .to_str()
                .expect("content type")
                .starts_with("application/json"),
            "{path}: expected JSON response, got {status}"
        );
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (status, serde_json::from_slice(&bytes).expect("JSON body"))
    }
    async fn setup(app: &Router) -> String {
        let (status, body) = json_call(
            app,
            request(
                "POST",
                "/api/v1/setup",
                None,
                json!({"username":"catalog-admin","password":"supersecret"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        body["token"].as_str().expect("setup token").into()
    }
    async fn facade_login(app: &Router) -> (String, String) {
        let mut r = request(
            "POST",
            "/jellyfin/Users/AuthenticateByName",
            None,
            json!({"Username":"catalog-admin","Pw":"supersecret"}),
        );
        r.headers_mut().insert("x-emby-authorization", "MediaBrowser Client=\"Jellyfin+Android+TV\", Device=\"test TV\", DeviceId=\"catalog-contract\", Version=\"0.19.10\"".parse().expect("metadata"));
        let (status, body) = json_call(app, r).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["User"]["Policy"]["IsAdministrator"], false);
        assert_eq!(body["User"]["Policy"]["EnableContentDownloading"], false);
        (
            body["AccessToken"].as_str().expect("login token").into(),
            body["User"]["Id"].as_str().expect("user wire").into(),
        )
    }
    struct PlaybackFixture {
        app: Router,
        state: AppState,
        root: tempfile::TempDir,
        token: String,
        user: String,
        item: String,
        source: String,
        native_item: i64,
    }
    async fn playback_fixture() -> PlaybackFixture {
        use plurx_core::domain::{
            AudioStream, ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult,
        };
        let root = tempfile::tempdir().expect("playback root");
        let store = std::sync::Arc::new(
            plurx_core::store::SqliteStore::open(&root.path().join("state.db")).expect("store"),
        );
        let dirs = crate::state::Dirs {
            artwork: root.path().join("artwork"),
            transcode: root.path().join("transcode"),
            cache: root.path().join("cache"),
            subs: root.path().join("subs"),
            runtime_cache: root.path().join("runtime"),
            renditions: root.path().join("renditions"),
        };
        let state = AppState::new_unhooked(
            "playback-contract".into(),
            store,
            dirs,
            "test-node".into(),
            Default::default(),
            Default::default(),
            std::sync::Arc::new(crate::logbuf::LogBuffer::new(64)),
        );
        let app = super::super::router(state.clone());
        setup(&app).await;
        state
            .store
            .set_jellyfin_compatibility(true)
            .await
            .expect("enable");
        let library = state
            .store
            .create_library(&NewLibrary {
                name: "Direct films".into(),
                kind: LibraryKind::Movies,
                paths: vec![root.path().into()],
                anime: false,
            })
            .await
            .expect("library");
        let native_item = state
            .store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Direct contract".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let path = root
            .path()
            .canonicalize()
            .expect("canonical media root")
            .join("movie.mp4");
        std::fs::write(&path, b"0123456789abcdef").expect("bytes");
        state.store.upsert_file(native_item,path.to_str().expect("path"),16,1,&ProbeResult {duration_ms:Some(100_000),container:Some("mp4".into()),video_codec:Some("h264".into()),video_codec_tag:Some("avc1".into()),video_profile:Some("Main".into()),width:Some(1920),height:Some(1080),bit_depth:Some(8),bitrate:Some(100_000),audio_streams:vec![AudioStream {index:0,codec:"aac".into(),channels:Some(2),sample_rate:Some(48_000),language:None,title:None,default:true}],raw_json:Some(json!({"streams":[{"index":0,"codec_type":"audio","codec_name":"aac","channels":2,"sample_rate":"48000"},{"index":1,"codec_type":"video","codec_name":"h264","width":1920,"height":1080}]}).to_string()),..Default::default()}).await.expect("file");
        let (token, user) = facade_login(&app).await;
        let (status, page) = json_call(
            &app,
            request(
                "GET",
                "/jellyfin/Items?Recursive=true",
                Some(&token),
                json!({}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let item = page["Items"][0]["Id"].as_str().expect("item").to_owned();
        let source = page["Items"][0]["MediaSources"][0]["Id"]
            .as_str()
            .expect("source")
            .to_owned();
        PlaybackFixture {
            app,
            state,
            root,
            token,
            user,
            item,
            source,
            native_item,
        }
    }
    async fn negotiate(f: &PlaybackFixture) -> Value {
        let (status,body)=json_call(&f.app,request("POST",&format!("/jellyfin/Items/{}/PlaybackInfo",f.item),Some(&f.token),json!({"UserId":f.user,"MediaSourceId":f.source,"AudioStreamIndex":0,"SubtitleStreamIndex":-1,"EnableDirectPlay":true,"DeviceProfile":{"DirectPlayProfiles":[{"Type":"Video","Container":"mp4","VideoCodec":"h264","AudioCodec":"aac"}]}}))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["ErrorCode"].is_null(), "negotiation refused: {body}");
        assert_eq!(body["MediaSources"][0]["SupportsDirectPlay"], true);
        assert_eq!(body["MediaSources"][0]["DefaultAudioStreamIndex"], 0);
        assert!(!body
            .to_string()
            .contains(f.root.path().to_str().expect("path")));
        body
    }
    async fn play_event(
        f: &PlaybackFixture,
        route: &str,
        play: &str,
        position: Option<i64>,
    ) -> StatusCode {
        let mut body = json!({"UserId":f.user,"ItemId":f.item,"MediaSourceId":f.source,"PlaySessionId":play,"PlayMethod":"DirectPlay"});
        if let Some(position) = position {
            body["PositionTicks"] = json!(position * 10_000);
        }
        f.app
            .clone()
            .oneshot(request("POST", route, Some(&f.token), body))
            .await
            .expect("event")
            .status()
    }
    #[tokio::test]
    async fn jellyfin_direct_range_head_and_stop_preserve_native_authority_and_final_position() {
        let f = playback_fixture().await;
        let negotiation = negotiate(&f).await;
        let play = negotiation["PlaySessionId"].as_str().expect("play");
        let url = negotiation["MediaSources"][0]["DirectStreamUrl"]
            .as_str()
            .expect("direct URL");
        let unauthorized = f
            .app
            .clone()
            .oneshot(request("GET", url, None, json!({})))
            .await
            .expect("unauthorized");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let mut ranged = request("GET", url, Some(&f.token), json!({}));
        ranged
            .headers_mut()
            .insert("range", "bytes=2-5".parse().expect("range"));
        let response = f.app.clone().oneshot(ranged).await.expect("range");
        if response.status() != StatusCode::PARTIAL_CONTENT {
            let status = response.status();
            let bytes = response
                .into_body()
                .collect()
                .await
                .expect("error body")
                .to_bytes();
            panic!(
                "range refused {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }
        assert_eq!(response.headers()["content-range"], "bytes 2-5/16");
        assert_eq!(
            &response
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes()[..],
            b"2345"
        );
        let head = f
            .app
            .clone()
            .oneshot(request("HEAD", url, Some(&f.token), json!({})))
            .await
            .expect("head");
        assert_eq!(head.status(), StatusCode::OK);
        assert!(head
            .into_body()
            .collect()
            .await
            .expect("head body")
            .to_bytes()
            .is_empty());
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing", play, Some(1000)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing/Progress", play, Some(5000)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(7000)).await,
            StatusCode::NO_CONTENT
        );
        let watch = f
            .state
            .store
            .watch_state(1, f.native_item)
            .await
            .expect("watch")
            .expect("watch");
        assert_eq!(watch.position_ms, 7000);
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(9000)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            f.state
                .store
                .watch_state(1, f.native_item)
                .await
                .expect("preserved"),
            Some(watch)
        );
        assert!(f.state.direct_plays.list().is_empty());
        let late = f
            .app
            .clone()
            .oneshot(request("GET", url, Some(&f.token), json!({})))
            .await
            .expect("late range");
        assert_eq!(late.status(), StatusCode::CONFLICT);
    }
    #[tokio::test]
    async fn jellyfin_stop_without_position_and_external_edit_never_write_zero_or_restore_progress()
    {
        let f = playback_fixture().await;
        let first = negotiate(&f).await;
        let play = first["PlaySessionId"].as_str().expect("play");
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing", play, Some(1000)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, None).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            f.state
                .store
                .watch_state(1, f.native_item)
                .await
                .expect("watch")
                .expect("watch")
                .position_ms,
            1000
        );
        let second = negotiate(&f).await;
        let play = second["PlaySessionId"].as_str().expect("second play");
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing", play, Some(2000)).await,
            StatusCode::NO_CONTENT
        );
        f.state
            .store
            .set_watched_tree(1, f.native_item, false)
            .await
            .expect("external unwatch");
        let edited = f
            .state
            .store
            .watch_state(1, f.native_item)
            .await
            .expect("edit");
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(8000)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            f.state
                .store
                .watch_state(1, f.native_item)
                .await
                .expect("edit preserved"),
            edited
        );
    }
    #[tokio::test]
    async fn jellyfin_failed_final_releases_exact_resources_and_retry_commits_before_success() {
        let f = playback_fixture().await;
        let negotiation = negotiate(&f).await;
        let play = negotiation["PlaySessionId"].as_str().expect("play");
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing", play, Some(1000)).await,
            StatusCode::NO_CONTENT
        );
        let scope = f
            .state
            .store
            .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
            .await
            .expect("scope")
            .expect("scope");
        let binding = f
            .state
            .store
            .jellyfin_play(play, &scope)
            .await
            .expect("binding")
            .expect("binding");
        let conn =
            rusqlite::Connection::open(f.root.path().join("state.db")).expect("fault connection");
        conn.execute_batch("CREATE TRIGGER injected_final_failure BEFORE INSERT ON watch_state BEGIN SELECT RAISE(ABORT,'injected final failure'); END;").expect("inject");
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(8000)).await,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            f.state
                .store
                .jellyfin_play(play, &scope)
                .await
                .expect("retry binding")
                .expect("retry binding")
                .state,
            "active"
        );
        let grant = f
            .state
            .store
            .file_grant_by_id(binding.direct_grant_id.as_deref().expect("grant"))
            .await
            .expect("grant")
            .expect("grant");
        assert!(grant.revoked_at.is_some());
        assert!(f.state.direct_plays.list().is_empty());
        assert_eq!(
            f.state
                .store
                .watch_state(1, f.native_item)
                .await
                .expect("old watch")
                .expect("watch")
                .position_ms,
            1000
        );
        conn.execute_batch("DROP TRIGGER injected_final_failure;")
            .expect("repair");
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(8000)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            f.state
                .store
                .watch_state(1, f.native_item)
                .await
                .expect("durable final")
                .expect("watch")
                .position_ms,
            8000
        );
        assert_eq!(
            f.state
                .store
                .jellyfin_play(play, &scope)
                .await
                .expect("terminal")
                .expect("terminal")
                .state,
            "ended"
        );
    }
    #[tokio::test]
    async fn jellyfin_negotiation_keeps_profiles_separate_and_rejects_unknown_constraints_before_allocation(
    ) {
        let f = playback_fixture().await;
        let path = format!("/jellyfin/Items/{}/PlaybackInfo", f.item);
        let direct =
            json!({"Type":"Video","Container":"mp4","VideoCodec":"h264","AudioCodec":"aac"});
        for profile in [
            json!({"DirectPlayProfiles":[{"Type":"Video","Container":"mp4","VideoCodec":"hevc","AudioCodec":"aac"},{"Type":"Video","Container":"mkv","VideoCodec":"h264","AudioCodec":"ac3"}]}),
            json!({"DirectPlayProfiles":[direct.clone()],"CodecProfiles":[{"Type":"Video","Conditions":[{"Property":"UnknownCapability","Condition":"Equals","Value":"true"}]}]}),
            json!({"DirectPlayProfiles":[direct.clone()],"CodecProfiles":"invalid constraints"}),
            json!({"DirectPlayProfiles":[direct.clone()],"MaxStaticBitrate":10_000}),
        ] {
            let (status,body)=json_call(&f.app,request("POST",&path,Some(&f.token),json!({"UserId":f.user,"MediaSourceId":f.source,"EnableDirectPlay":true,"MaxStreamingBitrate":1_000_000,"DeviceProfile":profile}))).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["ErrorCode"], "NotSupported");
            assert!(body["PlaySessionId"].is_null());
        }
        let conn = rusqlite::Connection::open(f.root.path().join("state.db")).expect("inspect");
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM jellyfin_plays", [], |row| row
                .get::<_, i64>(0))
                .expect("count"),
            0
        );
        let (status,body)=json_call(&f.app,request("POST",&path,Some(&f.token),json!({"MediaSourceId":f.source,"DeviceProfile":{"DirectPlayProfiles":[{"Type":"Video","Container":"mp4","VideoCodec":"h264","AudioCodec":"ac3"},direct]}}))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body["PlaySessionId"].is_string(),
            "an independently valid second profile wins"
        );
        let (status, _) = json_call(
            &f.app,
            request(
                "POST",
                &path,
                Some(&f.token),
                json!({"MediaSourceId":f.source,"UnknownPlaybackLimit":1}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    #[tokio::test]
    async fn jellyfin_logout_releases_only_presented_login_and_preserves_other_device() {
        let f = playback_fixture().await;
        let info = negotiate(&f).await;
        let play = info["PlaySessionId"].as_str().expect("play");
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing", play, Some(1000)).await,
            StatusCode::NO_CONTENT
        );
        let mut login = request(
            "POST",
            "/jellyfin/Users/AuthenticateByName",
            None,
            json!({"Username":"catalog-admin","Pw":"supersecret"}),
        );
        login.headers_mut().insert("x-emby-authorization", "MediaBrowser Client=\"Jellyfin+Android+TV\", DeviceId=\"logout-other-device\", Version=\"0.19.10\"".parse().expect("metadata"));
        let (status, other) = json_call(&f.app, login).await;
        assert_eq!(status, StatusCode::OK);
        let other_token = other["AccessToken"].as_str().expect("other login");
        let scope = f
            .state
            .store
            .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
            .await
            .expect("scope")
            .expect("scope");
        let active = f
            .state
            .store
            .jellyfin_play(play, &scope)
            .await
            .expect("binding")
            .expect("binding");
        let other_scope = f
            .state
            .store
            .jellyfin_login_scope(plurx_core::auth::hash_token(other_token))
            .await
            .expect("other scope")
            .expect("other scope");
        f.state
            .store
            .create_file_grant(plurx_core::store::NewFileGrant {
                id: "other-device-grant".into(),
                token_hash: plurx_core::auth::hash_token("other-device-secret"),
                file_id: active.negotiation.file_id,
                user_id: 1,
                source_token_hash: other_scope.token_digest,
                created_at: 1,
                expires_at: i64::MAX,
            })
            .await
            .expect("other native grant");
        let response = f
            .app
            .clone()
            .oneshot(request(
                "POST",
                "/jellyfin/Sessions/Logout",
                Some(&f.token),
                json!({}),
            ))
            .await
            .expect("logout");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(f
            .state
            .store
            .jellyfin_login_scope(scope.token_digest.clone())
            .await
            .expect("revoked login")
            .is_none());
        let ended = f
            .state
            .store
            .jellyfin_play(play, &scope)
            .await
            .expect("tombstone")
            .expect("tombstone");
        assert_eq!(ended.state, "ended");
        assert!(f
            .state
            .store
            .file_grant_by_id(active.direct_grant_id.as_deref().expect("grant"))
            .await
            .expect("grant")
            .expect("grant")
            .revoked_at
            .is_some());
        assert!(f
            .state
            .store
            .file_grant_by_id("other-device-grant")
            .await
            .expect("other grant")
            .expect("other grant")
            .revoked_at
            .is_none());
        let (status, _) = json_call(
            &f.app,
            request("GET", "/jellyfin/Users/Me", Some(other_token), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = json_call(
            &f.app,
            request("GET", "/jellyfin/Users/Me", Some(&f.token), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    #[tokio::test]
    async fn jellyfin_play_events_reject_other_login_even_when_it_claims_the_owner_device() {
        let f = playback_fixture().await;
        let negotiation = negotiate(&f).await;
        let play = negotiation["PlaySessionId"].as_str().expect("play");
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing", play, Some(1000)).await,
            StatusCode::NO_CONTENT
        );
        let mut login = request(
            "POST",
            "/jellyfin/Users/AuthenticateByName",
            None,
            json!({"Username":"catalog-admin","Pw":"supersecret"}),
        );
        login.headers_mut().insert("x-emby-authorization","MediaBrowser Client=\"Jellyfin+Android+TV\", DeviceId=\"other-playback-device\", Version=\"0.19.10\"".parse().expect("metadata"));
        let (status, other) = json_call(&f.app, login).await;
        assert_eq!(status, StatusCode::OK);
        let token = other["AccessToken"].as_str().expect("token");
        for route in [
            "/jellyfin/Sessions/Playing/Progress",
            "/jellyfin/Sessions/Playing/Stopped",
        ] {
            let mut report = request(
                "POST",
                route,
                Some(token),
                json!({"ItemId":f.item,"MediaSourceId":f.source,"PlaySessionId":play,"PositionTicks":80_000_000,"DeviceId":"catalog-contract"}),
            );
            report.headers_mut().insert("x-emby-authorization","MediaBrowser Client=\"Jellyfin+Android+TV\", DeviceId=\"catalog-contract\", Version=\"0.19.10\"".parse().expect("claimed owner"));
            let (status, _) = json_call(&f.app, report).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
        }
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(3000)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            f.state
                .store
                .watch_state(1, f.native_item)
                .await
                .expect("watch")
                .expect("watch")
                .position_ms,
            3000
        );
    }
    #[tokio::test]
    async fn jellyfin_disabled_bare_paths_and_mutations_are_json_404_and_native_shell_survives() {
        let (app, _) = super::super::tests::test_app_with_state();
        for (method, path) in [
            ("GET", "/jellyfin"),
            ("GET", "/jellyfin/"),
            ("GET", "/jellyfin/System/Info/Public"),
            ("POST", "/jellyfin/Users/AuthenticateByName"),
            ("POST", "/jellyfin/Items"),
            ("GET", "/jellyfin/unknown"),
        ] {
            let (status, body) = json_call(&app, request(method, path, None, json!({}))).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(body["error"].is_string());
        }
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("root request"),
            )
            .await
            .expect("root response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()["content-type"]
            .to_str()
            .expect("root content type")
            .starts_with("text/html"));
    }
    #[tokio::test]
    async fn jellyfin_connection_catalog_preserves_zero_paging_and_refuses_user_and_credential_conflicts(
    ) {
        use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
        let (app, state) = super::super::tests::test_app_with_state();
        let admin = setup(&app).await;
        let (status, saved) = json_call(
            &app,
            request(
                "PUT",
                "/api/v1/settings",
                Some(&admin),
                json!({"jellyfin_compatibility_enabled":true}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(saved["jellyfin_compatibility_enabled"], true);
        let library = state
            .store
            .create_library(&NewLibrary {
                name: "Contract films".into(),
                kind: LibraryKind::Movies,
                paths: vec!["/private/native-only".into()],
                anime: false,
            })
            .await
            .expect("library");
        let mut native_items = Vec::new();
        for name in ["First", "Second"] {
            native_items.push(
                state
                    .store
                    .insert_item(&NewItem {
                        library_id: library.id,
                        kind: ItemKind::Movie,
                        parent_id: None,
                        title: name.into(),
                        year: Some(2024),
                        season_number: None,
                        episode_number: None,
                    })
                    .await
                    .expect("item"),
            );
        }
        state
            .store
            .upsert_file(
                native_items[0],
                "/private/native-only/movie.mkv",
                100,
                1,
                &ProbeResult {
                    duration_ms: Some(10000),
                    container: Some("mkv".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("source");
        let (token, uid) = facade_login(&app).await;
        let (status, views) = json_call(
            &app,
            request(
                "GET",
                &format!("/jellyfin/Users/{uid}/Views"),
                Some(&token),
                Value::Null,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(views["TotalRecordCount"], 1);
        let parent = views["Items"][0]["Id"].as_str().expect("library wire");
        let base = format!("/jellyfin/Users/{uid}/Items?ParentId={parent}&SortBy=SortName");
        let (status, page) = json_call(
            &app,
            request(
                "GET",
                &format!("{base}&StartIndex=1&Limit=1"),
                Some(&token),
                Value::Null,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["StartIndex"], 1);
        assert_eq!(page["TotalRecordCount"], 2);
        assert_eq!(page["Items"][0]["Name"], "Second");
        assert_eq!(page["Items"][0]["UserData"]["PlaybackPositionTicks"], 0);
        let (_, first) = json_call(
            &app,
            request("GET", &format!("{base}&Limit=1"), Some(&token), Value::Null),
        )
        .await;
        assert!(!first.to_string().contains("native-only"));
        assert_eq!(first["Items"][0]["MediaSourceCount"], 1);
        for start in ["2", "9223372036854775807"] {
            let (_, empty) = json_call(
                &app,
                request(
                    "GET",
                    &format!("{base}&StartIndex={start}&Limit=1"),
                    Some(&token),
                    Value::Null,
                ),
            )
            .await;
            assert_eq!(empty["TotalRecordCount"], 2);
            assert!(empty["Items"].as_array().expect("items").is_empty());
        }
        let image_path = format!(
            "/jellyfin/Items/{}/Images/Primary",
            first["Items"][0]["Id"].as_str().expect("item wire")
        );
        assert_eq!(
            json_call(&app, request("GET", &image_path, None, Value::Null))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            json_call(&app, request("GET", &image_path, Some(&token), Value::Null))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        let other = WireId::random().to_hex();
        assert_eq!(
            json_call(
                &app,
                request(
                    "GET",
                    &format!("/jellyfin/Users/{other}/Views"),
                    Some(&token),
                    Value::Null
                )
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            json_call(
                &app,
                request(
                    "GET",
                    &format!("{base}&Limit=1&limit=2"),
                    Some(&token),
                    Value::Null
                )
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            json_call(
                &app,
                request(
                    "GET",
                    &format!("{base}&api_key=different"),
                    Some(&token),
                    Value::Null
                )
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            json_call(
                &app,
                request(
                    "GET",
                    "/jellyfin/Items",
                    Some("plx_scoped_key"),
                    Value::Null
                )
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            json_call(
                &app,
                request("POST", "/jellyfin/Items", Some(&token), json!({}))
            )
            .await
            .0,
            StatusCode::METHOD_NOT_ALLOWED
        );
        state
            .store
            .set_jellyfin_compatibility(false)
            .await
            .expect("disable");
        assert_eq!(
            json_call(&app, request("GET", &base, Some(&token), Value::Null))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
    #[test]
    fn jellyfin_source_projection_keeps_global_zero_and_omits_relative_indices_and_native_paths() {
        let row = json!({"wire_id":WireId::random().to_hex(),"path":"/secret/file.mkv","probe_json":{"format":{"filename":"/secret/file.mkv"}},"duration_ms":null,"container":null,
            "streams":[{"index":0,"codec_type":"audio","codec_name":"aac","tags":{"language":"eng"},"disposition":{"default":1}},
                {"codec_type":"audio","codec_name":"aac"}]});
        let source = source_dto(&row).expect("source DTO");
        let encoded = serde_json::to_value(source).expect("DTO");
        assert_eq!(
            encoded["MediaStreams"].as_array().expect("streams").len(),
            1
        );
        assert_eq!(encoded["MediaStreams"][0]["Index"], 0);
        assert!(!encoded.to_string().contains("secret"));
        assert!(encoded.get("RunTimeTicks").is_none());
        assert_eq!(encoded["SupportsDirectPlay"], false);
    }
}
