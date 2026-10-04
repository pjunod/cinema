//! Jellyfin connection and catalog facade over native authentication and Store.
mod ancillary;
mod metrics;
mod playback;
mod protocol;
mod representation;
mod startup;
mod transport;
mod vod;
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
        .route("/System/Info", get(protocol::system_info_authenticated))
        .route("/Users/Public", get(protocol::public_users))
        .route("/Sessions/Capabilities", post(protocol::capabilities))
        .route(
            "/Sessions/Capabilities/Full",
            post(protocol::capabilities_full).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/Search/Hints", get(protocol::search_hints))
        .route("/Items/{item_id}/Download", get(protocol::download))
        .route(
            "/UserPlayedItems/{item_id}",
            post(playback::played_item).delete(playback::played_item),
        )
        .route("/Sessions/Playing/Ping", post(playback::ping))
        .route(
            "/Videos/ActiveEncodings",
            axum::routing::delete(playback::active_encodings),
        )
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
            "/Videos/{item_id}/{play_id}/hls/{*resource}",
            get(transport::resource),
        )
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
        .route("/Items/{item_id}/Intros", get(ancillary::intros))
        .route("/MediaSegments/{item_id}", get(ancillary::segments))
        .route(
            "/Videos/{item_id}/{source_id}/Subtitles/{index}/{filename}",
            get(ancillary::subtitles),
        )
        .route(
            "/Videos/{item_id}/{source_id}/Subtitles/{index}/{start_ticks}/{filename}",
            get(ancillary::subtitles_from),
        )
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
        .route_layer(axum::middleware::from_fn(metrics::count_matched))
        .fallback(unmatched)
        .method_not_allowed_fallback(method_not_allowed)
}
pub(crate) use metrics::prometheus;
pub(super) async fn not_found() -> ApiError {
    ApiError::NotFound("Jellyfin endpoint")
}
/// A path no facade route matches: counted, so an unsupported request a
/// client makes shows up in the metrics rather than only in a client log.
async fn unmatched() -> ApiError {
    metrics::record(metrics::UNMATCHED, "not_found");
    not_found().await
}
async fn method_not_allowed() -> Response {
    metrics::record(metrics::UNMATCHED, "not_found");
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({"error":"method not allowed"})),
    )
        .into_response()
}

