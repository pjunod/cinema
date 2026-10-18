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
    http::{request::Parts, uri::PathAndQuery, HeaderMap, StatusCode, Uri},
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

/// The facade's path on Plurx's own port, beside the web app and Plex.
pub(crate) const MOUNT: &str = "/jellyfin";

/// Marks a request that reached the facade at the root of the Jellyfin
/// standard port, so the responses that name facade paths themselves name
/// them without a mount the client never used.
#[derive(Clone, Copy, Debug)]
struct RootMount;

/// The path prefix this client reaches the facade under: `/jellyfin` on
/// Plurx's port, nothing at the root of the Jellyfin standard port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Mount(pub(super) &'static str);
impl<S: Send + Sync> FromRequestParts<S> for Mount {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(if parts.extensions.get::<RootMount>().is_some() {
            ""
        } else {
            MOUNT
        }))
    }
}

/// The facade at the server root, for the Jellyfin standard-port listener.
///
/// Jellyfin clients given only a host connect to `http://<host>:8096/`.
/// Every request on that listener is mapped under [`MOUNT`] before routing,
/// so it reaches only the facade, never the native API, the web app or the
/// Plex routes, and passes the same enabled gate, serving fence, deadlines,
/// redaction and metrics as a request to `/jellyfin` on Plurx's port. A path
/// already under the mount is left as it is, so `http://<host>:8096/jellyfin`
/// works too.
pub(crate) fn standard_port_app(app: Router) -> Router {
    Router::new().fallback_service(tower::util::MapRequest::new(app, at_root))
}

// `OriginalUri` keeps the unmapped target on purpose: nothing in plurxd
// reads it, and routing, gates, metrics and redaction all key on the mapped
// URI and `MatchedPath`.
fn at_root(mut request: axum::extract::Request) -> axum::extract::Request {
    let path = request.uri().path();
    if path == MOUNT || path.starts_with("/jellyfin/") {
        return request;
    }
    // An asterisk-form target (`OPTIONS *`) has no leading slash, and
    // prefixing it would name `/jellyfin*`, outside the nest and so the SPA.
    if !path.starts_with('/') {
        *request.uri_mut() = Uri::from_static("/jellyfin/");
        request.extensions_mut().insert(RootMount);
        return request;
    }
    let target = match request.uri().query() {
        Some(query) => format!("{MOUNT}{path}?{query}"),
        None => format!("{MOUNT}{path}"),
    };
    let mut parts = request.uri().clone().into_parts();
    // Whatever happens, an unmapped path must never reach a native route,
    // so a target that cannot be re-parsed falls to the mount's JSON 404.
    parts.path_and_query = Some(
        target
            .parse()
            .unwrap_or_else(|_| PathAndQuery::from_static("/jellyfin/")),
    );
    *request.uri_mut() = Uri::from_parts(parts).unwrap_or_else(|_| Uri::from_static("/jellyfin/"));
    request.extensions_mut().insert(RootMount);
    request
}

/// What became of this process's Jellyfin standard-port listener.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum StandardPort {
    /// Turned off (`jellyfin_port = 0`), or startup has not opened it yet.
    #[default]
    Off,
    Listening(std::net::SocketAddr),
    Unavailable {
        address: std::net::SocketAddr,
        error: String,
    },
}

