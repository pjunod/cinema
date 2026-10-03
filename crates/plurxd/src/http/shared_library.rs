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
        .route_layer(middleware::from_fn_with_state(state, source_content_guard))
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
async fn authority(
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
    source_json(state,grant,json!({"items":page.records.into_iter().map(|r|r.item).collect::<Vec<_>>(),"next_cursor":next_cursor,"catalogue_revision":page.counters.library_revision,"scope_generation":page.counters.scope_generation,"catalogue_generation":page.counters.catalogue_generation}),vec![library_id],visible).await
}
async fn item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let (hash, grant) = authority(&state, &headers).await?;
    let record = state
        .store
        .source_catalogue_batch(
            &hash,
            grant,
            MetadataBatch {
                item_ids: vec![source_id(&id)?],
            },
        )
        .await
        .map_err(unavailable)?
        .and_then(|mut v| v.pop())
        .and_then(|v| v.record)
        .ok_or_else(missing)?;
    let visible = vec![(record.item.library_id.clone(), record.item.item_id.clone())];
    source_json(
        &state,
        grant,
        serde_json::to_value(record.item).map_err(|_| invalid())?,
        Vec::new(),
        visible,
    )
    .await
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
    source_json(&state,grant,json!({"items":entries.into_iter().map(|e|json!({"item_id":e.item_id,"item":e.record.map(|r|r.item)})).collect::<Vec<_>>()}),Vec::new(),visible).await
}

#[derive(Clone)]
struct SourceContentAuthority {
    grant: uuid::Uuid,
    server: uuid::Uuid,
    epoch: uuid::Uuid,
    libraries: Vec<SourceId>,
    items: Vec<(SourceId, SourceId)>,
}
static CONTENT_MONITORS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(64)));
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
    let bytes = serde_json::to_vec(&value).map_err(|_| invalid())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_catalogue_response_unavailable",
        ));
    }
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    response.extensions_mut().insert(SourceContentAuthority {
        grant,
        server: identity.server_id,
        epoch: identity.catalogue_epoch,
        libraries,
        items,
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
        state
            .store
            .source_content_authorized(
                authority.grant,
                authority.server,
                authority.epoch,
                &authority.libraries,
                &authority.items,
            )
            .await
            .ok()
    })
    .await
        == Ok(Some(true))
}
async fn source_content_guard(
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
    let monitored=connection.monitor(async move {
        let _permit=permit;
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

pub(crate) fn viewer_router() -> Router<AppState> {
    Router::new()
        .route("/shared/libraries", get(viewer_assigned_libraries))
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
fn shared_item(summary: &plurx_core::sharing::ImportSummary, item: SourceCatalogueItem) -> Value {
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
    json!({"source":"shared","reference":reference,"parent":parent,"title":item.title,"sort_title":item.sort_title,"kind":item.kind,"year":item.year,"overview":item.overview,"genres":item.genres,"season_number":item.season_number,"episode_number":item.episode_number})
}
async fn viewer_libraries(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    Path(import): Path<String>,
) -> Result<Json<Value>, ApiError> {
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
    Ok(Json(
        json!({"import_id":summary.id,"server_id":summary.source_server_id,"catalogue_epoch":summary.catalogue_epoch,"libraries":libraries}),
    ))
}
async fn viewer_items(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    Path((import, library)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
    viewer_page(
        &state,
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
    Path((import, item)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let import = import_id(&import)?;
    let item = source_id(&item)?;
    let query = query(raw.as_deref())?;
    let (_, metadata) = current_viewer_item(&state, user.id, import, item.clone()).await?;
    viewer_page(
        &state,
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
    user: i64,
    import: uuid::Uuid,
    library: SourceId,
    parent: Option<SourceId>,
    q: BrowseQuery,
) -> Result<Json<Value>, ApiError> {
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
                limit: q.limit.unwrap_or(DEFAULT_PAGE_SIZE),
            },
        )
        .await
        .map_err(peer_failure)?;
    let crate::sharing::CatalogueReply::Page(page) = reply else {
        return Err(invalid());
    };
    Ok(Json(
        json!({"items":page.items.into_iter().map(|item|shared_item(&summary,item)).collect::<Vec<_>>(),"next_cursor":page.next_cursor,"catalogue_revision":page.catalogue_revision,"scope_generation":page.scope_generation,"catalogue_generation":page.catalogue_generation}),
    ))
}
async fn viewer_batch(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    Path(import): Path<String>,
    body: Body,
) -> Result<Json<Value>, ApiError> {
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
    Ok(Json(
        json!({"items":batch.items.into_iter().map(|entry|json!({"item_id":entry.item_id,"item":entry.item.map(|item|shared_item(&summary,item))})).collect::<Vec<_>>()}),
    ))
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
async fn viewer_item(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    Path((import, item)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let import = import_id(&import)?;
    let item = source_id(&item)?;
    let (summary, metadata) = current_viewer_item(&state, user.id, import, item.clone()).await?;
    let progress = state
        .store
        .remote_watch(import, metadata.library_id.clone(), item, user.id)
        .await
        .map_err(unavailable)?;
    Ok(Json(
        json!({"item":shared_item(&summary,metadata),"watch":progress}),
    ))
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
) -> Result<Json<Value>, ApiError> {
    let imports = state.store.sharing_imports().await.map_err(unavailable)?;
    let mut libraries = Vec::new();
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
        for library in assigned {
            libraries.push(json!({"import_id":import.id,"server_id":import.source_server_id,"catalogue_epoch":import.catalogue_epoch,"library_id":library,"source_name":import.source_name,"availability":"unverified"}));
        }
    }
    Ok(Json(json!({"libraries":libraries})))
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
        let app = sharing::peer_router(fixture.state.clone())
            .route(
                "/ordinary/pending",
                get(|| async {
                    Response::new(Body::from_stream(futures_util::stream::pending::<
                        Result<Bytes, std::convert::Infallible>,
                    >()))
                }),
            )
            .layer(middleware::from_fn(
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
                        let tracked = request.uri().path().ends_with("/items");
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
}
