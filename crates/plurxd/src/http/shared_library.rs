//! Closed source catalogue presentation. Internal records cannot be serialized.
use super::{error::ApiError, sharing};
use crate::state::{clock_ms, AppState};
use axum::{
    body::{to_bytes, Body},
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use plurx_core::{
    sharing::{secret_hash, GrantState, SecretDomain, SourceId},
    sharing_catalogue::*,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(crate) fn peer_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/sharing/v1/libraries", get(libraries))
        .route("/sharing/v1/libraries/{id}/items", get(items))
        .route("/sharing/v1/items/{id}", get(item))
        .route("/sharing/v1/items/{id}/children", get(children))
        .route("/sharing/v1/items:batch", post(batch))
        .route(
            "/sharing/v1/art/{resource}",
            get(super::shared_artwork::source),
        )
        .route_layer(middleware::from_fn_with_state(state, source_content_guard))
        .route("/sharing/v1/current-scope", post(current_scope))
}
// Control checks have no content body to monitor. Their own bounded admission
// and deadline remain independent of catalogue requests and body monitors.
static SCOPE_CONTROL: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(32)));
async fn current_scope(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<plurx_core::sharing_catalogue_details::SourceScopeResponse>, ApiError> {
    let _permit = SCOPE_CONTROL
        .clone()
        .try_acquire_owned()
        .map_err(|_| fail(StatusCode::TOO_MANY_REQUESTS, "sharing_control_capacity"))?;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        if !crate::sharing::enabled(state.store.as_ref())
            .await
            .map_err(unavailable)?
        {
            return Err(missing());
        }
        let (hash, grant) = authority(&state, &headers).await?;
        let bytes = to_bytes(body, 32 * 1024).await.map_err(|_| invalid())?;
        let request: plurx_core::sharing_catalogue_details::SourceScopeRequest =
            serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        request.validate().map_err(|_| invalid())?;
        let authorized = state
            .store
            .source_scope_authorized(&hash, grant, &request)
            .await
            .map_err(unavailable)?;
        // Re-check the real consistent roster after the guarded read. This
        // serving proof cannot authorize any replicated write.
        let _ = authority(&state, &headers).await?;
        Ok(Json(
            plurx_core::sharing_catalogue_details::SourceScopeResponse { authorized },
        ))
    })
    .await
    .map_err(|_| {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_control_unavailable",
        )
    })?
}
fn fail(status: StatusCode, code: &'static str) -> ApiError {
    ApiError::typed(status, code, code.replace('_', " "))
}
fn invalid() -> ApiError {
    fail(StatusCode::BAD_REQUEST, "sharing_invalid_request")
}
fn unavailable(_: plurx_core::error::StoreError) -> ApiError {
    fail(
        StatusCode::SERVICE_UNAVAILABLE,
        "sharing_authority_unavailable",
    )
}
fn missing() -> ApiError {
    fail(StatusCode::NOT_FOUND, "sharing_not_found")
}
fn source_id(value: &str) -> Result<SourceId, ApiError> {
    SourceId::parse(value).map_err(|_| invalid())
}
pub(super) async fn authority(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(String, uuid::Uuid), ApiError> {
    let secret = sharing::credential(headers)?;
    let hash = secret_hash(SecretDomain::Grant, &secret);
    let grant = state
        .store
        .sharing_grant_status(&hash)
        .await
        .map_err(unavailable)?
        .ok_or_else(missing)?;
    if grant.grant.state != GrantState::Active {
        return Err(missing());
    }
    // This is serving-read admission only. Future shared writes must carry
    // their capability floor and effective grant predicate atomically.
    if state.membership.is_replicated() {
        let ready = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            state.membership.sharing_member_floor_ready(
                plurx_core::cluster::membership::SharingMemberFloor::CatalogueItemIdentity,
            ),
        )
        .await
        .map_err(|_| {
            fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_member_floor_unavailable",
            )
        })?
        .map_err(|_| {
            fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_member_floor_unavailable",
            )
        })?;
        if !ready {
            return Err(fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_member_floor_unavailable",
            ));
        }
    }
    Ok((hash, grant.grant.id))
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowseQuery {
    #[serde(alias = "query")]
    q: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}
fn query(raw: Option<&str>) -> Result<BrowseQuery, ApiError> {
    let raw = raw.unwrap_or("");
    if raw.len() > 6144 {
        return Err(invalid());
    }
    // Reject lossy form decoding: malformed UTF-8 must never become a changed
    // filter whose digest happens to authenticate another cursor context.
    for field in raw.split('&') {
        let mut decoded = Vec::with_capacity(field.len());
        let mut bytes = field.bytes();
        while let Some(byte) = bytes.next() {
            if byte == b'%' {
                let a = bytes
                    .next()
                    .and_then(|b| char::from(b).to_digit(16))
                    .ok_or_else(invalid)?;
                let b = bytes
                    .next()
                    .and_then(|b| char::from(b).to_digit(16))
                    .ok_or_else(invalid)?;
                decoded.push((a * 16 + b) as u8);
            } else {
                decoded.push(byte);
            }
        }
        std::str::from_utf8(&decoded).map_err(|_| invalid())?;
    }
    let uri: axum::http::Uri = format!("/?{raw}").parse().map_err(|_| invalid())?;
    let axum::extract::Query(q) =
        axum::extract::Query::<BrowseQuery>::try_from_uri(&uri).map_err(|_| invalid())?;
    if q.q
        .as_ref()
        .is_some_and(|s| s.len() > 512 || s.chars().any(char::is_control))
        || q.cursor
            .as_ref()
            .is_some_and(|s| s.len() > MAX_CURSOR_BYTES)
        || q.limit.is_some_and(|n| !(1..=MAX_PAGE_SIZE).contains(&n))
    {
        return Err(invalid());
    }
    Ok(q)
}
async fn libraries(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (hash, grant) = authority(&state, &headers).await?;
    let libraries = state
        .store
        .source_catalogue_libraries(&hash, grant)
        .await
        .map_err(unavailable)?
        .ok_or_else(missing)?;
    let visible = libraries
        .iter()
        .map(|library| library.library_id.clone())
        .collect();
    source_json(
        &state,
        grant,
        json!({"libraries":libraries}),
        visible,
        Vec::new(),
    )
    .await
}
async fn items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    browse(
        &state,
        &headers,
        source_id(&id)?,
        None,
        query(raw.as_deref())?,
    )
    .await
}
async fn children(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let parent = source_id(&id)?;
    let (hash, grant) = authority(&state, &headers).await?;
    let record = state
        .store
        .source_catalogue_batch(
            &hash,
            grant,
            MetadataBatch {
                item_ids: vec![parent.clone()],
            },
        )
        .await
        .map_err(unavailable)?
        .and_then(|mut v| v.pop())
        .and_then(|v| v.record)
        .ok_or_else(missing)?;
    browse(
        &state,
        &headers,
        record.item.library_id,
        Some(parent),
        query(raw.as_deref())?,
    )
    .await
}
async fn browse(
    state: &AppState,
    headers: &HeaderMap,
    library_id: SourceId,
    parent_id: Option<SourceId>,
    query: BrowseQuery,
) -> Result<Response, ApiError> {
    let (hash, grant) = authority(state, headers).await?;
    let identity = state
        .store
        .sharing_identity(clock_ms())
        .await
        .map_err(unavailable)?;
    let search = query.q.unwrap_or_default();
    let filter = serde_json::to_vec(&(parent_id.as_ref(), &search, "binary-numeric-v1"))
        .map_err(|_| invalid())?;
    let context = CursorContext {
        server_id: identity.server_id,
        catalogue_epoch: identity.catalogue_epoch,
        grant_id: grant,
        library_id: library_id.clone(),
        filter_digest: hex::encode(Sha256::digest(filter)),
    };
    let signer = CatalogueCursor::new(&state.sharing.key);
    let boundary = query
        .cursor
        .as_deref()
        .map(|cursor| signer.resume(cursor, &context, clock_ms()))
        .transpose()
        .map_err(|e| match e {
            CatalogueError::Expired => fail(StatusCode::GONE, "sharing_cursor_expired"),
            CatalogueError::QueryChanged => fail(StatusCode::CONFLICT, "sharing_query_changed"),
            CatalogueError::Invalid => invalid(),
        })?;
    let page = state
        .store
        .source_catalogue_page(CataloguePageRequest {
            credential_hash: hash,
            grant_id: grant,
            library_id: library_id.clone(),
            parent_id,
            query: search,
            boundary,
            limit: query.limit.unwrap_or(DEFAULT_PAGE_SIZE),
        })
        .await
        .map_err(unavailable)?
        .ok_or_else(missing)?;
    let next_cursor = if page.has_more {
        page.records
            .last()
            .map(|last| {
                signer.issue(
                    context,
                    CatalogueBoundary {
                        sort_key: last.boundary_sort_key.clone(),
                        item_id: last.item.item_id.clone(),
                    },
                    page.counters.clone(),
                    clock_ms(),
                )
            })
            .transpose()
            .map_err(|_| invalid())?
    } else {
        None
    };
    let visible = page
        .records
        .iter()
        .map(|record| (record.item.library_id.clone(), record.item.item_id.clone()))
        .collect();
    let artwork_items = super::shared_artwork::source_records(state, grant, page.records).await?;
    source_json(state,grant,json!({"items":artwork_items,"next_cursor":next_cursor,"catalogue_revision":page.counters.library_revision,"scope_generation":page.counters.scope_generation,"catalogue_generation":page.counters.catalogue_generation}),vec![library_id],visible).await
}
async fn item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let (hash, grant) = authority(&state, &headers).await?;
    use plurx_core::{
        sharing_catalogue_details::{CatalogueRevisionKey, SourceItemDetails},
        store::sharing_catalogue_details::SourceDetailsRead,
    };
    let snapshot = match state
        .store
        .source_item_details_snapshot(&hash, grant, source_id(&id)?)
        .await
        .map_err(unavailable)?
    {
        SourceDetailsRead::Authorized(value) => value,
        SourceDetailsRead::Unavailable => return Err(missing()),
        SourceDetailsRead::Capacity => {
            return Err(fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_details_capacity",
            ))
        }
    };
    let envelope = state
        .store
        .source_catalogue_revision_key(snapshot.server, snapshot.epoch)
        .await
        .map_err(unavailable)?
        .ok_or_else(|| {
            fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_revision_key_unavailable",
            )
        })?;
    let key = CatalogueRevisionKey::open(
        &state.sharing.key,
        plurx_core::sharing::SharingIdentity {
            server_id: snapshot.server,
            catalogue_epoch: snapshot.epoch,
            created_at_ms: 0,
        },
        &envelope,
    )
    .map_err(unavailable)?;
    let files = snapshot
        .files
        .iter()
        .map(|f| f.playable_file(&key))
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    let tuples = files
        .iter()
        .map(|f| {
            (
                snapshot.record.item.library_id.clone(),
                snapshot.record.item.item_id.clone(),
                f.file_id.clone(),
            )
        })
        .collect();
    let visible = vec![(
        snapshot.record.item.library_id.clone(),
        snapshot.record.item.item_id.clone(),
    )];
    let item = super::shared_artwork::source_records(&state, grant, vec![snapshot.record])
        .await?
        .pop()
        .ok_or_else(missing)?;
    let details = SourceItemDetails { item, files };
    details.validate().map_err(unavailable)?;
    let mut response = source_json(
        &state,
        grant,
        serde_json::to_value(details).map_err(|_| invalid())?,
        Vec::new(),
        visible,
    )
    .await?;
    let authority = response
        .extensions_mut()
        .get_mut::<SourceContentAuthority>()
        .ok_or_else(invalid)?;
    if authority.server != snapshot.server || authority.epoch != snapshot.epoch {
        return Err(missing());
    }
    authority.files = tuples;
    Ok(response)
}
async fn batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let (hash, grant) = authority(&state, &headers).await?;
    let bytes = to_bytes(body, 16 * 1024).await.map_err(|_| invalid())?;
    let request: MetadataBatch = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let entries = state
        .store
        .source_catalogue_batch(&hash, grant, request)
        .await
        .map_err(unavailable)?
        .ok_or_else(missing)?;
    let visible = entries
        .iter()
        .filter_map(|entry| {
            entry
                .record
                .as_ref()
                .map(|record| (record.item.library_id.clone(), record.item.item_id.clone()))
        })
        .collect();
    let records = entries
        .iter()
        .filter_map(|entry| entry.record.clone())
        .collect();
    let artwork_items = super::shared_artwork::source_records(&state, grant, records).await?;
    let mut mapped = artwork_items
        .into_iter()
        .map(|item| (item.item_id.clone(), item))
        .collect::<std::collections::BTreeMap<_, _>>();
    source_json(&state,grant,json!({"items":entries.into_iter().map(|e|json!({"item_id":e.item_id,"item":mapped.remove(&e.item_id)})).collect::<Vec<_>>()}),Vec::new(),visible).await
}

#[derive(Clone)]
struct SourceContentAuthority {
    grant: uuid::Uuid,
    server: uuid::Uuid,
    epoch: uuid::Uuid,
    libraries: Vec<SourceId>,
    items: Vec<(SourceId, SourceId)>,
    files: Vec<(SourceId, SourceId, SourceId)>,
}
static CONTENT_MONITORS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(64)));
fn bounded_json(value: Value) -> Result<Response, ApiError> {
    struct BoundedJson(Vec<u8>);
    impl std::io::Write for BoundedJson {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self
                .0
                .len()
                .checked_add(bytes.len())
                .is_none_or(|n| n > 4 * 1024 * 1024)
            {
                return Err(std::io::Error::other("sharing response capacity"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut bytes = BoundedJson(Vec::new());
    if serde_json::to_writer(&mut bytes, &value).is_err() {
        return Err(fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_catalogue_response_unavailable",
        ));
    }
    let mut response = Response::new(Body::from(bytes.0));
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    Ok(response)
}
async fn source_json(
    state: &AppState,
    grant: uuid::Uuid,
    value: Value,
    libraries: Vec<SourceId>,
    items: Vec<(SourceId, SourceId)>,
) -> Result<Response, ApiError> {
    let identity = state
        .store
        .sharing_identity(clock_ms())
        .await
        .map_err(unavailable)?;
    let mut response = bounded_json(value)?;
    response.extensions_mut().insert(SourceContentAuthority {
        grant,
        server: identity.server_id,
        epoch: identity.catalogue_epoch,
        libraries,
        items,
        files: Vec::new(),
    });
    Ok(response)
}
pub(super) fn source_file_json(
    grant: uuid::Uuid,
    target: &super::hls::SourcePlaybackTarget,
    value: Value,
) -> Result<Response, ApiError> {
    let mut response = bounded_json(value)?;
    response.extensions_mut().insert(SourceContentAuthority {
        grant,
        server: target.server_id,
        epoch: target.catalogue_epoch,
        libraries: vec![target.library_id.clone()],
        items: vec![(target.library_id.clone(), target.item_id.clone())],
        files: vec![(
            target.library_id.clone(),
            target.item_id.clone(),
            target.file_id.clone(),
        )],
    });
    Ok(response)
}
async fn source_content_current(state: &AppState, authority: &SourceContentAuthority) -> bool {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        if !crate::sharing::enabled(state.store.as_ref()).await.ok()? {
            return Some(false);
        }
        if state.membership.is_replicated()
            && !state
                .membership
                .sharing_member_floor_ready(
                    plurx_core::cluster::membership::SharingMemberFloor::CatalogueItemIdentity,
                )
                .await
                .ok()?
        {
            return Some(false);
        }
        let items_current = state
            .store
            .source_content_authorized(
                authority.grant,
                authority.server,
                authority.epoch,
                &authority.libraries,
                &authority.items,
            )
            .await
            .ok()?;
        if !items_current {
            return Some(false);
        }
        if authority.files.is_empty() {
            return Some(true);
        }
        state
            .store
            .source_content_files_authorized(
                authority.grant,
                authority.server,
                authority.epoch,
                &authority.files,
            )
            .await
            .ok()
    })
    .await
        == Ok(Some(true))
}
pub(super) async fn source_art_scope_current(
    state: &AppState,
    reference: &plurx_core::sharing_artwork::SourceArtReference,
) -> bool {
    source_content_current(
        state,
        &SourceContentAuthority {
            grant: reference.grant_id,
            server: reference.server_id,
            epoch: reference.catalogue_epoch,
            libraries: vec![reference.library_id.clone()],
            items: vec![(reference.library_id.clone(), reference.item_id.clone())],
            files: Vec::new(),
        },
    )
    .await
}
pub(super) async fn source_content_guard(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let connection = request
        .extensions()
        .get::<crate::SharingConnectionCancellation>()
        .cloned();
    let response = next.run(request).await;
    if !response.status().is_success() {
        return response;
    }
    let Some(authority) = response
        .extensions()
        .get::<SourceContentAuthority>()
        .cloned()
    else {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_body_authority_unavailable",
        )
        .into_response();
    };
    let Some(connection) = connection else {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_transport_authority_unavailable",
        )
        .into_response();
    };
    let Ok(permit) = CONTENT_MONITORS.clone().try_acquire_owned() else {
        return fail(StatusCode::TOO_MANY_REQUESTS, "sharing_body_capacity").into_response();
    };
    if !source_content_current(&state, &authority).await {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_body_authority_unavailable",
        )
        .into_response();
    }
    let cancel = connection.0.clone();
    let art_lease = response
        .extensions()
        .get::<std::sync::Arc<super::shared_artwork::ArtBodyLease>>()
        .cloned();
    let monitored=connection.monitor(async move {
        let _permit=permit;
        let _art_body_owner=art_lease;
        let mut interval=tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                ()=cancel.cancelled()=>return,
                _=interval.tick()=>{},
            }
            tokio::select! {
                ()=cancel.cancelled()=>return,
                current=source_content_current(&state,&authority)=> {if !current {cancel.cancel();return;}},
            }
        }
    });
    if monitored.is_err() {
        connection.0.cancel();
        return fail(
            StatusCode::TOO_MANY_REQUESTS,
            "sharing_connection_authority_capacity",
        )
        .into_response();
    }
    response
}
pub(super) fn attach_source_art_authority(
    response: &mut Response,
    reference: &plurx_core::sharing_artwork::SourceArtReference,
) {
    response.extensions_mut().insert(SourceContentAuthority {
        grant: reference.grant_id,
        server: reference.server_id,
        epoch: reference.catalogue_epoch,
        libraries: vec![reference.library_id.clone()],
        items: vec![(reference.library_id.clone(), reference.item_id.clone())],
        files: Vec::new(),
    });
}

