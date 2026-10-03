//! Closed source catalogue presentation. Internal records cannot be serialized.
use super::{error::ApiError, sharing};
use crate::state::{clock_ms, AppState};
use axum::{
    body::{to_bytes, Body},
    extract::{Path, RawQuery, State},
    http::{HeaderMap, StatusCode},
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

pub(crate) fn peer_router() -> Router<AppState> {
    Router::new()
        .route("/sharing/v1/libraries", get(libraries))
        .route("/sharing/v1/libraries/{id}/items", get(items))
        .route("/sharing/v1/items/{id}", get(item))
        .route("/sharing/v1/items/{id}/children", get(children))
        .route("/sharing/v1/items:batch", post(batch))
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
) -> Result<Json<Value>, ApiError> {
    let (hash, grant) = authority(&state, &headers).await?;
    let libraries = state
        .store
        .source_catalogue_libraries(&hash, grant)
        .await
        .map_err(unavailable)?
        .ok_or_else(missing)?;
    Ok(Json(
        json!({"libraries":libraries.into_iter().map(|l|json!({"library_id":l.library_id,"name":l.name,"kind":l.kind,"anime":l.anime})).collect::<Vec<_>>()}),
    ))
}
async fn items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, ApiError> {
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
) -> Result<Json<Value>, ApiError> {
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
) -> Result<Json<Value>, ApiError> {
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
            library_id,
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
    Ok(Json(
        json!({"items":page.records.into_iter().map(|r|r.item).collect::<Vec<_>>(),"next_cursor":next_cursor,"catalogue_revision":page.counters.library_revision,"scope_generation":page.counters.scope_generation,"catalogue_generation":page.counters.catalogue_generation}),
    ))
}
async fn item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
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
    Ok(Json(
        serde_json::to_value(record.item).map_err(|_| invalid())?,
    ))
}
async fn batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<Value>, ApiError> {
    let (hash, grant) = authority(&state, &headers).await?;
    let bytes = to_bytes(body, 16 * 1024).await.map_err(|_| invalid())?;
    let request: MetadataBatch = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let entries = state
        .store
        .source_catalogue_batch(&hash, grant, request)
        .await
        .map_err(unavailable)?
        .ok_or_else(missing)?;
    Ok(Json(
        json!({"items":entries.into_iter().map(|e|json!({"item_id":e.item_id,"item":e.record.map(|r|r.item)})).collect::<Vec<_>>()}),
    ))
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