struct Enabled(String);
/// The switch snapshot `enabled_gate` admitted this request under, so the
/// handler reads the same one rather than making a second linearizable read.
#[derive(Clone)]
struct AdmittedSwitch(plurx_core::store::JellyfinCompatibilityState);
impl FromRequestParts<AppState> for Enabled {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let setting = match parts.extensions.get::<AdmittedSwitch>() {
            Some(AdmittedSwitch(setting)) => setting.clone(),
            None => state.store.jellyfin_compatibility_state().await?,
        };
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
    /// The enabled switch generation this request was admitted under.
    generation: String,
    /// The login token this request itself presented, when it is one Plurx
    /// minted (hex). Media URLs the facade returns carry it as `ApiKey`, as
    /// Jellyfin's do: Android TV sends no header on media, subtitle or HLS
    /// requests (J0 trace). `None` for a caller that presented only a link.
    url_credential: Option<String>,
}
impl FromRequestParts<AppState> for ClientUser {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        presented_client(parts, state)
            .await?
            .1
            .ok_or(ApiError::Unauthorized)
    }
}
/// `None` only when no user token is presented at all; a presented token that
/// fails authentication is still an error, never an anonymous caller.
async fn presented_client(
    parts: &mut Parts,
    state: &AppState,
) -> Result<(String, Option<ClientUser>), ApiError> {
    let Enabled(generation) = Enabled::from_request_parts(parts, state).await?;
    let pairs = query_pairs(parts.uri.query())?;
    let values = carriers(&parts.headers, &pairs)?;
    let refs = values
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect::<Vec<_>>();
    let Some(token) = credentials::parse_user_token(&refs)
        .map_err(|_| ApiError::BadRequest("invalid credential carrier".into()))?
    else {
        return Ok((generation, None));
    };
    super::extract::authenticate_compatibility_token(state, token.expose()).await?;
    let mut client = ClientUser::for_login(
        state,
        plurx_core::auth::hash_token(token.expose()),
        generation.clone(),
    )
    .await?;
    client.url_credential = Some(token.expose().to_owned())
        .filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()));
    Ok((generation, Some(client)))
}
impl ClientUser {
    /// The caller must already have authenticated this login digest.
    async fn for_login(
        state: &AppState,
        token_hash: String,
        generation: String,
    ) -> Result<Self, ApiError> {
        let identity = identity(state, &token_hash).await?;
        Ok(Self {
            identity,
            token_hash,
            generation,
            url_credential: None,
        })
    }
}
/// A media request either presents a user token or relies on a scoped media
/// link in its query; the handler decides which once it has parsed the query.
struct MediaCaller {
    generation: String,
    client: Option<ClientUser>,
}
impl FromRequestParts<AppState> for MediaCaller {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let (generation, client) = presented_client(parts, state).await?;
        Ok(Self { generation, client })
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
        version: plurx_compat_jellyfin::catalog::PROTOCOL_BASELINE_VERSION,
        product_name: plurx_compat_jellyfin::catalog::PROTOCOL_BASELINE_PRODUCT,
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
    // Infuse names its share mode in the client field (`Infuse-Direct` on
    // the measured tvOS share; iPhone shares default to library mode). Every
    // mode is the same app, so the family is the product, not the mode.
    let family = match metadata.client.as_deref() {
        Some(client) if client == "Infuse" || client.starts_with("Infuse-") => {
            JellyfinClientFamily::Infuse
        }
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
    mut request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Response {
    match state.store.jellyfin_compatibility_state().await {
        Ok(setting) if setting.enabled => {
            request.extensions_mut().insert(AdmittedSwitch(setting));
            next.run(request).await
        }
        Ok(_) => not_found().await.into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

fn image_tag(wire: &str, name: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(format!("{wire}:{name}")))
}
// Mapped Primary/Backdrop artwork answers without a login while the switch is
// on (approved 2026-10-03, contract §5.1): the store read checks mapping,
// incarnation and switch generation, and `serve_jellyfin_artwork` spends the
// per-address miss budget. A presented login is not required or consulted.
async fn image(
    _enabled: Enabled,
    auth::ClientPeer(peer): auth::ClientPeer,
    State(state): State<AppState>,
    Path((id, kind)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    mapped_image(&state, peer, &headers, &id, &kind, raw.as_deref()).await
}
async fn indexed_image(
    _enabled: Enabled,
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
    let mapped = state
        .store
        .jellyfin_catalog_artwork(id, backdrop)
        .await?
        .ok_or(ApiError::NotFound("image"))?;
    super::images::serve_jellyfin_artwork(
        state,
        mapped,
        backdrop,
        width.unwrap_or(500),
        headers,
        address,
    )
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
    /// A returned media URL as the client requests it: clients prefix the
    /// configured server address, which already ends in `/jellyfin`.
    fn mounted(url: &str) -> String {
        assert!(
            url.starts_with("/Videos/"),
            "returned URLs are relative to the configured base: {url}"
        );
        format!("/jellyfin{url}")
    }
    fn without_key(url: &str, token: &str) -> String {
        let key = format!("ApiKey={token}");
        assert!(
            url.contains(&key),
            "a returned URL carries the login: {url}"
        );
        url.replace(&format!("&{key}"), "")
            .replace(&format!("?{key}"), "")
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
    #[tokio::test]
    async fn jellyfin_hls_requires_login_and_rejects_a_direct_binding_before_activation() {
        let f = playback_fixture().await;
        let (status, body) = json_call(&f.app, request("POST", &format!("/jellyfin/Items/{}/PlaybackInfo", f.item), Some(&f.token), json!({"DeviceProfile":{"DirectPlayProfiles":[{"Type":"Video","Container":"mp4","VideoCodec":"h264","AudioCodec":"aac"}]}}))).await;
        assert_eq!(status, StatusCode::OK);
        let play = body["PlaySessionId"].as_str().expect("direct binding");
        let root = format!(
            "/jellyfin/Videos/{}/master.m3u8?MediaSourceId={}&PlaySessionId={play}",
            f.item, f.source
        );
        let (status, _) = json_call(&f.app, request("GET", &root, None, Value::Null)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) =
            json_call(&f.app, request("GET", &root, Some(&f.token), Value::Null)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let client_scope = f
            .state
            .store
            .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
            .await
            .expect("scope")
            .expect("login");
        let unchanged = f
            .state
            .store
            .jellyfin_play(play, &client_scope)
            .await
            .expect("read")
            .expect("play");
        assert_eq!(unchanged.state, "pending");
        assert!(unchanged.native_incarnation_id.is_none());
        assert!(
            unchanged.direct_grant_id.is_some(),
            "the negotiated link grant exists, but HLS cannot activate a direct play"
        );
        let path = format!("/jellyfin/Videos/{}/{play}/hls/seg00001.ts", f.item);
        let (status, _) = json_call(&f.app, request("GET", &path, None, Value::Null)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) =
            json_call(&f.app, request("GET", &path, Some(&f.token), Value::Null)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn jellyfin_hls_prerequisite_refusals_allocate_no_native_reader_or_producer() {
        let f = playback_fixture().await;
        f.state
            .store
            .put_setting(plurx_core::store::keys::VOD_PRESENTATION, "0")
            .await
            .expect("disable native VOD");
        let path = format!("/jellyfin/Items/{}/PlaybackInfo", f.item);
        let (status, body) = json_call(&f.app, request("POST", &path, Some(&f.token), json!({"EnableDirectPlay":false,"MaxStreamingBitrate":750000,"DeviceProfile":{"TranscodingProfiles":[{"Type":"Video","Container":"ts","VideoCodec":"h264","AudioCodec":"aac","Protocol":"hls","MaxAudioChannels":"2","ManifestSubtitles":"vtt"}]}}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ErrorCode"], "NotSupported");
        assert!(body["MediaSources"].as_array().expect("sources").is_empty());
        assert!(body.get("PlaySessionId").is_none());
        assert!(f.state.transcode.active_session_ids().await.is_empty());
        assert!(f
            .state
            .transcode
            .vod_live_or_preparing_session_ids()
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn jellyfin_native_hls_copy_preserves_original_time_auth_and_inline_fragment_ranges() {
        Box::pin(native_hls_copy_flow(false, false)).await;
    }
    #[tokio::test]
    async fn jellyfin_native_hls_duplicate_entries_share_one_activation_and_preserve_ranges_and_stop(
    ) {
        Box::pin(native_hls_copy_flow(true, false)).await;
    }
    #[tokio::test]
    async fn jellyfin_native_hls_encoded_without_copy_index_preserves_ranges_clock_and_stop() {
        Box::pin(native_hls_copy_flow(false, true)).await;
    }
    async fn native_hls_copy_flow(duplicate: bool, encoded: bool) {
        let f = playback_fixture().await;
        // Production renews the native route's 12-second lease from the
        // owner loop. Without it, a flow slower than one lease (a loaded CI
        // runner) sees its own route expire and every request answer 409.
        let lease_owner = tokio::spawn(crate::media_sessions::lease_loop(f.state.clone()));
        let path = f
            .root
            .path()
            .canonicalize()
            .expect("canonical media")
            .join("movie.mp4");
        std::fs::copy(plurx_core::testfixtures::source("h264"), &path).expect("real native source");
        let metadata = std::fs::metadata(&path).expect("source metadata");
        let mtime = metadata
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch")
            .as_secs() as i64;
        let source_probe = if encoded {
            let mut probe = plurx_core::scan::probe::probe(&path)
                .await
                .expect("complete encoded source probe");
            // A text track the native master would advertise; this profile
            // asks for no manifest subtitles, so the facade must not.
            probe
                .subtitle_streams
                .push(plurx_core::domain::SubtitleStream {
                    index: 2,
                    codec: "subrip".into(),
                    language: Some("eng".into()),
                    title: None,
                    default: false,
                    forced: false,
                    hearing_impaired: false,
                });
            probe
        } else {
            plurx_core::domain::ProbeResult {
                duration_ms: Some(12_000), container: Some("mkv".into()), video_codec: Some("h264".into()),
                video_profile: Some("Main".into()), width: Some(640), height: Some(360), bit_depth: Some(8), bitrate: Some(1_000_000),
                audio_streams: vec![plurx_core::domain::AudioStream { index: 0, codec: "aac".into(), channel_layout: None, channels: Some(2), sample_rate: Some(48000), default: true, ..Default::default() }],
                raw_json: Some(json!({"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":640,"height":360,"profile":"Main","avg_frame_rate":"24/1"},{"index":1,"codec_type":"audio","codec_name":"aac","channels":2,"sample_rate":"48000"}]}).to_string()), ..Default::default()
            }
        };
        let file_id = f
            .state
            .store
            .upsert_file(
                f.native_item,
                path.to_str().expect("path"),
                metadata.len() as i64,
                mtime,
                &source_probe,
            )
            .await
            .expect("native source probe");
        let file = f
            .state
            .store
            .get_file(file_id)
            .await
            .expect("source")
            .expect("file");
        if !encoded {
            let indexed = Box::pin(crate::fragindex::build(
                &file,
                plurx_core::transcode::CopyVideoOptions::new(
                    crate::ffmpeg::has_dovi_rpu().await,
                    false,
                ),
                &f.root.path().join("native-index"),
                std::time::Duration::from_secs(120),
            ))
            .await;
            let crate::fragindex::IndexOutcome::Built(index) = indexed else {
                panic!("native index: {indexed:?}");
            };
            f.state
                .store
                .put_fragment_index(file_id, &index)
                .await
                .expect("index");
        }
        if encoded {
            assert!(!f
                .state
                .store
                .holds_fragment_index_for_source(file_id, metadata.len() as i64, mtime)
                .await
                .expect("no copy index"));
        }
        // The encoded run omits manifest subtitles, as Infuse's profile does:
        // its master alias must still be a multivariant wrapper.
        let mut transcoding = json!({"Type":"Video","Container":"ts","VideoCodec":"h264","AudioCodec":"aac","Protocol":"hls","MaxAudioChannels":"2"});
        if !encoded {
            transcoding["ManifestSubtitles"] = json!("vtt");
        }
        let (status, info) = json_call(&f.app, request("POST", &format!("/jellyfin/Items/{}/PlaybackInfo", f.item), Some(&f.token), json!({"EnableDirectPlay":false,"StartTimeTicks":20_000_000,"MaxStreamingBitrate":if encoded {750_000} else {2_000_000},"AllowVideoStreamCopy":!encoded,"DeviceProfile":{"TranscodingProfiles":[transcoding]}}))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            info["ErrorCode"].is_null(),
            "native prerequisites refused: {info}"
        );
        let play_id = info["PlaySessionId"].as_str().expect("play");
        let url = &mounted(
            info["MediaSources"][0]["TranscodingUrl"]
                .as_str()
                .expect("native HLS URL"),
        );
        let response = f
            .app
            .clone()
            .oneshot(request(
                "GET",
                &without_key(url, &f.token),
                None,
                Value::Null,
            ))
            .await
            .expect("unauthenticated root");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // Infuse lower-cases the first letter of every query key it was given.
        let lowered = url
            .replace("MediaSourceId=", "mediaSourceId=")
            .replace("PlaySessionId=", "playSessionId=")
            .replace("ApiKey=", "apiKey=");
        let response = if duplicate {
            let (first, second) = tokio::join!(
                f.app
                    .clone()
                    .oneshot(request("GET", url, Some(&f.token), Value::Null)),
                f.app
                    .clone()
                    .oneshot(request("GET", url, Some(&f.token), Value::Null)),
            );
            let first = first.expect("first canonical root");
            let second = second.expect("duplicate canonical root");
            let duplicate_status = second.status();
            let duplicate_bytes = second
                .into_body()
                .collect()
                .await
                .expect("duplicate manifest")
                .to_bytes();
            assert_eq!(
                duplicate_status,
                StatusCode::OK,
                "{}",
                String::from_utf8_lossy(&duplicate_bytes)
            );
            assert_eq!(
                f.state
                    .transcode
                    .vod_live_or_preparing_session_ids()
                    .await
                    .len(),
                1,
                "duplicate entries attach one native reader identity"
            );
            first
        } else {
            // Android TV sends no header on media: the URL's ApiKey is all.
            f.app
                .clone()
                .oneshot(request("GET", &lowered, None, Value::Null))
                .await
                .expect("native root")
        };
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("root body")
            .to_bytes();
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let master = String::from_utf8(bytes.to_vec()).expect("manifest");
        let scope = f
            .state
            .store
            .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
            .await
            .expect("scope")
            .expect("login");
        let play = f
            .state
            .store
            .jellyfin_play(play_id, &scope)
            .await
            .expect("binding")
            .expect("play");
        assert_eq!(play.negotiation.source_origin_ms, 0);
        let frozen: Value =
            serde_json::from_str(&play.negotiation.selection_json).expect("selection");
        assert_eq!(
            frozen["vod"]["body"]["copy"], !encoded,
            "frozen native recipe must reflect actual video copying"
        );
        if encoded {
            assert_eq!(frozen["vod"]["bitrate"], 750_000);
        }

        let route = f
            .state
            .store
            .media_session_route_by_incarnation(
                play.native_incarnation_id
                    .as_deref()
                    .expect("native binding"),
            )
            .await
            .expect("route")
            .expect("native");
        assert!(!master.contains(&route.session_id) && !master.contains("/api/v1/hls/"));
        assert!(
            master.contains("#EXT-X-STREAM-INF"),
            "the master alias is a multivariant wrapper: {master}"
        );
        if encoded {
            assert!(
                !master.contains("#EXT-X-MEDIA") && !master.contains("SUBTITLES="),
                "a play negotiated without manifest subtitles advertises none: {master}"
            );
        }
        let media_url = master
            .lines()
            .find(|line| line.contains("index.m3u8") && !line.starts_with('#'))
            .expect("media child");
        assert!(
            media_url.ends_with(&format!("index.m3u8?ApiKey={}", f.token)),
            "every child carries the URL credential: {media_url}"
        );
        if encoded {
            let subtitles = format!(
                "{}subs/2/index.m3u8",
                media_url.split_once("index.m3u8").expect("HLS mount").0
            );
            let refused = f
                .app
                .clone()
                .oneshot(request("GET", &subtitles, Some(&f.token), Value::Null))
                .await
                .expect("subtitle playlist");
            assert_eq!(refused.status(), StatusCode::NOT_FOUND);
        }
        let response = f
            .app
            .clone()
            .oneshot(request("GET", media_url, Some(&f.token), Value::Null))
            .await
            .expect("media playlist");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("media body")
            .to_bytes();
        let media = String::from_utf8(bytes.to_vec()).expect("media manifest");
        assert!(!media.contains("#EXT-X-MAP"));
        assert!(media.contains("#EXT-X-ENDLIST"));
        let segment_url = media
            .lines()
            .find(|line| line.contains(".ts?ApiKey="))
            .expect("inline child");
        let init = Box::pin(super::super::hls::segment(
            State(f.state.clone()),
            Path((route.session_id.clone(), "init.mp4".into())),
            HeaderMap::new(),
        ))
        .await
        .expect("native init");
        let init = init
            .into_body()
            .collect()
            .await
            .expect("init bytes")
            .to_bytes();
        let mut range = request("GET", segment_url, Some(&f.token), Value::Null);
        range.headers_mut().insert(
            "range",
            format!("bytes={}-{}", init.len() - 3, init.len() + 4)
                .parse()
                .expect("range"),
        );
        let response = f.app.clone().oneshot(range).await.expect("crossing range");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("range bytes")
            .to_bytes();
        assert_eq!(
            status,
            StatusCode::PARTIAL_CONTENT,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(bytes.len(), 8);
        assert_eq!(&bytes[..3], &init[init.len() - 3..]);
        let fresh = f
            .state
            .store
            .media_session_route_by_incarnation(&route.incarnation_id)
            .await
            .expect("frontier")
            .expect("route");
        assert_eq!(
            fresh.fetched_through_ms, 0,
            "a partial composite read cannot mark a full native fragment fetched"
        );
        // While a node has lost serving authority (a quorum leader restart),
        // the gate answers the facade as it answers native media: 503 with
        // Retry-After, before any store read. The binding and route are
        // untouched, so the same play answers once authority is back. That
        // the native session itself outlives a short loss is the serving
        // fence's own property (`serving_fence_flapping_losses_share_one_grace`).
        f.state.serving.validation_set_ready(false).await;
        let fenced = f
            .app
            .clone()
            .oneshot(request("GET", segment_url, None, Value::Null))
            .await
            .expect("fenced child");
        assert_eq!(fenced.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(fenced.headers().contains_key("retry-after"));
        f.state.serving.validation_set_ready(true).await;
        let recovered = f
            .app
            .clone()
            .oneshot(request("GET", segment_url, None, Value::Null))
            .await
            .expect("recovered child");
        assert_eq!(
            recovered.status(),
            StatusCode::OK,
            "the play survives a recovered authority loss"
        );
        if encoded {
            let response = f
                .app
                .clone()
                .oneshot(request("GET", segment_url, Some(&f.token), Value::Null))
                .await
                .expect("whole encoded fragment");
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = response
                .into_body()
                .collect()
                .await
                .expect("whole encoded body")
                .to_bytes();
            let decoded_input = f.root.path().join("encoded-alias-get.mp4");
            tokio::fs::write(&decoded_input, &bytes)
                .await
                .expect("decoder fixture");
            let decoded = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
                .arg(&decoded_input)
                .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "framehash", "-"])
                .kill_on_drop(true)
                .output()
                .await
                .expect("decode actual mapped video and audio");
            assert!(
                decoded.status.success(),
                "{}",
                String::from_utf8_lossy(&decoded.stderr)
            );
            let frames = String::from_utf8(decoded.stdout).expect("decoded frame hashes");
            assert!(
                frames.lines().filter(|line| line.starts_with("0,")).count() >= 2,
                "mapped video must decode: {frames}"
            );
            assert!(
                frames.lines().filter(|line| line.starts_with("1,")).count() >= 2,
                "mapped audio must decode: {frames}"
            );
        }

        let response = f
            .app
            .clone()
            .oneshot(request(
                "GET",
                &without_key(segment_url, &f.token),
                None,
                Value::Null,
            ))
            .await
            .expect("anonymous child");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // A long pause sends progress, not media requests. That authenticated
        // presence must renew the exact passive grant, or the idle route ends
        // and the client's resume receives 410.
        f.state
            .transcode
            .shorten_passive_grant_for_test(&route.session_id, std::time::Duration::from_secs(1))
            .await;
        let paused = f.app.clone().oneshot(request("POST", "/jellyfin/Sessions/Playing/Progress", Some(&f.token), json!({"UserId":f.user,"ItemId":f.item,"MediaSourceId":f.source,"PlaySessionId":play_id,"PlayMethod":"Transcode","PositionTicks":5000*10_000,"IsPaused":true}))).await.expect("paused progress");
        assert_eq!(paused.status(), StatusCode::NO_CONTENT);
        let renewed = f
            .state
            .transcode
            .passive_grant_remaining_for_test(&route.session_id)
            .await
            .expect("live passive grant");
        assert!(
            renewed > std::time::Duration::from_secs(500),
            "paused presence must renew the passive grant, left {renewed:?}"
        );
        // Past the reader idle TTL the reader is reaped; the paused client's
        // resume is a later segment, which must resurrect the same route
        // through the facade (the long-pause path a real client takes).
        let vod = f.state.transcode.vod_for_test();
        vod.force_reader_idle_for_test(&route.session_id).await;
        vod.maintain().await;
        assert_eq!(vod.active_sessions().await, 0, "the reader was reaped");
        let resume_url = media
            .lines()
            .rfind(|line| !line.is_empty() && !line.starts_with("#"))
            .expect("last segment");
        let resumed = f
            .app
            .clone()
            .oneshot(request("GET", resume_url, Some(&f.token), Value::Null))
            .await
            .expect("resume after idle reap");
        let resumed_status = resumed.status();
        let resumed_bytes = resumed
            .into_body()
            .collect()
            .await
            .expect("resumed body")
            .to_bytes();
        assert_eq!(
            resumed_status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&resumed_bytes)
        );
        assert!(!resumed_bytes.is_empty());
        assert_eq!(
            vod.active_sessions().await,
            1,
            "the resume resurrected the reader"
        );
        // Progress and Ping keep the play alive; ActiveEncodings releases its
        // encoding but keeps the binding, so the final Stopped still commits.
        for (method, endpoint, position) in [
            (
                "POST",
                "/jellyfin/Sessions/Playing/Progress".to_owned(),
                Some(5000),
            ),
            (
                "POST",
                format!("/jellyfin/Sessions/Playing/Ping?PlaySessionId={play_id}"),
                None,
            ),
            (
                "DELETE",
                format!("/jellyfin/Videos/ActiveEncodings?PlaySessionId={play_id}"),
                None,
            ),
            (
                "POST",
                "/jellyfin/Sessions/Playing/Stopped".to_owned(),
                Some(7000),
            ),
        ] {
            let body = position.map_or(Value::Null, |position: i64| json!({"UserId":f.user,"ItemId":f.item,"MediaSourceId":f.source,"PlaySessionId":play_id,"PlayMethod":"Transcode","PositionTicks":position*10_000}));
            let response = f
                .app
                .clone()
                .oneshot(request(method, &endpoint, Some(&f.token), body))
                .await
                .expect("native watch event");
            assert_eq!(
                response.status(),
                StatusCode::NO_CONTENT,
                "{method} {endpoint}"
            );
            if method == "DELETE" {
                let released = f
                    .state
                    .store
                    .media_session_route_by_incarnation(&route.incarnation_id)
                    .await
                    .expect("released")
                    .expect("native row");
                assert_eq!(released.state, "ended", "the encoding is released");
            }
        }
        let watch = f
            .state
            .store
            .watch_state(1, f.native_item)
            .await
            .expect("watch")
            .expect("saved");
        assert_eq!(
            watch.position_ms, 7000,
            "progress remains on the original movie timeline"
        );
        let ended = f
            .state
            .store
            .media_session_route_by_incarnation(&route.incarnation_id)
            .await
            .expect("ended")
            .expect("native row");
        assert_eq!(ended.state, "ended");
        assert!(f.state.transcode.active_session_ids().await.is_empty());
        assert!(f
            .state
            .transcode
            .vod_live_or_preparing_session_ids()
            .await
            .is_empty());
        let response = f
            .app
            .clone()
            .oneshot(request("GET", segment_url, Some(&f.token), Value::Null))
            .await
            .expect("late child");
        assert_ne!(response.status(), StatusCode::OK);
        lease_owner.abort();
        assert!(lease_owner.await.is_err_and(|error| error.is_cancelled()));
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
        let canonical_root = root.path().canonicalize().expect("canonical fixture dirs");
        let dirs = crate::state::Dirs {
            artwork: canonical_root.join("artwork"),
            transcode: canonical_root.join("transcode"),
            cache: canonical_root.join("cache"),
            subs: canonical_root.join("subs"),
            runtime_cache: canonical_root.join("runtime"),
            renditions: canonical_root.join("renditions"),
        };
        for path in [
            &dirs.artwork,
            &dirs.transcode,
            &dirs.cache,
            &dirs.subs,
            &dirs.runtime_cache,
            &dirs.renditions,
        ] {
            std::fs::create_dir_all(path).expect("fixture cache root");
        }
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
        state.store.upsert_file(native_item,path.to_str().expect("path"),16,1,&ProbeResult {duration_ms:Some(100_000),container:Some("mp4".into()),video_codec:Some("h264".into()),video_codec_tag:Some("avc1".into()),video_profile:Some("Main".into()),width:Some(1920),height:Some(1080),bit_depth:Some(8),bitrate:Some(100_000),audio_streams:vec![AudioStream {index:0,codec:"aac".into(), channel_layout: None,channels:Some(2),sample_rate:Some(48_000),language:None,title:None,default:true}],raw_json:Some(json!({"streams":[{"index":0,"codec_type":"audio","codec_name":"aac","channels":2,"sample_rate":"48000"},{"index":1,"codec_type":"video","codec_name":"h264","width":1920,"height":1080}]}).to_string()),..Default::default()}).await.expect("file");
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
    async fn jellyfin_replacement_activation_fences_old_events_and_preserves_new_direct_presence() {
        let f = playback_fixture().await;
        let old = negotiate(&f).await;
        let old_id = old["PlaySessionId"].as_str().expect("old");
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing", old_id, Some(1000)).await,
            StatusCode::NO_CONTENT
        );
        let next = negotiate(&f).await;
        let next_id = next["PlaySessionId"].as_str().expect("next");
        let scope = f
            .state
            .store
            .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
            .await
            .expect("scope")
            .expect("login");
        let old_binding = f
            .state
            .store
            .jellyfin_play(old_id, &scope)
            .await
            .expect("old")
            .expect("binding");
        let next_binding = f
            .state
            .store
            .jellyfin_play(next_id, &scope)
            .await
            .expect("next")
            .expect("binding");
        assert_eq!(
            old_binding.state, "active",
            "negotiation must keep incumbent"
        );
        assert_eq!(
            old_binding.negotiation.playback_id,
            next_binding.negotiation.playback_id
        );
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing", next_id, Some(2000)).await,
            StatusCode::NO_CONTENT
        );
        let old_grant = f
            .state
            .store
            .file_grant_by_id(old_binding.direct_grant_id.as_deref().expect("old grant"))
            .await
            .expect("old grant")
            .expect("old grant");
        assert!(
            old_grant.revoked_at.is_some(),
            "a replacement without Stopped releases its predecessor's grant"
        );
        let url = &mounted(
            next["MediaSources"][0]["DirectStreamUrl"]
                .as_str()
                .expect("URL"),
        );
        let response = f
            .app
            .clone()
            .oneshot(request("GET", url, Some(&f.token), json!({})))
            .await
            .expect("new direct");
        assert_eq!(response.status(), StatusCode::OK);
        response.into_body().collect().await.expect("direct body");
        // Native start notification is asynchronous; observe its committed
        // presence before exercising the late Stop, without adding an owner.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !f
            .state
            .direct_plays
            .list()
            .iter()
            .any(|p| p.registry_id.contains(next_id))
        {
            assert!(
                std::time::Instant::now() < deadline,
                "native start notification did not register this play"
            );
            tokio::task::yield_now().await;
        }
        assert!(
            f.state
                .direct_plays
                .list()
                .iter()
                .any(|p| p.registry_id.contains(next_id)),
            "new play has its own direct presence"
        );
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing/Stopped", old_id, Some(9000)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            f.state.direct_plays.list().len(),
            1,
            "old Stop cannot remove new presence"
        );
        assert_eq!(
            play_event(
                &f,
                "/jellyfin/Sessions/Playing/Progress",
                old_id,
                Some(9999)
            )
            .await,
            StatusCode::CONFLICT
        );
        let before = f
            .state
            .store
            .watch_state(scope.user_id, f.native_item)
            .await
            .expect("watch");
        assert!(before.is_none_or(|w| w.position_ms != 9000 && w.position_ms != 9999));
        assert_eq!(
            play_event(
                &f,
                "/jellyfin/Sessions/Playing/Stopped",
                next_id,
                Some(2500)
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert!(f.state.direct_plays.list().is_empty());
        assert_eq!(
            f.state
                .store
                .watch_state(scope.user_id, f.native_item)
                .await
                .expect("watch")
                .expect("final")
                .position_ms,
            2500
        );
    }

    #[tokio::test]
    async fn jellyfin_direct_range_head_and_stop_preserve_native_authority_and_final_position() {
        let f = playback_fixture().await;
        let negotiation = negotiate(&f).await;
        let play = negotiation["PlaySessionId"].as_str().expect("play");
        // Without the ApiKey it carries, the authenticated URL is just a path.
        let url = &without_key(
            &mounted(
                negotiation["MediaSources"][0]["DirectStreamUrl"]
                    .as_str()
                    .expect("direct URL"),
            ),
            &f.token,
        );
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
        // Infuse labels static Range delivery `DirectStream`; only a label
        // naming the other delivery is refused.
        for (method, expected) in [
            ("DirectStream", StatusCode::NO_CONTENT),
            ("Transcode", StatusCode::BAD_REQUEST),
        ] {
            let body = json!({"UserId":f.user,"ItemId":f.item,"MediaSourceId":f.source,"PlaySessionId":play,"PlayMethod":method,"PositionTicks":3000*10_000});
            let response = f
                .app
                .clone()
                .oneshot(request(
                    "POST",
                    "/jellyfin/Sessions/Playing/Progress",
                    Some(&f.token),
                    body,
                ))
                .await
                .expect("labelled progress");
            assert_eq!(response.status(), expected, "{method}");
        }
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
    async fn status_of(f: &PlaybackFixture, r: Request<Body>) -> (StatusCode, Vec<u8>) {
        let response = f.app.clone().oneshot(r).await.expect("response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes()
            .to_vec();
        (status, bytes)
    }
    fn android_direct_url(f: &PlaybackFixture, item: &str, tag: &str) -> String {
        // The exact query Jellyfin Android TV 0.19.10 builds for direct play:
        // no PlaySessionId and no credential, the source ETag as `tag`.
        format!(
            "/jellyfin/Videos/{item}/stream?container=mp4&static=true&tag={tag}&mediaSourceId={}&streamOptions=%7B%7D&enableAudioVbrEncoding=true",
            f.source
        )
    }
    #[tokio::test]
    async fn jellyfin_scoped_media_link_serves_one_title_without_login_and_ends_with_stop_or_logout(
    ) {
        let f = playback_fixture().await;
        let negotiation = negotiate(&f).await;
        let play = negotiation["PlaySessionId"].as_str().expect("play");
        let tag = negotiation["MediaSources"][0]["ETag"]
            .as_str()
            .expect("scoped link")
            .to_owned();
        assert_eq!(tag.len(), 64);
        let direct_url = &mounted(
            negotiation["MediaSources"][0]["DirectStreamUrl"]
                .as_str()
                .expect("direct URL"),
        );
        assert!(
            !direct_url.contains(&tag),
            "the authenticated URL must not carry the link"
        );
        let binding = f
            .state
            .store
            .jellyfin_play(
                play,
                &f.state
                    .store
                    .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
                    .await
                    .expect("scope")
                    .expect("scope"),
            )
            .await
            .expect("binding")
            .expect("binding");
        assert_eq!(binding.state, "pending");
        let grant = f
            .state
            .store
            .file_grant_by_id(
                binding
                    .direct_grant_id
                    .as_deref()
                    .expect("negotiated grant"),
            )
            .await
            .expect("grant")
            .expect("grant");
        assert!(
            grant.expires_at <= binding.negotiation.created_at_ms / 1000 + 86_400,
            "the link expires within 24 hours of the negotiation"
        );
        let url = android_direct_url(&f, &f.item, &tag);
        let mut ranged = request("GET", &url, None, Value::Null);
        ranged
            .headers_mut()
            .insert("range", "bytes=2-5".parse().expect("range"));
        let (status, body) = status_of(&f, ranged).await;
        assert_eq!(
            status,
            StatusCode::PARTIAL_CONTENT,
            "{}",
            String::from_utf8_lossy(&body)
        );
        assert_eq!(&body[..], b"2345");
        let (status, body) = status_of(&f, request("HEAD", &url, None, Value::Null)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_empty());
        // Device metadata without a token is still an anonymous caller.
        let mut metadata = request("GET", &url, None, Value::Null);
        metadata.headers_mut().insert(
            "x-emby-authorization",
            "MediaBrowser Client=\"Jellyfin Android TV\", DeviceId=\"catalog-contract\""
                .parse()
                .expect("metadata"),
        );
        assert_eq!(status_of(&f, metadata).await.0, StatusCode::OK);
        // The link names one title: another item path or a guessed tag refuse.
        let other = WireId::random().to_hex();
        assert_eq!(
            status_of(
                &f,
                request(
                    "GET",
                    &android_direct_url(&f, &other, &tag),
                    None,
                    Value::Null
                )
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        let mut guessed = tag.clone().into_bytes();
        guessed[0] = if guessed[0] == b'0' { b'1' } else { b'0' };
        let guessed = String::from_utf8(guessed).expect("hex");
        assert_eq!(
            status_of(
                &f,
                request(
                    "GET",
                    &android_direct_url(&f, &f.item, &guessed),
                    None,
                    Value::Null
                )
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        // The native open-in grant route refuses a Jellyfin link outright.
        assert_eq!(
            status_of(
                &f,
                request(
                    "GET",
                    &format!("/api/v1/grants/{tag}/content"),
                    None,
                    Value::Null
                )
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        // A presented login must be the one the link was issued under.
        assert_eq!(
            status_of(&f, request("GET", &url, Some(&f.token), Value::Null))
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(3000)).await,
            StatusCode::NO_CONTENT
        );
        let (status, body) = status_of(&f, request("GET", &url, None, Value::Null)).await;
        assert_eq!(status, StatusCode::GONE);
        assert!(String::from_utf8_lossy(&body).contains("media_link_gone"));

        // Revoking the login ends a link that was never used.
        let second = negotiate(&f).await;
        let second_tag = second["MediaSources"][0]["ETag"]
            .as_str()
            .expect("second link")
            .to_owned();
        assert_ne!(second_tag, tag);
        let second_url = android_direct_url(&f, &f.item, &second_tag);
        assert_eq!(
            f.app
                .clone()
                .oneshot(request(
                    "POST",
                    "/jellyfin/Sessions/Logout",
                    Some(&f.token),
                    Value::Null
                ))
                .await
                .expect("logout")
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            status_of(&f, request("GET", &second_url, None, Value::Null))
                .await
                .0,
            StatusCode::GONE
        );
        // The switch still owns the whole surface.
        f.state
            .store
            .set_jellyfin_compatibility(false)
            .await
            .expect("disable");
        assert_eq!(
            status_of(&f, request("GET", &second_url, None, Value::Null))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
    #[tokio::test]
    async fn jellyfin_direct_representation_matrix_keeps_native_range_and_validator_truth() {
        let f = playback_fixture().await;
        let negotiation = negotiate(&f).await;
        let url = mounted(
            negotiation["MediaSources"][0]["DirectStreamUrl"]
                .as_str()
                .expect("URL"),
        );
        let call = |range: Option<&str>, if_range: Option<&str>, method: &str| {
            let mut r = request(method, &url, Some(&f.token), Value::Null);
            if let Some(range) = range {
                r.headers_mut()
                    .insert("range", range.parse().expect("range"));
            }
            if let Some(value) = if_range {
                r.headers_mut()
                    .insert("if-range", value.parse().expect("if-range"));
            }
            f.app.clone().oneshot(r)
        };
        let full = call(None, None, "GET").await.expect("full");
        assert_eq!(full.status(), StatusCode::OK);
        assert_eq!(full.headers()["content-length"], "16");
        assert_eq!(full.headers()["accept-ranges"], "bytes");
        // Raw native files publish no strong validator, so none is invented.
        assert!(full.headers().get("etag").is_none());
        let unsatisfiable = call(Some("bytes=100-200"), None, "GET").await.expect("416");
        assert_eq!(unsatisfiable.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(unsatisfiable.headers()["content-range"], "bytes */16");
        let suffix = call(Some("bytes=-4"), None, "GET").await.expect("suffix");
        assert_eq!(suffix.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(suffix.headers()["content-range"], "bytes 12-15/16");
        // Without a validator an If-Range precondition cannot authorize a part.
        let guarded = call(Some("bytes=2-5"), Some("\"anything\""), "GET")
            .await
            .expect("if-range");
        assert_eq!(guarded.status(), StatusCode::OK);
        assert_eq!(
            guarded
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes()
                .len(),
            16
        );
        let head = call(Some("bytes=2-5"), None, "HEAD").await.expect("head");
        assert_eq!(head.status(), StatusCode::OK);
        assert_eq!(head.headers()["content-length"], "16");
    }
    #[tokio::test]
    async fn jellyfin_switch_off_ends_negotiated_plays_even_after_it_is_turned_back_on() {
        let f = playback_fixture().await;
        let negotiation = negotiate(&f).await;
        let play = negotiation["PlaySessionId"].as_str().expect("play");
        let tag = negotiation["MediaSources"][0]["ETag"]
            .as_str()
            .expect("link")
            .to_owned();
        let url = mounted(
            negotiation["MediaSources"][0]["DirectStreamUrl"]
                .as_str()
                .expect("URL"),
        );
        for enabled in [false, true] {
            f.state
                .store
                .set_jellyfin_compatibility(enabled)
                .await
                .expect("save switch");
        }
        assert_eq!(
            status_of(&f, request("GET", &url, Some(&f.token), Value::Null))
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            status_of(
                &f,
                request(
                    "GET",
                    &android_direct_url(&f, &f.item, &tag),
                    None,
                    Value::Null
                )
            )
            .await
            .0,
            StatusCode::GONE
        );
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing", play, Some(1000)).await,
            StatusCode::CONFLICT
        );
        // Stop still cleans an old-generation play up: it ends and releases
        // it, but writes no progress through it.
        assert_eq!(
            play_event(&f, "/jellyfin/Sessions/Playing/Stopped", play, Some(4000)).await,
            StatusCode::NO_CONTENT
        );
        let scope = f
            .state
            .store
            .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
            .await
            .expect("scope")
            .expect("login");
        let stopped = f
            .state
            .store
            .jellyfin_play(play, &scope)
            .await
            .expect("read")
            .expect("tombstone");
        assert_eq!(stopped.state, "ended");
        let grant = f
            .state
            .store
            .file_grant_by_id(stopped.direct_grant_id.as_deref().expect("grant"))
            .await
            .expect("grant")
            .expect("grant");
        assert!(grant.revoked_at.is_some());
        assert!(f
            .state
            .store
            .watch_state(scope.user_id, f.native_item)
            .await
            .expect("watch")
            .is_none_or(|watch| watch.position_ms != 4000));
        // A facade login is not a native bearer.
        let native = f
            .app
            .clone()
            .oneshot(request("GET", "/api/v1/me", Some(&f.token), Value::Null))
            .await
            .expect("native call");
        assert_eq!(native.status(), StatusCode::UNAUTHORIZED);
        // A fresh negotiation under the current generation plays normally.
        let fresh = negotiate(&f).await;
        let fresh_url = &mounted(
            fresh["MediaSources"][0]["DirectStreamUrl"]
                .as_str()
                .expect("fresh URL"),
        );
        assert_eq!(
            status_of(&f, request("GET", fresh_url, Some(&f.token), Value::Null))
                .await
                .0,
            StatusCode::OK
        );
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
    async fn jellyfin_codec_conditions_use_exact_probed_source_and_selected_audio_before_allocation(
    ) {
        let f = playback_fixture().await;
        for (kind, codec, property, value, expected) in [
            ("Video", "h264", "Height", "1080", true),
            ("Video", "h264", "Height", "720", false),
            ("VideoAudio", "aac", "AudioChannels", "2", true),
            ("VideoAudio", "aac", "AudioChannels", "1", false),
            ("Video", "h264", "VideoLevel", "52", false),
            ("Video", "-hevc", "Height", "720", false),
            ("Video", "-h264", "Height", "720", true),
        ] {
            let body = json!({"UserId":f.user,"AudioStreamIndex":0,"SubtitleStreamIndex":-1,
                "DeviceProfile":{"DirectPlayProfiles":[{"Type":"Video","Container":"mp4","VideoCodec":"h264","AudioCodec":"aac"}],
                    "CodecProfiles":[{"Type":kind,"Codec":codec,"Container":"mp4",
                        "Conditions":[{"Property":property,"Condition":"LessThanEqual","Value":value,"IsRequired":true}]}]}});
            let (status, answer) = json_call(
                &f.app,
                request(
                    "POST",
                    &format!("/jellyfin/Items/{}/PlaybackInfo", f.item),
                    Some(&f.token),
                    body,
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                answer["PlaySessionId"].is_string(),
                expected,
                "{property} {value}"
            );
        }
    }
    #[tokio::test]
    async fn jellyfin_ancillary_routes_share_native_markers_and_refuse_unbound_requests() {
        let f = playback_fixture().await;
        let id = f
            .state
            .store
            .jellyfin_resolve_entity(JellyfinEntityKind::File, &f.source)
            .await
            .expect("mapping")
            .expect("file");
        f.state
            .store
            .merge_file_probe_chapters(
                id,
                &json!([
                    {"start_time":"0.000","end_time":"10.000","tags":{"title":"Intro"}},
                    {"start_time":"90.000","end_time":"100.000","tags":{"title":"Credits"}}
                ])
                .to_string(),
            )
            .await
            .expect("chapters");
        let path = format!("/jellyfin/MediaSegments/{}", f.item);
        let (status, body) =
            json_call(&f.app, request("GET", &path, Some(&f.token), json!({}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["TotalRecordCount"], 2);
        assert_eq!(body["Items"][0]["Type"], "Intro");
        assert_eq!(body["Items"][0]["StartTicks"], 0);
        assert_eq!(body["Items"][0]["EndTicks"], 100_000_000);
        let (_, filtered) = json_call(
            &f.app,
            request(
                "GET",
                &format!("{path}?includeSegmentTypes=Outro"),
                Some(&f.token),
                json!({}),
            ),
        )
        .await;
        assert_eq!(filtered["TotalRecordCount"], 1);
        assert_eq!(filtered["Items"][0], body["Items"][1]);
        let intro = format!("/jellyfin/Items/{}/Intros", f.item);
        let (status, empty) =
            json_call(&f.app, request("GET", &intro, Some(&f.token), json!({}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(empty["TotalRecordCount"], 0);
        for (path, token, expected) in [
            (path.clone(), None, StatusCode::UNAUTHORIZED),
            (
                format!("{path}?includeSegmentTypes=MadeUp"),
                Some(f.token.as_str()),
                StatusCode::BAD_REQUEST,
            ),
            (
                format!("{intro}?UserId={}", "1".repeat(32)),
                Some(f.token.as_str()),
                StatusCode::FORBIDDEN,
            ),
            (
                format!("/jellyfin/MediaSegments/{}", "1".repeat(32)),
                Some(f.token.as_str()),
                StatusCode::NOT_FOUND,
            ),
        ] {
            let (status, _) = json_call(&f.app, request("GET", &path, token, json!({}))).await;
            assert_eq!(status, expected, "{path}");
        }
        f.state
            .store
            .set_jellyfin_compatibility(false)
            .await
            .expect("disable");
        let (status, _) = json_call(&f.app, request("GET", &path, Some(&f.token), json!({}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn jellyfin_subtitle_native_failures_and_bitmap_refusals_are_not_empty_success() {
        use plurx_core::domain::{ProbeResult, SubtitleStream};
        let f = playback_fixture().await;
        let path = f
            .root
            .path()
            .canonicalize()
            .expect("root")
            .join("movie.mp4");
        for (codec, expected) in [
            ("subrip", StatusCode::INTERNAL_SERVER_ERROR),
            ("hdmv_pgs_subtitle", StatusCode::BAD_REQUEST),
        ] {
            f.state.store.upsert_file(f.native_item, path.to_str().expect("path"), 16, 1, &ProbeResult {
                duration_ms: Some(100_000), container: Some("mp4".into()),
                subtitle_streams: vec![SubtitleStream { index: 2, codec: codec.into(), language: None, title: None, default: false, forced: false, hearing_impaired: false }],
                raw_json: Some(json!({"streams":[{"index":2,"codec_type":"subtitle","codec_name":codec}]}).to_string()),
                ..Default::default()
            }).await.expect("probe");
            for format in ["vtt", "srt"] {
                let route = format!(
                    "/jellyfin/Videos/{}/{}/Subtitles/2/Stream.{format}",
                    f.item, f.source
                );
                let (status, body) =
                    json_call(&f.app, request("GET", &route, Some(&f.token), json!({}))).await;
                assert_eq!(status, expected, "{body}");
            }
        }
    }

    #[tokio::test]
    async fn jellyfin_subtitle_aliases_reuse_native_extraction_with_global_indices_and_source_times(
    ) {
        use plurx_core::testfixtures;
        testfixtures::require_ffmpeg();
        let f = playback_fixture().await;
        let caption = f.root.path().join("source.srt");
        std::fs::write(
            &caption,
            "1\n00:00:00,000 --> 00:00:02,125\nFirst\n\n2\n00:00:03,456 --> 00:00:05,789\nSecond\n",
        )
        .expect("caption");
        let movie = f
            .root
            .path()
            .canonicalize()
            .expect("canonical fixture root")
            .join("movie.mp4");
        let staged = f.root.path().join("captions.mkv");
        testfixtures::run(
            std::process::Command::new(testfixtures::ffmpeg())
                .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
                .arg(testfixtures::source("clean-cra"))
                .arg("-i")
                .arg(&caption)
                .args([
                    "-map", "0:v:0", "-map", "0:a:0?", "-map", "1:0", "-c", "copy", "-c:s", "srt",
                ])
                .arg(&staged),
        );
        std::fs::rename(&staged, &movie).expect("replace fixture source");
        let probe = plurx_core::scan::probe::probe(&movie)
            .await
            .expect("real probe");
        let raw: Value = serde_json::from_str(probe.raw_json.as_deref().expect("raw probe"))
            .expect("probe JSON");
        let global_index = raw["streams"]
            .as_array()
            .expect("streams")
            .iter()
            .find(|s| s["codec_type"] == "subtitle")
            .expect("caption stream")["index"]
            .as_i64()
            .expect("global index");
        assert!(
            global_index > 0,
            "subtitle zero ordinal must not be confused with its global stream index"
        );
        let metadata = std::fs::metadata(&movie).expect("source identity");
        let mtime = metadata
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch")
            .as_secs() as i64;
        f.state
            .store
            .upsert_file(
                f.native_item,
                movie.to_str().expect("path"),
                metadata.len() as i64,
                mtime,
                &probe,
            )
            .await
            .expect("updated probe");
        for (format, content_type, timing) in [
            ("vtt", "text/vtt", "00:00.000 --> 00:02.125"),
            (
                "srt",
                "application/x-subrip",
                "00:00:00,000 --> 00:00:02,125",
            ),
        ] {
            let path = format!(
                "/jellyfin/Videos/{}/{}/Subtitles/{global_index}/Stream.{format}",
                f.item, f.source
            );
            let response = f
                .app
                .clone()
                .oneshot(request("GET", &path, Some(&f.token), json!({})))
                .await
                .expect("subtitle");
            assert_eq!(response.status(), StatusCode::OK);
            assert!(response.headers()["content-type"]
                .to_str()
                .expect("type")
                .starts_with(content_type));
            let bytes = response
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes();
            let text = std::str::from_utf8(&bytes).expect("subtitle UTF8");
            assert!(text.contains(timing), "{text}");
            assert!(text.contains("First") && text.contains("Second"));
            let response = f
                .app
                .clone()
                .oneshot(request("GET", &path, None, json!({})))
                .await
                .expect("auth refusal");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        // The five-segment form both pinned clients request (J0 traces):
        // Infuse with its login header and `Range: bytes=0-`, Android TV with
        // only the `ApiKey` from `DeliveryUrl`, in `subrip` or `vtt`.
        let subtitle = |path: String, token: Option<&str>| {
            let mut r = request("GET", &path, token, json!({}));
            r.headers_mut()
                .insert("range", "bytes=0-".parse().expect("range"));
            let app = f.app.clone();
            async move {
                let response = app.oneshot(r).await.expect("subtitle");
                let status = response.status();
                let kind = response
                    .headers()
                    .get("content-type")
                    .map(|v| v.to_str().expect("type").to_owned())
                    .unwrap_or_default();
                let bytes = response
                    .into_body()
                    .collect()
                    .await
                    .expect("body")
                    .to_bytes();
                (
                    status,
                    kind,
                    String::from_utf8(bytes.to_vec()).expect("UTF8"),
                )
            }
        };
        let base = format!(
            "/jellyfin/Videos/{}/{}/Subtitles/{global_index}",
            f.item, f.source
        );
        let (status, kind, text) = subtitle(format!("{base}/0/Stream.srt"), Some(&f.token)).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert!(kind.starts_with("application/x-subrip"));
        assert!(text.contains("00:00:00,000 --> 00:00:02,125"), "{text}");
        for (format, timing) in [
            ("subrip", "00:00:03,456 --> 00:00:05,789"),
            ("vtt", "00:03.456 --> 00:05.789"),
        ] {
            let (status, _, text) =
                subtitle(format!("{base}/0/Stream.{format}?ApiKey={}", f.token), None).await;
            assert_eq!(status, StatusCode::OK, "{format}: {text}");
            assert!(text.contains(timing), "{format}: {text}");
        }
        // A start position is Jellyfin's window: earlier cues dropped, the
        // rest moved by the start unless CopyTimestamps keeps source times.
        let (status, _, text) =
            subtitle(format!("{base}/30000000/Stream.vtt"), Some(&f.token)).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert!(!text.contains("First"), "{text}");
        assert!(
            text.contains("00:00:00.456 --> 00:00:02.789\nSecond"),
            "{text}"
        );
        let (status, _, text) = subtitle(
            format!("{base}/Stream.srt?StartPositionTicks=30000000&CopyTimestamps=true"),
            Some(&f.token),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(text, "1\n00:00:03,456 --> 00:00:05,789\nSecond\n\n");
        assert_eq!(
            subtitle(format!("{base}/0/Stream.ass"), Some(&f.token))
                .await
                .0,
            StatusCode::NOT_FOUND,
            "only implemented formats are served"
        );
        assert_eq!(
            subtitle(format!("{base}/0/Stream.vtt"), None).await.0,
            StatusCode::UNAUTHORIZED
        );
        let invalid = format!(
            "/jellyfin/Videos/{}/{}/Subtitles/0/Stream.vtt",
            f.item, f.source
        );
        let response = f
            .app
            .clone()
            .oneshot(request("GET", &invalid, Some(&f.token), json!({})))
            .await
            .expect("wrong index");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let old = f
            .state
            .store
            .get_item(f.native_item)
            .await
            .expect("item")
            .expect("live");
        let other = f
            .state
            .store
            .insert_item(&plurx_core::domain::NewItem {
                library_id: old.library_id,
                kind: plurx_core::domain::ItemKind::Movie,
                parent_id: None,
                title: "Other source membership".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("other item");
        f.state
            .store
            .upsert_file(
                other,
                movie.to_str().expect("path"),
                metadata.len() as i64,
                mtime,
                &probe,
            )
            .await
            .expect("move source");
        let route = format!(
            "/jellyfin/Videos/{}/{}/Subtitles/{global_index}/Stream.vtt",
            f.item, f.source
        );
        let (status, _) =
            json_call(&f.app, request("GET", &route, Some(&f.token), json!({}))).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "a moved source cannot serve through its old item"
        );
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
        // Clients gate on the protocol version before they ever sign in: a
        // Plurx build version here reads as an unsupported Jellyfin server.
        let (status, info) = json_call(
            &app,
            request("GET", "/jellyfin/System/Info/Public", None, Value::Null),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(info["Version"], "10.11.11");
        assert_eq!(info["ProductName"], "Jellyfin Server");
        assert_eq!(info["StartupWizardCompleted"], true);
        assert!(info["ServerName"]
            .as_str()
            .is_some_and(|name| !name.is_empty()));
        assert!(info["Id"].as_str().is_some_and(|id| !id.is_empty()));
        // Every Infuse share mode is the Infuse family; an unknown app is not.
        for (client, expected) in [
            ("Infuse-Library", StatusCode::OK),
            ("Infuse-Direct", StatusCode::OK),
            ("Infuse", StatusCode::OK),
            ("Some Other App", StatusCode::BAD_REQUEST),
        ] {
            let mut login = request(
                "POST",
                "/jellyfin/Users/AuthenticateByName",
                None,
                json!({"Username":"catalog-admin","Pw":"supersecret"}),
            );
            login.headers_mut().insert(
                "x-emby-authorization",
                format!("MediaBrowser Client=\"{client}\", DeviceId=\"family-{expected}\", Version=\"8.5.6\"")
                    .parse()
                    .expect("metadata"),
            );
            let (status, _) = json_call(&app, login).await;
            assert_eq!(status, expected, "{client}");
        }
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
        // Mapped artwork needs no login (approved 2026-10-03); this item has
        // none, so both callers get the same honest miss, never a 401.
        assert_eq!(
            json_call(&app, request("GET", &image_path, None, Value::Null))
                .await
                .0,
            StatusCode::NOT_FOUND
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

    fn subtitle_probe(container: &str) -> plurx_core::domain::ProbeResult {
        use plurx_core::domain::{AudioStream, ProbeResult, SubtitleStream};
        let track = |index: i64, codec: &str| SubtitleStream {
            index,
            codec: codec.into(),
            language: Some("eng".into()),
            title: None,
            default: false,
            forced: false,
            hearing_impaired: false,
        };
        ProbeResult {
            duration_ms: Some(100_000),
            container: Some(container.into()),
            video_codec: Some("h264".into()),
            video_codec_tag: Some("avc1".into()),
            video_profile: Some("Main".into()),
            width: Some(1920),
            height: Some(1080),
            bit_depth: Some(8),
            bitrate: Some(100_000),
            audio_streams: vec![AudioStream {
                index: 0,
                codec: "aac".into(),
                channel_layout: None,
                channels: Some(2),
                sample_rate: Some(48_000),
                language: None,
                title: None,
                default: true,
            }],
            subtitle_streams: vec![
                track(2, "subrip"),
                track(3, "hdmv_pgs_subtitle"),
                track(4, "ass"),
            ],
            raw_json: Some(
                json!({"streams":[
                    {"index":0,"codec_type":"audio","codec_name":"aac","channels":2,"sample_rate":"48000"},
                    {"index":1,"codec_type":"video","codec_name":"h264","width":1920,"height":1080},
                    {"index":2,"codec_type":"subtitle","codec_name":"subrip","tags":{"language":"eng"}},
                    {"index":3,"codec_type":"subtitle","codec_name":"hdmv_pgs_subtitle","tags":{"language":"eng"}},
                    {"index":4,"codec_type":"subtitle","codec_name":"ass","tags":{"language":"eng"}}]})
                .to_string(),
            ),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn jellyfin_subtitle_delivery_follows_each_client_profile_with_a_requestable_url() {
        let f = playback_fixture().await;
        let path = f
            .root
            .path()
            .canonicalize()
            .expect("root")
            .join("movie.mp4");
        f.state
            .store
            .upsert_file(
                f.native_item,
                path.to_str().expect("path"),
                16,
                1,
                &subtitle_probe("mkv"),
            )
            .await
            .expect("probe");
        let ask = |subtitles: Value, selected: i64| {
            json!({"UserId":f.user,"MediaSourceId":f.source,"AudioStreamIndex":0,"SubtitleStreamIndex":selected,
                "DeviceProfile":{"DirectPlayProfiles":[{"Type":"Video","Container":"mkv","VideoCodec":"h264","AudioCodec":"aac"}],
                "SubtitleProfiles":subtitles}})
        };
        let negotiate_with = |body: Value| {
            let request = request(
                "POST",
                &format!("/jellyfin/Items/{}/PlaybackInfo", f.item),
                Some(&f.token),
                body,
            );
            let app = f.app.clone();
            async move { json_call(&app, request).await }
        };
        let streams = |body: &Value| {
            body["MediaSources"][0]["MediaStreams"]
                .as_array()
                .expect("streams")
                .iter()
                .filter(|s| s["Type"] == "Subtitle")
                .cloned()
                .collect::<Vec<_>>()
        };
        // Jellyfin Android TV 0.19.10: the direct file's own tracks are
        // embedded, the bitmap one under Jellyfin's codec name.
        let android = json!([
            {"Format":"vtt","Method":"Embed"},{"Format":"vtt","Method":"External"},{"Format":"vtt","Method":"Hls"},
            {"Format":"subrip","Method":"Embed"},{"Format":"subrip","Method":"External"},
            {"Format":"pgssub","Method":"Embed"},{"Format":"pgssub","Method":"Encode"}]);
        let (status, body) = negotiate_with(ask(android, -1)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["MediaSources"][0]["SupportsDirectPlay"], true);
        let tracks = streams(&body);
        assert_eq!(tracks.len(), 3);
        assert!(
            tracks[..2].iter().all(|s| s["DeliveryMethod"] == "Embed"),
            "{tracks:?}"
        );
        assert!(tracks[..2].iter().all(|s| s.get("DeliveryUrl").is_none()));
        // Android TV cannot render ASS itself: a converted VTT sidecar.
        assert_eq!(tracks[2]["DeliveryMethod"], "External", "{tracks:?}");
        assert_eq!(tracks[2]["IsTextSubtitleStream"], true);
        assert!(tracks[2]["DeliveryUrl"]
            .as_str()
            .is_some_and(|url| url.contains("/Subtitles/4/0/Stream.vtt?ApiKey=")));
        // Infuse 8.5.6: External VTT only. The text track becomes a sidecar at
        // the five-segment route both clients request; the bitmap one cannot.
        let infuse = json!([{"Format":"vtt","Method":"External","AllowChunkedResponse":true},
            {"Format":"ass","Method":"External"},{"Format":"ssa","Method":"External"}]);
        let (status, body) = negotiate_with(ask(infuse.clone(), -1)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let tracks = streams(&body);
        assert_eq!(tracks[0]["Index"], 2);
        assert_eq!(tracks[0]["DeliveryMethod"], "External");
        assert_eq!(tracks[0]["IsExternalUrl"], false);
        assert_eq!(
            tracks[0]["DeliveryUrl"],
            format!(
                "/Videos/{}/{}/Subtitles/2/0/Stream.vtt?ApiKey={}",
                f.item, f.source, f.token
            )
        );
        assert_eq!(tracks[1]["DeliveryMethod"], "Encode");
        assert!(tracks[1].get("DeliveryUrl").is_none());
        // ASS is text the native extractor converts: a sidecar, not a burn.
        assert_eq!(tracks[2]["DeliveryMethod"], "External");
        let (status, body) = negotiate_with(ask(infuse.clone(), 4)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["MediaSources"][0]["SupportsDirectPlay"], true,
            "a sidecar-capable selection keeps direct play: {body}"
        );
        // Selecting a track the direct file cannot deliver makes the play a
        // transcode; this profile offers none, so negotiation refuses.
        let (status, body) = negotiate_with(ask(infuse.clone(), 3)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ErrorCode"], "NotSupported", "{body}");
        // Infuse's own request (J0) declares no DirectPlayProfiles and asks
        // for HTTP direct play: it streams the file itself, tracks and all,
        // so even a bitmap selection stays a direct play.
        let mut own = infuse_request(&f);
        own["SubtitleStreamIndex"] = json!(3);
        own["DeviceProfile"]["SubtitleProfiles"] = infuse;
        let (status, body) = negotiate_with(own).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["MediaSources"][0]["SupportsDirectPlay"], true,
            "{body}"
        );
        // No profile list grants nothing.
        let (_, body) = negotiate_with(ask(Value::Null, -1)).await;
        assert!(streams(&body)
            .iter()
            .all(|s| s["DeliveryMethod"] == "Encode"));
    }

    /// Infuse 8.5.6's normal-play PlaybackInfo body (J0 `profiles[0]`).
    fn infuse_request(f: &PlaybackFixture) -> Value {
        json!({"AutoOpenLiveStream":true,"IsPlayback":true,"UserId":f.user,"EnableDirectPlay":true,
            "MediaSourceId":f.source,"MaxStreamingBitrate":200_000_000,"DirectPlayProtocols":["Http"],
            "DeviceProfile":{"MaxStreamingBitrate":200_000_000,"MaxStaticBitrate":200_000_000,
                "MusicStreamingTranscodingBitrate":192_000,
                "TranscodingProfiles":[
                    {"MinSegments":1,"AudioCodec":"aac","Container":"aac","BreakOnNonKeyFrames":true,"MaxAudioChannels":"2","Type":"Audio","Context":"Streaming","Protocol":"hls"},
                    {"AudioCodec":"aac","MinSegments":1,"Container":"ts","ManifestSubtitles":"vtt","MaxAudioChannels":"2","BreakOnNonKeyFrames":true,"Type":"Video","Context":"Streaming","VideoCodec":"hevc,h264,av1","Protocol":"hls"}],
                "SubtitleProfiles":[{"AllowChunkedResponse":true,"Format":"vtt","Method":"External"},
                    {"Format":"ass","Method":"External"},{"Format":"ssa","Method":"External"}]}})
    }
    #[tokio::test]
    async fn jellyfin_infuse_direct_request_without_a_play_id_uses_its_own_newest_direct_play() {
        let f = playback_fixture().await;
        // Infuse's PlaybackInfo, as J0 captured it: no DirectPlayProfiles,
        // `EnableDirectPlay` with `DirectPlayProtocols: ["Http"]`.
        let infuse_info = |body: Value| {
            let request = request(
                "POST",
                &format!("/jellyfin/Items/{}/PlaybackInfo", f.item),
                Some(&f.token),
                body,
            );
            let app = f.app.clone();
            async move { json_call(&app, request).await }
        };
        let (status, older) = infuse_info(infuse_request(&f)).await;
        assert_eq!(status, StatusCode::OK, "{older}");
        assert_eq!(
            older["MediaSources"][0]["SupportsDirectPlay"], true,
            "{older}"
        );
        assert!(older["MediaSources"][0].get("TranscodingUrl").is_none());
        let (_, newer) = infuse_info(infuse_request(&f)).await;
        let newest = newer["PlaySessionId"].as_str().expect("play");
        let mut odd = infuse_request(&f);
        odd["DirectPlayProtocols"] = json!(["Carrier-Pigeon"]);
        assert_eq!(infuse_info(odd).await.0, StatusCode::BAD_REQUEST);
        // The direct URL Infuse builds: login header, `MediaSourceId`, `Static`.
        let infuse = format!(
            "/jellyfin/Videos/{}/stream?MediaSourceId={}&Static=true",
            f.item, f.source
        );
        let mut ranged = request("GET", &infuse, Some(&f.token), Value::Null);
        ranged
            .headers_mut()
            .insert("range", "bytes=0-".parse().expect("range"));
        let (status, bytes) = status_of(&f, ranged).await;
        assert_eq!(
            status,
            StatusCode::PARTIAL_CONTENT,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(bytes, b"0123456789abcdef");
        let scope = f
            .state
            .store
            .jellyfin_login_scope(plurx_core::auth::hash_token(&f.token))
            .await
            .expect("scope")
            .expect("login");
        let state_of = |play: String| {
            let store = std::sync::Arc::clone(&f.state.store);
            let scope = scope.clone();
            async move {
                store
                    .jellyfin_play(&play, &scope)
                    .await
                    .expect("read")
                    .expect("play")
                    .state
            }
        };
        assert_eq!(state_of(newest.to_owned()).await, "active");
        assert_ne!(
            state_of(older["PlaySessionId"].as_str().expect("play").to_owned()).await,
            "active",
            "the request resolved the newest negotiation only"
        );
        // Without a login the same URL is not a capability.
        assert_eq!(
            status_of(&f, request("GET", &infuse, None, Value::Null))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        // Another source of the item resolves nothing.
        let other = format!(
            "/jellyfin/Videos/{}/stream?MediaSourceId={}&Static=true",
            f.item, f.item
        );
        assert_eq!(
            status_of(&f, request("GET", &other, Some(&f.token), Value::Null))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn jellyfin_contract_routes_answer_their_minimum_semantics_and_are_counted() {
        let f = playback_fixture().await;
        let call = |method: &str, path: &str, token: Option<&str>, body: Value| {
            let request = request(method, path, token, body);
            let app = f.app.clone();
            async move {
                let response = app.oneshot(request).await.expect("response");
                let status = response.status();
                let bytes = response
                    .into_body()
                    .collect()
                    .await
                    .expect("body")
                    .to_bytes();
                (
                    status,
                    serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
                )
            }
        };
        let token = Some(f.token.as_str());
        // Public users: always empty, no login needed.
        assert_eq!(
            call("GET", "/jellyfin/Users/Public", None, Value::Null).await,
            (StatusCode::OK, json!([]))
        );
        // Authenticated system information: the baseline plus Plurx's build.
        assert_eq!(
            call("GET", "/jellyfin/System/Info", None, Value::Null)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        let (status, info) = call("GET", "/jellyfin/System/Info", token, Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(info["Version"], "10.11.11");
        assert!(info["PackageName"]
            .as_str()
            .is_some_and(|p| p.starts_with("plurx ")));
        assert!(info.get("ProgramDataPath").is_none() && info.get("LogPath").is_none());
        // Capabilities: validated and accepted, not stored.
        assert_eq!(
            call("POST", "/jellyfin/Sessions/Capabilities?PlayableMediaTypes=Video&SupportsMediaControl=false", token, Value::Null).await.0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            call(
                "POST",
                "/jellyfin/Sessions/Capabilities?SupportsMediaControl=maybe",
                token,
                Value::Null
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                "POST",
                "/jellyfin/Sessions/Capabilities/Full",
                token,
                json!({"PlayableMediaTypes":["Video"]})
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            call(
                "POST",
                "/jellyfin/Sessions/Capabilities/Full",
                token,
                json!([])
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        // Search hints over the catalog search.
        let (status, hints) = call(
            "GET",
            "/jellyfin/Search/Hints?searchTerm=direct&Limit=10",
            token,
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{hints}");
        assert_eq!(hints["TotalRecordCount"], 1);
        assert_eq!(hints["SearchHints"][0]["Id"], f.item);
        assert_eq!(hints["SearchHints"][0]["Type"], "Movie");
        let (_, people) = call(
            "GET",
            "/jellyfin/Search/Hints?searchTerm=direct&IncludeItemTypes=Person",
            token,
            Value::Null,
        )
        .await;
        assert_eq!(people["TotalRecordCount"], 0);
        assert_eq!(
            call("GET", "/jellyfin/Search/Hints", token, Value::Null)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        // Downloads: an honest refusal for a mapped item.
        let (status, refused) = call(
            "GET",
            &format!("/jellyfin/Items/{}/Download", f.item),
            token,
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(refused["code"], "download_not_offered");
        // Watched state through the 10.9+ route, user optional.
        let played = format!("/jellyfin/UserPlayedItems/{}", f.item);
        let (status, data) = call("POST", &played, token, Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(data["Played"], true);
        let (status, data) = call(
            "DELETE",
            &format!("{played}?userId={}", f.user),
            token,
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(data["Played"], false);
        assert_eq!(
            call(
                "POST",
                &format!("{played}?userId={}", f.item),
                token,
                Value::Null
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        // Ping and ActiveEncodings act only on a named play of this login.
        let mut second = request(
            "POST",
            "/jellyfin/Users/AuthenticateByName",
            None,
            json!({"Username":"catalog-admin","Pw":"supersecret"}),
        );
        second.headers_mut().insert("x-emby-authorization", "MediaBrowser Client=\"Jellyfin+Android+TV\", Device=\"other TV\", DeviceId=\"second-device\", Version=\"0.19.10\"".parse().expect("metadata"));
        let (status, second) = json_call(&f.app, second).await;
        assert_eq!(status, StatusCode::OK);
        let other = second["AccessToken"]
            .as_str()
            .expect("second login")
            .to_owned();
        let play = negotiate(&f).await["PlaySessionId"]
            .as_str()
            .expect("play")
            .to_owned();
        let direct = format!(
            "/jellyfin/Videos/{}/stream?MediaSourceId={}&PlaySessionId={play}",
            f.item, f.source
        );
        assert_eq!(
            status_of(&f, request("GET", &direct, token, Value::Null))
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            call(
                "POST",
                &format!("/jellyfin/Sessions/Playing/Ping?playSessionId={play}"),
                token,
                Value::Null
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            call(
                "POST",
                "/jellyfin/Sessions/Playing/Ping",
                token,
                Value::Null
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                "POST",
                &format!(
                    "/jellyfin/Sessions/Playing/Ping?playSessionId={play}&deviceId=someone-else"
                ),
                token,
                Value::Null
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                "DELETE",
                "/jellyfin/Videos/ActiveEncodings?deviceId=catalog-contract",
                token,
                Value::Null
            )
            .await
            .0,
            StatusCode::BAD_REQUEST,
            "never a kill by device alone"
        );
        for (method, path) in [
            (
                "POST",
                format!("/jellyfin/Sessions/Playing/Ping?playSessionId={play}"),
            ),
            (
                "DELETE",
                format!("/jellyfin/Videos/ActiveEncodings?playSessionId={play}"),
            ),
        ] {
            assert_eq!(
                call(method, &path, Some(&other), Value::Null).await.0,
                StatusCode::NOT_FOUND,
                "{method} {path}: another login's play is not this login's"
            );
        }
        assert_eq!(
            call("DELETE", &format!("/jellyfin/Videos/ActiveEncodings?deviceId=catalog-contract&playSessionId={play}"), token, Value::Null).await.0,
            StatusCode::NO_CONTENT
        );
        // Every request above is counted under its template; an unknown path
        // under `unmatched` (a delta: the counter is process-wide).
        let unmatched = || {
            super::metrics::prometheus()
                .lines()
                .find_map(|line| {
                    line.strip_prefix(
                        "plurx_jellyfin_requests_total{route=\"unmatched\",outcome=\"not_found\"} ",
                    )
                    .map(|n| n.parse::<u64>().expect("count"))
                })
                .unwrap_or(0)
        };
        let before = unmatched();
        call("GET", "/jellyfin/No/Such/Route", token, Value::Null).await;
        assert!(unmatched() > before, "an unknown route is counted");
        let text = super::metrics::prometheus();
        for line in [
            "route=\"/jellyfin/Search/Hints\",outcome=\"ok\"",
            "route=\"/jellyfin/Items/{item_id}/Download\",outcome=\"forbidden\"",
            "route=\"/jellyfin/Sessions/Playing/Ping\",outcome=\"refused\"",
            "route=\"unmatched\",outcome=\"not_found\"",
        ] {
            assert!(text.contains(line), "{line} missing from:\n{text}");
        }
        assert!(!text.contains(&f.item) && !text.contains(&f.token) && !text.contains(&play));
    }
}