#[derive(Debug, Default)]
pub(crate) struct StandardPortStatus(std::sync::RwLock<StandardPort>);
impl StandardPortStatus {
    pub(crate) fn record(&self, port: StandardPort) {
        *self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = port;
    }
    pub(crate) fn current(&self) -> StandardPort {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

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
    mut q: BrowseQuery,
    target: Option<WireId>,
    mode: JellyfinCatalogMode,
) -> Result<wire::Items<wire::Item>, ApiError> {
    if let Some(user) = &q.user {
        check_user(client, &user.to_hex())?;
    }
    // Kinds Jellyfin defines but the catalogue never holds select nothing.
    if q.kinds_absent {
        return Ok(wire::Items {
            items: vec![],
            total_record_count: 0,
            start_index: q.start as u64,
        });
    }
    // `SeriesId`/`SeasonId` narrow to that show's or season's descendants:
    // the most specific one is the parent of a recursive read.
    if let Some(scope) = q.season.take().or(q.series.take()) {
        q.parent = Some(scope);
        q.recursive = true;
    }
    // Library folders are not catalogue rows: `items` answers a request for
    // only them at the top level, and anywhere else none exist. A request
    // that also names catalogue kinds lists just those.
    if q.views && q.kinds.is_empty() {
        return Ok(wire::Items {
            items: vec![],
            total_record_count: 0,
            start_index: q.start as u64,
        });
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
        limit: 0,
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
    // The store reads at most STORE_PAGE_ROWS rows at a time. Jellyfin has no
    // page ceiling, and a client that omits `Limit` expects every row, so a
    // larger page is assembled from consecutive reads, up to RESPONSE_ROWS;
    // the true TotalRecordCount lets the client page past that.
    // Jellyfin answers a request without `Limit` with every row; routes with
    // their own default (Latest) set it before reaching here.
    let wanted = q.limit.unwrap_or(i64::MAX).min(RESPONSE_ROWS);
    let server = server_id(state).await?;
    let mut items = Vec::new();
    let total = loop {
        query.start = q.start.saturating_add(items.len() as i64);
        query.limit = (wanted - items.len() as i64).min(STORE_PAGE_ROWS);
        let page = Box::pin(catalog_page(state, &query)).await?;
        let read = page.items.len() as i64;
        for row in &page.items {
            items.push(item_dto(row, &server)?);
        }
        if read == 0
            || items.len() as i64 >= wanted
            || query.start.saturating_add(read) >= page.total
        {
            break page.total;
        }
    };
    if q.limit.is_none() && total > q.start.saturating_add(items.len() as i64) {
        tracing::info!(
            target: "plurxd::http::jellyfin",
            total,
            returned = items.len(),
            "Jellyfin catalog response stopped at its row bound; the client must page"
        );
    }
    Ok(wire::Items {
        items,
        total_record_count: total.max(0) as u64,
        start_index: q.start as u64,
    })
}

/// The most rows one store read returns (the store's own bound).
const STORE_PAGE_ROWS: i64 = 500;
/// The most rows one facade response carries, however large `Limit` is.
const RESPONSE_ROWS: i64 = 10_000;

/// One store read, with any Jellyfin identities it lacks allocated first.
async fn catalog_page(
    state: &AppState,
    query: &JellyfinCatalogQuery,
) -> Result<plurx_core::store::JellyfinCatalogPage, ApiError> {
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
            return Ok(page);
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
) -> Result<Response, ApiError> {
    let q = browse(raw.as_deref())?;
    // `IncludeItemTypes=CollectionFolder` alone is how a client lists the
    // user's libraries through `/Items`; those are the mapped views.
    if q.views && q.kinds.is_empty() && q.parent.is_none() {
        if let Some(user) = &q.user {
            check_user(&client, &user.to_hex())?;
        }
        return Ok(current_views(client, State(state)).await?.into_response());
    }
    Ok(Json(catalog(&client, &state, q, None).await?).into_response())
}
async fn user_items(
    client: ClientUser,
    state: State<AppState>,
    Path(id): Path<String>,
    raw: RawQuery,
) -> Result<Response, ApiError> {
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
    q.limit = Some(1);
    q.recursive = true;
    // A single-item route names its item; Jellyfin ignores type filters here.
    (q.kinds, q.kinds_absent, q.views) = (vec![], false, false);
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
    (q.series, q.season) = (None, None);
    q.recursive = false;
    (q.kinds, q.kinds_absent, q.views) = (vec!["season".into()], false, false);
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
    q.series = None;
    q.recursive = true;
    (q.kinds, q.kinds_absent, q.views) = (vec!["episode".into()], false, false);
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
    // Jellyfin's GetLatestMedia default; a home row without Limit is not a
    // request for the whole catalogue.
    q.limit.get_or_insert(20);
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
        Ok(_) => {
            let mut response = not_found().await.into_response();
            response.extensions_mut().insert(FacadeOff);
            response
        }
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// Marks the switch-off 404, which `cache_policy` does not log: with the
/// facade off, a probe of its paths is not a compatibility gap.
#[derive(Clone, Copy, Debug)]
struct FacadeOff;

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
    let method = request.method().clone();
    let uri = request.uri().clone();
    let mut response = next.run(request).await;
    // A supported client sending something the facade cannot answer is the
    // exception this ring exists for: it is how a compatibility gap becomes
    // attributable without a packet capture. Successful requests and auth
    // challenges stay out; an artwork miss is an ordinary answer.
    let status = response.status();
    if matches!(status.as_u16(), 400 | 403 | 404 | 405)
        && response.extensions().get::<FacadeOff>().is_none()
        && !(status == StatusCode::NOT_FOUND && uri.path().contains("/Images/"))
    {
        if let Some(also_suppressed) = REFUSAL_LINES.admit(0) {
            let target = refused_request_target(&uri);
            tracing::warn!(
                target: "plurxd::http::jellyfin",
                status = status.as_u16(),
                %method,
                request = %target,
                also_suppressed,
                "Jellyfin client request refused"
            );
        }
    }
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

/// At most one refusal line per second, with a count of the ones skipped.
static REFUSAL_LINES: std::sync::LazyLock<super::AccessLineThrottle> =
    std::sync::LazyLock::new(super::AccessLineThrottle::default);

/// Query keys whose values are browse parameters, kept in the refusal line
/// because they say which part of the protocol is missing. Every other value
/// is replaced: `ApiKey`, `api_key`, the direct-play `tag` capability and any
/// key a client invents may carry a bearer.
const LOGGED_QUERY_VALUES: &[&str] = &[
    "startindex",
    "limit",
    "sortby",
    "sortorder",
    "includeitemtypes",
    "excludeitemtypes",
    "recursive",
    "parentid",
    "seriesid",
    "seasonid",
    "userid",
    "fields",
    "filters",
    "enableuserdata",
    "imagetypelimit",
    "enableimagetypes",
    "enabletotalrecordcount",
    "excludelocationtypes",
    "isfavorite",
    "isplayed",
    "ismissing",
    "mediatypes",
    "years",
    "genres",
    "namestartswith",
    "mediasourceid",
    "container",
    "static",
    "audiostreamindex",
    "subtitlestreamindex",
    "starttimeticks",
    "maxstreamingbitrate",
];

/// The refused request as a log line: the redacted path plus its query, with
/// only browse-parameter values kept (see [`LOGGED_QUERY_VALUES`]).
fn refused_request_target(uri: &Uri) -> String {
    let mut line = super::safe_trace_target(uri);
    if let Some(query) = uri.query() {
        let pairs = query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
                if value.is_empty() {
                    key.to_owned()
                } else if LOGGED_QUERY_VALUES.contains(&key.to_ascii_lowercase().as_str()) {
                    format!("{key}={value}")
                } else {
                    format!("{key}=[REDACTED]")
                }
            })
            .collect::<Vec<_>>();
        line.push('?');
        line.push_str(&pairs.join("&"));
    }
    if line.len() > 1024 {
        let mut cut = 1024;
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line.truncate(cut);
    }
    line
}

#[cfg(test)]
mod tests;
