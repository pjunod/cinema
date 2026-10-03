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
    Ok((hash, grant.grant.id))
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowseQuery {
    q: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}
fn query(raw: Option<&str>) -> Result<BrowseQuery, ApiError> {
    let raw = raw.unwrap_or("");
    if raw.len() > 6144 {
        return Err(invalid());
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
