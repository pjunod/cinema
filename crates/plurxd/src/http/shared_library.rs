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
pub(super) async fn authority<
    H: super::shared_source_playback::forwarding::AuthenticationHeaders + Send + Sync + ?Sized,
>(
    state: &AppState,
    headers: &H,
) -> Result<(String, uuid::Uuid), ApiError> {
    let hash = if let Some(hash) = headers.authenticated_hash() {
        hash.to_owned()
    } else {
        let secret = sharing::credential(headers.raw_headers())?;
        secret_hash(SecretDomain::Grant, &secret)
    };
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
    if let Some(principal) = headers.authenticated_principal() {
        let viewer = super::shared_source_playback::viewer_hash(headers)?;
        if plurx_core::playback_principal::PlaybackPrincipal::sharing(grant.grant.id, &viewer)
            .as_ref()
            .ok()
            != Some(principal)
        {
            return Err(missing());
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
    RawQuery(query): RawQuery,
) -> Result<Response, ApiError> {
    let (hash, grant) = authority(&state, &headers).await?;
    // Older receivers reject unknown response fields. They must keep receiving
    // the original closed shape; an upgraded receiver explicitly opts in.
    let presentation = match query.as_deref() {
        None => false,
        Some("presentation=1") => true,
        Some(_) => return Err(invalid()),
    };
    use plurx_core::{
        sharing_catalogue_details::{CatalogueRevisionKey, SourceItemDetails},
        store::sharing_catalogue_details::SourceDetailsRead,
    };
    let snapshot = match state
        .store
        .source_item_details_snapshot_with_presentation(&hash, grant, source_id(&id)?, presentation)
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
    let details = SourceItemDetails {
        item,
        files,
        presentation: snapshot.presentation.map(Box::new),
    };
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
// Bounds Source media bodies that are still producing bytes. Completed and
// in-memory responses hold no permit.
static CONTENT_MONITORS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(64)));
/// A monitored body owns its monitor's lifetime. Hyper drops a body at EOF,
/// once a declared length is yielded, on stream reset or with its connection;
/// that drop is the "this response is finished with the writer" signal, so the
/// monitor and its global permit end with the response, not the keep-alive
/// connection that carried it.
struct MonitoredBody {
    inner: Body,
    _permit: tokio::sync::OwnedSemaphorePermit,
    _live: tokio_util::sync::DropGuard,
}
impl http_body::Body for MonitoredBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;
    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}
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
    attach_source_file_authority(&mut response, grant, target);
    Ok(response)
}
pub(super) fn attach_source_file_authority(
    response: &mut Response,
    grant: uuid::Uuid,
    target: &super::hls::SourcePlaybackTarget,
) {
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
    guard_source_response(state, connection, response).await
}
/// Source status can return either guarded live Start content or bounded
/// cleanup-only facts. Only the former passes this same accepted-writer guard.
pub(super) async fn guard_source_response(
    state: AppState,
    connection: Option<crate::SharingConnectionCancellation>,
    response: Response,
) -> Response {
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
    let Some(source_guard) = response
        .extensions()
        .get::<std::sync::Arc<crate::transcode::source_actor::SourceResponseGuard>>()
        .cloned()
    else {
        // Catalogue JSON and artwork are complete in memory when the handler
        // returns, so this check is their authority point. Re-observing bytes
        // that already exist cannot withdraw them; artwork memory is owned by
        // its Bytes (`shared_artwork::asset_response`), not by a monitor.
        if !source_content_current(&state, &authority).await {
            return fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_body_authority_unavailable",
            )
            .into_response();
        }
        return response;
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
    let drain = connection.drain_token();
    let closed = connection.closed();
    // Cancelled by the body's drop, or with the connection as its parent.
    let live = connection.0.child_token();
    let body_live = live.clone();
    let monitored = connection.monitor(async move {
        // Source media keeps producing bytes after the handler returns, and
        // replicated revocation has no change feed, so authority is
        // re-observed while this body is alive. Its drop ends the observation.
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = body_live.cancelled() => break,
                () = source_guard.cancelled() => {
                    let mode=source_guard.retirement_mode();
                    match mode {crate::sharing_connection_custody::DriverCloseMode::Drain=>drain.cancel(),crate::sharing_connection_custody::DriverCloseMode::Revoke=>cancel.cancel()}
                    let _=super::shared_source_playback::settle_ingress_custody(&state,source_guard.assignment(),source_guard.retirement_deadline(),mode).await;
                    break;
                },
                _ = interval.tick() => {},
            }
            tokio::select! {
                () = body_live.cancelled() => break,
                current = source_content_current(&state, &authority) => {
                    if !current {
                        source_guard.request_revocation();cancel.cancel();
                        let _=super::shared_source_playback::settle_ingress_custody(&state,source_guard.assignment(),source_guard.retirement_deadline(),crate::sharing_connection_custody::DriverCloseMode::Revoke).await;
                        break;
                    }
                },
            }
        }
        // EOF only releases the stream's copy. Queued Hyper DATA remains owned
        // until the accepted connection future and its writer have dropped, and
        // a retiring Source must still be able to close that writer. Both are
        // events; nothing is polled here.
        tokio::select! {
            () = closed.wait() => {},
            () = source_guard.cancelled() => {
                let mode=source_guard.retirement_mode();
                match mode {crate::sharing_connection_custody::DriverCloseMode::Drain=>drain.cancel(),crate::sharing_connection_custody::DriverCloseMode::Revoke=>cancel.cancel()}
                let _=super::shared_source_playback::settle_ingress_custody(&state,source_guard.assignment(),source_guard.retirement_deadline(),mode).await;
                // A cancellation request is not an accepted-writer join receipt.
                closed.wait().await;
            },
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
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(MonitoredBody {
            inner: body,
            _permit: permit,
            _live: live.drop_guard(),
        }),
    )
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
/// A pre-session file asset body: file-scoped like a decision, for the login
/// that reached it (the viewer's own, or the exact session's original login).
pub(super) async fn attach_receiver_file_authority(
    state: &AppState,
    login_hash: &str,
    user: i64,
    summary: &plurx_core::sharing::ImportSummary,
    reference: &plurx_core::sharing_file_locators::FileLocatorReference,
    response: &mut Response,
) -> Result<(), ApiError> {
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
    let authority = ReceiverContentAuthority {
        access: ReceiverAccess::Viewer,
        hash: login_hash.to_owned(),
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
/// Every receiver catalogue and artwork body is complete in memory when its
/// handler returns, so this pre-return check is its authority point and no
/// monitor follows the response. Receiver media bodies are owned by the
/// receiver playback actor instead.
pub(super) async fn receiver_content_guard(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
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
    if !receiver_content_current(&state, &authority).await {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "sharing_body_authority_unavailable",
        )
        .into_response();
    }
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
        .route(
            "/shared/imports/{import}/items/{item}/watched",
            post(viewer_watched),
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
    // The Source answered "not in your scope" (deleted, moved out or
    // unexported). That is a typed absence, not an offline Source.
    if matches!(
        error,
        crate::sharing_client::PeerError::Authentication
            | crate::sharing_client::PeerError::Rejected(StatusCode::NOT_FOUND)
    ) {
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
/// A Source cursor refusal must stay a typed reopen result at B. Mapping it to
/// "Source unavailable" left an expired or substituted cursor retried forever.
fn page_failure(error: crate::sharing_client::PeerError, cursor: bool) -> ApiError {
    use crate::sharing_client::PeerError::Rejected;
    match error {
        Rejected(StatusCode::GONE) if cursor => fail(StatusCode::GONE, "sharing_cursor_expired"),
        Rejected(StatusCode::CONFLICT) if cursor => {
            fail(StatusCode::CONFLICT, "sharing_query_changed")
        }
        // B already validated every other query field the Source checks.
        Rejected(StatusCode::BAD_REQUEST) if cursor => {
            fail(StatusCode::BAD_REQUEST, "sharing_cursor_invalid")
        }
        other => peer_failure(other),
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
    let had_cursor = q.cursor.is_some();
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
        .map_err(|error| page_failure(error, had_cursor))?;
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
    let presentation = details.presentation;
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
    // This detail was just read from the Source under current authority. A
    // file can be launched exactly when B issued its signed locator; this
    // is launch capability, never a claim that a producer is ready.
    let delivery_status = if files.iter().any(|file| file.get("file_base").is_some()) {
        "available"
    } else {
        "unavailable"
    };
    let mut item = shared_item(&summary, metadata, user.id, art_key.as_ref());
    if let Some(presentation) = presentation {
        item["presentation"] = json!(presentation);
    }
    receiver_json(&state,&token,user.id,vec![scope],
        json!({"item":item,"files":files,"lifecycle_generation":summary.lifecycle_generation,"watch":progress,"delivery_status":delivery_status})).await
}
async fn viewer_progress(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    super::extract::RawToken(token): super::extract::RawToken,
    Path((import, item)): Path<(String, String)>,
    body: Body,
) -> Result<Response, ApiError> {
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(15), to_bytes(body, 1024))
        .await
        .map_err(|_| invalid())?
        .map_err(|_| invalid())?;
    let beat = super::shared_receiver_playback::ReceiverProgressBeat::parse(&bytes)
        .map_err(|_| invalid())?;
    let session = uuid::Uuid::parse_str(&beat.session_id).map_err(|_| invalid())?;
    let actor = state
        .sharing
        .receiver_starts
        .by_session(session)
        .ok_or_else(|| {
            fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_progress_session_binding_unavailable",
            )
        })?;
    let (outcome, current) = actor
        .record_progress(
            &state,
            user.id,
            &plurx_core::auth::hash_token(&token),
            import_id(&import)?,
            &source_id(&item)?,
            &beat,
        )
        .await
        .map_err(|_| {
            fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_progress_session_binding_unavailable",
            )
        })?;
    use plurx_core::sharing_receiver_progress::ReceiverProgressOutcome;
    let (status, value) = match outcome {
        ReceiverProgressOutcome::Applied | ReceiverProgressOutcome::Replay => {
            (StatusCode::OK, json!({"sequence":beat.sequence}))
        }
        ReceiverProgressOutcome::Stale => (
            StatusCode::CONFLICT,
            json!({"code":"sharing_progress_stale","current_sequence":current}),
        ),
        ReceiverProgressOutcome::Conflict => (
            StatusCode::CONFLICT,
            json!({"code":"sharing_progress_conflict","current_sequence":current}),
        ),
        ReceiverProgressOutcome::Refused => {
            return Err(fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_progress_session_binding_unavailable",
            ))
        }
    };
    Ok((
        status,
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(value),
    )
        .into_response())
}
/// Explicit B-private watched state. Current Source membership comes from one
/// fresh pinned read under current assignment; the Store write then takes the
/// next global history sequence, so a beat issued before this override cannot
/// restore the old position. Source history and Local watch state are untouched.
async fn viewer_watched(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    Path((import, item)): Path<(String, String)>,
    body: Body,
) -> Result<Response, ApiError> {
    use plurx_core::store::sharing_catalogue::{RemoteProgressOutcome, RemoteWatchedOverride};
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct WatchedBody {
        watched: bool,
    }
    let import = import_id(&import)?;
    let item = source_id(&item)?;
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(15), to_bytes(body, 64))
        .await
        .map_err(|_| invalid())?
        .map_err(|_| invalid())?;
    let WatchedBody { watched } = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let user = user.id;
    let (summary, metadata) = current_viewer_item(&state, user, import, item.clone()).await?;
    if !matches!(
        metadata.kind,
        SourceItemKind::Movie | SourceItemKind::Episode
    ) {
        return Err(fail(StatusCode::CONFLICT, "sharing_watch_unsupported"));
    }
    let (outcome, watch) = state
        .store
        .set_remote_watched(RemoteWatchedOverride {
            import_id: import,
            library_id: metadata.library_id,
            item_id: item,
            user_id: user,
            lifecycle_generation: summary.lifecycle_generation,
            assignment_generation: summary.assignment_generation,
            watched,
            updated_at_ms: clock_ms(),
        })
        .await
        .map_err(unavailable)?;
    match (outcome, watch) {
        (RemoteProgressOutcome::Applied, Some(watch)) => Ok((
            StatusCode::OK,
            [(axum::http::header::CACHE_CONTROL, "no-store")],
            Json(json!({"updated":1,"watch":watch})),
        )
            .into_response()),
        (RemoteProgressOutcome::Conflict, _) => Err(fail(
            StatusCode::CONFLICT,
            "sharing_history_library_changed",
        )),
        _ => Err(missing()),
    }
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
mod tests;