#[derive(Clone)]
pub(super) struct ReceiverSourceScope {
    scope: plurx_core::store::sharing_catalogue::ReceiverCatalogueScope,
    items: Vec<plurx_core::sharing_catalogue_details::SourceScopeItem>,
    files: Vec<plurx_core::sharing_catalogue_details::SourceScopeFile>,
}
#[derive(Clone, Copy)]
enum ReceiverAccess {
    Viewer,
    Administrator,
}
#[derive(Clone)]
struct ReceiverContentAuthority {
    access: ReceiverAccess,
    hash: String,
    user: i64,
    scopes: Vec<plurx_core::store::sharing_catalogue::ReceiverCatalogueScope>,
    sources: Vec<ReceiverSourceScope>,
}
pub(super) fn receiver_scope(
    summary: &plurx_core::sharing::ImportSummary,
    mut libraries: Vec<SourceId>,
    items: Vec<(SourceId, SourceId)>,
    files: Vec<(SourceId, SourceId, SourceId)>,
) -> Result<ReceiverSourceScope, ApiError> {
    libraries.sort();
    libraries.dedup();
    if libraries.is_empty() || libraries.len() > 64 || items.len() > 200 || files.len() > 64 {
        return Err(invalid());
    }
    Ok(ReceiverSourceScope {
        scope: receiver_import_scope(summary, libraries)?,
        items: items
            .into_iter()
            .map(
                |(library_id, item_id)| plurx_core::sharing_catalogue_details::SourceScopeItem {
                    library_id,
                    item_id,
                },
            )
            .collect(),
        files: files
            .into_iter()
            .map(|(library_id, item_id, file_id)| {
                plurx_core::sharing_catalogue_details::SourceScopeFile {
                    library_id,
                    item_id,
                    file_id,
                }
            })
            .collect(),
    })
}
fn receiver_import_scope(
    summary: &plurx_core::sharing::ImportSummary,
    libraries: Vec<SourceId>,
) -> Result<plurx_core::store::sharing_catalogue::ReceiverCatalogueScope, ApiError> {
    if libraries.len() > 64 {
        return Err(invalid());
    }
    Ok(
        plurx_core::store::sharing_catalogue::ReceiverCatalogueScope {
            import_id: summary.id,
            source_server_id: summary.source_server_id,
            catalogue_epoch: summary.catalogue_epoch,
            lifecycle_generation: summary.lifecycle_generation,
            assignment_generation: summary.assignment_generation,
            endpoint_generation: summary.endpoint_generation,
            claim_id: summary.claim_id,
            remote_grant_id: summary.remote_grant_id.ok_or_else(missing)?,
            libraries,
        },
    )
}
async fn receiver_login_current(
    state: &AppState,
    authority: &ReceiverContentAuthority,
) -> Option<bool> {
    match authority.access {
        ReceiverAccess::Viewer => state
            .store
            .receiver_catalogue_authorized(
                &authority.hash,
                authority.user,
                &authority.scopes,
                clock_ms() / 1000,
            )
            .await
            .ok(),
        ReceiverAccess::Administrator => {
            let [scope] = authority.scopes.as_slice() else {
                return Some(false);
            };
            state
                .store
                .receiver_admin_catalogue_authorized(
                    &authority.hash,
                    authority.user,
                    scope,
                    clock_ms() / 1000,
                )
                .await
                .ok()
        }
    }
}
async fn receiver_content_current(state: &AppState, authority: &ReceiverContentAuthority) -> bool {
    use futures_util::{stream, StreamExt};
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        if !crate::sharing::enabled(state.store.as_ref()).await.ok()? {
            return Some(false);
        }
        if !receiver_login_current(state, authority).await? {
            return Some(false);
        }
        let mut checks = stream::iter(authority.sources.clone().into_iter().map(|source| {
            let state = state.clone();
            async move {
                state
                    .sharing
                    .current_catalogue_scope(&state, &source.scope, source.items, source.files)
                    .await
            }
        }))
        .buffer_unordered(32);
        while let Some(current) = checks.next().await {
            if !current {
                return Some(false);
            }
        }
        // Local login/assignment may change during the remote checks.
        receiver_login_current(state, authority).await
    })
    .await
        == Ok(Some(true))
}
async fn receiver_json(
    state: &AppState,
    token: &str,
    user: i64,
    sources: Vec<ReceiverSourceScope>,
    value: Value,
) -> Result<Response, ApiError> {
    let scopes = sources.iter().map(|s| s.scope.clone()).collect();
    receiver_json_scoped(state, token, user, scopes, sources, value).await
}
pub(super) async fn receiver_file_json(
    state: &AppState,
    token: &str,
    user: i64,
    summary: &plurx_core::sharing::ImportSummary,
    reference: &plurx_core::sharing_file_locators::FileLocatorReference,
    value: Value,
) -> Result<Response, ApiError> {
    let source = receiver_scope(
        summary,
        vec![reference.item.library_id.clone()],
        vec![(
            reference.item.library_id.clone(),
            reference.item.item_id.clone(),
        )],
        vec![(
            reference.item.library_id.clone(),
            reference.item.item_id.clone(),
            reference.file_id.clone(),
        )],
    )?;
    receiver_json_scoped(
        state,
        token,
        user,
        vec![source.scope.clone()],
        vec![source],
        value,
    )
    .await
}
async fn receiver_json_scoped(
    state: &AppState,
    token: &str,
    user: i64,
    scopes: Vec<plurx_core::store::sharing_catalogue::ReceiverCatalogueScope>,
    sources: Vec<ReceiverSourceScope>,
    value: Value,
) -> Result<Response, ApiError> {
    receiver_json_authorized(
        state,
        token,
        user,
        scopes,
        sources,
        value,
        ReceiverAccess::Viewer,
    )
    .await
}
async fn receiver_json_authorized(
    state: &AppState,
    token: &str,
    user: i64,
    scopes: Vec<plurx_core::store::sharing_catalogue::ReceiverCatalogueScope>,
    sources: Vec<ReceiverSourceScope>,
    value: Value,
    access: ReceiverAccess,
) -> Result<Response, ApiError> {
    if scopes.len() > 32 || sources.len() > 32 {
        return Err(invalid());
    }
    let authority = ReceiverContentAuthority {
        access,
        hash: plurx_core::auth::hash_token(token),
        user,
        scopes,
        sources,
    };
    // This also fences history reads whose import changed during a network read.
    if !receiver_content_current(state, &authority).await {
        return Err(fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_body_authority_unavailable",
        ));
    }
    let mut response = bounded_json(value)?;
    response.extensions_mut().insert(authority);
    Ok(response)
}
pub(super) async fn attach_receiver_art_authority(
    state: &AppState,
    token: &str,
    user: i64,
    summary: &plurx_core::sharing::ImportSummary,
    reference: &plurx_core::sharing_artwork::ReceiverArtReference,
    response: &mut Response,
) -> Result<(), ApiError> {
    let source = receiver_scope(
        summary,
        vec![reference.item.library_id.clone()],
        vec![(
            reference.item.library_id.clone(),
            reference.item.item_id.clone(),
        )],
        vec![],
    )?;
    let authority = ReceiverContentAuthority {
        access: ReceiverAccess::Viewer,
        hash: plurx_core::auth::hash_token(token),
        user,
        scopes: vec![source.scope.clone()],
        sources: vec![source],
    };
    if !receiver_content_current(state, &authority).await {
        return Err(fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_body_authority_unavailable",
        ));
    }
    response.extensions_mut().insert(authority);
    Ok(())
}
static RECEIVER_MONITORS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(32)));
pub(super) async fn receiver_content_guard(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let connection = request
        .extensions()
        .get::<crate::SharingConnectionCancellation>()
        .cloned();
    let response = next.run(request).await;
    if !response.status().is_success() {
        return response;
    }
    let Some(authority) = response
        .extensions()
        .get::<ReceiverContentAuthority>()
        .cloned()
    else {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_body_authority_unavailable",
        )
        .into_response();
    };
    let Some(connection) = connection else {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_transport_authority_unavailable",
        )
        .into_response();
    };
    let Ok(permit) = RECEIVER_MONITORS.clone().try_acquire_owned() else {
        return fail(StatusCode::TOO_MANY_REQUESTS, "sharing_body_capacity").into_response();
    };
    if !receiver_content_current(&state, &authority).await {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_body_authority_unavailable",
        )
        .into_response();
    }
    let art_lease = response
        .extensions()
        .get::<std::sync::Arc<super::shared_artwork::ArtBodyLease>>()
        .cloned();
    let cancel = connection.0.clone();
    if connection.monitor(async move{
        let _permit=permit;
        let _art_body_owner=art_lease;
        let mut interval=tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select!{()=cancel.cancelled()=>return,_=interval.tick()=>{}}
            tokio::select!{()=cancel.cancelled()=>return,current=receiver_content_current(&state,&authority)=>{if !current{cancel.cancel();return;}}}
        }
    }).is_err(){connection.0.cancel();return fail(StatusCode::TOO_MANY_REQUESTS,"sharing_connection_authority_capacity").into_response();}
    response
}

pub(crate) fn admin_library_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/sharing/imports/{import}/libraries", get(admin_libraries))
        .route_layer(middleware::from_fn_with_state(
            state,
            receiver_content_guard,
        ))
}
async fn admin_libraries(
    State(state): State<AppState>,
    super::extract::AdminUser(user): super::extract::AdminUser,
    super::extract::RawToken(token): super::extract::RawToken,
    Path(import): Path<String>,
) -> Result<Response, ApiError> {
    let hash = plurx_core::auth::hash_token(&token);
    let (summary, libraries) = state
        .sharing
        .read_admin_libraries(&state, import_id(&import)?, user.id, &hash)
        .await
        .map_err(peer_failure)?;
    let scope = receiver_import_scope(
        &summary,
        libraries
            .iter()
            .map(|library| library.library_id.clone())
            .collect(),
    )?;
    let source = ReceiverSourceScope {
        scope: scope.clone(),
        items: vec![],
        files: vec![],
    };
    receiver_json_authorized(&state, &token, user.id, vec![scope], vec![source],
        json!({"import_id":summary.id,"server_id":summary.source_server_id,"catalogue_epoch":summary.catalogue_epoch,"lifecycle_generation":summary.lifecycle_generation,"expected_assignment_generation":summary.assignment_generation,"state":summary.state,"libraries":libraries}),
        ReceiverAccess::Administrator).await
}
pub(crate) fn viewer_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/shared/imports/{import}/art/{resource}",
            get(super::shared_artwork::receiver),
        )
        .route("/shared/libraries", get(viewer_assigned_libraries))
        .route("/shared/continue-watching", get(viewer_continue_groups))
        .route(
            "/shared/imports/{import}/continue-watching",
            get(viewer_continue_import),
        )
        .route("/shared/imports/{import}/libraries", get(viewer_libraries))
        .route(
            "/shared/imports/{import}/libraries/{library}/items",
            get(viewer_items),
        )
        .route(
            "/shared/imports/{import}/items/{item}/children",
            get(viewer_children),
        )
        .route("/shared/imports/{import}/items/{item}", get(viewer_item))
        .route(
            "/shared/imports/{import}/items/{item}/progress",
            post(viewer_progress),
        )
        .route("/shared/imports/{import}/items:batch", post(viewer_batch))
        .route_layer(middleware::from_fn_with_state(
            state,
            receiver_content_guard,
        ))
}
fn import_id(value: &str) -> Result<uuid::Uuid, ApiError> {
    let id = uuid::Uuid::parse_str(value).map_err(|_| invalid())?;
    if id.to_string() != value {
        return Err(invalid());
    }
    Ok(id)
}
fn peer_failure(error: crate::sharing_client::PeerError) -> ApiError {
    if matches!(error, crate::sharing_client::PeerError::Authentication) {
        missing()
    } else if matches!(
        error,
        crate::sharing_client::PeerError::Rejected(StatusCode::TOO_MANY_REQUESTS)
    ) {
        fail(StatusCode::TOO_MANY_REQUESTS, "sharing_metadata_capacity")
    } else {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_source_unavailable",
        )
    }
}
fn shared_item(
    summary: &plurx_core::sharing::ImportSummary,
    item: SourceCatalogueItem,
    user: i64,
    art_key: Option<&plurx_core::sharing_file_locators::FileLocatorKey>,
) -> Value {
    let reference = SharedReference {
        import_id: summary.id,
        server_id: summary.source_server_id,
        catalogue_epoch: summary.catalogue_epoch,
        library_id: item.library_id.clone(),
        item_id: item.item_id.clone(),
    };
    let parent = item.parent_id.clone().map(|id| SharedReference {
        item_id: id,
        ..reference.clone()
    });
    let mut art = Vec::new();
    if let Some(key) = art_key {
        for entry in item.art {
            let context = plurx_core::sharing_artwork::ReceiverArtReference {
                item: reference.clone(),
                user_id: user,
                lifecycle_generation: summary.lifecycle_generation,
                source: entry.resource,
            };
            if let Ok(resource) = key.issue_art(&context, clock_ms()) {
                art.push(json!({"kind":entry.kind,"variant":entry.variant,"url":resource.url()}));
            }
        }
    }
    let poster = art
        .iter()
        .find(|a| a["kind"] == "poster" && a["variant"] == "w300")
        .map(|a| a["url"].clone());
    let backdrop = art
        .iter()
        .find(|a| a["kind"] == "backdrop" && a["variant"] == "w780")
        .map(|a| a["url"].clone());
    let mut value = json!({"source":"shared","reference":reference,"parent":parent,"title":item.title,"sort_title":item.sort_title,"kind":item.kind,"year":item.year,"overview":item.overview,"genres":item.genres,"season_number":item.season_number,"episode_number":item.episode_number});
    if !art.is_empty() {
        value["art"] = json!(art);
    }
    if let Some(poster) = poster {
        value["poster_url"] = poster;
    }
    if let Some(backdrop) = backdrop {
        value["backdrop_url"] = backdrop;
    }
    value
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinueQuery {
    limit: Option<usize>,
}
fn continue_limit(raw: Option<&str>) -> Result<usize, ApiError> {
    let raw = raw.unwrap_or("");
    if raw.len() > 128 {
        return Err(invalid());
    }
    let uri: axum::http::Uri = format!("/?{raw}").parse().map_err(|_| invalid())?;
    let axum::extract::Query(query) =
        axum::extract::Query::<ContinueQuery>::try_from_uri(&uri).map_err(|_| invalid())?;
    let limit = query.limit.unwrap_or(200);
    if !(1..=200).contains(&limit) {
        return Err(invalid());
    }
    Ok(limit)
}
async fn viewer_continue_groups(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    super::extract::RawToken(token): super::extract::RawToken,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let groups = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        state
            .store
            .remote_continue_watch_groups(user.id, continue_limit(raw.as_deref())?),
    )
    .await
    .map_err(|_| {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_history_unavailable",
        )
    })?
    .map_err(unavailable)?;
    let scopes = groups.iter().map(|g| g.scope.clone()).collect();
    let groups=groups.into_iter().map(|g|json!({"import_id":g.scope.import_id,"server_id":g.scope.source_server_id,"catalogue_epoch":g.scope.catalogue_epoch,"source_name":g.source_name,"count":g.items.len()})).collect::<Vec<_>>();
    receiver_json_scoped(
        &state,
        &token,
        user.id,
        scopes,
        Vec::new(),
        json!({"groups":groups}),
    )
    .await
}
async fn viewer_continue_import(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    super::extract::RawToken(token): super::extract::RawToken,
    Path(import): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let art_key = super::shared_artwork::receiver_key(&state).await?;
    let import = import_id(&import)?;
    let groups = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        state
            .store
            .remote_continue_watch_groups(user.id, continue_limit(raw.as_deref())?),
    )
    .await
    .map_err(|_| {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_history_unavailable",
        )
    })?
    .map_err(unavailable)?;
    let group = groups
        .into_iter()
        .find(|g| g.scope.import_id == import)
        .ok_or_else(missing)?;
    let mut sources = Vec::new();
    let mut items = Vec::new();
    let availability = match state
        .sharing
        .read_catalogue(
            &state,
            import,
            user.id,
            crate::sharing::CatalogueRead::Batch(MetadataBatch {
                item_ids: group.items.iter().map(|i| i.item_id.clone()).collect(),
            }),
        )
        .await
    {
        Ok((summary, crate::sharing::CatalogueReply::Batch(batch))) => {
            if summary.source_server_id != group.scope.source_server_id
                || summary.catalogue_epoch != group.scope.catalogue_epoch
                || summary.lifecycle_generation != group.scope.lifecycle_generation
                || summary.claim_id != group.scope.claim_id
                || summary.remote_grant_id != Some(group.scope.remote_grant_id)
            {
                return Err(fail(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "sharing_history_authority_unavailable",
                ));
            }
            let mut tuples = Vec::new();
            for entry in batch.items {
                let Some(item) = entry.item else {
                    continue;
                };
                let Some(history) = group
                    .items
                    .iter()
                    .find(|h| h.item_id == item.item_id && h.library_id == item.library_id)
                else {
                    continue;
                };
                // A fresh changed library is not an implicit history move.
                tuples.push((item.library_id.clone(), item.item_id.clone()));
                items.push(json!({"item":shared_item(&summary,item,user.id,art_key.as_ref()),"watch":history.watch}));
            }
            if !tuples.is_empty() {
                sources.push(receiver_scope(
                    &summary,
                    tuples.iter().map(|t| t.0.clone()).collect(),
                    tuples,
                    Vec::new(),
                )?);
            }
            "online"
        }
        Err(crate::sharing_client::PeerError::Rejected(StatusCode::TOO_MANY_REQUESTS)) => "busy",
        Err(_) => "unavailable",
        _ => return Err(invalid()),
    };
    receiver_json_scoped(&state,&token,user.id,vec![group.scope.clone()],sources,json!({"import_id":group.scope.import_id,"server_id":group.scope.source_server_id,"catalogue_epoch":group.scope.catalogue_epoch,"source_name":group.source_name,"count":group.items.len(),"availability":availability,"items":items})).await
}

async fn viewer_libraries(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    super::extract::RawToken(token): super::extract::RawToken,
    Path(import): Path<String>,
) -> Result<Response, ApiError> {
    let (summary, reply) = state
        .sharing
        .read_catalogue(
            &state,
            import_id(&import)?,
            user.id,
            crate::sharing::CatalogueRead::Libraries,
        )
        .await
        .map_err(peer_failure)?;
    let crate::sharing::CatalogueReply::Libraries(libraries) = reply else {
        return Err(invalid());
    };
    let scope = receiver_scope(
        &summary,
        libraries.iter().map(|l| l.library_id.clone()).collect(),
        Vec::new(),
        Vec::new(),
    )?;
    receiver_json(&state,&token,user.id,vec![scope],
        json!({"import_id":summary.id,"server_id":summary.source_server_id,"catalogue_epoch":summary.catalogue_epoch,"libraries":libraries})).await
}
async fn viewer_items(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    super::extract::RawToken(token): super::extract::RawToken,
    Path((import, library)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    viewer_page(
        &state,
        &token,
        user.id,
        import_id(&import)?,
        source_id(&library)?,
        None,
        query(raw.as_deref())?,
    )
    .await
}
async fn viewer_children(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    super::extract::RawToken(token): super::extract::RawToken,
    Path((import, item)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    let import = import_id(&import)?;
    let item = source_id(&item)?;
    let query = query(raw.as_deref())?;
    let (_, metadata) = current_viewer_item(&state, user.id, import, item.clone()).await?;
    viewer_page(
        &state,
        &token,
        user.id,
        import,
        metadata.library_id,
        Some(item),
        query,
    )
    .await
}
async fn viewer_page(
    state: &AppState,
    token: &str,
    user: i64,
    import: uuid::Uuid,
    library: SourceId,
    parent: Option<SourceId>,
    q: BrowseQuery,
) -> Result<Response, ApiError> {
    let art_key = super::shared_artwork::receiver_key(state).await?;
    let scope_library = library.clone();
    let scope_parent = parent.clone();
    let (summary, reply) = state
        .sharing
        .read_catalogue(
            state,
            import,
            user,
            crate::sharing::CatalogueRead::Page {
                library,
                parent,
                q: q.q.unwrap_or_default(),
                cursor: q.cursor,
                limit: q
                    .limit
                    .unwrap_or(DEFAULT_PAGE_SIZE)
                    .min(if scope_parent.is_some() { 199 } else { 200 }),
            },
        )
        .await
        .map_err(peer_failure)?;
    let crate::sharing::CatalogueReply::Page(page) = reply else {
        return Err(invalid());
    };
    let mut tuples = page
        .items
        .iter()
        .map(|i| (i.library_id.clone(), i.item_id.clone()))
        .collect::<Vec<_>>();
    if let Some(parent) = scope_parent {
        tuples.push((scope_library.clone(), parent));
    }
    let scope = receiver_scope(&summary, vec![scope_library], tuples, Vec::new())?;
    receiver_json(state,token,user,vec![scope],
        json!({"items":page.items.into_iter().map(|item|shared_item(&summary,item,user,art_key.as_ref())).collect::<Vec<_>>(),"next_cursor":page.next_cursor,"catalogue_revision":page.catalogue_revision,"scope_generation":page.scope_generation,"catalogue_generation":page.catalogue_generation})).await
}
async fn viewer_batch(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    super::extract::RawToken(token): super::extract::RawToken,
    Path(import): Path<String>,
    body: Body,
) -> Result<Response, ApiError> {
    let art_key = super::shared_artwork::receiver_key(&state).await?;
    let bytes = to_bytes(body, 16 * 1024).await.map_err(|_| invalid())?;
    let batch: MetadataBatch = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    batch.validate().map_err(|_| invalid())?;
    let (summary, reply) = state
        .sharing
        .read_catalogue(
            &state,
            import_id(&import)?,
            user.id,
            crate::sharing::CatalogueRead::Batch(batch),
        )
        .await
        .map_err(peer_failure)?;
    let crate::sharing::CatalogueReply::Batch(batch) = reply else {
        return Err(invalid());
    };
    let tuples = batch
        .items
        .iter()
        .filter_map(|e| e.item.as_ref())
        .map(|i| (i.library_id.clone(), i.item_id.clone()))
        .collect::<Vec<_>>();
    // A batch containing only absent items still needs a current assigned scope.
    let libraries = if tuples.is_empty() {
        state
            .store
            .assigned_catalogue_libraries(
                summary.id,
                user.id,
                summary.lifecycle_generation,
                summary.assignment_generation,
            )
            .await
            .map_err(unavailable)?
    } else {
        tuples.iter().map(|t| t.0.clone()).collect()
    };
    let scope = receiver_scope(&summary, libraries, tuples, Vec::new())?;
    receiver_json(&state,&token,user.id,vec![scope],
        json!({"items":batch.items.into_iter().map(|entry|json!({"item_id":entry.item_id,"item":entry.item.map(|item|shared_item(&summary,item,user.id,art_key.as_ref()))})).collect::<Vec<_>>()})).await
}

async fn current_viewer_item(
    state: &AppState,
    user: i64,
    import: uuid::Uuid,
    item: SourceId,
) -> Result<(plurx_core::sharing::ImportSummary, SourceCatalogueItem), ApiError> {
    let (summary, reply) = state
        .sharing
        .read_catalogue(
            state,
            import,
            user,
            crate::sharing::CatalogueRead::Batch(MetadataBatch {
                item_ids: vec![item.clone()],
            }),
        )
        .await
        .map_err(peer_failure)?;
    let crate::sharing::CatalogueReply::Batch(mut batch) = reply else {
        return Err(invalid());
    };
    let metadata = batch
        .items
        .pop()
        .and_then(|entry| entry.item)
        .ok_or_else(missing)?;
    if metadata.item_id != item {
        return Err(missing());
    }
    Ok((summary, metadata))
}
/// Receiver-generated file alias. Source metadata never chooses a B URL or
/// lifecycle; the signed locator identifies a tuple and grants no authority.
fn receiver_file_metadata(
    summary: &plurx_core::sharing::ImportSummary,
    reference: SharedReference,
    file: plurx_core::sharing_catalogue_details::SourcePlayableFile,
    key: Option<&plurx_core::sharing_file_locators::FileLocatorKey>,
) -> Result<Value, ApiError> {
    if reference.import_id != summary.id
        || reference.server_id != summary.source_server_id
        || reference.catalogue_epoch != summary.catalogue_epoch
        || summary.lifecycle_generation <= 0
    {
        return Err(invalid());
    }
    let locator_reference = plurx_core::sharing_file_locators::FileLocatorReference {
        item: reference,
        lifecycle_generation: summary.lifecycle_generation,
        file_id: file.file_id.clone(),
        revision: file.revision.clone(),
    };
    let mut value = serde_json::to_value(&file).map_err(|_| invalid())?;
    value["reference"] = json!({
        "item":locator_reference.item,
        "file_id":locator_reference.file_id,
        "revision":locator_reference.revision,
        "lifecycle_generation":summary.lifecycle_generation,
    });
    if let Some(key) = key {
        let locator = key.issue(&locator_reference).map_err(|_| {
            fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_authority_unavailable",
            )
        })?;
        value["file_base"] = json!(locator.file_base());
    }
    Ok(value)
}

async fn viewer_item(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    super::extract::RawToken(token): super::extract::RawToken,
    Path((import, item)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let art_key = super::shared_artwork::receiver_key(&state).await?;
    let import = import_id(&import)?;
    let item = source_id(&item)?;
    let (summary, reply) = state
        .sharing
        .read_catalogue(
            &state,
            import,
            user.id,
            crate::sharing::CatalogueRead::Item(item.clone()),
        )
        .await
        .map_err(peer_failure)?;
    let crate::sharing::CatalogueReply::Item(details) = reply else {
        return Err(invalid());
    };
    let scope = receiver_scope(
        &summary,
        vec![details.item.library_id.clone()],
        vec![(
            details.item.library_id.clone(),
            details.item.item_id.clone(),
        )],
        details
            .files
            .iter()
            .map(|f| {
                (
                    details.item.library_id.clone(),
                    details.item.item_id.clone(),
                    f.file_id.clone(),
                )
            })
            .collect(),
    )?;
    let metadata = details.item;
    let files = details
        .files
        .into_iter()
        .map(|file| {
            let reference = SharedReference {
                import_id: summary.id,
                server_id: summary.source_server_id,
                catalogue_epoch: summary.catalogue_epoch,
                library_id: metadata.library_id.clone(),
                item_id: metadata.item_id.clone(),
            };
            receiver_file_metadata(&summary, reference, file, art_key.as_ref())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let progress = state
        .store
        .remote_watch(import, metadata.library_id.clone(), item, user.id)
        .await
        .map_err(unavailable)?;
    receiver_json(&state,&token,user.id,vec![scope],
        json!({"item":shared_item(&summary,metadata,user.id,art_key.as_ref()),"files":files,"lifecycle_generation":summary.lifecycle_generation,"watch":progress,"delivery_status":"unavailable"})).await
}
async fn viewer_progress(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    Path((import, item)): Path<(String, String)>,
    body: Body,
) -> Result<Json<Value>, ApiError> {
    let bytes = to_bytes(body, 1024).await.map_err(|_| invalid())?;
    let _: plurx_core::store::sharing_catalogue::RemoteWatch =
        serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    current_viewer_item(&state, user.id, import_id(&import)?, source_id(&item)?).await?;
    Err(fail(
        StatusCode::SERVICE_UNAVAILABLE,
        "sharing_progress_session_binding_unavailable",
    ))
}
async fn viewer_assigned_libraries(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    super::extract::RawToken(token): super::extract::RawToken,
) -> Result<Response, ApiError> {
    let imports = state.store.sharing_imports().await.map_err(unavailable)?;
    let mut libraries = Vec::new();
    let mut scopes = Vec::new();
    for import in imports.into_iter().filter(|i| i.state == "active") {
        let assigned = state
            .store
            .assigned_catalogue_libraries(
                import.id,
                user.id,
                import.lifecycle_generation,
                import.assignment_generation,
            )
            .await
            .map_err(unavailable)?;
        if !assigned.is_empty() {
            scopes.push(receiver_scope(
                &import,
                assigned.clone(),
                Vec::new(),
                Vec::new(),
            )?);
        }
        for library in assigned {
            libraries.push(json!({"import_id":import.id,"server_id":import.source_server_id,"catalogue_epoch":import.catalogue_epoch,"library_id":library,"source_name":import.source_name,"availability":"unverified"}));
        }
    }
    receiver_json(
        &state,
        &token,
        user.id,
        scopes,
        json!({"libraries":libraries}),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use plurx_core::{
        domain::{LibraryKind, NewLibrary},
        sharing::*,
    };
    use tower::ServiceExt;

    static BODY_FIXTURES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    use bytes::Bytes;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    struct PayloadOwner {
        bytes: Vec<u8>,
        dropped: Arc<AtomicBool>,
    }
    impl AsRef<[u8]> for PayloadOwner {
        fn as_ref(&self) -> &[u8] {
            &self.bytes
        }
    }
    impl Drop for PayloadOwner {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }
    struct TrackedBody {
        inner: Body,
        body_dropped: Arc<AtomicBool>,
        data_dropped: Arc<AtomicBool>,
    }
    impl Drop for TrackedBody {
        fn drop(&mut self) {
            self.body_dropped.store(true, Ordering::SeqCst);
        }
    }
    impl http_body::Body for TrackedBody {
        type Data = Bytes;
        type Error = axum::Error;
        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
            let flag = self.data_dropped.clone();
            std::pin::Pin::new(&mut self.inner)
                .poll_frame(cx)
                .map(|frame| {
                    frame.map(|frame| {
                        frame.map(|frame| {
                            frame.map_data(|data| {
                                Bytes::from_owner(PayloadOwner {
                                    bytes: data.to_vec(),
                                    dropped: flag,
                                })
                            })
                        })
                    })
                })
        }
        fn is_end_stream(&self) -> bool {
            self.inner.is_end_stream()
        }
        fn size_hint(&self) -> http_body::SizeHint {
            self.inner.size_hint()
        }
    }
    struct BodyFixture {
        _directory: tempfile::TempDir,
        state: AppState,
        secret: plurx_core::secrets::Secret,
        grant: uuid::Uuid,
        library: i64,
        private: i64,
        item: i64,
        path: std::path::PathBuf,
    }
    async fn body_fixture() -> BodyFixture {
        use plurx_core::{
            domain::{ItemKind, MetadataPatch, NewItem},
            store::SqliteStore,
        };
        let directory = tempfile::tempdir().expect("source fixture directory");
        let path = directory.path().join("source.sqlite");
        let store = SqliteStore::open(&path).expect("source fixture store");
        let (_, mut state) = super::super::tests::test_app_with_state();
        state.store = Arc::new(store);
        state
            .store
            .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
            .await
            .expect("enable source fixture");
        state
            .store
            .sharing_identity(1000)
            .await
            .expect("source identity");
        let mut libraries = Vec::new();
        for name in ["Shared", "Private"] {
            libraries.push(
                state
                    .store
                    .create_library(&NewLibrary {
                        name: name.into(),
                        kind: LibraryKind::Movies,
                        paths: vec![directory.path().join(name)],
                        anime: false,
                    })
                    .await
                    .expect("source library")
                    .id,
            );
        }
        let connection = rusqlite::Connection::open(&path).expect("candidate fixture writer");
        connection
            .execute_batch(
                plurx_core::store::sharing_catalogue_source::CANDIDATE_ITEM_IDENTITY_SCHEMA,
            )
            .expect("candidate item identity");
        connection
            .execute_batch(plurx_core::store::sharing_catalogue_source::CANDIDATE_SCHEMA)
            .expect("candidate catalogue");
        drop(connection);
        let mut first = 0;
        for index in 0..200 {
            let item = state
                .store
                .insert_item(&NewItem {
                    library_id: libraries[0],
                    kind: ItemKind::Movie,
                    parent_id: None,
                    title: format!("Fixture {index:03}"),
                    year: None,
                    season_number: None,
                    episode_number: None,
                })
                .await
                .expect("source item");
            state
                .store
                .apply_metadata(
                    item,
                    &MetadataPatch {
                        overview: Some("x".repeat(8000)),
                        ..Default::default()
                    },
                )
                .await
                .expect("bounded large metadata");
            if index == 0 {
                first = item;
            }
        }
        let invitation = uuid::Uuid::new_v4();
        let grant = uuid::Uuid::new_v4();
        let secret = new_secret().expect("synthetic grant secret");
        state
            .store
            .create_share_invitation(InvitationRecord {
                id: invitation,
                token_hash: "a".repeat(64),
                library_ids: vec![libraries[0]],
                created_at_ms: 1000,
                expires_at_ms: 2000,
            })
            .await
            .expect("invitation");
        state
            .store
            .claim_share(ShareClaim {
                invitation_id: invitation,
                invitation_hash: "a".repeat(64),
                claim_id: uuid::Uuid::new_v4(),
                grant_id: grant,
                recipient_server_id: uuid::Uuid::new_v4(),
                recipient_name: "Synthetic recipient".into(),
                credential_hash: secret_hash(SecretDomain::Grant, &secret),
                now_ms: 1001,
            })
            .await
            .expect("grant");
        state
            .store
            .approve_share(grant, 1, 1002)
            .await
            .expect("approval");
        BodyFixture {
            _directory: directory,
            state,
            secret,
            grant,
            library: libraries[0],
            private: libraries[1],
            item: first,
            path,
        }
    }
    async fn body_server(
        fixture: &BodyFixture,
        body_dropped: Arc<AtomicBool>,
        data_dropped: Arc<AtomicBool>,
    ) -> (
        std::net::SocketAddr,
        tokio::sync::oneshot::Receiver<crate::SharingConnectionCancellation>,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<anyhow::Result<()>>,
    ) {
        let app = sharing::peer_router(fixture.state.clone()).route(
            "/ordinary/pending",
            get(|| async {
                Response::new(Body::from_stream(futures_util::stream::pending::<
                    Result<Bytes, std::convert::Infallible>,
                >()))
            }),
        );
        body_server_app(app, body_dropped, data_dropped).await
    }
    fn fixture_tracks_shared_body(path: &str) -> bool {
        if path.starts_with("/sharing/v1/art/")
            || (path.contains("/shared/imports/") && path.contains("/art/"))
        {
            return true;
        }
        path.ends_with("/items")
            || path.starts_with("/sharing/v1/items/")
            || (path.contains("/shared/imports/")
                && (path.contains("/items/") || path.ends_with("/continue-watching")))
            || (path.contains("/sharing/imports/") && path.ends_with("/libraries"))
    }

    #[test]
    fn sharing_body_fixture_tracks_continue_watching_and_item_responses() {
        for path in [
            "/api/v1/shared/imports/fixture/continue-watching",
            "/api/v1/sharing/imports/fixture/libraries",
            "/api/v1/shared/imports/fixture/items/7",
            "/api/v1/shared/imports/fixture/libraries/7/items",
            "/sharing/v1/items/7",
        ] {
            assert!(fixture_tracks_shared_body(path), "{path}");
        }
        for path in ["/ordinary/pending", "/api/v1/shared/continue-watching"] {
            assert!(!fixture_tracks_shared_body(path), "{path}");
        }
    }

    async fn body_server_app(
        app: Router,
        body_dropped: Arc<AtomicBool>,
        data_dropped: Arc<AtomicBool>,
    ) -> (
        std::net::SocketAddr,
        tokio::sync::oneshot::Receiver<crate::SharingConnectionCancellation>,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<anyhow::Result<()>>,
    ) {
        let socket = tokio::net::TcpSocket::new_v4().expect("fixture socket");
        socket
            .set_send_buffer_size(4096)
            .expect("bounded send buffer");
        socket
            .bind("127.0.0.1:0".parse().expect("loopback address"))
            .expect("bind");
        let listener = socket.listen(8).expect("listen");
        let address = listener.local_addr().expect("address");
        let (accepted, captured) = tokio::sync::oneshot::channel();
        let accepted = Arc::new(Mutex::new(Some(accepted)));
        let app = app.layer(middleware::from_fn(
            move |request: Request<Body>, next: Next| {
                let accepted = accepted.clone();
                let body_dropped = body_dropped.clone();
                let data_dropped = data_dropped.clone();
                async move {
                    if let Some(sender) = accepted.lock().expect("capture owner").take() {
                        sender
                            .send(
                                request
                                    .extensions()
                                    .get::<crate::SharingConnectionCancellation>()
                                    .expect("accepted connection seam")
                                    .clone(),
                            )
                            .ok();
                    }
                    let tracked = fixture_tracks_shared_body(request.uri().path());
                    let response = next.run(request).await;
                    if tracked {
                        let (parts, body) = response.into_parts();
                        Response::from_parts(
                            parts,
                            Body::new(TrackedBody {
                                inner: body,
                                body_dropped,
                                data_dropped,
                            }),
                        )
                    } else {
                        response
                    }
                }
            },
        ));
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let served = tokio::spawn(crate::serve_http(
            listener,
            app,
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        (address, captured, stop, served)
    }
    async fn await_flag(flag: &AtomicBool, expected: bool) {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while flag.load(Ordering::SeqCst) != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bounded fixture observation");
    }

    struct ActiveMonitor(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for ActiveMonitor {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    #[test]
    fn sharing_receiver_file_alias_binds_current_lifecycle_and_complete_lossless_reference() {
        use plurx_core::{
            secrets::CredentialKey,
            sharing::{ImportSummary, SharingIdentity},
            sharing_file_locators::FileLocatorKey,
        };
        let summary = ImportSummary {
            id: uuid::Uuid::new_v4(),
            source_server_id: uuid::Uuid::new_v4(),
            catalogue_epoch: uuid::Uuid::new_v4(),
            source_name: "Source".into(),
            claim_id: uuid::Uuid::new_v4(),
            remote_grant_id: Some(uuid::Uuid::new_v4()),
            state: "active".into(),
            assignment_generation: 1,
            lifecycle_generation: i64::MAX,
            endpoint_generation: 1,
            observed_endpoint_revision: None,
            endpoints: vec![],
        };
        let reference = SharedReference {
            import_id: summary.id,
            server_id: summary.source_server_id,
            catalogue_epoch: summary.catalogue_epoch,
            library_id: source_id("0").expect("zero library"),
            item_id: source_id("9223372036854775807").expect("lossless item"),
        };
        // A closed metadata DTO fixture; this test allocates no media/producer.
        let file: plurx_core::sharing_catalogue_details::SourcePlayableFile = serde_json::from_value(json!({"file_id":"0","revision":"a".repeat(64),"size":"23","dolby_vision":{},"audio_offset_ms":0,"probed":true,"audio_streams":[],"subtitle_streams":[],"chapters":[],"skip_regions":[]})).expect("closed Source metadata");
        let master = CredentialKey::from_bytes([59; 32]);
        let receiver = SharingIdentity {
            server_id: uuid::Uuid::new_v4(),
            catalogue_epoch: uuid::Uuid::new_v4(),
            created_at_ms: 1000,
        };
        let envelope =
            FileLocatorKey::generate_sealed(&master, &receiver).expect("sealed B purpose key");
        let key = FileLocatorKey::open(&master, &receiver, &envelope).expect("B key");
        let payload = receiver_file_metadata(&summary, reference.clone(), file.clone(), Some(&key))
            .expect("B alias");
        assert_eq!(payload["file_id"], "0");
        assert_eq!(
            payload["reference"]["item"]["item_id"],
            "9223372036854775807"
        );
        assert_eq!(
            payload["reference"]["lifecycle_generation"].as_i64(),
            Some(i64::MAX)
        );
        let base = payload["file_base"].as_str().expect("advertised alias");
        let locator = base
            .strip_prefix(&format!("/api/v1/shared/imports/{}/files/", summary.id))
            .expect("B-only file namespace");
        assert_eq!(locator.len(), 236);
        let recovered = key
            .verify(locator, summary.id, summary.lifecycle_generation)
            .expect("full signed identity");
        assert_eq!(recovered.item, reference);
        assert_eq!(recovered.file_id.as_str(), "0");
        assert_eq!(recovered.revision.as_str(), "a".repeat(64));
        assert!(key
            .verify(locator, summary.id, summary.lifecycle_generation - 1)
            .is_err());
        let browse = receiver_file_metadata(&summary, reference.clone(), file.clone(), None)
            .expect("browse without purpose readiness");
        assert!(browse.get("file_base").is_none());
        let mut foreign = reference;
        foreign.server_id = uuid::Uuid::new_v4();
        assert!(receiver_file_metadata(&summary, foreign, file, Some(&key)).is_err());
    }

    #[tokio::test]
    async fn sharing_catalogue_connection_monitor_registry_is_bounded_and_owned() {
        let connection = crate::SharingConnectionCancellation::new();
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for _ in 0..32 {
            let active = active.clone();
            connection
                .monitor(async move {
                    active.fetch_add(1, Ordering::SeqCst);
                    let _owner = ActiveMonitor(active);
                    std::future::pending::<()>().await;
                })
                .expect("bounded registry slot");
        }
        assert!(connection.monitor(std::future::pending::<()>()).is_err());
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while active.load(Ordering::SeqCst) != 32 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("all monitors started");
        drop(connection);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while active.load(Ordering::SeqCst) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("connection owner aborts and releases every monitor");
    }
    #[tokio::test]
    async fn sharing_catalogue_idle_connection_retains_bounded_authority_until_close() {
        let _serial = BODY_FIXTURES.lock().await;
        let fixture = body_fixture().await;
        let baseline = CONTENT_MONITORS.available_permits();
        let (address, captured, stop, served) = body_server(
            &fixture,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        let client = tokio::net::TcpStream::connect(address)
            .await
            .expect("client");
        let (mut sender, driver) =
            hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                .handshake::<_, Body>(hyper_util::rt::TokioIo::new(client))
                .await
                .expect("HTTP2 handshake");
        let driver = tokio::spawn(driver);
        let request = || {
            Request::builder()
                .uri("http://fixture/sharing/v1/libraries")
                .header(
                    "authorization",
                    format!("CinemaShare {}", fixture.secret.expose()),
                )
                .body(Body::empty())
                .expect("request")
        };
        // The actual handler refuses before registering work when all global
        // monitor slots are occupied. It never waits in an admission queue.
        let occupied = CONTENT_MONITORS
            .clone()
            .try_acquire_many_owned(baseline as u32)
            .expect("occupy global bounded registry");
        let refused = sender
            .send_request(request())
            .await
            .expect("capacity response");
        assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
        refused
            .into_body()
            .collect()
            .await
            .expect("closed refusal body");
        drop(occupied);
        let connection = captured.await.expect("accepted connection");
        for count in 1..=32 {
            let response = sender
                .send_request(request())
                .await
                .expect("allowed metadata");
            assert_eq!(response.status(), StatusCode::OK);
            response
                .into_body()
                .collect()
                .await
                .expect("fully consumed response");
            assert_eq!(CONTENT_MONITORS.available_permits(), baseline - count);
        }
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        assert!(!connection.0.is_cancelled());
        assert_eq!(
            CONTENT_MONITORS.available_permits(),
            baseline - 32,
            "idle completed responses retain bounded authority until connection end"
        );
        // A 33rd request cannot create a monitor on this accepted connection.
        // Cancellation may close the transport before its 429 reaches the client.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            sender.send_request(request()),
        )
        .await
        .expect("capacity refusal deadline");
        if let Ok(response) = result {
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        }
        tokio::time::timeout(std::time::Duration::from_secs(3), connection.0.cancelled())
            .await
            .expect("full connection registry closes");
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while CONTENT_MONITORS.available_permits() != baseline {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("all idle monitor permits released");
        drop(sender);
        let _ = driver.await;
        stop.send(()).expect("fixture stop");
        served.await.expect("server task").expect("server result");
    }
    #[tokio::test]
    async fn sharing_catalogue_blocked_http1_closes_on_grant_scope_and_item_revocation() {
        let _serial = BODY_FIXTURES.lock().await;
        for change in 0..3 {
            let fixture = body_fixture().await;
            let body_dropped = Arc::new(AtomicBool::new(false));
            let data_dropped = Arc::new(AtomicBool::new(false));
            let baseline = CONTENT_MONITORS.available_permits();
            let (address, captured, stop, served) =
                body_server(&fixture, body_dropped.clone(), data_dropped.clone()).await;
            let socket = tokio::net::TcpSocket::new_v4().expect("client socket");
            socket
                .set_recv_buffer_size(4096)
                .expect("small receive buffer");
            let mut client = socket.connect(address).await.expect("client connection");
            client.write_all(format!("GET /sharing/v1/libraries/{}/items?limit=200 HTTP/1.1\r\nHost: fixture\r\nAuthorization: CinemaShare {}\r\n\r\n",fixture.library,fixture.secret.expose()).as_bytes()).await.expect("catalogue request");
            let mut head = Vec::new();
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(client.read_u8().await.expect("response head"));
                }
            })
            .await
            .expect("response head deadline");
            assert!(
                head.starts_with(b"HTTP/1.1 200"),
                "{}",
                String::from_utf8_lossy(&head)
            );
            let connection = captured.await.expect("captured connection");
            assert_eq!(CONTENT_MONITORS.available_permits(), baseline - 1);
            await_flag(&body_dropped, true).await;
            assert!(
                !data_dropped.load(Ordering::SeqCst),
                "writer must retain data behind blocked socket"
            );
            match change {
                0 => fixture
                    .state
                    .store
                    .revoke_share(fixture.grant, 2000)
                    .await
                    .expect("grant revocation"),
                1 => {
                    let status = fixture
                        .state
                        .store
                        .sharing_grant_status(&secret_hash(SecretDomain::Grant, &fixture.secret))
                        .await
                        .expect("grant")
                        .expect("active");
                    fixture
                        .state
                        .store
                        .share_scope(
                            fixture.grant,
                            status.grant.mutation_generation,
                            vec![fixture.private],
                            2000,
                        )
                        .await
                        .expect("scope revocation");
                }
                _ => {
                    rusqlite::Connection::open(&fixture.path)
                        .expect("item writer")
                        .execute(
                            "UPDATE items SET library_id=?1 WHERE id=?2",
                            rusqlite::params![fixture.private, fixture.item],
                        )
                        .expect("item move removes effective scope");
                }
            }
            tokio::time::timeout(std::time::Duration::from_secs(3), connection.0.cancelled())
                .await
                .expect("revoked blocked writer closes within grant bound");
            await_flag(&data_dropped, true).await;
            await_flag(&body_dropped, true).await;
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while CONTENT_MONITORS.available_permits() != baseline {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("monitor permit released");
            drop(client);
            stop.send(()).expect("fixture stop");
            served.await.expect("server task").expect("server result");
        }
    }
    #[tokio::test]
    async fn sharing_artwork_blocked_http1_http2_releases_bytes_on_grant_delete_or_move() {
        use plurx_core::{
            sharing_artwork::{artwork_expiry, ArtKind, ArtVariant, SourceArtReference},
            sharing_catalogue_details::CatalogueRevisionKey,
        };
        let _serial = BODY_FIXTURES.lock().await;
        let _art_serial = super::super::shared_artwork::ART_FIXTURES.lock().await;
        for h2 in [false, true] {
            for change in 0..4 {
                if !h2 && change == 3 {
                    continue;
                }
                let mut fixture = body_fixture().await;
                fixture.state.artwork_dir = fixture
                    ._directory
                    .path()
                    .canonicalize()
                    .expect("canonical owned path")
                    .join("artwork");
                std::fs::create_dir(&fixture.state.artwork_dir).expect("owned artwork fixture");
                let mut bytes = vec![23u8; 8 * 1024 * 1024];
                bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
                std::fs::write(fixture.state.artwork_dir.join("fixture.png"), bytes)
                    .expect("bounded original");
                let identity = fixture
                    .state
                    .store
                    .sharing_identity(1000)
                    .await
                    .expect("identity");
                let envelope = CatalogueRevisionKey::generate_sealed(
                    &fixture.state.sharing.key,
                    identity.clone(),
                )
                .expect("fixture key");
                let key = CatalogueRevisionKey::open(
                    &fixture.state.sharing.key,
                    identity.clone(),
                    &envelope,
                )
                .expect("fixture key open");
                let writer = rusqlite::Connection::open(&fixture.path).expect("fixture writer");
                writer
                    .execute_batch(
                        plurx_core::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA,
                    )
                    .expect("candidate key only");
                writer
                    .execute(
                        "INSERT INTO sharing_catalogue_keys VALUES(1,?1,?2,?3)",
                        rusqlite::params![
                            identity.server_id.to_string(),
                            identity.catalogue_epoch.to_string(),
                            envelope.as_stored()
                        ],
                    )
                    .expect("key selection");
                writer
                    .execute(
                        "UPDATE items SET poster_path='fixture.png' WHERE id=?1",
                        [fixture.item],
                    )
                    .expect("poster");
                drop(writer);
                let now = clock_ms();
                let resource = key
                    .issue_art(
                        &SourceArtReference {
                            server_id: identity.server_id,
                            catalogue_epoch: identity.catalogue_epoch,
                            grant_id: fixture.grant,
                            library_id: SourceId::parse(&fixture.library.to_string())
                                .expect("library"),
                            item_id: SourceId::parse(&fixture.item.to_string()).expect("item"),
                            kind: ArtKind::Poster,
                            variant: ArtVariant::Original,
                            expires_at_ms: artwork_expiry(now).expect("expiry"),
                        },
                        now,
                    )
                    .expect("opaque resource");
                let reference = resource.reference_unverified().expect("closed reference");
                assert!(key.verify_art(&resource, fixture.grant, clock_ms()).is_ok());
                let snapshot = fixture
                    .state
                    .store
                    .source_art_snapshot(
                        &secret_hash(SecretDomain::Grant, &fixture.secret),
                        fixture.grant,
                        &reference,
                    )
                    .await
                    .expect("snapshot query");
                let plurx_core::store::sharing_catalogue_artwork::SourceArtRead::Authorized(
                    snapshot,
                ) = snapshot
                else {
                    panic!("current source artwork snapshot absent");
                };
                assert_eq!(snapshot.filename(), "fixture.png");
                let asset = super::super::images::shared_artwork_asset(
                    &fixture.state,
                    &snapshot,
                    &secret_hash(SecretDomain::Grant, &fixture.secret),
                    super::super::shared_artwork::ArtBodyLease::source(false).expect("lease"),
                )
                .await
                .expect("opened image")
                .expect("original exists");
                assert!(asset.still_current().await);
                drop(asset);
                assert!(
                    source_art_scope_current(&fixture.state, &reference).await,
                    "current source scope"
                );
                if !h2 && change == 0 {
                    let variant = plurx_core::sharing_artwork::SourceArtReference {
                        variant: ArtVariant::W300,
                        ..reference.clone()
                    };
                    let variant = key
                        .issue_art(&variant, clock_ms())
                        .expect("exact canonical variant");
                    let mut headers = HeaderMap::new();
                    headers.insert(
                        "authorization",
                        axum::http::HeaderValue::from_str(&format!(
                            "CinemaShare {}",
                            fixture.secret.expose()
                        ))
                        .expect("fixture auth"),
                    );
                    for _ in 0..2 {
                        let response = super::super::shared_artwork::source(
                            State(fixture.state.clone()),
                            headers.clone(),
                            Path(variant.as_str().to_owned()),
                            RawQuery(None),
                        )
                        .await
                        .expect_err("unpublished variant never falls back to original")
                        .into_response();
                        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
                    }
                    let jobs = rusqlite::Connection::open(&fixture.path)
                        .expect("job census")
                        .query_row(
                            "SELECT count(*) FROM background_jobs WHERE kind='artwork_derivative'",
                            [],
                            |row| row.get::<_, i64>(0),
                        )
                        .expect("count");
                    assert_eq!(jobs, 1, "canonical demand is deduplicated");
                    headers.insert(
                        "authorization",
                        axum::http::HeaderValue::from_str(&format!(
                            "CinemaShare {}",
                            new_secret().expect("wrong credential").expose()
                        ))
                        .expect("auth"),
                    );
                    let denied = super::super::shared_artwork::source(
                        State(fixture.state.clone()),
                        headers,
                        Path(variant.as_str().to_owned()),
                        RawQuery(None),
                    )
                    .await
                    .expect_err("wrong credential before demand")
                    .into_response();
                    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
                    assert_eq!(rusqlite::Connection::open(&fixture.path).expect("job census").query_row("SELECT count(*) FROM background_jobs WHERE kind='artwork_derivative'",[],|row|row.get::<_,i64>(0)).expect("count"),jobs,"refused authority cannot add render work");
                }
                let dropped = Arc::new(AtomicBool::new(false));
                let data = Arc::new(AtomicBool::new(false));
                let before = super::super::shared_artwork::source_capacity();
                let (address, captured, stop, served) =
                    body_server(&fixture, dropped.clone(), data.clone()).await;
                let socket = tokio::net::TcpSocket::new_v4().expect("socket");
                socket.set_recv_buffer_size(4096).expect("bounded window");
                let client = socket.connect(address).await.expect("connect");
                let mut raw = None;
                let mut h2_sender = None;
                let mut h2_driver = None;
                let mut h2_body = None;
                if h2 {
                    let (mut sender, driver) = hyper::client::conn::http2::Builder::new(
                        hyper_util::rt::TokioExecutor::new(),
                    )
                    .initial_stream_window_size(1024)
                    .initial_connection_window_size(1024)
                    .handshake::<_, Body>(hyper_util::rt::TokioIo::new(client))
                    .await
                    .expect("H2");
                    h2_driver = Some(tokio::spawn(driver));
                    let response = sender
                        .send_request(
                            Request::builder()
                                .uri(format!(
                                    "http://fixture/sharing/v1/art/{}",
                                    resource.as_str()
                                ))
                                .header(
                                    "authorization",
                                    format!("CinemaShare {}", fixture.secret.expose()),
                                )
                                .body(Body::empty())
                                .expect("request"),
                        )
                        .await
                        .expect("art head");
                    assert_eq!(response.status(), StatusCode::OK);
                    h2_body = Some(response.into_body());
                    h2_sender = Some(sender);
                } else {
                    let mut client = client;
                    client.write_all(format!("GET /sharing/v1/art/{} HTTP/1.1\r\nHost: fixture\r\nAuthorization: CinemaShare {}\r\n\r\n",resource.as_str(),fixture.secret.expose()).as_bytes()).await.expect("request");
                    let mut head = Vec::new();
                    tokio::time::timeout(std::time::Duration::from_secs(5), async {
                        while !head.ends_with(b"\r\n\r\n") {
                            head.push(client.read_u8().await.expect("head"));
                        }
                    })
                    .await
                    .expect("head deadline");
                    assert!(
                        head.starts_with(b"HTTP/1.1 200"),
                        "{}",
                        String::from_utf8_lossy(&head)
                    );
                    raw = Some(client);
                }
                let connection = captured.await.expect("connection");
                await_flag(&dropped, true).await;
                assert!(
                    !data.load(Ordering::SeqCst),
                    "queued DATA retains actual bytes"
                );
                assert_eq!(
                    super::super::shared_artwork::source_capacity(),
                    (before.0 - 1, before.1 - 8193)
                );
                if change == 3 {
                    h2_body
                        .take()
                        .expect("first artwork body")
                        .collect()
                        .await
                        .expect("consume artwork");
                    for count in 2..=4 {
                        let response = h2_sender
                            .as_mut()
                            .expect("sender")
                            .send_request(
                                Request::builder()
                                    .uri(format!(
                                        "http://fixture/sharing/v1/art/{}",
                                        resource.as_str()
                                    ))
                                    .header(
                                        "authorization",
                                        format!("CinemaShare {}", fixture.secret.expose()),
                                    )
                                    .body(Body::empty())
                                    .expect("art request"),
                            )
                            .await
                            .expect("idle art");
                        assert_eq!(response.status(), StatusCode::OK);
                        response.into_body().collect().await.expect("consume");
                        assert_eq!(
                            super::super::shared_artwork::source_capacity(),
                            (before.0 - count, before.1 - count * 8193)
                        );
                    }
                    let response = h2_sender
                        .as_mut()
                        .expect("sender")
                        .send_request(
                            Request::builder()
                                .uri(format!(
                                    "http://fixture/sharing/v1/art/{}",
                                    resource.as_str()
                                ))
                                .header(
                                    "authorization",
                                    format!("CinemaShare {}", fixture.secret.expose()),
                                )
                                .body(Body::empty())
                                .expect("art request"),
                        )
                        .await
                        .expect("bounded refusal");
                    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
                    response.into_body().collect().await.expect("refusal");
                    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                    assert!(!connection.0.is_cancelled());
                    assert_eq!(
                        super::super::shared_artwork::source_capacity(),
                        (0, before.1 - 4 * 8193),
                        "idle completed responses retain actual bounded ownership"
                    );
                    drop(h2_sender);
                    if let Some(driver) = h2_driver {
                        driver.abort();
                        let _ = driver.await;
                    }
                    tokio::time::timeout(std::time::Duration::from_secs(3), async {
                        while super::super::shared_artwork::source_capacity() != before {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("connection close releases all idle art owners");
                    stop.send(()).expect("stop");
                    served.await.expect("server task").expect("server");
                    continue;
                }
                match change {
                    0 => fixture
                        .state
                        .store
                        .revoke_share(fixture.grant, clock_ms())
                        .await
                        .expect("revoke"),
                    1 => {
                        rusqlite::Connection::open(&fixture.path)
                            .expect("writer")
                            .execute("DELETE FROM items WHERE id=?1", [fixture.item])
                            .expect("delete");
                    }
                    _ => {
                        rusqlite::Connection::open(&fixture.path)
                            .expect("writer")
                            .execute(
                                "UPDATE items SET library_id=?1 WHERE id=?2",
                                rusqlite::params![fixture.private, fixture.item],
                            )
                            .expect("move");
                    }
                }
                tokio::time::timeout(std::time::Duration::from_secs(3), connection.0.cancelled())
                    .await
                    .expect("revocation closes accepted transport");
                await_flag(&data, true).await;
                tokio::time::timeout(std::time::Duration::from_secs(3), async {
                    while super::super::shared_artwork::source_capacity() != before {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("actual task/byte/operation cleanup");
                drop(raw);
                drop(h2_body);
                drop(h2_sender);
                if let Some(driver) = h2_driver {
                    let _ = driver.await;
                }
                stop.send(()).expect("stop");
                served.await.expect("server task").expect("server");
            }
        }
    }
    #[tokio::test]
    async fn sharing_catalogue_details_blocked_writer_closes_on_file_delete_or_move() {
        use plurx_core::sharing_catalogue_details::CatalogueRevisionKey;
        let _serial = BODY_FIXTURES.lock().await;
        for moved in [false, true] {
            let fixture = body_fixture().await;
            let identity = fixture
                .state
                .store
                .sharing_identity(1000)
                .await
                .expect("identity");
            let envelope =
                CatalogueRevisionKey::generate_sealed(&fixture.state.sharing.key, identity.clone())
                    .expect("fixture purpose key");
            let probe=json!({"chapters":(0..1024).map(|n|json!({"start_time":n.to_string(),"end_time":(n+1).to_string(),"tags":{"title":"x".repeat(512)}})).collect::<Vec<_>>(),"private_path":"/private/never-export"}).to_string();
            let writer = rusqlite::Connection::open(&fixture.path).expect("fixture writer");
            writer
                .execute_batch(
                    plurx_core::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA,
                )
                .expect("fixture-only key table");
            writer
                .execute(
                    "INSERT INTO sharing_catalogue_keys VALUES(1,?1,?2,?3)",
                    rusqlite::params![
                        identity.server_id.to_string(),
                        identity.catalogue_epoch.to_string(),
                        envelope.as_stored()
                    ],
                )
                .expect("fixture-only key selection");
            writer.execute("INSERT INTO files(id,item_id,path,size,mtime,probe_json) VALUES(1,?1,'/private/movie.mkv',20,1000,?2)",rusqlite::params![fixture.item,probe]).expect("bounded large details");
            writer.execute("INSERT INTO files(id,item_id,path,size,mtime,probe_json) SELECT 2,item_id,'/private/second.mkv',size,mtime,probe_json FROM files WHERE id=1",[]).expect("second bounded file");
            writer.execute("INSERT INTO files(id,item_id,path,size,mtime,probe_json) SELECT 3,item_id,'/private/third.mkv',size,mtime,probe_json FROM files WHERE id=1",[]).expect("third bounded file");
            drop(writer);
            let body_dropped = Arc::new(AtomicBool::new(false));
            let data_dropped = Arc::new(AtomicBool::new(false));
            let baseline = CONTENT_MONITORS.available_permits();
            let (address, captured, stop, served) =
                body_server(&fixture, body_dropped.clone(), data_dropped.clone()).await;
            let socket = tokio::net::TcpSocket::new_v4().expect("socket");
            socket
                .set_recv_buffer_size(4096)
                .expect("small receive buffer");
            let mut client = socket.connect(address).await.expect("connect");
            client.write_all(format!("GET /sharing/v1/items/{} HTTP/1.1\r\nHost: fixture\r\nAuthorization: CinemaShare {}\r\n\r\n",fixture.item,fixture.secret.expose()).as_bytes()).await.expect("details request");
            let mut head = Vec::new();
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(client.read_u8().await.expect("response head"));
                }
            })
            .await
            .expect("head deadline");
            assert!(
                head.starts_with(b"HTTP/1.1 200"),
                "{}",
                String::from_utf8_lossy(&head)
            );
            let connection = captured.await.expect("connection");
            await_flag(&body_dropped, true).await;
            assert!(
                !data_dropped.load(Ordering::SeqCst),
                "actual blocked DATA owner"
            );
            let writer = rusqlite::Connection::open(&fixture.path).expect("file writer");
            writer
                .execute(
                    "UPDATE files SET path='/private/revised.mkv' WHERE id=1",
                    [],
                )
                .expect("revision change");
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            assert!(
                !connection.0.is_cancelled(),
                "revision changes preserve metadata delivery"
            );
            if moved {
                writer
                    .execute("UPDATE files SET item_id=?1 WHERE id=1", [fixture.item + 1])
                    .expect("move file to another exported item");
            } else {
                writer
                    .execute("DELETE FROM files WHERE id=1", [])
                    .expect("delete file");
            }
            drop(writer);
            tokio::time::timeout(std::time::Duration::from_secs(3), connection.0.cancelled())
                .await
                .expect("file revocation bound");
            await_flag(&data_dropped, true).await;
            await_flag(&body_dropped, true).await;
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while CONTENT_MONITORS.available_permits() != baseline {
                    tokio::task::yield_now().await
                }
            })
            .await
            .expect("all permits release");
            drop(client);
            stop.send(()).expect("stop");
            served.await.expect("task").expect("result");
        }
    }
    #[tokio::test]
    async fn sharing_catalogue_http2_retains_authority_after_body_drop_and_other_stream_flush() {
        let _serial = BODY_FIXTURES.lock().await;
        let fixture = body_fixture().await;
        let body_dropped = Arc::new(AtomicBool::new(false));
        let data_dropped = Arc::new(AtomicBool::new(false));
        let baseline = CONTENT_MONITORS.available_permits();
        let (address, captured, stop, served) =
            body_server(&fixture, body_dropped.clone(), data_dropped.clone()).await;
        let client = tokio::net::TcpStream::connect(address)
            .await
            .expect("client connection");
        let mut builder =
            hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new());
        builder.initial_stream_window_size(1024);
        let (mut sender, driver) = builder
            .handshake::<_, Body>(hyper_util::rt::TokioIo::new(client))
            .await
            .expect("HTTP2 handshake");
        let driver = tokio::spawn(driver);
        let response = sender
            .send_request(
                Request::builder()
                    .uri(format!(
                        "http://fixture/sharing/v1/libraries/{}/items?limit=200",
                        fixture.library
                    ))
                    .header(
                        "authorization",
                        format!("CinemaShare {}", fixture.secret.expose()),
                    )
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("metadata response");
        assert_eq!(response.status(), StatusCode::OK);
        let connection = captured.await.expect("connection scope");
        await_flag(&body_dropped, true).await;
        assert!(
            !data_dropped.load(Ordering::SeqCst),
            "body ended while HTTP2 data still awaits window capacity"
        );
        assert_eq!(CONTENT_MONITORS.available_permits(), baseline - 1);
        let other = sender
            .send_request(
                Request::builder()
                    .uri("http://fixture/sharing/v1/identity")
                    .body(Body::empty())
                    .expect("other request"),
            )
            .await
            .expect("other stream response");
        assert_eq!(other.status(), StatusCode::OK);
        other
            .into_body()
            .collect()
            .await
            .expect("other stream flush");
        assert!(
            !data_dropped.load(Ordering::SeqCst),
            "other stream flush cannot retire blocked metadata"
        );
        let ordinary = sender
            .send_request(
                Request::builder()
                    .uri("http://fixture/ordinary/pending")
                    .body(Body::empty())
                    .expect("ordinary request"),
            )
            .await
            .expect("ordinary response");
        let replacement = new_secret().expect("rotation fixture");
        assert_eq!(
            fixture
                .state
                .store
                .rotate_share(
                    fixture.grant,
                    uuid::Uuid::new_v4(),
                    &secret_hash(SecretDomain::Grant, &fixture.secret),
                    &secret_hash(SecretDomain::Grant, &replacement),
                    2000
                )
                .await
                .expect("credential rotation"),
            MutationOutcome::Applied
        );
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        assert!(
            !connection.0.is_cancelled(),
            "credential rotation alone must preserve accepted content authority"
        );
        rusqlite::Connection::open(&fixture.path)
            .expect("item writer")
            .execute(
                "UPDATE items SET library_id=?1 WHERE id=?2",
                rusqlite::params![fixture.private, fixture.item],
            )
            .expect("effective item revocation");
        tokio::time::timeout(std::time::Duration::from_secs(3), connection.0.cancelled())
            .await
            .expect("blocked HTTP2 connection closes within grant bound");
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                ordinary.into_body().collect()
            )
            .await
            .expect("multiplex cancellation deadline")
            .is_err(),
            "connection cancellation also retires unrelated multiplexed streams"
        );
        await_flag(&data_dropped, true).await;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while CONTENT_MONITORS.available_permits() != baseline {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("monitor released");
        drop(response);
        drop(sender);
        let _ = driver.await;
        stop.send(()).expect("fixture stop");
        served.await.expect("server task").expect("server result");
    }
    #[tokio::test]
    async fn sharing_catalogue_viewer_routes_require_login_and_fail_closed_without_import() {
        let (app, state) = super::super::tests::test_app_with_state();
        let import = uuid::Uuid::new_v4();
        let path = format!("/api/v1/shared/imports/{import}/libraries/12/items");
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(&path)
                    .body(Body::empty())
                    .expect("synthetic catalogue fixture"),
            )
            .await
            .expect("synthetic catalogue fixture");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/setup")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"username":"catalogue-viewer","password":"synthetic-password"})
                            .to_string(),
                    ))
                    .expect("synthetic catalogue fixture"),
            )
            .await
            .expect("synthetic catalogue fixture");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("synthetic catalogue fixture")
            .to_bytes();
        let setup: Value = serde_json::from_slice(&bytes).expect("synthetic catalogue fixture");
        let token = setup["token"]
            .as_str()
            .expect("synthetic catalogue fixture");
        for (path, status) in [
            (path, StatusCode::SERVICE_UNAVAILABLE),
            (
                format!("/api/v1/shared/imports/{import}/libraries/01/items"),
                StatusCode::BAD_REQUEST,
            ),
            (
                format!("/api/v1/shared/imports/{import}/libraries/12/items?q=%FF"),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(path)
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::empty())
                        .expect("synthetic catalogue fixture"),
                )
                .await
                .expect("synthetic catalogue fixture");
            assert_eq!(response.status(), status);
            let bytes = response
                .into_body()
                .collect()
                .await
                .expect("synthetic catalogue fixture")
                .to_bytes();
            let body = String::from_utf8(bytes.to_vec()).expect("synthetic catalogue fixture");
            assert!(!body.contains("synthetic-password"));
        }
        assert!(state
            .store
            .sharing_imports()
            .await
            .expect("synthetic catalogue fixture")
            .is_empty());
    }
    enum BlockedReceiverClient {
        Http1(tokio::net::TcpStream),
        Http2(
            hyper::client::conn::http2::SendRequest<Body>,
            hyper::body::Incoming,
            tokio::task::JoinHandle<Result<(), hyper::Error>>,
        ),
    }
    async fn blocked_receiver_client(
        address: std::net::SocketAddr,
        path: &str,
        token: &str,
        h2: bool,
    ) -> BlockedReceiverClient {
        let socket = tokio::net::TcpSocket::new_v4().expect("receiver client socket");
        socket
            .set_recv_buffer_size(4096)
            .expect("small receiver window");
        let mut client = socket.connect(address).await.expect("receiver client");
        if h2 {
            let (mut sender, driver) =
                hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .initial_stream_window_size(1024)
                    .initial_connection_window_size(2048)
                    .handshake::<_, Body>(hyper_util::rt::TokioIo::new(client))
                    .await
                    .expect("receiver H2 handshake");
            let driver = tokio::spawn(driver);
            let response = sender
                .send_request(
                    Request::builder()
                        .uri(format!("http://fixture{path}"))
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("receiver response");
            assert_eq!(response.status(), StatusCode::OK);
            BlockedReceiverClient::Http2(sender, response.into_body(), driver)
        } else {
            client.write_all(format!("GET {path} HTTP/1.1\r\nHost: fixture\r\nAuthorization: Bearer {token}\r\n\r\n").as_bytes()).await.expect("receiver request");
            let mut head = Vec::new();
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(client.read_u8().await.expect("receiver header"));
                }
            })
            .await
            .expect("receiver headers deadline");
            assert!(
                head.starts_with(b"HTTP/1.1 200"),
                "{}",
                String::from_utf8_lossy(&head)
            );
            BlockedReceiverClient::Http1(client)
        }
    }
    async fn release_receiver_client(client: BlockedReceiverClient) {
        match client {
            BlockedReceiverClient::Http1(socket) => drop(socket),
            BlockedReceiverClient::Http2(sender, body, driver) => {
                drop(body);
                drop(sender);
                let _ = driver.await;
            }
        }
    }
    #[tokio::test]
    async fn sharing_receiver_blocked_http1_and_http2_cancel_on_login_loss() {
        let _serial = BODY_FIXTURES.lock().await;
        for (h2, loss) in [false, true]
            .into_iter()
            .flat_map(|h2| (0..3).map(move |loss| (h2, loss)))
        {
            let fixture = body_fixture().await;
            let user = fixture
                .state
                .store
                .create_user("receiver-login-viewer", "synthetic-hash", false)
                .await
                .expect("viewer");
            let token = "synthetic-receiver-login";
            let hash = plurx_core::auth::hash_token(token);
            fixture
                .state
                .store
                .create_token(&hash, user.id, None)
                .await
                .expect("login");
            let import = uuid::Uuid::new_v4();
            let local = fixture
                .state
                .store
                .sharing_identity(1000)
                .await
                .expect("B identity");
            let seal = |purpose, value: &str| {
                fixture
                    .state
                    .sharing
                    .key
                    .seal_sharing(purpose, local.server_id, import, value)
                    .expect("sealed fixture")
            };
            fixture
                .state
                .store
                .create_share_import(NewImport {
                    id: import,
                    source: SharingIdentity {
                        server_id: uuid::Uuid::new_v4(),
                        catalogue_epoch: uuid::Uuid::new_v4(),
                        created_at_ms: 1000,
                    },
                    source_name: "Configured offline Source".into(),
                    claim_id: uuid::Uuid::new_v4(),
                    credential: seal(
                        plurx_core::secrets::SharingSecretPurpose::Credential,
                        "synthetic-credential",
                    ),
                    claim_secret: seal(
                        plurx_core::secrets::SharingSecretPurpose::Claim,
                        "synthetic-claim",
                    ),
                    endpoints: vec![Endpoint {
                        ipv4: "100.101.102.103".parse().expect("IP"),
                        ipv6: None,
                        ts_fqdn: "offline.fixture.ts.net".into(),
                        port: 1,
                        spki_sha256: "a".repeat(64),
                    }],
                    now_ms: 1000,
                })
                .await
                .expect("import");
            fixture
                .state
                .store
                .settle_share_claim(import, uuid::Uuid::new_v4(), true, 1001)
                .await
                .expect("claim");
            fixture
                .state
                .store
                .assign_share_viewers(
                    import,
                    1,
                    vec![Assignment {
                        library_id: source_id("12").expect("ID"),
                        user_id: user.id,
                    }],
                    1002,
                )
                .await
                .expect("assignment");
            let summary = fixture
                .state
                .store
                .sharing_imports()
                .await
                .expect("imports")
                .into_iter()
                .find(|i| i.id == import)
                .expect("current import");
            let scope = receiver_scope(
                &summary,
                vec![source_id("12").expect("ID")],
                Vec::new(),
                Vec::new(),
            )
            .expect("scope")
            .scope;
            let app=Router::new().route("/test/shared/items",get(move |State(state):State<AppState>,super::super::extract::AuthUser(user):super::super::extract::AuthUser,super::super::extract::RawToken(token):super::super::extract::RawToken| { let scope = scope.clone(); async move{receiver_json_scoped(&state,&token,user.id,vec![scope],Vec::new(),json!({"receiver_test_payload":"x".repeat(2*1024*1024)})).await} }))
                .route_layer(middleware::from_fn_with_state(fixture.state.clone(),receiver_content_guard)).with_state(fixture.state.clone());
            let baseline = RECEIVER_MONITORS.available_permits();
            let body_dropped = Arc::new(AtomicBool::new(false));
            let data_dropped = Arc::new(AtomicBool::new(false));
            let (address, captured, stop, served) =
                body_server_app(app, body_dropped.clone(), data_dropped.clone()).await;
            let client = blocked_receiver_client(address, "/test/shared/items", token, h2).await;
            let connection = captured.await.expect("accepted receiver connection");
            assert_eq!(RECEIVER_MONITORS.available_permits(), baseline - 1);
            await_flag(&body_dropped, true).await;
            assert!(
                !data_dropped.load(Ordering::SeqCst),
                "actual receiver DATA remains blocked"
            );
            let before = fixture
                .state
                .store
                .list_tokens_for_user(user.id)
                .await
                .expect("last seen")[0]
                .last_seen_at;
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            assert!(!connection.0.is_cancelled());
            assert_eq!(
                fixture
                    .state
                    .store
                    .list_tokens_for_user(user.id)
                    .await
                    .expect("last seen")[0]
                    .last_seen_at,
                before,
                "monitor must not renew idle login"
            );
            match loss {
                0 => {
                    assert!(fixture
                        .state
                        .store
                        .delete_token(&hash)
                        .await
                        .expect("revoke login"));
                }
                1 => fixture
                    .state
                    .store
                    .disable_share_import(import, 1003)
                    .await
                    .expect("disable import"),
                2 => {
                    fixture
                        .state
                        .store
                        .assign_share_viewers(import, 2, Vec::new(), 1003)
                        .await
                        .expect("remove assignment");
                }
                _ => unreachable!(),
            }
            tokio::time::timeout(std::time::Duration::from_secs(3), connection.0.cancelled())
                .await
                .expect("blocked B connection closes");
            await_flag(&data_dropped, true).await;
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while RECEIVER_MONITORS.available_permits() != baseline {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("receiver monitor permit released");
            release_receiver_client(client).await;
            stop.send(()).expect("stop");
            served.await.expect("server task").expect("server result");
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires disposable CGNAT network and PLURX_SHARING_FIXTURE_IP"]
    async fn sharing_receiver_pinned_decision_http1_http2_revalidates_file_assignment_and_login() {
        use http_body_util::BodyExt;
        use plurx_core::{
            config::{SharingEgressConfig, SharingNetworkConfig},
            secrets::SharingSecretPurpose,
            sharing_catalogue_details::CatalogueRevisionKey,
            sharing_file_locators::{FileLocatorKey, FileLocatorReference},
            sharing_tls::{LiveNodeTls, SharingTlsListener},
            store::SqliteStore,
        };
        async fn exchange(
            address: std::net::SocketAddr,
            h2: bool,
            request: Request<Body>,
        ) -> (StatusCode, Bytes) {
            let socket = tokio::net::TcpStream::connect(address)
                .await
                .expect("B socket");
            if h2 {
                let (mut sender, driver) =
                    hyper::client::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                        .handshake::<_, Body>(hyper_util::rt::TokioIo::new(socket))
                        .await
                        .expect("H2 handshake");
                let driver = tokio::spawn(driver);
                let response = sender.send_request(request).await.expect("B response");
                let status = response.status();
                let payload = response
                    .into_body()
                    .collect()
                    .await
                    .expect("B body")
                    .to_bytes();
                drop(sender);
                driver.abort();
                let _ = driver.await;
                (status, payload)
            } else {
                let (mut sender, driver) = hyper::client::conn::http1::handshake::<_, Body>(
                    hyper_util::rt::TokioIo::new(socket),
                )
                .await
                .expect("H1 handshake");
                let driver = tokio::spawn(driver);
                let response = sender.send_request(request).await.expect("B response");
                let status = response.status();
                let payload = response
                    .into_body()
                    .collect()
                    .await
                    .expect("B body")
                    .to_bytes();
                drop(sender);
                driver.abort();
                let _ = driver.await;
                (status, payload)
            }
        }
        let _serial = BODY_FIXTURES.lock().await;
        let address: std::net::IpAddr = std::env::var("PLURX_SHARING_FIXTURE_IP")
            .expect("explicit CGNAT fixture")
            .parse()
            .expect("address");
        assert!(is_tailnet_address(address));
        for h2 in [false, true] {
            let source = body_fixture().await;
            let tls_dir = tempfile::tempdir().expect("Source TLS fixture");
            let tls =
                Arc::new(LiveNodeTls::open(tls_dir.path(), clock_ms() / 1000).expect("Source TLS"));
            let (pin, _) = tls.status().expect("Source SPKI");
            let listener = tokio::net::TcpListener::bind((address, 0))
                .await
                .expect("Source CGNAT listener");
            let endpoint = Endpoint {
                ipv4: match address {
                    std::net::IpAddr::V4(ip) => ip,
                    _ => panic!("IPv4 fixture"),
                },
                ipv6: None,
                ts_fqdn: "source.fixture.ts.net".into(),
                port: listener.local_addr().expect("Source address").port(),
                spki_sha256: pin,
            };
            let (source_stop, source_stopped) = tokio::sync::oneshot::channel();
            let source_task = tokio::spawn(crate::serve_http(
                SharingTlsListener::new(listener, tls),
                sharing::peer_router(source.state.clone()),
                async move {
                    let _ = source_stopped.await;
                },
                crate::HTTP_TIMEOUTS,
            ));
            let receiver_dir = tempfile::tempdir().expect("B fixture");
            let receiver_path = receiver_dir.path().join("receiver.sqlite");
            let (_, mut receiver) = super::super::tests::test_app_with_state();
            receiver.store = Arc::new(SqliteStore::open(&receiver_path).expect("B Store"));
            receiver.runtime_cache_dir = receiver_dir
                .path()
                .canonicalize()
                .expect("canonical B cache")
                .join("runtime");
            std::fs::create_dir(&receiver.runtime_cache_dir).expect("runtime cache fixture");
            receiver.sharing = Arc::new(crate::sharing::SharingManager::new(
                receiver.sharing.key.clone(),
                receiver_dir.path().join("unused-tls"),
                SharingNetworkConfig {
                    bind: "127.0.0.1:32444".parse().expect("unused bind"),
                    egress: SharingEgressConfig::LocalAddress { address },
                },
            ));
            receiver
                .store
                .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
                .await
                .expect("enable B");
            let user = receiver
                .store
                .create_user("receiver-current-viewer", "synthetic-hash", false)
                .await
                .expect("viewer");
            let token = "synthetic-receiver-current-login";
            let hash = plurx_core::auth::hash_token(token);
            receiver
                .store
                .create_token(&hash, user.id, None)
                .await
                .expect("login");
            let local = receiver
                .store
                .sharing_identity(1000)
                .await
                .expect("B identity");
            let remote = source
                .state
                .store
                .sharing_identity(1000)
                .await
                .expect("Source identity");
            rusqlite::Connection::open(&source.path)
                .expect("Source fixture writer")
                .execute(
                    "UPDATE sharing_exports SET recipient_server_id=?1 WHERE id=?2",
                    rusqlite::params![local.server_id.to_string(), source.grant.to_string()],
                )
                .expect("bind grant to actual B identity");
            let import = uuid::Uuid::new_v4();
            let credential = crate::sharing::ImportCredential::encode(
                &source.secret,
                uuid::Uuid::new_v4(),
                1000,
                "Fixture B",
                2000,
                None,
            )
            .expect("closed credential");
            assert_eq!(
                receiver
                    .store
                    .create_share_import(NewImport {
                        id: import,
                        source: remote.clone(),
                        source_name: "Fixture Source".into(),
                        claim_id: uuid::Uuid::new_v4(),
                        credential: receiver
                            .sharing
                            .key
                            .seal_sharing(
                                SharingSecretPurpose::Credential,
                                local.server_id,
                                import,
                                credential.expose()
                            )
                            .expect("seal credential"),
                        claim_secret: receiver
                            .sharing
                            .key
                            .seal_sharing(
                                SharingSecretPurpose::Claim,
                                local.server_id,
                                import,
                                "synthetic-claim"
                            )
                            .expect("seal claim"),
                        endpoints: vec![endpoint.clone()],
                        now_ms: 1000
                    })
                    .await
                    .expect("B import"),
                ImportOutcome::Created
            );
            receiver
                .store
                .settle_share_claim(import, source.grant, true, 1001)
                .await
                .expect("active B import");
            let library = source_id(&source.library.to_string()).expect("ID");
            receiver
                .store
                .assign_share_viewers(
                    import,
                    1,
                    vec![Assignment {
                        library_id: library.clone(),
                        user_id: user.id,
                    }],
                    1002,
                )
                .await
                .expect("assignment");

            let source_file = source._directory.path().join("decision.mp4");
            std::fs::write(&source_file, b"actual fixture presence").expect("Source file");
            let envelope =
                CatalogueRevisionKey::generate_sealed(&source.state.sharing.key, remote.clone())
                    .expect("Source key");
            let source_key =
                CatalogueRevisionKey::open(&source.state.sharing.key, remote.clone(), &envelope)
                    .expect("Source signer");
            let writer = rusqlite::Connection::open(&source.path).expect("Source writer");
            writer
                .execute_batch(
                    plurx_core::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA,
                )
                .expect("Source schema");
            writer
                .execute(
                    "INSERT INTO sharing_catalogue_keys VALUES(1,?1,?2,?3)",
                    rusqlite::params![
                        remote.server_id.to_string(),
                        remote.catalogue_epoch.to_string(),
                        envelope.as_stored()
                    ],
                )
                .expect("Source key");
            writer.execute("INSERT INTO files(id,item_id,path,size,mtime,duration_ms,container,video_codec,width,height,bit_depth,bitrate) VALUES(0,?1,?2,23,1000,60000,'mp4','h264',1920,1080,8,1000000)",rusqlite::params![source.item,source_file.to_str().expect("path")]).expect("Source file identity zero");
            drop(writer);
            let hash = secret_hash(SecretDomain::Grant, &source.secret);
            let plurx_core::store::sharing_catalogue_details::SourceDetailsRead::Authorized(
                witness,
            ) = source
                .state
                .store
                .source_item_file_witness(
                    &hash,
                    source.grant,
                    source_id(&source.item.to_string()).expect("item"),
                    source_id("0").expect("file"),
                )
                .await
                .expect("witness")
            else {
                panic!("authorized")
            };
            let reference = FileLocatorReference {
                item: SharedReference {
                    import_id: import,
                    server_id: remote.server_id,
                    catalogue_epoch: remote.catalogue_epoch,
                    library_id: library.clone(),
                    item_id: source_id(&source.item.to_string()).expect("item"),
                },
                file_id: source_id("0").expect("zero"),
                revision: source_key.file_revision(&witness).expect("revision"),
                lifecycle_generation: 1,
            };
            let envelope =
                FileLocatorKey::generate_sealed(&receiver.sharing.key, &local).expect("B key");
            let writer = rusqlite::Connection::open(&receiver_path).expect("B writer");
            writer
                .execute_batch(
                    plurx_core::store::sharing_file_locators::CANDIDATE_FILE_LOCATOR_KEY_SCHEMA,
                )
                .expect("B schema");
            writer
                .execute(
                    "INSERT INTO sharing_file_locator_keys VALUES(1,?1,?2,?3)",
                    rusqlite::params![
                        local.server_id.to_string(),
                        local.catalogue_epoch.to_string(),
                        envelope.as_stored()
                    ],
                )
                .expect("B key");
            drop(writer);
            let key =
                FileLocatorKey::open(&receiver.sharing.key, &local, &envelope).expect("B signer");
            let locator = key.issue(&reference).expect("signed locator");
            let caps:plurx_core::playback::DeviceCaps=serde_json::from_value(json!({"v":2,"video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"]})).expect("caps");
            let input = super::super::shared_playback::SourceDecisionRequest {
                reference: super::super::hls::SourcePlaybackTarget {
                    server_id: remote.server_id,
                    catalogue_epoch: remote.catalogue_epoch,
                    library_id: library.clone(),
                    item_id: reference.item.item_id.clone(),
                    file_id: reference.file_id.clone(),
                    revision: reference.revision.clone(),
                },
                caps: caps.clone(),
                audio: None,
                subtitle: None,
                audio_offset_ms: None,
                force: None,
            };
            let (_, expected) = receiver
                .sharing
                .read_file_decision(&receiver, user.id, &reference, &input)
                .await
                .expect("actual pinned Source decision");
            let expected =
                super::super::sharing_decision_decode::DecodedDecision::parse(expected.decision, 0)
                    .expect("complete engine");
            let expected = serde_json::to_value(
                super::super::sharing_playback_wire::project_decoded_decision(
                    expected, &reference, &key,
                )
                .expect("expected B projection"),
            )
            .expect("JSON");
            let (b_address, _captured, stop, served) = body_server_app(
                super::super::router(receiver.clone()),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            )
            .await;
            // A real authenticated item read must advertise the same signed
            // tuple used by decision clients; no client invents its lifecycle.
            let request = Request::builder()
                .uri(format!(
                    "http://fixture/api/v1/shared/imports/{import}/items/{}",
                    source.item
                ))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("B item request");
            let (status, payload) = exchange(b_address, h2, request).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "{}",
                String::from_utf8_lossy(&payload)
            );
            let details: Value = serde_json::from_slice(&payload).expect("actual B item details");
            assert_eq!(details["lifecycle_generation"].as_i64(), Some(1));
            let advertised = details["files"]
                .as_array()
                .expect("files")
                .iter()
                .find(|file| file["file_id"] == "0")
                .expect("Source file zero");
            assert_eq!(advertised["file_base"], locator.file_base());
            assert_eq!(
                advertised["reference"]["lifecycle_generation"].as_i64(),
                Some(1)
            );
            assert_eq!(
                advertised["reference"]["item"],
                serde_json::to_value(&reference.item).expect("full reference")
            );
            let path = format!(
                "{}/decision",
                advertised["file_base"].as_str().expect("B alias")
            );
            for case in 0..5 {
                if case == 2 {
                    rusqlite::Connection::open(&source.path)
                        .expect("Source writer")
                        .execute("UPDATE files SET mtime=mtime+1 WHERE id=0", [])
                        .expect("change actual revision");
                }
                if case == 3 {
                    rusqlite::Connection::open(&source.path)
                        .expect("Source writer")
                        .execute("UPDATE files SET mtime=1000 WHERE id=0", [])
                        .expect("restore revision");
                    receiver
                        .store
                        .assign_share_viewers(import, 2, vec![], 1003)
                        .await
                        .expect("remove current viewer assignment");
                }
                if case == 4 {
                    receiver
                        .store
                        .assign_share_viewers(
                            import,
                            3,
                            vec![Assignment {
                                library_id: library.clone(),
                                user_id: user.id,
                            }],
                            1004,
                        )
                        .await
                        .expect("restore current assignment");
                    receiver
                        .store
                        .delete_token(&plurx_core::auth::hash_token(token))
                        .await
                        .expect("revoke actual B login");
                }
                let request_path = if case == 1 {
                    let mut forged = locator.file_base();
                    let last = forged.pop().expect("nonempty signed locator");
                    forged.push(if last == 'a' { 'b' } else { 'a' });
                    format!("{forged}/decision")
                } else {
                    path.clone()
                };
                let request = Request::builder()
                    .method("POST")
                    .uri(format!("http://fixture{request_path}"))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({"caps":caps})).expect("body"),
                    ))
                    .expect("request");
                let (status, payload) = exchange(b_address, h2, request).await;
                if case == 0 {
                    assert_eq!(
                        status,
                        StatusCode::OK,
                        "{}",
                        String::from_utf8_lossy(&payload)
                    );
                    assert_eq!(
                        serde_json::from_slice::<Value>(&payload).expect("complete B engine"),
                        expected
                    );
                } else {
                    assert!(
                        !status.is_success(),
                        "must refuse changed authority H2={h2} case={case}"
                    );
                }
                for database in [&source.path, &receiver_path] {
                    let sessions: i64 = rusqlite::Connection::open(database)
                        .expect("session census")
                        .query_row("SELECT count(*) FROM media_sessions", [], |r| r.get(0))
                        .expect("census");
                    assert_eq!(sessions, 0, "decisions never allocate Source/B sessions");
                }
            }
            let _ = stop.send(());
            served.await.expect("B server").expect("B shutdown");
            let _ = source_stop.send(());
            source_task
                .await
                .expect("Source server")
                .expect("Source shutdown");
        }
    }
    /// Runs only inside the explicitly assigned disposable CGNAT container.
    /// The real Source router, SPKI dialer, identity check, receiver router,
    /// Store authorities and accepted-connection ownership remain in the path.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires disposable CGNAT network and PLURX_SHARING_FIXTURE_IP"]
    async fn sharing_receiver_pinned_source_blocked_http1_http2_revalidate_current_scope() {
        use plurx_core::{
            config::{SharingEgressConfig, SharingNetworkConfig},
            secrets::SharingSecretPurpose,
            sharing_tls::{LiveNodeTls, SharingTlsListener},
            store::SqliteStore,
        };
        let _serial = BODY_FIXTURES.lock().await;
        let address: std::net::IpAddr = std::env::var("PLURX_SHARING_FIXTURE_IP")
            .expect("explicit disposable CGNAT address")
            .parse()
            .expect("numeric address");
        assert!(is_tailnet_address(address));
        for h2 in [false, true] {
            for change in 0..16 {
                let mut source = body_fixture().await;
                let artwork = change >= 9;
                if artwork {
                    source.state.artwork_dir = source
                        ._directory
                        .path()
                        .canonicalize()
                        .expect("canonical Source art")
                        .join("artwork");
                    std::fs::create_dir(&source.state.artwork_dir).expect("art namespace");
                    let mut bytes = vec![23u8; 8 * 1024 * 1024];
                    bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
                    std::fs::write(source.state.artwork_dir.join("fixture.png"), bytes)
                        .expect("bounded asset");
                    rusqlite::Connection::open(&source.path)
                        .expect("poster writer")
                        .execute(
                            "UPDATE items SET poster_path='fixture.png' WHERE id=?1",
                            [source.item],
                        )
                        .expect("poster");
                }
                let tls_dir = tempfile::tempdir().expect("Source TLS fixture");
                let tls = Arc::new(
                    LiveNodeTls::open(tls_dir.path(), clock_ms() / 1000).expect("Source TLS"),
                );
                let (pin, _) = tls.status().expect("Source SPKI");
                let listener = tokio::net::TcpListener::bind((address, 0))
                    .await
                    .expect("Source CGNAT listener");
                let endpoint = Endpoint {
                    ipv4: match address {
                        std::net::IpAddr::V4(ip) => ip,
                        _ => panic!("IPv4 fixture"),
                    },
                    ipv6: None,
                    ts_fqdn: "source.fixture.ts.net".into(),
                    port: listener.local_addr().expect("Source address").port(),
                    spki_sha256: pin,
                };
                let (source_stop, source_stopped) = tokio::sync::oneshot::channel();
                let source_task = tokio::spawn(crate::serve_http(
                    SharingTlsListener::new(listener, tls),
                    sharing::peer_router(source.state.clone()),
                    async move {
                        let _ = source_stopped.await;
                    },
                    crate::HTTP_TIMEOUTS,
                ));
                let receiver_dir = tempfile::tempdir().expect("B fixture");
                let receiver_path = receiver_dir.path().join("receiver.sqlite");
                let (_, mut receiver) = super::super::tests::test_app_with_state();
                receiver.store = Arc::new(SqliteStore::open(&receiver_path).expect("B Store"));
                receiver.runtime_cache_dir = receiver_dir
                    .path()
                    .canonicalize()
                    .expect("canonical B cache")
                    .join("runtime");
                std::fs::create_dir(&receiver.runtime_cache_dir).expect("runtime cache fixture");
                receiver.sharing = Arc::new(crate::sharing::SharingManager::new(
                    receiver.sharing.key.clone(),
                    receiver_dir.path().join("unused-tls"),
                    SharingNetworkConfig {
                        bind: "127.0.0.1:32444".parse().expect("unused bind"),
                        egress: SharingEgressConfig::LocalAddress { address },
                    },
                ));
                receiver
                    .store
                    .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
                    .await
                    .expect("enable B");
                let user = receiver
                    .store
                    .create_user("receiver-current-viewer", "synthetic-hash", false)
                    .await
                    .expect("viewer");
                let token = "synthetic-receiver-current-login";
                let hash = plurx_core::auth::hash_token(token);
                receiver
                    .store
                    .create_token(&hash, user.id, None)
                    .await
                    .expect("login");
                let local = receiver
                    .store
                    .sharing_identity(1000)
                    .await
                    .expect("B identity");
                let remote = source
                    .state
                    .store
                    .sharing_identity(1000)
                    .await
                    .expect("Source identity");
                rusqlite::Connection::open(&source.path)
                    .expect("Source fixture writer")
                    .execute(
                        "UPDATE sharing_exports SET recipient_server_id=?1 WHERE id=?2",
                        rusqlite::params![local.server_id.to_string(), source.grant.to_string()],
                    )
                    .expect("bind grant to actual B identity");
                let import = uuid::Uuid::new_v4();
                let credential = crate::sharing::ImportCredential::encode(
                    &source.secret,
                    uuid::Uuid::new_v4(),
                    1000,
                    "Fixture B",
                    2000,
                    None,
                )
                .expect("closed credential");
                assert_eq!(
                    receiver
                        .store
                        .create_share_import(NewImport {
                            id: import,
                            source: remote.clone(),
                            source_name: "Fixture Source".into(),
                            claim_id: uuid::Uuid::new_v4(),
                            credential: receiver
                                .sharing
                                .key
                                .seal_sharing(
                                    SharingSecretPurpose::Credential,
                                    local.server_id,
                                    import,
                                    credential.expose()
                                )
                                .expect("seal credential"),
                            claim_secret: receiver
                                .sharing
                                .key
                                .seal_sharing(
                                    SharingSecretPurpose::Claim,
                                    local.server_id,
                                    import,
                                    "synthetic-claim"
                                )
                                .expect("seal claim"),
                            endpoints: vec![endpoint.clone()],
                            now_ms: 1000
                        })
                        .await
                        .expect("B import"),
                    ImportOutcome::Created
                );
                receiver
                    .store
                    .settle_share_claim(import, source.grant, true, 1001)
                    .await
                    .expect("active B import");
                let library = source_id(&source.library.to_string()).expect("ID");
                receiver
                    .store
                    .assign_share_viewers(
                        import,
                        1,
                        vec![Assignment {
                            library_id: library.clone(),
                            user_id: user.id,
                        }],
                        1002,
                    )
                    .await
                    .expect("assignment");
                if change == 0 {
                    let mut refused = endpoint.clone();
                    refused.port = 1;
                    let started = std::time::Instant::now();
                    let (peer, identity) = tokio::time::timeout(
                        std::time::Duration::from_secs(1),
                        crate::sharing_client::PeerConnection::verified(
                            &receiver.sharing,
                            &[refused, endpoint.clone()],
                            &remote,
                        ),
                    )
                    .await
                    .expect("dead DNS/refused hint cannot delay reachable approved hint")
                    .expect("pinned reachable hint");
                    assert_eq!(identity.server_id, remote.server_id);
                    assert!(started.elapsed() < std::time::Duration::from_secs(1));
                    drop(peer);
                }
                if change == 1 {
                    use plurx_core::store::sharing_catalogue::{RemoteWatch, RemoteWatchUpdate};
                    let item_ids: Vec<i64> = {
                        let writer = rusqlite::Connection::open(&source.path)
                            .expect("Source identity reader");
                        let mut statement = writer
                            .prepare("SELECT id FROM items WHERE library_id=?1 ORDER BY id")
                            .expect("IDs");
                        let rows = statement
                            .query_map([source.library], |row| row.get(0))
                            .expect("current IDs")
                            .collect::<Result<Vec<_>, _>>()
                            .expect("closed IDs");
                        rows
                    };
                    assert_eq!(item_ids.len(), 200);
                    for item in item_ids {
                        receiver
                            .store
                            .save_remote_watch(RemoteWatchUpdate {
                                import_id: import,
                                library_id: library.clone(),
                                item_id: source_id(&item.to_string()).expect("ID"),
                                user_id: user.id,
                                lifecycle_generation: 1,
                                assignment_generation: 2,
                                progress: RemoteWatch {
                                    position_ms: 1000,
                                    duration_ms: Some(60000),
                                    watched: false,
                                    sequence: 1,
                                    updated_at_ms: 1001,
                                },
                            })
                            .await
                            .expect("B-owned Continue Watching history");
                    }
                }
                let details = matches!(change, 3 | 4);
                if details || artwork {
                    use plurx_core::sharing_catalogue_details::CatalogueRevisionKey;
                    let envelope = CatalogueRevisionKey::generate_sealed(
                        &source.state.sharing.key,
                        remote.clone(),
                    )
                    .expect("revision fixture key");
                    let writer =
                        rusqlite::Connection::open(&source.path).expect("details fixture writer");
                    writer.execute_batch(plurx_core::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA).expect("candidate key table");
                    writer
                        .execute(
                            "INSERT INTO sharing_catalogue_keys VALUES(1,?1,?2,?3)",
                            rusqlite::params![
                                remote.server_id.to_string(),
                                remote.catalogue_epoch.to_string(),
                                envelope.as_stored()
                            ],
                        )
                        .expect("fixture purpose key");
                    let probe=json!({"chapters":(0..1024).map(|n|json!({"start_time":n.to_string(),"end_time":(n+1).to_string(),"tags":{"title":"x".repeat(512)}})).collect::<Vec<_>>()}).to_string();
                    for file in 1..=if details { 3 } else { 0 } {
                        writer.execute("INSERT INTO files(id,item_id,path,size,mtime,probe_json) VALUES(?1,?2,?4,20,1000,?3)",rusqlite::params![file,source.item,probe,format!("/private/fixture-{file}.mkv")]).expect("bounded details file");
                    }
                }
                let art_path = if artwork {
                    use plurx_core::{
                        sharing_artwork::{ArtVariant, ReceiverArtReference},
                        sharing_file_locators::FileLocatorKey,
                    };
                    let envelope = FileLocatorKey::generate_sealed(&receiver.sharing.key, &local)
                        .expect("B fixture purpose material");
                    let writer =
                        rusqlite::Connection::open(&receiver_path).expect("B key fixture writer");
                    writer.execute_batch(plurx_core::store::sharing_file_locators::CANDIDATE_FILE_LOCATOR_KEY_SCHEMA).expect("candidate B key schema");
                    writer
                        .execute(
                            "INSERT INTO sharing_file_locator_keys VALUES(1,?1,?2,?3)",
                            rusqlite::params![
                                local.server_id.to_string(),
                                local.catalogue_epoch.to_string(),
                                envelope.as_stored()
                            ],
                        )
                        .expect("B key selection");
                    drop(writer);
                    let key = FileLocatorKey::open(&receiver.sharing.key, &local, &envelope)
                        .expect("B key");
                    let (summary, reply) = receiver
                        .sharing
                        .read_catalogue(
                            &receiver,
                            import,
                            user.id,
                            crate::sharing::CatalogueRead::Item(
                                source_id(&source.item.to_string()).expect("item"),
                            ),
                        )
                        .await
                        .expect("fresh pinned Source art resources");
                    let crate::sharing::CatalogueReply::Item(details) = reply else {
                        panic!("details");
                    };
                    let art = details
                        .item
                        .art
                        .into_iter()
                        .find(|art| art.variant == ArtVariant::Original)
                        .expect("fresh Source opaque original");
                    Some(
                        key.issue_art(
                            &ReceiverArtReference {
                                item: SharedReference {
                                    import_id: import,
                                    server_id: remote.server_id,
                                    catalogue_epoch: remote.catalogue_epoch,
                                    library_id: library.clone(),
                                    item_id: source_id(&source.item.to_string()).expect("item"),
                                },
                                user_id: user.id,
                                lifecycle_generation: summary.lifecycle_generation,
                                source: art.resource,
                            },
                            clock_ms(),
                        )
                        .expect("user/import bound B resource")
                        .url(),
                    )
                } else {
                    None
                };
                let app = super::super::router(receiver.clone()).route(
                    "/ordinary/pending",
                    get(|| async {
                        Response::new(Body::from_stream(futures_util::stream::pending::<
                            Result<Bytes, std::convert::Infallible>,
                        >()))
                    }),
                );
                let body_dropped = Arc::new(AtomicBool::new(false));
                let data_dropped = Arc::new(AtomicBool::new(false));
                let baseline = RECEIVER_MONITORS.available_permits();
                let (b_address, captured, stop, served) =
                    body_server_app(app, body_dropped.clone(), data_dropped.clone()).await;
                let art_baseline = super::super::shared_artwork::receiver_capacity();
                let path = if let Some(path) = art_path {
                    path
                } else if change == 1 {
                    format!("/api/v1/shared/imports/{import}/continue-watching?limit=200")
                } else if details {
                    format!("/api/v1/shared/imports/{import}/items/{}", source.item)
                } else {
                    format!(
                        "/api/v1/shared/imports/{import}/libraries/{}/items?limit=200",
                        source.library
                    )
                };
                let mut client = blocked_receiver_client(b_address, &path, token, h2).await;
                let connection = captured.await.expect("accepted B connection");
                assert_eq!(RECEIVER_MONITORS.available_permits(), baseline - 1);
                await_flag(&body_dropped, true).await;
                assert!(
                    !data_dropped.load(Ordering::SeqCst),
                    "B DATA blocked h2={h2}/change={change}"
                );
                if artwork {
                    assert_eq!(
                        super::super::shared_artwork::receiver_capacity(),
                        (art_baseline.0 - 1, art_baseline.1 - 8193),
                        "actual B artwork bytes held behind DATA"
                    );
                }
                let ordinary = if let BlockedReceiverClient::Http2(sender, _, _) = &mut client {
                    Some(
                        sender
                            .send_request(
                                Request::builder()
                                    .uri("http://fixture/ordinary/pending")
                                    .body(Body::empty())
                                    .expect("ordinary multiplex request"),
                            )
                            .await
                            .expect("ordinary multiplex response")
                            .into_body(),
                    )
                } else {
                    None
                };
                if change == 0 {
                    rusqlite::Connection::open(&source.path)
                        .expect("metadata writer")
                        .execute(
                            "UPDATE items SET title='Fresh changed metadata' WHERE id=?1",
                            [source.item],
                        )
                        .expect("benign metadata update");
                    let (_, fresh) = receiver
                        .sharing
                        .read_catalogue(
                            &receiver,
                            import,
                            user.id,
                            crate::sharing::CatalogueRead::Page {
                                library: library.clone(),
                                parent: None,
                                q: String::new(),
                                cursor: None,
                                limit: 200,
                            },
                        )
                        .await
                        .expect("full fresh read despite populated cache");
                    let crate::sharing::CatalogueReply::Page(fresh) = fresh else {
                        panic!("page reply");
                    };
                    assert_eq!(
                        fresh
                            .items
                            .iter()
                            .find(|item| item.item_id.as_str() == source.item.to_string())
                            .expect("current item")
                            .title,
                        "Fresh changed metadata",
                        "cache must replace changed fresh bytes"
                    );
                    // Both real credential rotation and assignment/endpoint additions
                    // preserve captured effective tuples. No counter-only revocation.
                    let active = receiver
                        .store
                        .sharing_import(import)
                        .await
                        .expect("load")
                        .expect("active");
                    receiver
                        .sharing
                        .resume_rotation(&receiver, active, true)
                        .await
                        .expect("real pinned benign credential rotation");
                    receiver
                        .store
                        .assign_share_viewers(
                            import,
                            2,
                            vec![
                                Assignment {
                                    library_id: library.clone(),
                                    user_id: user.id,
                                },
                                Assignment {
                                    library_id: source_id(&source.private.to_string()).expect("ID"),
                                    user_id: user.id,
                                },
                            ],
                            1003,
                        )
                        .await
                        .expect("benign assignment expansion");
                    receiver
                        .store
                        .set_sharing_import_endpoints(import, 1, vec![endpoint], None, 1004)
                        .await
                        .expect("benign endpoint refresh");
                    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                    assert!(
                        !connection.0.is_cancelled(),
                        "benign changes preserve accepted B response"
                    );
                    assert!(!data_dropped.load(Ordering::SeqCst));
                }
                match change {
                    0 => source
                        .state
                        .store
                        .revoke_share(source.grant, 2000)
                        .await
                        .expect("Source grant revoke"),
                    1 => {
                        let grant = source
                            .state
                            .store
                            .sharing_exports(None)
                            .await
                            .expect("exports")
                            .remove(0)
                            .grant;
                        source
                            .state
                            .store
                            .share_scope(
                                source.grant,
                                grant.mutation_generation,
                                vec![source.private],
                                2000,
                            )
                            .await
                            .expect("Source scope loss");
                    }
                    2 => {
                        rusqlite::Connection::open(&source.path)
                            .expect("Source writer")
                            .execute(
                                "UPDATE items SET library_id=?1 WHERE id=?2",
                                rusqlite::params![source.private, source.item],
                            )
                            .expect("Source item move");
                    }
                    3 => {
                        rusqlite::Connection::open(&source.path)
                            .expect("Source writer")
                            .execute("DELETE FROM files WHERE id=1", [])
                            .expect("Source file delete");
                    }
                    4 => {
                        rusqlite::Connection::open(&source.path)
                            .expect("Source writer")
                            .execute("UPDATE files SET item_id=?1 WHERE id=1", [source.item + 1])
                            .expect("Source file move");
                    }
                    5 => {
                        assert!(receiver
                            .store
                            .delete_token(&hash)
                            .await
                            .expect("B login revoke"));
                    }
                    6 => receiver
                        .store
                        .disable_share_import(import, 2000)
                        .await
                        .expect("B import loss"),
                    7 => {
                        receiver
                            .store
                            .assign_share_viewers(import, 2, Vec::new(), 2000)
                            .await
                            .expect("B assignment loss");
                    }
                    9 => source
                        .state
                        .store
                        .revoke_share(source.grant, clock_ms())
                        .await
                        .expect("art grant loss"),
                    10 => {
                        rusqlite::Connection::open(&source.path)
                            .expect("writer")
                            .execute("DELETE FROM items WHERE id=?1", [source.item])
                            .expect("art item delete");
                    }
                    11 => {
                        rusqlite::Connection::open(&source.path)
                            .expect("writer")
                            .execute(
                                "UPDATE items SET library_id=?1 WHERE id=?2",
                                rusqlite::params![source.private, source.item],
                            )
                            .expect("art item move");
                    }
                    12 => {
                        assert!(receiver
                            .store
                            .delete_token(&hash)
                            .await
                            .expect("art B login loss"));
                    }
                    13 => receiver
                        .store
                        .disable_share_import(import, clock_ms())
                        .await
                        .expect("art B import loss"),
                    14 => {
                        receiver
                            .store
                            .assign_share_viewers(import, 2, Vec::new(), clock_ms())
                            .await
                            .expect("art B assignment loss");
                    }
                    15 => {
                        source
                            .state
                            .store
                            .put_setting(plurx_core::store::keys::SHARING_ENABLED, "false")
                            .await
                            .expect("art Source offline");
                    }
                    _ => {
                        assert!(
                            receiver.sharing.catalogue_cache_entries() > 0,
                            "fresh metadata populated cache"
                        );
                        source
                            .state
                            .store
                            .put_setting(plurx_core::store::keys::SHARING_ENABLED, "false")
                            .await
                            .expect("Source unavailable");
                        assert!(
                            receiver
                                .sharing
                                .read_catalogue(
                                    &receiver,
                                    import,
                                    user.id,
                                    crate::sharing::CatalogueRead::Page {
                                        library: library.clone(),
                                        parent: None,
                                        q: String::new(),
                                        cursor: None,
                                        limit: 200
                                    }
                                )
                                .await
                                .is_err(),
                            "cached metadata cannot replace fresh Source authority"
                        );
                    }
                }
                tokio::time::timeout(std::time::Duration::from_secs(3), connection.0.cancelled())
                    .await
                    .expect("B blocked connection current-authority deadline");
                await_flag(&data_dropped, true).await;
                if let Some(mut ordinary) = ordinary {
                    assert!(
                        tokio::time::timeout(std::time::Duration::from_secs(3), ordinary.frame())
                            .await
                            .expect("multiplex close deadline")
                            .is_none_or(|frame| frame.is_err()),
                        "connection cancellation closes unrelated H2 stream"
                    );
                }
                tokio::time::timeout(std::time::Duration::from_secs(3), async {
                    while RECEIVER_MONITORS.available_permits() != baseline {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("B permit cleanup");
                assert_eq!(
                    receiver.sharing.scope_control_available(),
                    32,
                    "B control permit cleanup"
                );
                assert_eq!(
                    SCOPE_CONTROL.available_permits(),
                    32,
                    "Source control permit cleanup"
                );
                if artwork {
                    tokio::time::timeout(std::time::Duration::from_secs(3), async {
                        while super::super::shared_artwork::receiver_capacity() != art_baseline {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("B art operation/byte settlement");
                }
                release_receiver_client(client).await;
                stop.send(()).expect("stop B");
                served.await.expect("B task").expect("B result");
                source_stop.send(()).expect("stop Source");
                source_task
                    .await
                    .expect("Source task")
                    .expect("Source result");
            }
        }
    }

    /// Actual pinned Source read and accepted connection authority for an administrator
    /// whose import has no viewer assignments. This is an isolated CGNAT fixture.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires disposable CGNAT network and PLURX_SHARING_FIXTURE_IP"]
    async fn sharing_admin_pinned_library_read_bootstraps_empty_matrix_and_fences_connection() {
        use plurx_core::{
            config::{SharingEgressConfig, SharingNetworkConfig},
            secrets::SharingSecretPurpose,
            sharing_tls::{LiveNodeTls, SharingTlsListener},
            store::SqliteStore,
        };
        let _serial = BODY_FIXTURES.lock().await;
        let address: std::net::IpAddr = std::env::var("PLURX_SHARING_FIXTURE_IP")
            .expect("explicit CGNAT")
            .parse()
            .expect("address");
        assert!(is_tailnet_address(address));
        for h2 in [false, true] {
            for loss in 0..5 {
                let source = body_fixture().await;
                let tls_dir = tempfile::tempdir().expect("Source TLS fixture");
                let tls = Arc::new(
                    LiveNodeTls::open(tls_dir.path(), clock_ms() / 1000).expect("Source TLS"),
                );
                let (pin, _) = tls.status().expect("Source SPKI");
                let listener = tokio::net::TcpListener::bind((address, 0))
                    .await
                    .expect("Source CGNAT listener");
                let endpoint = Endpoint {
                    ipv4: match address {
                        std::net::IpAddr::V4(ip) => ip,
                        _ => panic!("IPv4 fixture"),
                    },
                    ipv6: None,
                    ts_fqdn: "source.fixture.ts.net".into(),
                    port: listener.local_addr().expect("Source address").port(),
                    spki_sha256: pin,
                };
                let (source_stop, source_stopped) = tokio::sync::oneshot::channel();
                let source_task = tokio::spawn(crate::serve_http(
                    SharingTlsListener::new(listener, tls),
                    sharing::peer_router(source.state.clone()),
                    async move {
                        let _ = source_stopped.await;
                    },
                    crate::HTTP_TIMEOUTS,
                ));
                let receiver_dir = tempfile::tempdir().expect("B fixture");
                let receiver_path = receiver_dir.path().join("receiver.sqlite");
                let (_, mut receiver) = super::super::tests::test_app_with_state();
                receiver.store = Arc::new(SqliteStore::open(&receiver_path).expect("B Store"));
                receiver.sharing = Arc::new(crate::sharing::SharingManager::new(
                    receiver.sharing.key.clone(),
                    receiver_dir.path().join("unused-tls"),
                    SharingNetworkConfig {
                        bind: "127.0.0.1:32444".parse().expect("unused bind"),
                        egress: SharingEgressConfig::LocalAddress { address },
                    },
                ));
                receiver
                    .store
                    .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
                    .await
                    .expect("enable B");
                let user = receiver
                    .store
                    .create_user("receiver-library-admin", "synthetic-hash", true)
                    .await
                    .expect("viewer");
                let token = "synthetic-receiver-current-login";
                let hash = plurx_core::auth::hash_token(token);
                receiver
                    .store
                    .create_token(&hash, user.id, None)
                    .await
                    .expect("login");
                let local = receiver
                    .store
                    .sharing_identity(1000)
                    .await
                    .expect("B identity");
                let remote = source
                    .state
                    .store
                    .sharing_identity(1000)
                    .await
                    .expect("Source identity");
                rusqlite::Connection::open(&source.path)
                    .expect("Source fixture writer")
                    .execute(
                        "UPDATE sharing_exports SET recipient_server_id=?1 WHERE id=?2",
                        rusqlite::params![local.server_id.to_string(), source.grant.to_string()],
                    )
                    .expect("bind grant to actual B identity");
                let import = uuid::Uuid::new_v4();
                let credential = crate::sharing::ImportCredential::encode(
                    &source.secret,
                    uuid::Uuid::new_v4(),
                    1000,
                    "Fixture B",
                    2000,
                    None,
                )
                .expect("closed credential");
                assert_eq!(
                    receiver
                        .store
                        .create_share_import(NewImport {
                            id: import,
                            source: remote.clone(),
                            source_name: "Fixture Source".into(),
                            claim_id: uuid::Uuid::new_v4(),
                            credential: receiver
                                .sharing
                                .key
                                .seal_sharing(
                                    SharingSecretPurpose::Credential,
                                    local.server_id,
                                    import,
                                    credential.expose()
                                )
                                .expect("seal credential"),
                            claim_secret: receiver
                                .sharing
                                .key
                                .seal_sharing(
                                    SharingSecretPurpose::Claim,
                                    local.server_id,
                                    import,
                                    "synthetic-claim"
                                )
                                .expect("seal claim"),
                            endpoints: vec![endpoint.clone()],
                            now_ms: 1000
                        })
                        .await
                        .expect("B import"),
                    ImportOutcome::Created
                );
                receiver
                    .store
                    .settle_share_claim(import, source.grant, true, 1001)
                    .await
                    .expect("active B import");

                let snapshot = receiver
                    .store
                    .sharing_import_assignments(import)
                    .await
                    .expect("snapshot")
                    .expect("import");
                assert!(snapshot.assignments.is_empty());
                let (summary, libraries) = receiver
                    .sharing
                    .read_admin_libraries(&receiver, import, user.id, &hash)
                    .await
                    .expect("actual fresh pinned admin read");
                assert_eq!(summary.id, import);
                assert_eq!(libraries.len(), 1);
                assert_eq!(libraries[0].library_id.as_str(), source.library.to_string());
                assert!(
                    receiver
                        .sharing
                        .read_catalogue(
                            &receiver,
                            import,
                            user.id,
                            crate::sharing::CatalogueRead::Libraries
                        )
                        .await
                        .is_err(),
                    "admin does not gain viewer authority"
                );
                let app = super::super::router(receiver.clone());
                let body_dropped = Arc::new(AtomicBool::new(false));
                let data_dropped = Arc::new(AtomicBool::new(false));
                let baseline = RECEIVER_MONITORS.available_permits();
                let (b_address, captured, stop, served) =
                    body_server_app(app, body_dropped.clone(), data_dropped.clone()).await;
                let path = format!("/api/v1/sharing/imports/{import}/libraries");
                let client = blocked_receiver_client(b_address, &path, token, h2).await;
                let connection = captured.await.expect("accepted B connection");
                assert_eq!(RECEIVER_MONITORS.available_permits(), baseline - 1);
                await_flag(&body_dropped, true).await;
                match loss {
                    0 => {
                        receiver
                            .store
                            .set_admin(user.id, false)
                            .await
                            .expect("demote current admin");
                    }
                    1 => {
                        assert!(receiver.store.delete_token(&hash).await.expect("logout"));
                    }
                    2 => receiver
                        .store
                        .disable_share_import(import, 1003)
                        .await
                        .expect("disconnect"),
                    3 => source
                        .state
                        .store
                        .revoke_share(source.grant, 1003)
                        .await
                        .expect("Source revoke"),
                    4 => receiver
                        .store
                        .put_setting(plurx_core::store::keys::SHARING_ENABLED, "false")
                        .await
                        .expect("sharing off"),
                    _ => unreachable!(),
                }
                tokio::time::timeout(std::time::Duration::from_secs(3), connection.0.cancelled())
                    .await
                    .expect("current admin/source connection authority closes");
                tokio::time::timeout(std::time::Duration::from_secs(3), async {
                    while RECEIVER_MONITORS.available_permits() != baseline {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("monitor permit returned");
                release_receiver_client(client).await;
                stop.send(()).expect("stop B");
                served.await.expect("B task").expect("B server");
                source_stop.send(()).expect("stop Source");
                source_task
                    .await
                    .expect("Source task")
                    .expect("Source server");
            }
        }
    }

    #[tokio::test]
    async fn sharing_continue_http_keeps_offline_groups_separate_without_stale_titles() {
        use plurx_core::{
            secrets::SharingSecretPurpose,
            store::sharing_catalogue::{RemoteWatch, RemoteWatchUpdate},
        };
        let _serial = BODY_FIXTURES.lock().await;
        let (_, state) = super::super::tests::test_app_with_state();
        state
            .store
            .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
            .await
            .expect("enable B");
        let user = state
            .store
            .create_user("continue-current-viewer", "synthetic-hash", false)
            .await
            .expect("viewer");
        let token = "synthetic-continue-login";
        state
            .store
            .create_token(&plurx_core::auth::hash_token(token), user.id, None)
            .await
            .expect("login");
        let local = state
            .store
            .sharing_identity(1000)
            .await
            .expect("B identity");
        let mut imports = Vec::new();
        for n in 1..=2 {
            let import = uuid::Uuid::new_v4();
            let secret = new_secret().expect("synthetic credential");
            let credential = crate::sharing::ImportCredential::encode(
                &secret,
                uuid::Uuid::new_v4(),
                1000,
                "Fixture B",
                2000,
                None,
            )
            .expect("closed credential");
            state
                .store
                .create_share_import(NewImport {
                    id: import,
                    source: SharingIdentity {
                        server_id: uuid::Uuid::new_v4(),
                        catalogue_epoch: uuid::Uuid::new_v4(),
                        created_at_ms: 1000,
                    },
                    source_name: format!("B configured Source {n}"),
                    claim_id: uuid::Uuid::new_v4(),
                    credential: state
                        .sharing
                        .key
                        .seal_sharing(
                            SharingSecretPurpose::Credential,
                            local.server_id,
                            import,
                            credential.expose(),
                        )
                        .expect("seal"),
                    claim_secret: state
                        .sharing
                        .key
                        .seal_sharing(
                            SharingSecretPurpose::Claim,
                            local.server_id,
                            import,
                            "synthetic-claim",
                        )
                        .expect("claim"),
                    endpoints: vec![Endpoint {
                        ipv4: "100.101.102.103".parse().expect("fixture IP"),
                        ipv6: None,
                        ts_fqdn: "offline.fixture.ts.net".into(),
                        port: 1,
                        spki_sha256: "a".repeat(64),
                    }],
                    now_ms: 1000,
                })
                .await
                .expect("import");
            state
                .store
                .settle_share_claim(import, uuid::Uuid::new_v4(), true, 1001)
                .await
                .expect("active import");
            state
                .store
                .assign_share_viewers(
                    import,
                    1,
                    vec![Assignment {
                        library_id: source_id("12").expect("ID"),
                        user_id: user.id,
                    }],
                    1002,
                )
                .await
                .expect("assignment");
            state
                .store
                .save_remote_watch(RemoteWatchUpdate {
                    import_id: import,
                    library_id: source_id("12").expect("ID"),
                    item_id: source_id("9007199254740993").expect("ID"),
                    user_id: user.id,
                    lifecycle_generation: 1,
                    assignment_generation: 2,
                    progress: RemoteWatch {
                        position_ms: n * 1000,
                        duration_ms: Some(60000),
                        watched: false,
                        sequence: n,
                        updated_at_ms: 1000 + n,
                    },
                })
                .await
                .expect("B history");
            imports.push(import);
        }
        let app = super::super::router(state.clone());
        let call = |path: String| {
            let app = app.clone();
            let connection = crate::SharingConnectionCancellation::new();
            async move {
                let response = app
                    .oneshot(
                        Request::builder()
                            .uri(path)
                            .header("authorization", format!("Bearer {token}"))
                            .extension(connection.clone())
                            .body(Body::empty())
                            .expect("request"),
                    )
                    .await
                    .expect("response");
                let status = response.status();
                let bytes = response
                    .into_body()
                    .collect()
                    .await
                    .expect("body")
                    .to_bytes();
                let value: Value = serde_json::from_slice(&bytes).expect("JSON");
                drop(connection);
                (status, value)
            }
        };
        let start = std::time::Instant::now();
        let (status, index) = call("/api/v1/shared/continue-watching".into()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(index["groups"].as_array().expect("groups").len(), 2);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "local group index never waits for Source network"
        );
        assert_eq!(index["groups"][0]["count"], 1);
        assert!(index["groups"][0].get("items").is_none());
        for import in &imports {
            let (status, group) =
                call(format!("/api/v1/shared/imports/{import}/continue-watching")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(group["availability"], "unavailable");
            assert_eq!(group["count"], 1);
            assert_eq!(group["items"], json!([]));
            assert!(group["source_name"]
                .as_str()
                .expect("B label")
                .starts_with("B configured"));
            assert!(!group.to_string().contains("synthetic-continue-login"));
        }
        let (status, _) = call("/api/v1/shared/continue-watching?limit=201".into()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call("/api/v1/shared/continue-watching?path=injected".into()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        state
            .store
            .assign_share_viewers(imports[0], 2, Vec::new(), 2000)
            .await
            .expect("remove assignment");
        let (status, index) = call("/api/v1/shared/continue-watching".into()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(index["groups"].as_array().expect("groups").len(), 1);
        state
            .store
            .delete_token(&plurx_core::auth::hash_token(token))
            .await
            .expect("logout");
        let (status, _) = call("/api/v1/shared/continue-watching".into()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn sharing_current_scope_http_checks_recipient_tuples_and_control_capacity() {
        let _serial = BODY_FIXTURES.lock().await;
        let fixture = body_fixture().await;
        let identity = fixture
            .state
            .store
            .sharing_identity(1000)
            .await
            .expect("identity");
        let grant = fixture
            .state
            .store
            .sharing_grant_status(&secret_hash(SecretDomain::Grant, &fixture.secret))
            .await
            .expect("status")
            .expect("grant")
            .grant;
        let scope = plurx_core::sharing_catalogue_details::SourceScopeRequest {
            server_id: identity.server_id,
            catalogue_epoch: identity.catalogue_epoch,
            grant_id: fixture.grant,
            recipient_server_id: grant.recipient_server_id,
            libraries: vec![source_id(&fixture.library.to_string()).expect("ID")],
            items: vec![plurx_core::sharing_catalogue_details::SourceScopeItem {
                library_id: source_id(&fixture.library.to_string()).expect("ID"),
                item_id: source_id(&fixture.item.to_string()).expect("ID"),
            }],
            files: Vec::new(),
        };
        let router = sharing::peer_router(fixture.state.clone());
        let request = |body: Vec<u8>| {
            Request::builder()
                .method("POST")
                .uri("/sharing/v1/current-scope")
                .header(
                    "authorization",
                    format!("CinemaShare {}", fixture.secret.expose()),
                )
                .body(Body::from(body))
                .expect("request")
        };
        for changed in 0..5 {
            let mut scope = scope.clone();
            match changed {
                1 => scope.recipient_server_id = uuid::Uuid::new_v4(),
                2 => scope.server_id = uuid::Uuid::new_v4(),
                3 => scope.grant_id = uuid::Uuid::new_v4(),
                4 => {
                    scope.items[0].library_id = source_id(&fixture.private.to_string()).expect("ID")
                }
                _ => {}
            }
            let response = router
                .clone()
                .oneshot(request(serde_json::to_vec(&scope).expect("scope")))
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK);
            let body = response
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes();
            assert_eq!(
                serde_json::from_slice::<Value>(&body).expect("closed response"),
                json!({"authorized":changed==0})
            );
        }
        let full = SCOPE_CONTROL
            .clone()
            .try_acquire_many_owned(32)
            .expect("control cap");
        assert_eq!(
            router
                .clone()
                .oneshot(request(serde_json::to_vec(&scope).expect("scope")))
                .await
                .expect("response")
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        drop(full);
        assert_eq!(
            router
                .clone()
                .oneshot(request(vec![b' '; 32 * 1024 + 1]))
                .await
                .expect("response")
                .status(),
            StatusCode::BAD_REQUEST
        );
        fixture
            .state
            .store
            .revoke_share(fixture.grant, 2000)
            .await
            .expect("revoke");
        assert_eq!(
            router
                .oneshot(request(serde_json::to_vec(&scope).expect("scope")))
                .await
                .expect("response")
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(SCOPE_CONTROL.available_permits(), 32);
    }

    #[tokio::test]
    async fn sharing_catalogue_http_requires_live_grant_and_qualified_order_maintenance() {
        let (_, state) = super::super::tests::test_app_with_state();
        state
            .store
            .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
            .await
            .expect("enable candidate routes");
        let router = sharing::peer_router(state.clone());
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/sharing/v1/libraries")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .expect("private response"),
            "no-store"
        );
        let library = state
            .store
            .create_library(&NewLibrary {
                name: "Exported movies".into(),
                kind: LibraryKind::Movies,
                paths: vec!["/source/private/path".into()],
                anime: false,
            })
            .await
            .expect("library")
            .id;
        state
            .store
            .sharing_identity(clock_ms())
            .await
            .expect("identity");
        let secret = new_secret().expect("grant secret");
        let invitation = uuid::Uuid::new_v4();
        let grant = uuid::Uuid::new_v4();
        let now = clock_ms();
        state
            .store
            .create_share_invitation(InvitationRecord {
                id: invitation,
                token_hash: "a".repeat(64),
                library_ids: vec![library],
                created_at_ms: now,
                expires_at_ms: now + 60000,
            })
            .await
            .expect("invitation");
        state
            .store
            .claim_share(ShareClaim {
                invitation_id: invitation,
                invitation_hash: "a".repeat(64),
                claim_id: uuid::Uuid::new_v4(),
                grant_id: grant,
                recipient_server_id: uuid::Uuid::new_v4(),
                recipient_name: "Receiver".into(),
                credential_hash: secret_hash(SecretDomain::Grant, &secret),
                now_ms: now,
            })
            .await
            .expect("claim");
        state
            .store
            .approve_share(grant, 1, now)
            .await
            .expect("approve");
        for (method, path, body) in [
            ("GET", "/sharing/v1/libraries".to_owned(), ""),
            ("GET", format!("/sharing/v1/libraries/{library}/items"), ""),
            ("GET", "/sharing/v1/items/1".to_owned(), ""),
            ("GET", "/sharing/v1/items/1/children".to_owned(), ""),
            (
                "POST",
                "/sharing/v1/items:batch".to_owned(),
                "{\"item_ids\":[\"1\"]}",
            ),
        ] {
            let response = router
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method(method)
                        .uri(path)
                        .header("authorization", format!("CinemaShare {}", secret.expose()))
                        .body(Body::from(body))
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "missing candidate must fail closed"
            );
            let bytes = response
                .into_body()
                .collect()
                .await
                .expect("bounded body")
                .to_bytes();
            let text = std::str::from_utf8(&bytes).expect("error JSON");
            assert!(!text.contains("Exported movies"));
            assert!(!text.contains("/source/private/path"));
            assert!(text.contains("sharing_authority_unavailable"));
        }
    }
    #[test]
    fn sharing_catalogue_http_rejects_unknown_duplicate_and_oversized_queries() {
        for raw in [
            "offset=1",
            "limit=201",
            "limit=0",
            "q=%",
            "q=%FF",
            "q=%C3%28",
            "limit=1&limit=2",
            "cursor=x&cursor=y",
            "q=x&q=y",
        ] {
            assert!(query(Some(raw)).is_err(), "{raw}");
        }
        assert!(query(Some(&format!("q={}", "a".repeat(513)))).is_err());
        assert!(query(Some(&format!("cursor={}", "a".repeat(4097)))).is_err());
        assert!(query(Some("q=film&limit=60")).is_ok());
    }
    #[tokio::test]
    async fn sharing_source_preparation_uses_complete_live_file_and_real_principal_engine() {
        use super::super::{
            hls::{prepare_source_playback, CreateSession, SourcePlaybackTarget},
            stream,
        };
        use plurx_core::{
            sharing_catalogue_details::CatalogueRevisionKey,
            store::sharing_catalogue_details::SourceDetailsRead,
        };
        let _serial = BODY_FIXTURES.lock().await;
        let fixture = body_fixture().await;
        let identity = fixture
            .state
            .store
            .sharing_identity(1000)
            .await
            .expect("identity");
        let envelope =
            CatalogueRevisionKey::generate_sealed(&fixture.state.sharing.key, identity.clone())
                .expect("fixture key");
        let key =
            CatalogueRevisionKey::open(&fixture.state.sharing.key, identity.clone(), &envelope)
                .expect("open");
        let source = fixture._directory.path().join("prepared.mp4");
        std::fs::write(&source, b"actual source presence").expect("actual file");
        let writer = rusqlite::Connection::open(&fixture.path).expect("writer");
        writer
            .execute_batch(
                plurx_core::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA,
            )
            .expect("fixture purpose schema");
        writer
            .execute(
                "INSERT INTO sharing_catalogue_keys VALUES(1,?1,?2,?3)",
                rusqlite::params![
                    identity.server_id.to_string(),
                    identity.catalogue_epoch.to_string(),
                    envelope.as_stored()
                ],
            )
            .expect("fixture purpose key");
        writer.execute("INSERT INTO files(id,item_id,path,size,mtime,duration_ms,container,video_codec,width,height,bit_depth,bitrate) VALUES(0,?1,?2,20,1000,60000,'mp4','h264',1920,1080,8,1000000)",rusqlite::params![fixture.item,source.to_str().expect("path")]).expect("actual Source file");
        let hash = secret_hash(SecretDomain::Grant, &fixture.secret);
        let SourceDetailsRead::Authorized(witness) = fixture
            .state
            .store
            .source_item_file_witness(
                &hash,
                fixture.grant,
                SourceId::parse(&fixture.item.to_string()).expect("valid Source fixture value"),
                SourceId::parse("0").expect("valid Source fixture value"),
            )
            .await
            .expect("witness")
        else {
            panic!("authorized witness")
        };
        let target = SourcePlaybackTarget {
            server_id: identity.server_id,
            catalogue_epoch: identity.catalogue_epoch,
            library_id: SourceId::parse(&fixture.library.to_string())
                .expect("valid Source fixture value"),
            item_id: SourceId::parse(&fixture.item.to_string())
                .expect("valid Source fixture value"),
            file_id: SourceId::parse("0").expect("valid Source fixture value"),
            revision: key.file_revision(&witness).expect("revision"),
        };
        let caps:plurx_core::playback::DeviceCaps=serde_json::from_value(json!({"v":2,"video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"]})).expect("caps");
        let body = || {
            serde_json::from_value::<CreateSession>(json!({"playback_id":"shared-player","request_id":"shared-request","copy":true,"height":1080,"quality_auto":false,"presentation":"vod","caps":caps})).expect("body")
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("CinemaShare {}", fixture.secret.expose())
                .parse()
                .expect("valid Source fixture value"),
        );
        headers.insert(
            "cinemashare-viewer",
            "a".repeat(64).parse().expect("valid Source fixture value"),
        );
        let prepared = prepare_source_playback(&fixture.state, &headers, target.clone(), body())
            .await
            .expect("actual Source preparation");
        // Response projection is pure presentation evidence; this StartInfo
        // fixture does not assert physical admission or publication.
        let session = uuid::Uuid::new_v4().to_string();
        let incarnation = uuid::Uuid::new_v4().to_string();
        let info = crate::transcode::StartInfo {
            session_id: session.clone(),
            playlist_url: format!("/api/v1/hls/{session}/index.m3u8"),
            duration_ms: Some(60_000),
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            target_height: 1080,
            kind: prepared.request().kind,
            encoder: "vod",
            grade: plurx_core::transcode::OutputGrade::Sdr,
            vod: true,
            control_lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
        };
        let response = prepared
            .start_response(&fixture.state, &info, &incarnation, 7)
            .await
            .expect("complete Source response");
        assert_eq!(response.session_id, session);
        assert_eq!(response.playlist_url, info.playlist_url);
        assert_eq!(response.duration_ms, info.duration_ms);
        assert_eq!(response.height, info.target_height);
        assert_eq!(response.encoder, info.encoder);
        assert_eq!(response.media_origin_ms, Some(0));
        assert!(response.vod);
        assert_eq!(response.prior_kbps, None);
        assert_eq!(response.plan_notes, prepared.plan_notes());
        let control = response.control.as_ref().expect("private control");
        assert_eq!(control.generation, incarnation);
        assert_eq!(control.control_epoch, 7);
        assert_eq!(control.url, format!("/api/v1/hls/{session}/control"));
        assert!(prepared
            .start_response(&fixture.state, &info, &incarnation, 0)
            .await
            .is_err());
        assert!(prepared
            .start_response(&fixture.state, &info, "not-an-incarnation", 7)
            .await
            .is_err());
        let decision_input = || super::super::shared_playback::SourceDecisionRequest {
            reference: target.clone(),
            caps: caps.clone(),
            audio: None,
            subtitle: None,
            audio_offset_ms: None,
            force: None,
        };
        let peer_response = super::super::shared_playback::source_decision(
            &fixture.state,
            &headers,
            decision_input(),
        )
        .await
        .expect("current peer decision");
        let peer_payload: Value = serde_json::from_slice(
            &to_bytes(peer_response.into_body(), 4 * 1024 * 1024)
                .await
                .expect("bounded body"),
        )
        .expect("peer payload");
        assert_eq!(peer_payload["protocol"], 1);
        assert_eq!(
            peer_payload["reference"],
            serde_json::to_value(&target).expect("reference")
        );
        assert_eq!(
            peer_payload["decision"],
            serde_json::to_value(prepared.decision()).expect("complete engine")
        );
        let mut wrong = decision_input();
        wrong.reference.server_id = uuid::Uuid::new_v4();
        assert!(
            super::super::shared_playback::source_decision(&fixture.state, &headers, wrong)
                .await
                .is_err()
        );
        let mut empty = decision_input();
        empty.caps = plurx_core::playback::DeviceCaps::default();
        assert!(
            super::super::shared_playback::source_decision(&fixture.state, &headers, empty)
                .await
                .is_err()
        );
        assert_eq!(prepared.file().id, 0);
        assert!(crate::media_sessions::source_session_request_is_valid(
            prepared.request(),
            prepared.principal()
        ));
        assert!(!crate::media_sessions::worker_session_request_is_valid(
            prepared.request()
        ));
        let mut negative = prepared.request().clone();
        negative.file_id = -1;
        assert!(!crate::media_sessions::source_session_request_is_valid(
            &negative,
            prepared.principal()
        ));
        assert!(!crate::media_sessions::source_session_request_is_valid(
            prepared.request(),
            &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 }
        ));
        assert_eq!(prepared.file().path, source);
        assert_eq!(prepared.principal().local_user_id(), None);
        assert_eq!(prepared.decision().prior_kbps, None);
        assert_eq!(
            prepared.request().request_id.as_deref(),
            Some("shared-request")
        );
        assert!(matches!(
            prepared.request().kind,
            crate::transcode::SessionKind::Copy { .. }
        ));
        assert_ne!(
            prepared.fingerprint(),
            prepared.request().durable_intent_fingerprint(
                &plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 1 }
            )
        );
        let mut other_headers = headers.clone();
        other_headers.insert(
            "cinemashare-viewer",
            "b".repeat(64).parse().expect("valid Source fixture value"),
        );
        let other = prepare_source_playback(&fixture.state, &other_headers, target.clone(), body())
            .await
            .expect("distinct Source viewer");
        assert_ne!(
            prepared.fingerprint(),
            other.fingerprint(),
            "equal numeric Source files and request IDs do not collapse distinct sharing viewers"
        );
        let prior = plurx_core::domain::NetworkPrior {
            sustained_kbps: Some(1),
            ..Default::default()
        };
        assert!(
            super::super::hls::resolve_plan_for_principal(
                super::super::hls::FilePlanInputs {
                    snapshot: None,
                    state: &fixture.state,
                    file_id: 0,
                    source: Some(prepared.file()),
                    network_prior: Some(&prior)
                },
                prepared.principal(),
                None,
                body()
            )
            .await
            .is_err(),
            "Source planner refuses Local network-prior injection"
        );
        let user = fixture
            .state
            .store
            .create_user("local-engine-fixture", "synthetic-hash", false)
            .await
            .expect("real local account");
        let Json(local) = stream::decision(
            super::super::extract::AuthUser(user),
            State(fixture.state.clone()),
            axum::extract::Path(0),
            axum::extract::Query(stream::Caps {
                caps_v2: Some(caps.clone()),
                ..Default::default()
            }),
            HeaderMap::new(),
            super::super::network::RemoteAddress(None),
        )
        .await
        .expect("actual Local decision");
        assert_eq!(serde_json::to_value(prepared.decision()).expect("valid Source fixture value"),serde_json::to_value(local).expect("valid Source fixture value"),"full actual engine decision parity, including source facts, tracks, delivery, quality candidates and ladder");
        for changed in [
            SourcePlaybackTarget {
                server_id: uuid::Uuid::new_v4(),
                ..target.clone()
            },
            SourcePlaybackTarget {
                catalogue_epoch: uuid::Uuid::new_v4(),
                ..target.clone()
            },
            SourcePlaybackTarget {
                library_id: SourceId::parse(&fixture.private.to_string())
                    .expect("valid Source fixture value"),
                ..target.clone()
            },
            SourcePlaybackTarget {
                revision: plurx_core::sharing_catalogue_details::FileRevision::parse(
                    &"0".repeat(64),
                )
                .expect("valid Source fixture value"),
                ..target.clone()
            },
        ] {
            assert!(
                prepare_source_playback(&fixture.state, &headers, changed, body())
                    .await
                    .is_err()
            );
        }
        writer
            .execute(
                "UPDATE files SET mtime=2000 WHERE id=?1",
                [prepared.file().id],
            )
            .expect("real file revision changed");
        assert!(
            prepare_source_playback(&fixture.state, &headers, target.clone(), body())
                .await
                .is_err()
        );
        writer
            .execute(
                "UPDATE files SET mtime=1000 WHERE id=?1",
                [prepared.file().id],
            )
            .expect("restore snapshot");
        writer
            .execute(
                "UPDATE items SET library_id=?1 WHERE id=?2",
                rusqlite::params![fixture.private, fixture.item],
            )
            .expect("move outside grant");
        assert!(
            prepare_source_playback(&fixture.state, &headers, target, body())
                .await
                .is_err()
        );
        let sessions: i64 = writer
            .query_row("SELECT count(*) FROM media_sessions", [], |row| row.get(0))
            .expect("actual session rows");
        assert_eq!(
            sessions, 0,
            "preparation allocates no physical or durable Source session"
        );
    }
}
